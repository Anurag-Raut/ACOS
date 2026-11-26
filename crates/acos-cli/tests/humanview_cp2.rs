//! HUMANVIEW1 Checkpoint 2 integration tests:
//!
//! 1. `cp2_ws_input_auth_host_and_cap_enforced` — spawns the real `acos view`
//!    binary and checks Acceptance 4: token check (403), Host check (403),
//!    non-WebSocket rejection (400), missing session (404), and connection cap
//!    on `/api/input/<session_id>`.
//! 2. `cp2_ws_live_tap_text_back_and_source_labels` (`#[ignore]`) — boots real
//!    ART sessions under `acos serve` + `acos view` and tests Acceptance 1
//!    (tap on `counter.apk`, text on `text-input.apk`, `KEYCODE_BACK` on
//!    `multi-activity.apk`) and Acceptance 2 (`source: panel` vs
//!    `source: agent` on `/api/events/<session_id>`).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpStream};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
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
    for candidate in [
        repo_root().join("fixtures/build/apps").join(name),
        PathBuf::from("/home/Qui/Projects/acos/fixtures/build/apps").join(name),
    ] {
        if candidate.is_file() {
            return candidate;
        }
    }
    panic!("fixture APK not found: {name}");
}

fn scratch(tag: &str) -> PathBuf {
    let dir = repo_root()
        .join(".cache")
        .join(format!("hv-cp2-{tag}-{}", std::process::id()));
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

struct WsClient {
    stream: TcpStream,
}

impl WsClient {
    fn fetch_guest_gen(port: u16, token: &str, session_id: &str) -> u64 {
        let mut s = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).expect("connect /api/sessions");
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        write!(
            s,
            "GET /api/sessions?t={token} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        s.flush().unwrap();
        let mut raw = Vec::new();
        let _ = s.read_to_end(&mut raw);
        let text = String::from_utf8_lossy(&raw);
        let body = text
            .split_once("\r\n\r\n")
            .map(|(_, b)| b)
            .expect("HTTP response body from /api/sessions");
        let doc: Value = serde_json::from_str(body).expect("JSON from /api/sessions");
        doc["sessions"]
            .as_array()
            .and_then(|arr| {
                arr.iter()
                    .find(|item| item["session_id"].as_str() == Some(session_id))
            })
            .and_then(|item| item["guest_gen"].as_u64())
            .unwrap_or_else(|| {
                panic!("guest_gen not found for session {session_id} in /api/sessions: {doc}")
            })
    }

    fn connect(port: u16, token: &str, session_id: &str) -> Self {
        let guest_gen = Self::fetch_guest_gen(port, token, session_id);
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).expect("connect ws");
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        write!(
            stream,
            "GET /api/input/{session_id}?t={token}&g={guest_gen} HTTP/1.1\r\n\
             Host: 127.0.0.1:{port}\r\n\
             Upgrade: websocket\r\n\
             Connection: Upgrade\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
             Sec-WebSocket-Version: 13\r\n\r\n"
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
        assert!(
            head_str.starts_with("HTTP/1.1 101"),
            "expected 101 Switching Protocols, got: {head_str}"
        );
        Self { stream }
    }

    fn send_json(&mut self, msg: &Value) -> Value {
        let raw = msg.to_string();
        let payload = raw.as_bytes();
        let mut frame = Vec::with_capacity(payload.len() + 10);
        frame.push(0x81);
        if payload.len() <= 125 {
            frame.push(0x80 | (payload.len() as u8));
        } else if payload.len() <= u16::MAX as usize {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        } else {
            panic!("payload too large for test client");
        }
        let mask = [0x37u8, 0xfa, 0x21, 0x3d];
        frame.extend_from_slice(&mask);
        for (i, b) in payload.iter().enumerate() {
            frame.push(*b ^ mask[i & 3]);
        }
        self.stream.write_all(&frame).unwrap();
        self.stream.flush().unwrap();
        self.read_json()
    }

    fn read_json(&mut self) -> Value {
        loop {
            let mut hdr = [0u8; 2];
            self.stream.read_exact(&mut hdr).unwrap();
            let opcode = hdr[0] & 0x0f;
            let len7 = hdr[1] & 0x7f;
            let len = match len7 {
                0..=125 => usize::from(len7),
                126 => {
                    let mut ext = [0u8; 2];
                    self.stream.read_exact(&mut ext).unwrap();
                    usize::from(u16::from_be_bytes(ext))
                }
                _ => panic!("unexpected 64-bit length"),
            };
            let mut payload = vec![0u8; len];
            if len > 0 {
                self.stream.read_exact(&mut payload).unwrap();
            }
            if opcode == 0x9 || opcode == 0xa {
                continue;
            }
            assert_eq!(opcode, 0x1, "expected text frame");
            return serde_json::from_slice(&payload).expect("WS JSON reply");
        }
    }
}

struct SseEvents {
    stop: Arc<AtomicBool>,
    events: Arc<Mutex<Vec<Value>>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl SseEvents {
    fn connect(port: u16, token: &str, session_id: &str) -> Self {
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        write!(
            stream,
            "GET /api/events/{session_id}?t={token} HTTP/1.1\r\n\
             Host: 127.0.0.1:{port}\r\n\r\n"
        )
        .unwrap();
        stream.flush().unwrap();

        let stop = Arc::new(AtomicBool::new(false));
        let events = Arc::new(Mutex::new(Vec::new()));
        let stop_t = Arc::clone(&stop);
        let events_t = Arc::clone(&events);

        let handle = std::thread::spawn(move || {
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            let mut event_name = String::new();
            while !stop_t.load(Ordering::Relaxed) {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {
                        let trimmed = line.trim_end_matches(&['\r', '\n'][..]);
                        if let Some(ev) = trimmed.strip_prefix("event: ") {
                            event_name = ev.to_string();
                        } else if let Some(data) = trimmed.strip_prefix("data: ") {
                            if event_name == "ev" {
                                if let Ok(v) = serde_json::from_str::<Value>(data) {
                                    if let Some(ev) = v.get("event") {
                                        events_t.lock().unwrap().push(ev.clone());
                                    }
                                }
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
            events,
            handle: Some(handle),
        }
    }

    fn wait_until(&self, pred: impl Fn(&[Value]) -> bool, timeout: Duration) -> Vec<Value> {
        let deadline = Instant::now() + timeout;
        loop {
            let snap = self.events.lock().unwrap().clone();
            if pred(&snap) {
                return snap;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for SSE events; seen so far: {snap:#?}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for SseEvents {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn http_status(port: u16, host: &str, raw_req: &str) -> u16 {
    let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let full = format!("{raw_req}Host: {host}\r\nConnection: close\r\n\r\n");
    stream.write_all(full.as_bytes()).unwrap();
    let mut buf = Vec::new();
    let _ = stream.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf)
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

fn find_in_tree(node: &Value, text_contains: Option<&str>, class_suffix: Option<&str>) -> Option<Value> {
    if node.is_object() {
        let matches_text = text_contains.is_none_or(|needle| {
            node.get("text")
                .and_then(Value::as_str)
                .is_some_and(|s| s.contains(needle))
        });
        let matches_class = class_suffix.is_none_or(|suf| {
            node.get("class")
                .and_then(Value::as_str)
                .is_some_and(|s| s.ends_with(suf))
        });
        if matches_text && matches_class {
            return Some(node.clone());
        }
    }
    for key in ["result", "tree", "nodes", "children", "root"] {
        if let Some(child) = node.get(key) {
            if let Some(found) = find_in_tree(child, text_contains, class_suffix) {
                return Some(found);
            }
        }
    }
    if let Some(arr) = node.as_array() {
        for item in arr {
            if let Some(found) = find_in_tree(item, text_contains, class_suffix) {
                return Some(found);
            }
        }
    }
    None
}

fn center_of(node: &Value) -> (i64, i64) {
    let b = node.get("bounds").expect("bounds");
    if let Some(s) = b.as_str() {
        let s = s.trim();
        let rest = s.strip_prefix('[').expect("bounds [");
        let mid = rest.find("][").expect("bounds ][");
        let first = &rest[..mid];
        let second = rest[mid + 2..].strip_suffix(']').expect("bounds ]");
        let (l, t) = first.split_once(',').expect("bounds lt");
        let (r, bt) = second.split_once(',').expect("bounds rb");
        let l: i64 = l.trim().parse().unwrap();
        let t: i64 = t.trim().parse().unwrap();
        let r: i64 = r.trim().parse().unwrap();
        let bt: i64 = bt.trim().parse().unwrap();
        return ((l + r) / 2, (t + bt) / 2);
    }
    if let Some(arr) = b.as_array() {
        let l = arr[0].as_i64().unwrap();
        let t = arr[1].as_i64().unwrap();
        let r = arr[2].as_i64().unwrap();
        let bt = arr[3].as_i64().unwrap();
        return ((l + r) / 2, (t + bt) / 2);
    }
    let l = b["left"].as_i64().unwrap();
    let t = b["top"].as_i64().unwrap();
    let r = b["right"].as_i64().unwrap();
    let bt = b["bottom"].as_i64().unwrap();
    ((l + r) / 2, (t + bt) / 2)
}

fn find_in_tree_by_id(node: &Value, id_suffix: &str) -> Option<Value> {
    if node.is_object() {
        let matches_id = node
            .get("id")
            .or_else(|| node.get("resource_id"))
            .and_then(Value::as_str)
            .is_some_and(|s| s == id_suffix || s.ends_with(&format!(":id/{id_suffix}")) || s.ends_with(&format!("/{id_suffix}")));
        if matches_id {
            return Some(node.clone());
        }
    }
    for key in ["result", "tree", "nodes", "children", "root"] {
        if let Some(child) = node.get(key) {
            if let Some(found) = find_in_tree_by_id(child, id_suffix) {
                return Some(found);
            }
        }
    }
    if let Some(arr) = node.as_array() {
        for item in arr {
            if let Some(found) = find_in_tree_by_id(item, id_suffix) {
                return Some(found);
            }
        }
    }
    None
}

#[test]
fn cp2_ws_input_auth_host_and_cap_enforced() {
    let dir = scratch("auth");
    let viewreg = dir.join("viewreg");
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let entry = json!({
        "v": 1,
        "session_id": "s_auth",
        "daemon_pid": std::process::id(),
        "started_ms": now_ms,
        "client": {"name": "cp2-test", "version": "1"},
        "project_dir": "/tmp",
        "apk": "x.apk",
        "package": "x",
        "display": {"w": 1080, "h": 2400},
        "stream_socket": null,
    });
    std::fs::write(
        viewreg.join(format!("s_auth-{}.json", std::process::id())),
        entry.to_string(),
    )
    .unwrap();

    let hub = ViewHubProc::start(&viewreg);
    let host = format!("127.0.0.1:{}", hub.port);

    // Wrong / missing token -> 403.
    assert_eq!(
        http_status(
            hub.port,
            &host,
            "GET /api/input/s_auth HTTP/1.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n"
        ),
        403
    );
    assert_eq!(
        http_status(
            hub.port,
            &host,
            &format!(
                "GET /api/input/s_auth?t={}bad HTTP/1.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n",
                hub.token
            )
        ),
        403
    );

    // Wrong Host -> 403.
    assert_eq!(
        http_status(
            hub.port,
            &format!("evil.example:{}", hub.port),
            &format!(
                "GET /api/input/s_auth?t={} HTTP/1.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n",
                hub.token
            )
        ),
        403
    );

    // Valid auth + Host without WS headers -> 400.
    assert_eq!(
        http_status(
            hub.port,
            &host,
            &format!("GET /api/input/s_auth?t={} HTTP/1.1\r\n", hub.token)
        ),
        400
    );

    // Connection cap (MAX_CONNECTIONS = 64): hold 64 sockets, 65th gets closed immediately without response.
    let mut holders = Vec::new();
    for _ in 0..64 {
        holders.push(TcpStream::connect((Ipv4Addr::LOCALHOST, hub.port)).unwrap());
    }
    std::thread::sleep(Duration::from_millis(150));
    let mut over_cap = TcpStream::connect((Ipv4Addr::LOCALHOST, hub.port)).unwrap();
    over_cap
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let _ = write!(
        over_cap,
        "GET /api/input/s_auth?t={} HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
        hub.token
    );
    let mut buf = Vec::new();
    let read_res = over_cap.read_to_end(&mut buf);
    match read_res {
        Ok(0) => {}
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe
            ) => {}
        other => panic!(
            "expected immediate EOF/reset on over-cap socket, got {other:?} (buf len={})",
            buf.len()
        ),
    }
    assert!(buf.is_empty(), "over-cap /api/input request was served");

    // Dropping the held sockets frees the cap and subsequent requests succeed.
    drop(holders);
    let recovery_deadline = Instant::now() + Duration::from_secs(5);
    let mut recovered = false;
    while Instant::now() < recovery_deadline {
        if http_status(
            hub.port,
            &host,
            &format!("GET /api/sessions?t={} HTTP/1.1\r\n", hub.token),
        ) == 200
        {
            recovered = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(
        recovered,
        "hub did not recover with HTTP 200 within 5 s after held connections closed"
    );

    drop(hub);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[ignore = "live ART runtime test; run under run-exclusive.sh + guarded.sh"]
fn cp2_ws_live_tap_text_back_and_source_labels() {
    let daemon = Daemon::start("live");
    let hub = ViewHubProc::start(&daemon.viewreg);
    let mut conn = daemon.connect();

    // 1. counter.apk: tap via WebSocket increments counter + agent vs panel source labels.
    let counter_apk = find_fixture_apk("counter.apk");
    let base_counter = json!({ "session_id": "cp2-counter" });
    let launch_c = conn.call(
        "acos_launch",
        json!({
            "base": base_counter,
            "apk": counter_apk,
            "animations": "off",
            "device_name": "CP2 Counter",
        }),
    );
    assert_eq!(launch_c["ok"], json!(true), "{launch_c:#?}");

    let obs0 = conn.call("acos_observe", json!({ "base": base_counter, "tree": true }));
    let inc_btn = find_in_tree(&obs0, Some("Increment"), Some("Button")).expect("Increment button");
    let (ix, iy) = center_of(&inc_btn);

    let sse = SseEvents::connect(hub.port, &hub.token, "cp2-counter");
    let mut ws = WsClient::connect(hub.port, &hub.token, "cp2-counter");

    let r1 = ws.send_json(&json!({"t": "touch", "phase": "down", "id": 0, "x": ix, "y": iy}));
    assert_eq!(r1["ok"], json!(true), "{r1:#?}");
    let r2 = ws.send_json(&json!({"t": "touch", "phase": "up", "id": 0, "x": ix, "y": iy}));
    assert_eq!(r2["ok"], json!(true), "{r2:#?}");

    let obs1 = conn.call("acos_observe", json!({ "base": base_counter, "tree": true }));
    let count1 = find_in_tree_by_id(&obs1, "count")
        .and_then(|n| n["text"].as_str().map(str::to_string));
    assert_eq!(
        count1.as_deref(),
        Some("1"),
        "counter did not increment to 1 after WS tap: {obs1:#?}"
    );

    let agent_tap = conn.call("acos_tap", json!({ "base": base_counter, "x": ix, "y": iy }));
    assert_eq!(agent_tap["ok"], json!(true), "{agent_tap:#?}");
    let obs2 = conn.call("acos_observe", json!({ "base": base_counter, "tree": true }));
    let count2 = find_in_tree_by_id(&obs2, "count")
        .and_then(|n| n["text"].as_str().map(str::to_string));
    assert_eq!(
        count2.as_deref(),
        Some("2"),
        "counter did not increment to 2 after agent tap: {obs2:#?}"
    );

    let feed = sse.wait_until(
        |evs| {
            evs.iter().any(|e| e["source"] == "panel" && e["cmd"] == "touch")
                && evs.iter().any(|e| e["source"] == "agent" && e["cmd"] == "tap")
        },
        Duration::from_secs(10),
    );
    assert!(feed.iter().any(|e| e["source"] == "panel"));
    assert!(feed.iter().any(|e| e["source"] == "agent"));
    drop(ws);
    drop(sse);
    let _ = conn.call("acos_destroy", json!({ "base": base_counter }));

    // 2. text-input.apk: tap EditText + type via WebSocket.
    let text_apk = find_fixture_apk("text-input.apk");
    let base_text = json!({ "session_id": "cp2-text" });
    let launch_t = conn.call(
        "acos_launch",
        json!({
            "base": base_text,
            "apk": text_apk,
            "animations": "off",
        }),
    );
    assert_eq!(launch_t["ok"], json!(true), "{launch_t:#?}");

    let obs_t0 = conn.call("acos_observe", json!({ "base": base_text, "tree": true }));
    let edit = find_in_tree(&obs_t0, None, Some("EditText")).expect("EditText");
    let (ex, ey) = center_of(&edit);

    let mut ws_text = WsClient::connect(hub.port, &hub.token, "cp2-text");
    assert_eq!(
        ws_text.send_json(&json!({"t": "touch", "phase": "down", "id": 0, "x": ex, "y": ey}))["ok"],
        json!(true)
    );
    assert_eq!(
        ws_text.send_json(&json!({"t": "touch", "phase": "up", "id": 0, "x": ex, "y": ey}))["ok"],
        json!(true)
    );
    let typed_r = ws_text.send_json(&json!({"t": "text", "s": "hello human"}));
    assert_eq!(typed_r["ok"], json!(true), "{typed_r:#?}");

    let obs_t1 = conn.call("acos_observe", json!({ "base": base_text, "tree": true }));
    assert!(
        find_in_tree(&obs_t1, Some("hello human"), Some("EditText")).is_some(),
        "EditText did not receive WS typed text: {obs_t1:#?}"
    );
    drop(ws_text);
    let _ = conn.call("acos_destroy", json!({ "base": base_text }));

    // 3. multi-activity.apk: open SecondActivity and finish it with KEYCODE_BACK via WS.
    let multi_apk = find_fixture_apk("multi-activity.apk");
    let base_multi = json!({ "session_id": "cp2-multi" });
    let launch_m = conn.call(
        "acos_launch",
        json!({
            "base": base_multi,
            "apk": multi_apk,
            "animations": "off",
        }),
    );
    assert_eq!(launch_m["ok"], json!(true), "{launch_m:#?}");

    let obs_m0 = conn.call("acos_observe", json!({ "base": base_multi, "tree": true }));
    let send_btn = find_in_tree(&obs_m0, Some("Send"), Some("Button")).expect("Send button");
    let (sx, sy) = center_of(&send_btn);

    let mut ws_multi = WsClient::connect(hub.port, &hub.token, "cp2-multi");
    assert_eq!(
        ws_multi.send_json(&json!({"t": "touch", "phase": "down", "id": 0, "x": sx, "y": sy}))["ok"],
        json!(true)
    );
    assert_eq!(
        ws_multi.send_json(&json!({"t": "touch", "phase": "up", "id": 0, "x": sx, "y": sy}))["ok"],
        json!(true)
    );

    // Wait briefly / observe until SecondActivity is on top (Finish button visible).
    let obs_m1 = conn.call("acos_observe", json!({ "base": base_multi, "tree": true }));
    assert!(
        find_in_tree(&obs_m1, Some("Finish"), Some("Button")).is_some(),
        "SecondActivity did not appear after tapping Send: {obs_m1:#?}"
    );

    let back_r = ws_multi.send_json(&json!({"t": "key", "code": "KEYCODE_BACK", "phase": "press"}));
    assert_eq!(back_r["ok"], json!(true), "{back_r:#?}");

    let obs_m2 = conn.call("acos_observe", json!({ "base": base_multi, "tree": true }));
    if find_in_tree(&obs_m2, Some("Send"), Some("Button")).is_none() {
        let logs = conn.call("acos_logs", json!({ "base": base_multi, "all": true, "limit": 200 }));
        panic!(
            "KEYCODE_BACK via WS did not return to MainActivity:\nobs_m1={obs_m1:#?}\nobs_m2={obs_m2:#?}\nlogs={logs:#?}"
        );
    }

    drop(ws_multi);
    let _ = conn.call("acos_destroy", json!({ "base": base_multi }));
}
