//! The ADR-0031 gate, run against the **real** pinned engine over the **real**
//! wire protocol: a local test page is loaded, JavaScript round-trips both ways,
//! a web button is tapped by its accessibility text, frames are timed at
//! 1080×2400@2.625, and PSS is measured for 1 vs 3 browser contexts.
//!
//! Guard requirements, because this starts a Chromium and is heavy:
//!
//! ```text
//! tools/art-runtime/run-exclusive.sh --task WVB -- tools/guard/guarded.sh --mem 8G --time 420 -- \
//!   env ACOS_WEBVIEW_TEST=1 CARGO_TARGET_DIR=$PWD/.cache/wvb-target \
//!   cargo test -p acos-webview --test engine_gate -- --nocapture --test-threads=1
//! ```
//!
//! Without `ACOS_WEBVIEW_TEST=1` the test prints a loud SKIP and returns — it
//! never reports a pass it did not measure.

use std::collections::HashMap;
use std::io::{BufReader, Read, Write};
use std::net::TcpListener;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use acos_webview::broker::{Broker, BrokerConfig};
use acos_webview::engine::{self, EngineCache};
use acos_webview::server::{serve, ServerConfig};
use acos_webview::view::Viewport;
use acos_webview::wire::{read_frame, write_frame, Blob, Frame};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// A minimal guest: it speaks WIRE.md over a Unix socket, answers host requests
// from a handler closure, and records what the page asked for.
// ---------------------------------------------------------------------------

type Handler = Box<dyn Fn(&str, &Value) -> Value + Send + Sync>;

struct Pending {
    tx: SyncSender<Vec<Frame>>,
    wants_blob: bool,
}

struct Guest {
    writer: Arc<Mutex<UnixStream>>,
    pending: Arc<Mutex<HashMap<String, Pending>>>,
    next: Mutex<u32>,
    closed: Arc<AtomicBool>,
    /// Every host→guest request seen, for assertions about the bridge.
    seen: Arc<Mutex<Vec<(String, Value)>>>,
}

impl Guest {
    fn connect(path: &std::path::Path, handler: Handler) -> Arc<Self> {
        let stream = UnixStream::connect(path).expect("connect to broker");
        let read_half = stream.try_clone().expect("clone");
        let guest = Arc::new(Self {
            writer: Arc::new(Mutex::new(stream)),
            pending: Arc::new(Mutex::new(HashMap::new())),
            next: Mutex::new(1),
            closed: Arc::new(AtomicBool::new(false)),
            seen: Arc::new(Mutex::new(Vec::new())),
        });
        let pending = Arc::clone(&guest.pending);
        let closed = Arc::clone(&guest.closed);
        let seen = Arc::clone(&guest.seen);
        let writer = Arc::clone(&guest.writer);
        std::thread::Builder::new()
            .name("test-guest-reader".into())
            .spawn(move || {
                let mut reader = BufReader::new(read_half);
                let mut awaiting_blob: Option<(String, Value)> = None;
                loop {
                    match read_frame(&mut reader) {
                        Ok(Frame::Json(msg)) => {
                            let id = msg["id"].as_str().unwrap_or("").to_string();
                            if msg.get("op").is_some() && !id.is_empty() && id.starts_with('h') {
                                seen.lock().unwrap().push((id.clone(), msg.clone()));
                                let op = msg["op"].as_str().unwrap_or("").to_string();
                                let answer = handler(&op, &msg);
                                let mut out = json!({"id": id, "ok": true, "result": answer});
                                if answer.get("__error").is_some() {
                                    out = json!({"id": id, "ok": false, "error": {
                                        "code": answer["__error"],
                                        "message": answer["__message"],
                                    }});
                                }
                                let bytes = acos_webview::wire::encode_json(&out).unwrap();
                                let mut w = writer.lock().unwrap();
                                if write_frame(&mut *w, &bytes).is_err() {
                                    break;
                                }
                                continue;
                            }
                            if id.is_empty() {
                                continue; // event
                            }
                            let wants = pending
                                .lock()
                                .unwrap()
                                .get(&id)
                                .map(|p| p.wants_blob)
                                .unwrap_or(false);
                            if wants {
                                awaiting_blob = Some((id, msg));
                            } else if let Some(p) = pending.lock().unwrap().remove(&id) {
                                let _ = p.tx.send(vec![Frame::Json(msg)]);
                            }
                        }
                        Ok(Frame::Blob(b)) => {
                            if let Some((id, msg)) = awaiting_blob.take() {
                                if let Some(p) = pending.lock().unwrap().remove(&id) {
                                    let _ = p.tx.send(vec![Frame::Json(msg), Frame::Blob(b)]);
                                }
                            }
                        }
                        Err(_) => break,
                    }
                }
                closed.store(true, Ordering::SeqCst);
                for (_, p) in pending.lock().unwrap().drain() {
                    let _ = p.tx.send(vec![]);
                }
            })
            .expect("reader thread");
        guest
    }

    fn call_inner(&self, op: &str, params: Value, wants_blob: bool) -> Vec<Frame> {
        let id = {
            let mut n = self.next.lock().unwrap();
            let id = format!("g{}", *n);
            *n += 1;
            id
        };
        let (tx, rx) = sync_channel(1);
        self.pending
            .lock()
            .unwrap()
            .insert(id.clone(), Pending { tx, wants_blob });
        let mut msg = params;
        msg["id"] = json!(id);
        msg["op"] = json!(op);
        {
            let mut w = self.writer.lock().unwrap();
            let bytes = acos_webview::wire::encode_json(&msg).unwrap();
            write_frame(&mut *w, &bytes).expect("write request");
        }
        match rx.recv_timeout(Duration::from_secs(120)) {
            Ok(frames) if !frames.is_empty() => frames,
            Ok(_) => panic!("{op}: broker closed the connection"),
            Err(e) => panic!("{op}: no response ({e})"),
        }
    }

    fn call(&self, op: &str, params: Value) -> Value {
        match self.call_inner(op, params, false).remove(0) {
            Frame::Json(v) => {
                assert_eq!(v["ok"], json!(true), "{op} failed: {}", v["error"]);
                v["result"].clone()
            }
            other => panic!("expected a JSON response, got {other:?}"),
        }
    }

    /// Expect a structured error (never a silent success).
    fn call_expect_error(&self, op: &str, params: Value) -> (String, String) {
        match self.call_inner(op, params, false).remove(0) {
            Frame::Json(v) => {
                assert_eq!(v["ok"], json!(false), "{op} unexpectedly succeeded: {v}");
                (
                    v["error"]["code"].as_str().unwrap_or("").to_string(),
                    v["error"]["message"].as_str().unwrap_or("").to_string(),
                )
            }
            other => panic!("expected a JSON response, got {other:?}"),
        }
    }

    fn render(&self, view: i64) -> (Value, Blob) {
        let frames = self.call_inner("render", json!({"view": view, "format": "png"}), true);
        assert_eq!(frames.len(), 2, "render must emit JSON + BLOB");
        let meta = match &frames[0] {
            Frame::Json(v) => {
                assert_eq!(v["ok"], json!(true), "render failed: {}", v["error"]);
                v["result"].clone()
            }
            other => panic!("expected JSON first, got {other:?}"),
        };
        match &frames[1] {
            Frame::Blob(b) => (meta, b.clone()),
            other => panic!("expected BLOB second, got {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------------
// The local test page's HTTP server (the page is "served by the test").
// ---------------------------------------------------------------------------

const PAGE: &str = r#"<!doctype html><html><head><meta charset="utf-8">
<title>acos-wv-test</title></head><body style="margin:0;font:16px sans-serif">
<h1 id="h">WebView gate</h1>
<button id="b" style="display:block;width:240px;height:80px;font-size:20px"
  onclick="document.getElementById('status').textContent='tapped'">Tap me</button>
<div id="status" style="height:40px">idle</div>
<div id="n">hello-page</div>
</body></html>"#;

fn serve_page() -> (u16, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind test page");
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).unwrap();
    let handle = std::thread::spawn(move || {
        let stop = Instant::now() + Duration::from_secs(180);
        while Instant::now() < stop {
            match listener.accept() {
                Ok((mut sock, _)) => {
                    let mut buf = [0u8; 2048];
                    let _ = sock.read(&mut buf);
                    let body = PAGE.as_bytes();
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = sock.write_all(resp.as_bytes());
                    let _ = sock.write_all(body);
                    let _ = sock.flush();
                }
                Err(_) => std::thread::sleep(Duration::from_millis(20)),
            }
        }
    });
    (port, handle)
}

// ---------------------------------------------------------------------------

fn evidence_dir() -> PathBuf {
    let dir = std::env::var_os("ACOS_WEBVIEW_EVIDENCE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .and_then(|p| p.parent())
                .map(|root| root.join("results/handoff/WVB/evidence"))
                .unwrap_or_else(|| PathBuf::from("evidence"))
        });
    std::fs::create_dir_all(&dir).ok();
    dir
}

#[test]
fn engine_gate() {
    if acos_test_support::require_prereq(
        std::env::var("ACOS_WEBVIEW_TEST").as_deref() == Ok("1"),
        "engine_gate",
        "set ACOS_WEBVIEW_TEST=1 to run this heavy Chromium test (the release gate runs it with the env set; see tools/verify/test-prereq-optouts.txt)",
    )
    .is_none()
    {
        return;
    }
    let mut report = json!({"test": "engine_gate", "startedAt": unix_now()});
    let cache = EngineCache::default_cache();
    eprintln!("engine cache: {}", cache.root().display());
    // Installing on first use is part of the deliverable; the gate run may
    // therefore download ~122 MiB once (checksum pinned).
    let bin = cache.ensure().expect("engine install");
    let verified = cache.cached().expect("verified install");
    eprintln!(
        "engine: {} ({})",
        bin.display(),
        engine::version_of(&bin).expect("version probe")
    );
    report["engine"] = json!({
        "path": bin.display().to_string(),
        "version": engine::ENGINE_VERSION,
        "zipSha256": verified.zip_sha256,
        "binarySha256": verified.binary_sha256,
        "binaryBytes": verified.binary_size,
    });

    let root = cache
        .root()
        .join("run")
        .join(format!("gate-{}", std::process::id()));
    std::fs::create_dir_all(&root).expect("run root");
    let mut cfg = BrokerConfig::new(EngineCache::new(cache.root()));
    cfg.run_root = root.clone();
    // Short enough that the idle-shutdown arm of the lifecycle is measured in
    // this run instead of being asserted by reading the code.
    cfg.idle_shutdown = Duration::from_secs(3);
    let broker = Broker::new(cfg);
    let socket = root.join("webview.sock");
    let server = serve(
        Arc::clone(&broker),
        &ServerConfig {
            socket_path: socket.clone(),
            session_id: "gate".to_string(),
        },
    )
    .expect("serve");

    // Guest-side handler: the asset provider and the JS bridge.
    let seen = Arc::new(Mutex::new(Vec::<(String, Value)>::new()));
    let seen_for_handler = Arc::clone(&seen);
    let handler: Handler = Box::new(move |op, msg| {
        seen_for_handler
            .lock()
            .unwrap()
            .push((op.to_string(), msg.clone()));
        match op {
            "nav.shouldOverride" => json!({"allow": true}),
            "js.call" => {
                let args = msg["argsJson"].as_str().unwrap_or("[]");
                let method = msg["method"].as_str().unwrap_or("");
                if method == "echo" {
                    json!({"resultJson": format!("{args}")})
                } else if method == "fail" {
                    json!({"__error": "JS_EXCEPTION", "__message": "asked to fail"})
                } else {
                    json!({"resultJson": "null"})
                }
            }
            "req.intercept" => {
                let url = msg["url"].as_str().unwrap_or("");
                let (body, mime) = if url.ends_with("/index.html") {
                    (
                        "<!doctype html><html><head><title>asset-start</title></head><body>\
                         <p id=\"a\">asset page</p><script src=\"app.js\"></script></body></html>",
                        "text/html",
                    )
                } else if url.ends_with("/app.js") {
                    ("document.title = 'asset-js-ok';", "text/javascript")
                } else {
                    return json!({"action": "fail", "reason": "ERR_FILE_NOT_FOUND"});
                };
                json!({
                    "action": "fulfill",
                    "status": 200,
                    "mime": mime,
                    "headers": {"access-control-allow-origin": "*"},
                    "bodyB64": acos_webview::server::asset_body_b64(body.as_bytes()),
                })
            }
            other => json!({"__error": "UNSUPPORTED", "__message": format!("test guest: {other}")}),
        }
    });
    let guest = Guest::connect(&socket, handler);

    // --- hello ------------------------------------------------------------
    let hello = guest.call("hello", json!({"proto": 2, "client": "gate/1"}));
    assert_eq!(hello["proto"], json!(2));
    assert_eq!(
        hello["engine"]["version"].as_str(),
        Some(engine::ENGINE_VERSION),
        "engine version must be the pinned one: {hello}"
    );
    report["hello"] = hello.clone();
    report["engine"]["identity"] = hello["engine"].clone();
    report["engine"]["userAgent"] = hello["engine"]["userAgent"].clone();
    let vp = Viewport::new(1080, 2400, 2.625).unwrap();

    // Fixed engine cost before any view exists.
    let mem_fixed = guest.call("memory", json!({}));
    eprintln!("fixed engine cost: {mem_fixed}");
    report["memory"] = json!({"fixed": mem_fixed["engine"].clone()});

    // --- load the local page ---------------------------------------------
    let (port, _page_server) = serve_page();
    let url = format!("http://127.0.0.1:{port}/");
    let create = guest.call(
        "view.create",
        json!({"view": 1, "width": vp.width, "height": vp.height, "density": vp.density, "url": "about:blank"}),
    );
    assert_eq!(create["width"], json!(1080));
    let t_load = Instant::now();
    let loaded = guest.call("view.loadUrl", json!({"view": 1, "url": url}));
    let load_ms = t_load.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(
        loaded["title"],
        json!("acos-wv-test"),
        "page title: {loaded}"
    );
    assert_eq!(loaded["url"], json!(url), "final url: {loaded}");
    eprintln!("load: {loaded} in {load_ms:.1} ms");
    report["gate_a_load"] =
        json!({"url": url, "title": loaded["title"], "loadMs": round1(load_ms), "result": loaded});

    // --- JS host → page ---------------------------------------------------
    let eval = guest.call(
        "view.evaluateJavascript",
        json!({"view": 1, "script": "document.getElementById('n').textContent"}),
    );
    assert_eq!(eval["valueJson"], json!("\"hello-page\""), "eval: {eval}");
    assert_eq!(eval["type"], json!("string"));
    report["gate_b_host_to_page"] = eval.clone();

    // --- JS page → host → page (JS interface, both ways) ------------------
    guest.call(
        "view.addJavascriptInterface",
        json!({"view": 1, "name": "acosEcho", "methods": ["echo", "fail"]}),
    );
    let round = guest.call(
        "view.evaluateJavascript",
        json!({"view": 1, "script": "window.acosEcho.echo('ping').then(r => r[0])"}),
    );
    assert_eq!(
        round["valueJson"],
        json!("\"ping\""),
        "bridge round trip: {round}"
    );
    // The provider must have seen a js.call with the method and args.
    let calls: Vec<Value> = seen
        .lock()
        .unwrap()
        .iter()
        .filter(|(op, _)| op == "js.call")
        .map(|(_, m)| m.clone())
        .collect();
    assert_eq!(
        calls.len(),
        1,
        "expected exactly one js.call, saw {calls:?}"
    );
    assert_eq!(calls[0]["method"], json!("echo"));
    assert_eq!(calls[0]["argsJson"], json!("[\"ping\"]"));
    // A provider-side error must reject the page promise, not resolve it.
    let failed = guest.call(
        "view.evaluateJavascript",
        json!({"view": 1, "script": "window.acosEcho.fail().then(() => 'resolved', e => 'rejected:' + e.message)"}),
    );
    assert_eq!(
        failed["valueJson"],
        json!("\"rejected:JS_EXCEPTION: asked to fail\""),
        "provider errors must reject the promise: {failed}"
    );
    report["gate_b_page_to_host"] = json!({"value": round["valueJson"], "jsCall": calls[0],
        "errorPropagation": failed["valueJson"]});

    // --- frames at 1080x2400@2.625 ---------------------------------------
    let mut times = Vec::new();
    let mut last_blob: Option<Blob> = None;
    for _ in 0..20 {
        let t0 = Instant::now();
        let (meta, blob) = guest.render(1);
        times.push(t0.elapsed().as_secs_f64() * 1000.0);
        // WV7: the wire reports the frame's REAL pixel size. This engine
        // cannot emit a device-resolution capture under begin-frame control
        // (`beginFrame`'s screenshot is CSS-sized and ignores
        // deviceScaleFactor; `Page.captureScreenshot` never answers), so a
        // 1080x2400 @2.625 View renders 411x914 CSS pixels. The guest scales
        // them into the View; the wire never claims pixels the PNG lacks.
        assert_eq!(meta["width"], json!(411));
        assert_eq!(meta["height"], json!(914));
        assert_eq!(meta["format"], json!("png"));
        assert_eq!(meta["bytes"], json!(blob.data.len()));
        assert!(blob.data.starts_with(b"\x89PNG"), "frame is not a PNG");
        assert!(
            meta["damage"]["w"].as_u64().unwrap_or(0) > 0
                || !meta["hasDamage"].as_bool().unwrap_or(true)
        );
        last_blob = Some(blob);
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p50 = times[times.len() / 2];
    let p95 = times[(times.len() * 95 / 100).min(times.len() - 1)];
    let max = *times.last().unwrap();
    eprintln!("frames: p50 {p50:.2} ms, p95 {p95:.2} ms, max {max:.2} ms");
    assert!(p95 < 50.0, "frame p95 {p95:.2} ms exceeds the 50 ms gate");
    report["gate_d_frames"] = json!({
        "viewport": {"width": 1080, "height": 2400, "density": 2.625},
        "n": times.len(), "p50Ms": round1(p50), "p95Ms": round1(p95), "maxMs": round1(max),
        "pngBytes": last_blob.as_ref().map(|b| b.data.len()),
        "gateMs": 50, "pass": p95 < 50.0,
    });

    // --- tap a web button by its AX text ---------------------------------
    let ax = guest.call("ax", json!({"view": 1}));
    let nodes = ax["nodes"].as_array().cloned().unwrap_or_default();
    assert!(
        ax["count"].as_u64().unwrap_or(0) > 3,
        "ax tree too small: {ax}"
    );
    let (node, x, y) = acos_webview::ax::find_by_text(&nodes, "Tap me")
        .unwrap_or_else(|| panic!("no 'Tap me' node in AX tree: {nodes:#?}"));
    assert!(
        x > 0.0 && y > 0.0,
        "AX bounds must be display pixels, got ({x},{y})"
    );
    eprintln!(
        "ax tap: role={} name={:?} bounds={:?} centre=({x:.1},{y:.1})",
        node["role"], node["name"], node["bounds"]
    );
    guest.call("input.tap", json!({"view": 1, "x": x, "y": y}));
    let mut status = json!(null);
    for _ in 0..40 {
        std::thread::sleep(Duration::from_millis(50));
        status = guest.call(
            "view.evaluateJavascript",
            json!({"view": 1, "script": "document.getElementById('status').textContent"}),
        );
        if status["valueJson"] == json!("\"tapped\"") {
            break;
        }
    }
    assert_eq!(
        status["valueJson"],
        json!("\"tapped\""),
        "tap did not change the page: {status}"
    );
    report["gate_c_ax_tap"] = json!({
        "node": node, "centre": {"x": round1(x), "y": round1(y)},
        "axCount": ax["count"], "statusAfterTap": status["valueJson"],
    });

    // --- guest-served assets over the same channel ------------------------
    guest.call(
        "view.create",
        json!({"view": 2, "width": vp.width, "height": vp.height, "density": vp.density, "url": "about:blank"}),
    );
    let asset = guest.call(
        "view.loadUrl",
        json!({"view": 2, "url": "file:///android_asset/index.html"}),
    );
    let title = guest.call(
        "view.evaluateJavascript",
        json!({"view": 2, "script": "document.title"}),
    );
    assert_eq!(
        title["valueJson"],
        json!("\"asset-js-ok\""),
        "asset page/script: {title}"
    );
    let intercepts = seen
        .lock()
        .unwrap()
        .iter()
        .filter(|(op, _)| op == "req.intercept")
        .count();
    assert!(
        intercepts >= 2,
        "expected the asset page + script to be intercepted, saw {intercepts}"
    );
    report["asset_interception"] =
        json!({"url": asset["url"], "title": title["valueJson"], "intercepts": intercepts});

    // --- PSS: 1 vs 3 browser contexts ------------------------------------
    std::thread::sleep(Duration::from_millis(500));
    let mem1 = guest.call("memory", json!({}));
    guest.call("view.destroy", json!({"view": 2}));
    // Two more *sessions* = two more browser contexts, each with one view.
    let mut extra: Vec<(Arc<Guest>, acos_webview::ServerHandle, i64)> = Vec::new();
    for (i, view_id) in [(1i64, 3i64), (2, 4)] {
        let sock = root.join(format!("webview-{i}.sock"));
        let srv = serve(
            Arc::clone(&broker),
            &ServerConfig {
                socket_path: sock.clone(),
                session_id: format!("gate-{i}"),
            },
        )
        .expect("serve extra session");
        let handler2: Handler = Box::new(|op, msg| match op {
            "nav.shouldOverride" => json!({"allow": true}),
            "req.intercept" => {
                let _ = msg;
                json!({"action": "continue"})
            }
            _ => json!({"action": "continue"}),
        });
        let g = Guest::connect(&sock, handler2);
        g.call("hello", json!({"proto": 2, "client": "gate/2"}));
        g.call(
            "view.create",
            json!({"view": view_id, "width": 1080, "height": 2400, "density": 2.625, "url": url}),
        );
        extra.push((g, srv, view_id));
    }
    std::thread::sleep(Duration::from_millis(800));
    let mem3 = guest.call("memory", json!({"detail": true}));
    let pss1 = mem1["engine"]["pssMiB"].as_f64().unwrap_or(0.0);
    let pss3 = mem3["engine"]["pssMiB"].as_f64().unwrap_or(0.0);
    let per_session = (pss3 - pss1) / 2.0;
    eprintln!(
        "PSS: 1 context {pss1:.1} MiB, 3 contexts {pss3:.1} MiB, per extra {per_session:.1} MiB"
    );
    assert!(
        per_session <= 60.0,
        "per-session PSS {per_session:.1} MiB exceeds the 60 MiB gate"
    );
    report["gate_e_memory"] = json!({
        "pss1MiB": pss1, "pss3MiB": pss3, "perExtraContextMiB": round1(per_session),
        "fixedEngineMiB": mem_fixed["engine"]["pssMiB"], "gateMiB": 60,
        "pass": per_session <= 60.0, "contexts": 3, "targets": mem3["targets"],
        "processes": mem3["engine"]["processes"],
    });

    // --- structured failures (never a silent success) ---------------------
    let (code, _msg) = guest.call_expect_error(
        "view.evaluateJavascript",
        json!({"view": 99, "script": "1"}),
    );
    assert_eq!(code, "NO_VIEW");
    let (code, _msg) = guest.call_expect_error("bogus.op", json!({"view": 1}));
    assert_eq!(code, "UNSUPPORTED");
    let (code, _msg) = guest.call_expect_error(
        "view.setViewport",
        json!({"view": 1, "width": 0, "height": 10, "density": 1.0}),
    );
    assert_eq!(code, "INVALID_VIEWPORT");
    guest.call("view.destroy", json!({"view": 1}));
    let (code, _msg) = guest.call_expect_error("view.destroy", json!({"view": 1}));
    assert_eq!(code, "NO_VIEW");
    report["structured_errors"] = json!(["NO_VIEW", "UNSUPPORTED", "INVALID_VIEWPORT"]);

    // --- idle shutdown then lazy restart ---------------------------------
    for (g, _srv, view_id) in &extra {
        g.call("view.destroy", json!({"view": view_id}));
    }
    let pid_before = broker.engine_pid().expect("engine pid while up");
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut down = Value::Null;
    while Instant::now() < deadline {
        down = guest.call("engineStatus", json!({}));
        if down["state"] == json!("down") {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    assert_eq!(
        down["state"],
        json!("down"),
        "engine did not idle-shutdown: {down}"
    );
    assert!(
        !std::path::Path::new(&format!("/proc/{pid_before}")).exists(),
        "engine process {pid_before} is still alive after idle shutdown"
    );
    eprintln!("idle shutdown ok (pid {pid_before} gone)");
    // A fresh hello re-creates the session on a new engine generation.
    let hello2 = guest.call("hello", json!({"proto": 2, "client": "gate/3"}));
    assert_eq!(hello2["engine"]["version"], json!(engine::ENGINE_VERSION));
    guest.call(
        "view.create",
        json!({"view": 7, "width": 1080, "height": 2400, "density": 2.625, "url": "about:blank"}),
    );
    let after = guest.call("engineStatus", json!({}));
    assert_eq!(
        after["state"],
        json!("up"),
        "engine did not restart: {after}"
    );
    let v = guest.call(
        "view.evaluateJavascript",
        json!({"view": 7, "script": "1 + 1"}),
    );
    assert_eq!(v["valueJson"], json!("2"));
    eprintln!("lazy restart ok: {after}");
    report["lifecycle"] = json!({
        "idleShutdown": "down", "pidReaped": pid_before, "restart": after,
        "restarts": after["restarts"],
    });

    report["finishedAt"] = json!(unix_now());
    report["status"] = json!("PASS");
    let path = evidence_dir().join("gate.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&report).unwrap()).expect("write evidence");
    eprintln!("evidence: {}", path.display());

    // Teardown: stop the engine, prove the process is gone.
    let pid = broker.engine_pid();
    server.stop();
    broker.shutdown();
    drop(extra);
    drop(guest);
    std::thread::sleep(Duration::from_millis(500));
    if let Some(pid) = pid {
        assert!(
            !std::path::Path::new(&format!("/proc/{pid}")).exists(),
            "engine process {pid} outlived shutdown"
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}
