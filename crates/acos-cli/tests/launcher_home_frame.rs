//! Multi-process compositor cache regression test (`system.launcher:real` + `counter.apk`).
//!
//! When `counter.apk` (process tag 0) and `com.acos.launcher` (process tag 1)
//! run in separate bwrap PID namespaces, `GraphicBuffer::getId()` produces the
//! same ids (`0x2_0000_0000`, ...) in both processes. Before keying
//! `SurfaceState::buffer_cache` by `(cached_token, buffer_id)`, pressing Home
//! switched the UI tree to `ACOS Home` while every screenshot and `acos view`
//! frame stayed stuck on Counter's pixels.
//!
//! Ignored by default because it boots real ART guests; run under
//! `run-exclusive.sh` + `guarded.sh`.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpStream};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

mod common;

use common::ShortSocketDir;

fn repo_root() -> PathBuf {
    let raw = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    std::fs::canonicalize(&raw).unwrap_or(raw)
}

fn acos_bin() -> &'static str {
    env!("CARGO_BIN_EXE_acos")
}

fn find_bin(name: &str, env_var: &str) -> Option<PathBuf> {
    if let Ok(p) = std::env::var(env_var) {
        let pb = PathBuf::from(p);
        if pb.is_file() {
            return Some(pb);
        }
    }
    let sibling = Path::new(acos_bin()).with_file_name(name);
    if sibling.is_file() {
        return Some(sibling);
    }
    None
}

fn find_fixture_apk(name: &str) -> PathBuf {
    let path = repo_root().join("fixtures/build/apps").join(name);
    assert!(path.is_file(), "fixture APK not found: {}", path.display());
    path
}

fn scratch(tag: &str) -> PathBuf {
    let dir = repo_root()
        .join(".cache")
        .join(format!("launcher-home-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("runtime")).unwrap();
    std::fs::create_dir_all(dir.join("cache")).unwrap();
    std::fs::create_dir_all(dir.join("viewreg")).unwrap();
    dir
}

struct Daemon {
    child: Child,
    socket: PathBuf,
    dir: PathBuf,
    viewreg: PathBuf,
    _sockets: ShortSocketDir,
}

impl Daemon {
    fn start(tag: &str) -> Self {
        let dir = scratch(tag);
        let viewreg = dir.join("viewreg");
        let sockets = ShortSocketDir::new(tag);
        let socket = sockets.socket("serve.sock");
        let mut cmd = Command::new(acos_bin());
        cmd.arg("serve")
            .arg("--socket")
            .arg(&socket)
            .arg("--idle-timeout")
            .arg("180")
            .env("ACOS_RUNTIME_DIR", dir.join("runtime"))
            .env("ACOS_CACHE_DIR", dir.join("cache"))
            .env("ACOS_MEMORY_GUARD", "off")
            .env("ACOS_VIEW_REGISTRY_DIR", &viewreg)
            .env_remove("ACOS_SESSION_SOCKET")
            .stdout(Stdio::null())
            .stderr(Stdio::from(
                std::fs::File::create(dir.join("serve.err")).unwrap(),
            ));
        if let Some(art) = find_bin("acos-art", "ACOS_ART_BIN") {
            cmd.env("ACOS_ART_BIN", art);
        }
        let child = cmd.spawn().expect("spawn acos serve");
        let deadline = Instant::now() + Duration::from_secs(20);
        while !socket.exists() {
            assert!(
                Instant::now() < deadline,
                "serve socket {} never appeared",
                socket.display()
            );
            std::thread::sleep(Duration::from_millis(25));
        }
        Self {
            child,
            socket,
            dir,
            viewreg,
            _sockets: sockets,
        }
    }

    fn connect(&self) -> Conn {
        Conn::new(&self.socket)
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

struct ViewHubProc {
    child: Child,
    port: u16,
    token: String,
}

impl ViewHubProc {
    fn start(viewreg: &Path) -> Self {
        let mut child = Command::new(acos_bin())
            .arg("view")
            .arg("--port")
            .arg("0")
            .arg("--json")
            .arg("--registry-dir")
            .arg(viewreg)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn acos view");
        let stdout = child.stdout.take().expect("view stdout");
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        reader.read_line(&mut line).expect("read acos view banner");
        let info: Value = serde_json::from_str(&line).expect("parse acos view JSON");
        let port = info["port"].as_u64().expect("port") as u16;
        let token = info["token"].as_str().expect("token").to_string();
        Self { child, port, token }
    }
}

impl Drop for ViewHubProc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Conn {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    next: u64,
}

impl Conn {
    fn new(path: &Path) -> Self {
        let stream = UnixStream::connect(path).expect("connect to serve socket");
        stream
            .set_read_timeout(Some(Duration::from_secs(120)))
            .unwrap();
        Self {
            writer: stream.try_clone().unwrap(),
            reader: BufReader::new(stream),
            next: 0,
        }
    }

    fn call(&mut self, tool: &str, args: Value) -> Value {
        self.next += 1;
        let req = json!({ "id": self.next, "tool": tool, "args": args });
        self.writer
            .write_all((req.to_string() + "\n").as_bytes())
            .unwrap();
        self.writer.flush().unwrap();
        let mut line = String::new();
        self.reader.read_line(&mut line).unwrap();
        let resp: Value = serde_json::from_str(&line).expect("response JSON");
        assert_eq!(resp["id"], json!(self.next));
        resp["envelope"].clone()
    }
}

#[derive(Clone, Debug)]
struct MjpegPart {
    stream_idx: u64,
    content_type: String,
    w: u32,
    h: u32,
    seq: u64,
    bytes: Vec<u8>,
}

struct MjpegStream {
    stop: Arc<AtomicBool>,
    state: Arc<(Mutex<VecDeque<MjpegPart>>, Condvar)>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl MjpegStream {
    fn connect(port: u16, token: &str, session_id: &str) -> Self {
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).expect("connect frames");
        stream
            .set_read_timeout(Some(Duration::from_millis(500)))
            .unwrap();
        write!(
            stream,
            "GET /api/frames/{session_id}?t={token} HTTP/1.1\r\n\
             Host: 127.0.0.1:{port}\r\n\r\n"
        )
        .unwrap();
        stream.flush().unwrap();

        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while stream.read(&mut byte).unwrap_or(0) == 1 {
            head.push(byte[0]);
            if head.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let head_str = String::from_utf8_lossy(&head);
        assert!(head_str.starts_with("HTTP/1.1 200"), "frames head: {head_str}");

        let stop = Arc::new(AtomicBool::new(false));
        let state = Arc::new((Mutex::new(VecDeque::new()), Condvar::new()));
        let stop_t = Arc::clone(&stop);
        let state_t = Arc::clone(&state);

        let handle = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 32 * 1024];
            let mut idx = 0u64;
            while !stop_t.load(Ordering::Relaxed) {
                match stream.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&chunk[..n]);
                        loop {
                            let Some(hdr_end) = buf.windows(4).position(|w| w == b"\r\n\r\n")
                            else {
                                break;
                            };
                            let hdr_str =
                                String::from_utf8_lossy(&buf[..hdr_end]).into_owned();
                            let mut content_type = String::new();
                            let mut content_length = 0usize;
                            let mut w = 0u32;
                            let mut h = 0u32;
                            let mut seq = 0u64;
                            for line in hdr_str.lines() {
                                if let Some((k, v)) = line.split_once(':') {
                                    let k = k.trim();
                                    let v = v.trim();
                                    if k.eq_ignore_ascii_case("content-type") {
                                        content_type = v.to_string();
                                    } else if k.eq_ignore_ascii_case("content-length") {
                                        content_length = v.parse().unwrap_or(0);
                                    } else if k.eq_ignore_ascii_case("x-frame-seq") {
                                        seq = v.parse().unwrap_or(0);
                                    } else if k.eq_ignore_ascii_case("x-frame-size") {
                                        if let Some((ws, hs)) = v.split_once('x') {
                                            w = ws.parse().unwrap_or(0);
                                            h = hs.parse().unwrap_or(0);
                                        }
                                    }
                                }
                            }
                            let body_start = hdr_end + 4;
                            let total_need = body_start + content_length + 2;
                            if buf.len() < total_need {
                                break;
                            }
                            let bytes = buf[body_start..body_start + content_length].to_vec();
                            buf.drain(..total_need);
                            if !content_type.is_empty() && !bytes.is_empty() {
                                idx += 1;
                                let (lock, cv) = &*state_t;
                                let mut q = lock.lock().unwrap();
                                while q.len() >= 32 {
                                    q.pop_front();
                                }
                                q.push_back(MjpegPart {
                                    stream_idx: idx,
                                    content_type,
                                    w,
                                    h,
                                    seq,
                                    bytes,
                                });
                                cv.notify_all();
                            }
                        }
                    }
                    Err(e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) =>
                    {
                        continue;
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            stop,
            state,
            handle: Some(handle),
        }
    }

    fn wait_match(
        &self,
        pred: impl Fn(&MjpegPart) -> bool,
        timeout: Duration,
        label: &str,
    ) -> MjpegPart {
        let (lock, cv) = &*self.state;
        let deadline = Instant::now() + timeout;
        let mut q = lock.lock().unwrap();
        loop {
            while let Some(part) = q.pop_front() {
                if pred(&part) {
                    return part;
                }
            }
            let now = Instant::now();
            assert!(now < deadline, "timed out waiting for MJPEG frame: {label}");
            let (guard, _) = cv.wait_timeout(q, deadline - now).unwrap();
            q = guard;
        }
    }
}

impl Drop for MjpegStream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn find_in_tree(node: &Value, text_contains: Option<&str>, id_suffix: Option<&str>) -> Option<Value> {
    if node.is_object() {
        let matches_text = text_contains.is_none_or(|needle| {
            node.get("text")
                .and_then(Value::as_str)
                .is_some_and(|s| s.contains(needle))
        });
        let matches_id = id_suffix.is_none_or(|suf| {
            node.get("id")
                .or_else(|| node.get("resource_id"))
                .and_then(Value::as_str)
                .is_some_and(|s| s == suf || s.ends_with(&format!(":id/{suf}")))
        });
        if matches_text && matches_id {
            return Some(node.clone());
        }
    }
    for key in ["result", "tree", "nodes", "children", "root"] {
        if let Some(child) = node.get(key) {
            if let Some(found) = find_in_tree(child, text_contains, id_suffix) {
                return Some(found);
            }
        }
    }
    if let Some(arr) = node.as_array() {
        for item in arr {
            if let Some(found) = find_in_tree(item, text_contains, id_suffix) {
                return Some(found);
            }
        }
    }
    None
}

fn decode_rgb(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
    let img = image::load_from_memory(bytes)
        .expect("decode PNG frame")
        .into_rgb8();
    let (w, h) = img.dimensions();
    (w, h, img.into_raw())
}

fn changed_pixels(a: &[u8], b: &[u8]) -> usize {
    assert_eq!(a.len(), b.len(), "frame buffer dimensions must match");
    a.chunks_exact(3)
        .zip(b.chunks_exact(3))
        .filter(|(pa, pb)| {
            pa[0].abs_diff(pb[0]) >= 16
                || pa[1].abs_diff(pb[1]) >= 16
                || pa[2].abs_diff(pb[2]) >= 16
        })
        .count()
}

fn non_black_pixels(rgb: &[u8]) -> usize {
    rgb.chunks_exact(3)
        .filter(|px| px[0] > 8 || px[1] > 8 || px[2] > 8)
        .count()
}

#[test]
#[ignore = "boots real ART multi-process session; run under run-exclusive.sh + guarded.sh"]
fn pressing_home_with_system_launcher_switches_pixels_from_counter_to_launcher() {
    let _ = find_fixture_apk("launcher.apk");
    let counter_apk = find_fixture_apk("counter.apk");
    let daemon = Daemon::start("counter-launcher");
    let hub = ViewHubProc::start(&daemon.viewreg);
    let mut conn = daemon.connect();
    let session_id = "s_launcher_home_frame";
    let base = json!({ "session_id": session_id });

    let launch = conn.call(
        "acos_launch",
        json!({
            "base": base,
            "apk": counter_apk,
            "animations": "off",
            "device_name": "Launcher Counter",
            "services": [{"id": "system.launcher", "profile": "real"}],
        }),
    );
    assert_eq!(launch["ok"], json!(true), "launch failed: {launch:#?}");

    let obs_counter = conn.call(
        "acos_observe",
        json!({
            "base": base,
            "tree": true,
            "settle": "strict",
            "screenshot": true,
        }),
    );
    assert_eq!(obs_counter["ok"], json!(true), "{obs_counter:#?}");
    assert!(
        find_in_tree(&obs_counter, Some("Increment"), Some("inc")).is_some(),
        "Counter Increment button missing: {obs_counter:#?}"
    );
    let counter_shot = PathBuf::from(
        obs_counter["result"]["screenshot"]["path"]
            .as_str()
            .expect("counter screenshot path"),
    );
    let (cw, ch, counter_rgb) = decode_rgb(&std::fs::read(&counter_shot).expect("counter PNG"));
    assert_eq!((cw, ch), (1080, 2400));
    assert!(non_black_pixels(&counter_rgb) > 10_000, "Counter PNG looks blank");

    // Subscribe to the live view MJPEG stream before pressing Home.
    let frames = MjpegStream::connect(hub.port, &hub.token, session_id);
    let live_counter = frames.wait_match(
        |f| f.content_type == "image/png" && f.w == 540 && f.h == 1200,
        Duration::from_secs(10),
        "initial Counter MJPEG frame",
    );
    let (_, _, live_counter_rgb) = decode_rgb(&live_counter.bytes);

    // Press Home -> ATMS switches foreground task to com.acos.launcher (process tag 1).
    let key_home = conn.call("acos_key", json!({ "base": base, "code": "home" }));
    assert_eq!(key_home["ok"], json!(true), "home key failed: {key_home:#?}");

    let deadline = Instant::now() + Duration::from_secs(20);
    let obs_home_tree = loop {
        let tree = conn.call(
            "acos_observe_tree",
            json!({ "base": base, "settle": "strict" }),
        );
        if tree["ok"] == json!(true)
            && find_in_tree(&tree, Some("ACOS Home"), Some("launcher_title")).is_some()
        {
            break tree;
        }
        assert!(
            Instant::now() < deadline,
            "Launcher tree never appeared after Home: {tree:#?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(
        find_in_tree(&obs_home_tree, Some("Counter"), Some("app_row_0")).is_some(),
        "Launcher app row missing: {obs_home_tree:#?}"
    );

    let obs_launcher = conn.call(
        "acos_observe",
        json!({
            "base": base,
            "tree": true,
            "settle": "strict",
            "screenshot": true,
        }),
    );
    assert_eq!(obs_launcher["ok"], json!(true), "{obs_launcher:#?}");
    assert!(
        find_in_tree(&obs_launcher, Some("ACOS Home"), Some("launcher_title")).is_some(),
        "Expected ACOS Home in observe tree: {obs_launcher:#?}"
    );
    let launcher_shot = PathBuf::from(
        obs_launcher["result"]["screenshot"]["path"]
            .as_str()
            .expect("launcher screenshot path"),
    );

    let (lw, lh, launcher_rgb) = decode_rgb(&std::fs::read(&launcher_shot).expect("launcher PNG"));
    assert_eq!((lw, lh), (1080, 2400));
    assert!(
        non_black_pixels(&launcher_rgb) > 10_000,
        "Launcher PNG looks blank"
    );

    let diff_home = changed_pixels(&counter_rgb, &launcher_rgb);
    assert!(
        diff_home > 5_000,
        "Screenshot after Home stayed stuck on Counter pixels (diff={diff_home})"
    );

    fn downscale_2x2_rgb(w: u32, h: u32, rgb: &[u8]) -> Vec<u8> {
        let dw = (w / 2) as usize;
        let dh = (h / 2) as usize;
        let sw = w as usize;
        let mut out = vec![0u8; dw * dh * 3];
        for y in 0..dh {
            let sy0 = y * 2;
            let sy1 = sy0 + 1;
            for x in 0..dw {
                let sx0 = x * 2;
                let sx1 = sx0 + 1;
                let idx00 = (sy0 * sw + sx0) * 3;
                let idx10 = (sy0 * sw + sx1) * 3;
                let idx01 = (sy1 * sw + sx0) * 3;
                let idx11 = (sy1 * sw + sx1) * 3;
                let dst = (y * dw + x) * 3;
                for c in 0..3 {
                    let sum = u32::from(rgb[idx00 + c])
                        + u32::from(rgb[idx10 + c])
                        + u32::from(rgb[idx01 + c])
                        + u32::from(rgb[idx11 + c]);
                    out[dst + c] = (sum / 4) as u8;
                }
            }
        }
        out
    }

    let counter_half_rgb = downscale_2x2_rgb(cw, ch, &counter_rgb);
    let launcher_half_rgb = downscale_2x2_rgb(lw, lh, &launcher_rgb);

    // Live view MJPEG stream must switch from Counter to Launcher pixels (not just differ).
    let live_launcher = frames.wait_match(
        |f| {
            if f.stream_idx <= live_counter.stream_idx || f.seq <= live_counter.seq {
                return false;
            }
            let (w, h, rgb) = decode_rgb(&f.bytes);
            w == 540
                && h == 1200
                && changed_pixels(&live_counter_rgb, &rgb) > 1_000
                && changed_pixels(&launcher_half_rgb, &rgb) < 2_500
        },
        Duration::from_secs(10),
        "live MJPEG frame matching Launcher after Home",
    );
    assert!(live_launcher.seq > live_counter.seq);

    // Tapping Counter in Launcher brings Counter back and updates pixels again.
    let tap_counter = conn.call(
        "acos_tap",
        json!({ "base": base, "id": "app_row_0" }),
    );
    assert_eq!(tap_counter["ok"], json!(true), "{tap_counter:#?}");

    let obs_back = conn.call(
        "acos_observe",
        json!({
            "base": base,
            "tree": true,
            "settle": "strict",
            "screenshot": true,
        }),
    );
    assert_eq!(obs_back["ok"], json!(true), "{obs_back:#?}");
    assert!(
        find_in_tree(&obs_back, Some("Increment"), Some("inc")).is_some(),
        "Counter did not return after tapping app_row_0: {obs_back:#?}"
    );
    let back_shot = PathBuf::from(
        obs_back["result"]["screenshot"]["path"]
            .as_str()
            .expect("counter_again screenshot path"),
    );
    let (bw, bh, back_rgb) = decode_rgb(&std::fs::read(&back_shot).expect("counter_again PNG"));
    assert_eq!((bw, bh), (1080, 2400));
    let diff_back_vs_launcher = changed_pixels(&launcher_rgb, &back_rgb);
    let diff_back_vs_counter = changed_pixels(&counter_rgb, &back_rgb);
    assert!(
        diff_back_vs_launcher > 5_000,
        "Returning to Counter must differ from Launcher (diff={diff_back_vs_launcher})"
    );
    assert!(
        diff_back_vs_counter < 5_000,
        "Returning to Counter should match Counter's pixels (diff={diff_back_vs_counter})"
    );

    // Live view MJPEG stream must also switch back to Counter's pixels.
    let live_back = frames.wait_match(
        |f| {
            if f.stream_idx <= live_launcher.stream_idx || f.seq <= live_launcher.seq {
                return false;
            }
            let (w, h, rgb) = decode_rgb(&f.bytes);
            w == 540
                && h == 1200
                && changed_pixels(&launcher_half_rgb, &rgb) > 1_000
                && changed_pixels(&counter_half_rgb, &rgb) < 2_500
        },
        Duration::from_secs(10),
        "live MJPEG frame returning to Counter",
    );
    assert!(live_back.seq > live_launcher.seq);

    println!(
        "LAUNCHER_HOME_FRAME_OK diff_counter_vs_launcher={} diff_back_vs_launcher={} diff_back_vs_counter={}",
        diff_home, diff_back_vs_launcher, diff_back_vs_counter
    );

    drop(frames);
    let destroy = conn.call("acos_destroy", json!({ "base": base }));
    assert_eq!(destroy["ok"], json!(true), "{destroy:#?}");
    assert_eq!(destroy["result"]["teardown"]["verified"], json!(true));
    assert_eq!(destroy["result"]["teardown"]["survivors"], json!([]));
}
