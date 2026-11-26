//! HUMANVIEW1 Checkpoint 3 integration tests:
//!
//! 1. `cp3_http_sessions_and_frame_upgrade_contract` — verifies `/api/sessions`
//!    naming fields (`device_name`, `app_label`, `watch_only`), HTTP MJPEG
//!    `/api/frames/<id>` upgrade from 0.5 PNG to 1.0 PNG on `/api/input/<id>`
//!    connect, revert on WS close, and `watch_only` refusal to upgrade.
//! 2. `cp3_live_drag_fling_long_press_and_rotate` (`#[ignore]`) — boots real
//!    ART sessions on `list-scroll.apk` and `touch-probe.apk` under `acos serve`
//!    + `acos view` and tests:
//!    - Acceptance 1 (drag scrolls list, long press triggers `GestureDetector.onLongPress`,
//!      rotate swaps display to 2400x1080 and updates `/api/sessions` + frames)
//!    - Acceptance 5 (gesture fidelity: pipelined MOVE sequence + fast swipe fling
//!      moves list past release point, plus wheel `scroll` advances the list)
//!    - Live PNG 1.0x @ 30fps upgrade while `/api/input` is connected
//! 3. `cp3_verify_run_watch_only_refused_under_view` (`#[ignore]`) — boots
//!    `acos verify --engine art` alongside `acos view` and tests Acceptance 3:
//!    `/api/sessions` reports `watch_only: true`, all WS `/api/input` commands
//!    are refused with `VERIFY_WATCH_ONLY`, `/api/events` records
//!    `VERIFY_WATCH_ONLY`, `/api/frames` stays 0.5 PNG (verified on fresh frames
//!    while WS is open), and verify passes.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpStream};
use std::os::unix::net::{UnixListener, UnixStream};
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
        .join(format!("hv-cp3-{tag}-{}", std::process::id()));
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

    fn get_json(&self, path: &str) -> Value {
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, self.port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let sep = if path.contains('?') { '&' } else { '?' };
        write!(
            stream,
            "GET {path}{sep}t={} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n",
            self.token, self.port
        )
        .unwrap();
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).unwrap();
        let pos = buf
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .expect("http headers")
            + 4;
        serde_json::from_slice(&buf[pos..]).expect("JSON body")
    }
}

impl Drop for ViewHubProc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct ChildGuard(Option<Child>);

impl ChildGuard {
    fn new(child: Child) -> Self {
        Self(Some(child))
    }

    fn wait_with_output(mut self) -> std::io::Result<std::process::Output> {
        self.0
            .take()
            .expect("child already awaited")
            .wait_with_output()
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
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

    fn send_only(&mut self, msg: &Value) {
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
            panic!("payload too large");
        }
        let mask = [0x37u8, 0xfa, 0x21, 0x3d];
        frame.extend_from_slice(&mask);
        for (i, b) in payload.iter().enumerate() {
            frame.push(*b ^ mask[i & 3]);
        }
        self.stream.write_all(&frame).unwrap();
        self.stream.flush().unwrap();
    }

    fn send_json(&mut self, msg: &Value) -> Value {
        self.send_only(msg);
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

#[derive(Clone, Debug)]
#[allow(dead_code)]
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
            let mut chunk = [0u8; 16 * 1024];
            let mut idx = 0u64;
            while !stop_t.load(Ordering::Relaxed) {
                match stream.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&chunk[..n]);
                        loop {
                            let Some(hdr_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
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

    fn drain_seen(&self) -> Vec<MjpegPart> {
        let (lock, _) = &*self.state;
        lock.lock().unwrap().drain(..).collect()
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

fn find_in_tree_by_id(node: &Value, id_suffix: &str) -> Option<Value> {
    for field in ["resource_id", "resource-id", "id"] {
        if let Some(rid) = node.get(field).and_then(Value::as_str) {
            if rid == id_suffix || rid.ends_with(&format!(":id/{id_suffix}")) {
                return Some(node.clone());
            }
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

fn visible_row_indices(node: &Value) -> Vec<u32> {
    fn walk(n: &Value, out: &mut Vec<u32>) {
        if let Some(text) = n.get("text").and_then(Value::as_str) {
            if let Some(rest) = text.strip_prefix("Row ") {
                if let Ok(num) = rest.trim().parse::<u32>() {
                    out.push(num);
                }
            }
        }
        for key in ["result", "tree", "nodes", "children", "root"] {
            if let Some(child) = n.get(key) {
                walk(child, out);
            }
        }
        if let Some(arr) = n.as_array() {
            for item in arr {
                walk(item, out);
            }
        }
    }
    let mut rows = Vec::new();
    walk(node, &mut rows);
    rows.sort_unstable();
    rows.dedup();
    rows
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

#[test]
fn cp3_http_sessions_and_frame_upgrade_contract() {
    let dir = scratch("contract");
    let viewreg = dir.join("viewreg");
    let sock_dir = ShortSocketDir::new("cp3-fake");
    let stream_sock = sock_dir.socket("stream.sock");
    let listener = UnixListener::bind(&stream_sock).unwrap();

    let subscribes = Arc::new(Mutex::new(Vec::<Value>::new()));
    let subs_t = Arc::clone(&subscribes);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let subs = Arc::clone(&subs_t);
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                let _ = reader.read_line(&mut line);
                let Ok(req) = serde_json::from_str::<Value>(&line) else {
                    return;
                };
                subs.lock().unwrap().push(req.clone());
                let sub = &req["subscribe"];
                let fmt = sub["format"].as_str().unwrap_or("png");
                let scale = sub["scale"].as_f64().unwrap_or(0.5);
                let (w, h) = if (scale - 1.0).abs() < 0.05 {
                    (1080u32, 2400u32)
                } else {
                    (540u32, 1200u32)
                };
                let hello = json!({"v":1,"session":1,"display":{"w":1080,"h":2400},"format":fmt,"scale":scale});
                let send_msg = |s: &mut UnixStream, kind: u8, payload: &[u8]| -> std::io::Result<()> {
                    let len = (payload.len() + 1) as u32;
                    s.write_all(&len.to_be_bytes())?;
                    s.write_all(&[kind])?;
                    s.write_all(payload)?;
                    s.flush()
                };
                let _ = send_msg(&mut stream, b'H', hello.to_string().as_bytes());
                let img_bytes: &[u8] = if fmt == "jpeg" {
                    &[0xff, 0xd8, 0xff, 0xd9]
                } else {
                    &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]
                };
                for seq in 1..=500u64 {
                    let hdr = format!(
                        "{{\"seq\":{seq},\"generation\":1,\"w\":{w},\"h\":{h},\"format\":\"{fmt}\",\"ts_ms\":{}}}\n",
                        1000 + seq
                    );
                    let mut p = hdr.into_bytes();
                    p.extend_from_slice(img_bytes);
                    if send_msg(&mut stream, b'F', &p).is_err() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
            });
        }
    });

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let entry = json!({
        "v": 1,
        "session_id": "s_cp3",
        "daemon_pid": std::process::id(),
        "started_ms": now_ms,
        "client": {"name": "cp3", "version": "1"},
        "project_dir": "/tmp",
        "apk": "list-scroll.apk",
        "package": "dev.acos.fixtures.listscroll",
        "app_label": "ListScroll",
        "device_name": "Pixel Demo",
        "watch_only": false,
        "display": {"w": 1080, "h": 2400},
        "stream_socket": stream_sock.display().to_string(),
    });
    std::fs::write(
        viewreg.join(format!("s_cp3-{}.json", std::process::id())),
        entry.to_string(),
    )
    .unwrap();

    let hub = ViewHubProc::start(&viewreg);
    let sess_doc = hub.get_json("/api/sessions");
    assert_eq!(sess_doc["sessions"][0]["device_name"], json!("Pixel Demo"));
    assert_eq!(sess_doc["sessions"][0]["app_label"], json!("ListScroll"));
    assert_eq!(sess_doc["sessions"][0]["watch_only"], json!(false));

    // Watch-only starts with 0.5 PNG @ 15fps.
    let frames = MjpegStream::connect(hub.port, &hub.token, "s_cp3");
    let p0 = frames.wait_match(
        |p| p.content_type == "image/png" && p.w == 540,
        Duration::from_secs(5),
        "initial 0.5 PNG frame",
    );
    assert_eq!(p0.h, 1200);

    // Connecting WS /api/input upgrades upstream subscription to 1.0 PNG @ 30fps.
    let ws = WsClient::connect(hub.port, &hub.token, "s_cp3");
    let p1 = frames.wait_match(
        |p| p.content_type == "image/png" && p.w == 1080,
        Duration::from_secs(5),
        "upgraded 1.0 PNG frame",
    );
    assert_eq!(p1.h, 2400);
    assert!(p1.bytes.starts_with(&[0x89, b'P', b'N', b'G']));

    // Closing WS reverts upstream back to 0.5 PNG @ 15fps.
    drop(ws);
    let p2 = frames.wait_match(
        |p| p.content_type == "image/png" && p.w == 540,
        Duration::from_secs(5),
        "reverted 0.5 PNG frame",
    );
    assert_eq!(p2.h, 1200);

    drop(frames);
    let subs = subscribes.lock().unwrap();
    assert!(subs.len() >= 3, "expected >=3 subscribes: {subs:#?}");
    assert_eq!(subs[0]["subscribe"]["format"], json!("png"));
    assert_eq!(subs[0]["subscribe"]["scale"], json!(0.5));
    assert_eq!(subs[1]["subscribe"]["format"], json!("png"));
    assert_eq!(subs[1]["subscribe"]["scale"], json!(1.0));
    assert_eq!(subs[1]["subscribe"]["max_fps"], json!(30));
    assert_eq!(subs[2]["subscribe"]["format"], json!("png"));
    assert_eq!(subs[2]["subscribe"]["scale"], json!(0.5));

    drop(hub);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[ignore = "live ART runtime test; run under run-exclusive.sh + guarded.sh"]
fn cp3_live_drag_fling_long_press_and_rotate() {
    let daemon = Daemon::start("live");
    let hub = ViewHubProc::start(&daemon.viewreg);
    let mut conn = daemon.connect();

    // 1. list-scroll.apk: frame upgrade, drag, fling velocity, scroll wheel, rotate.
    let list_apk = find_fixture_apk("list-scroll.apk");
    let base_list = json!({ "session_id": "cp3-list" });
    let launch_l = conn.call(
        "acos_launch",
        json!({
            "base": base_list,
            "apk": list_apk,
            "animations": "on",
            "device_name": "CP3 List",
        }),
    );
    assert_eq!(launch_l["ok"], json!(true), "{launch_l:#?}");

    let obs_l0 = conn.call("acos_observe", json!({ "base": base_list, "tree": true }));
    let rows0 = visible_row_indices(&obs_l0);
    assert!(
        rows0.contains(&0),
        "expected Row 0 initially visible: {rows0:?}"
    );

    // Watch-only MJPEG starts at 0.5 PNG (540x1200).
    let frames = MjpegStream::connect(hub.port, &hub.token, "cp3-list");
    let f_watch = frames.wait_match(
        |p| p.content_type == "image/png" && p.w == 540 && p.h == 1200,
        Duration::from_secs(10),
        "live watch-only PNG 540x1200",
    );
    assert!(f_watch.bytes.starts_with(&[0x89, b'P', b'N', b'G']));

    // Opening WS /api/input upgrades to full-resolution 1.0 PNG (1080x2400).
    let sse = SseEvents::connect(hub.port, &hub.token, "cp3-list");
    let mut ws = WsClient::connect(hub.port, &hub.token, "cp3-list");
    let f_interactive = frames.wait_match(
        |p| p.content_type == "image/png" && p.w == 1080 && p.h == 2400,
        Duration::from_secs(10),
        "live interactive PNG 1080x2400",
    );
    assert!(
        f_interactive.bytes.starts_with(&[0x89, b'P', b'N', b'G']),
        "expected PNG magic bytes"
    );

    // Drag (slow controlled swipe up over 360 px: 10 MOVE events, low release velocity).
    assert_eq!(
        ws.send_json(&json!({"t": "touch", "phase": "down", "id": 0, "x": 540, "y": 1660}))["ok"],
        json!(true)
    );
    for step in 1..=10 {
        let y = 1660 - step * 36;
        std::thread::sleep(Duration::from_millis(35));
        assert_eq!(
            ws.send_json(&json!({"t": "touch", "phase": "move", "id": 0, "x": 540, "y": y}))["ok"],
            json!(true)
        );
    }
    std::thread::sleep(Duration::from_millis(80));
    assert_eq!(
        ws.send_json(&json!({"t": "touch", "phase": "up", "id": 0, "x": 540, "y": 1300}))["ok"],
        json!(true)
    );

    let obs_l1 = conn.call("acos_observe", json!({ "base": base_list, "tree": true }));
    let rows1 = visible_row_indices(&obs_l1);
    let min1 = *rows1.first().unwrap_or(&0);
    assert!(
        min1 > 0 && min1 < 12,
        "slow 360px drag should scroll modestly past Row 0: before={rows0:?}, after={rows1:?}"
    );

    // Acceptance 5: Gesture fidelity + fling velocity — pipelined fast swipe (9
    // MOVEs over 720 px, ~5 ms/step) triggers ListView fling well past the
    // ~5-row finger drag displacement.
    ws.send_only(&json!({"t": "touch", "phase": "down", "id": 0, "x": 540, "y": 1820}));
    for step in 1..=9 {
        let y = 1820 - step * 80;
        std::thread::sleep(Duration::from_millis(5));
        ws.send_only(&json!({"t": "touch", "phase": "move", "id": 0, "x": 540, "y": y}));
    }
    std::thread::sleep(Duration::from_millis(5));
    ws.send_only(&json!({"t": "touch", "phase": "up", "id": 0, "x": 540, "y": 1100}));
    for _ in 0..11 {
        assert_eq!(ws.read_json()["ok"], json!(true));
    }
    std::thread::sleep(Duration::from_millis(550));
    let obs_fling = conn.call("acos_observe", json!({ "base": base_list, "tree": true }));
    let rows_fling = visible_row_indices(&obs_fling);
    let min_fling = *rows_fling.first().unwrap_or(&0);
    let fling_advance = min_fling.saturating_sub(min1);
    assert!(
        fling_advance >= 10,
        "fast 720px swipe did not fling past ~5-row drag span: slow_drag={min1} rows, fling_advance={fling_advance} rows (rows_fling={rows_fling:?})"
    );

    // Wheel scroll also strictly moves the list (scroll down unless fling
    // already reached the last row, in which case scroll back up).
    let at_bottom = rows_fling.contains(&49);
    let wheel_dy = if at_bottom { -450 } else { 450 };
    assert_eq!(
        ws.send_json(&json!({"t": "scroll", "x": 540, "y": 1200, "dy": wheel_dy}))["ok"],
        json!(true)
    );
    std::thread::sleep(Duration::from_millis(150));
    let obs_wheel = conn.call("acos_observe", json!({ "base": base_list, "tree": true }));
    let rows_wheel = visible_row_indices(&obs_wheel);
    let min_wheel = *rows_wheel
        .first()
        .unwrap_or_else(|| panic!("wheel scroll observation contained no rows: {obs_wheel:#?}"));
    if at_bottom {
        assert!(
            min_wheel < min_fling,
            "wheel scroll up did not move list: before={min_fling}, after={rows_wheel:?}"
        );
    } else {
        assert!(
            min_wheel > min_fling,
            "wheel scroll down did not advance list: before={min_fling}, after={rows_wheel:?}"
        );
    }

    // Check SSE feed shows panel touch events.
    let feed = sse.wait_until(
        |evs| evs.iter().any(|e| e["source"] == "panel" && e["cmd"] == "touch"),
        Duration::from_secs(5),
    );
    assert!(feed.iter().any(|e| e["source"] == "panel"));

    // Rotate (Acceptance 1): display swaps to 2400x1080 in reply, /api/sessions, and frames.
    let rot_r = ws.send_json(&json!({"t": "rotate", "rotation": 1}));
    assert_eq!(rot_r["ok"], json!(true), "{rot_r:#?}");
    assert_eq!(rot_r["width"], json!(2400), "{rot_r:#?}");
    assert_eq!(rot_r["height"], json!(1080), "{rot_r:#?}");
    let sessions_after_rot = hub.get_json("/api/sessions");
    assert_eq!(
        sessions_after_rot["sessions"][0]["display"]["w"],
        json!(2400)
    );
    assert_eq!(
        sessions_after_rot["sessions"][0]["display"]["h"],
        json!(1080)
    );
    let _f_rot = frames.wait_match(
        |p| p.content_type == "image/png" && p.w == 2400 && p.h == 1080,
        Duration::from_secs(10),
        "rotated 2400x1080 frame",
    );

    drop(ws);
    drop(sse);
    // Closing WS drops back to 0.5 PNG (1200x540 while rotated).
    let _f_reverted = frames.wait_match(
        |p| p.content_type == "image/png" && p.w == 1200 && p.h == 540,
        Duration::from_secs(10),
        "reverted 0.5 PNG after WS disconnect",
    );
    drop(frames);
    let _ = conn.call("acos_destroy", json!({ "base": base_list }));

    // 2. touch-probe.apk: long press on GestureProbeView sets status to "long-pressed".
    let probe_apk = find_fixture_apk("touch-probe.apk");
    let base_probe = json!({ "session_id": "cp3-probe" });
    let launch_p = conn.call(
        "acos_launch",
        json!({
            "base": base_probe,
            "apk": probe_apk,
            "animations": "off",
        }),
    );
    assert_eq!(launch_p["ok"], json!(true), "{launch_p:#?}");

    let obs_p0 = conn.call("acos_observe", json!({ "base": base_probe, "tree": true }));
    let gesture_node = find_in_tree_by_id(&obs_p0, "gesture")
        .or_else(|| {
            obs_p0["result"]["tree_path"]
                .as_str()
                .and_then(|p| std::fs::read_to_string(p).ok())
                .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                .and_then(|full| find_in_tree_by_id(&full, "gesture"))
        })
        .unwrap_or_else(|| panic!("gesture node missing in {obs_p0:#?}"));
    let (gx, gy) = center_of(&gesture_node);

    let mut ws_probe = WsClient::connect(hub.port, &hub.token, "cp3-probe");
    assert_eq!(
        ws_probe.send_json(&json!({"t": "touch", "phase": "down", "id": 0, "x": gx, "y": gy}))["ok"],
        json!(true)
    );
    std::thread::sleep(Duration::from_millis(750));
    assert_eq!(
        ws_probe.send_json(&json!({"t": "touch", "phase": "move", "id": 0, "x": gx + 2, "y": gy + 1}))["ok"],
        json!(true)
    );
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(
        ws_probe.send_json(&json!({"t": "touch", "phase": "up", "id": 0, "x": gx + 2, "y": gy + 1}))["ok"],
        json!(true)
    );

    let obs_p1 = conn.call("acos_observe", json!({ "base": base_probe, "tree": true }));
    let status_node = find_in_tree_by_id(&obs_p1, "status")
        .unwrap_or_else(|| panic!("status node missing in {obs_p1:#?}"));
    assert_eq!(
        status_node["text"].as_str(),
        Some("long-pressed"),
        "long press via WS did not trigger GestureDetector.onLongPress: {obs_p1:#?}"
    );

    drop(ws_probe);
    let _ = conn.call("acos_destroy", json!({ "base": base_probe }));
}

#[test]
#[ignore = "live ART verify + view test; run under run-exclusive.sh + guarded.sh"]
fn cp3_verify_run_watch_only_refused_under_view() {
    let counter_apk = find_fixture_apk("counter.apk");
    let dir = scratch("verify");
    let viewreg = dir.join("viewreg");
    let scenario = dir.join("watch-only-cp3.yaml");
    std::fs::write(
        &scenario,
        format!(
            r#"version: 1
name: watch-only-cp3
description: Launch counter, hold under acos view while WS input attempts are refused, tap inc once from scenario to produce a fresh frame, and assert count is 1.
app: {apk}
engine: art
fallback: report-and-escalate
api_level: 34
isolation: process
display:
  mode: offscreen
  width: 1080
  height: 2400
  density_dpi: 420
capabilities: [lifecycle, ui.offscreen, ui.text, input]
expectation: pass
budgets:
  memory_mib: 512
  wall_ms: 90000
steps:
  - launch:
      activity: dev.acos.fixtures.counter/.MainActivity
      clear_state: true
  - wait_for:
      selector: {{id: count}}
      timeout_ms: 5000
  - wait:
      ms: 5000
  - tap:
      selector: {{id: inc}}
  - wait:
      ms: 3000
  - assert_text:
      selector: {{id: count}}
      text: {{exact: "1"}}
"#,
            apk = counter_apk.display()
        ),
    )
    .unwrap();

    let hub = ViewHubProc::start(&viewreg);
    let artifacts = dir.join("artifacts");
    let mut cmd = Command::new(acos_bin());
    cmd.args(["verify", "--engine", "art", "--overwrite", "--json"])
        .arg(&scenario)
        .arg("--artifacts")
        .arg(&artifacts)
        .env("ACOS_RUNTIME_DIR", dir.join("runtime"))
        .env("ACOS_CACHE_DIR", dir.join("cache"))
        .env("ACOS_MEMORY_GUARD", "off")
        .env("ACOS_VIEW_REGISTRY_DIR", &viewreg)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(art) = find_bin("acos-art", "ACOS_ART_BIN") {
        cmd.env("ACOS_ART_BIN", art);
    }

    let child = ChildGuard::new(cmd.spawn().expect("spawn acos verify"));

    // Wait for session to appear in /api/sessions with watch_only: true.
    let deadline = Instant::now() + Duration::from_secs(60);
    let session_id = loop {
        let doc = hub.get_json("/api/sessions");
        if let Some(arr) = doc["sessions"].as_array() {
            if let Some(s) = arr.first() {
                if s["stream"] == json!(true) {
                    assert_eq!(s["watch_only"], json!(true), "{s:#?}");
                    break s["session_id"].as_str().unwrap().to_string();
                }
            }
        }
        assert!(Instant::now() < deadline, "verify session never appeared");
        std::thread::sleep(Duration::from_millis(50));
    };

    let frames = MjpegStream::connect(hub.port, &hub.token, &session_id);
    let sse = SseEvents::connect(hub.port, &hub.token, &session_id);
    let f0 = frames.wait_match(
        |p| p.content_type == "image/png" && p.w == 540,
        Duration::from_secs(10),
        "verify initial PNG frame",
    );

    let mut ws = WsClient::connect(hub.port, &hub.token, &session_id);
    for msg in [
        json!({"t": "touch", "phase": "down", "id": 0, "x": 540, "y": 1200}),
        json!({"t": "touch", "phase": "up", "id": 0, "x": 540, "y": 1200}),
        json!({"t": "scroll", "x": 540, "y": 1200, "dy": 200}),
        json!({"t": "key", "code": "KEYCODE_BACK", "phase": "press"}),
        json!({"t": "text", "s": "nope"}),
        json!({"t": "rotate", "rotation": 1}),
    ] {
        let r = ws.send_json(&msg);
        assert_eq!(r["ok"], json!(false), "expected refusal for {msg}: {r:#?}");
        assert_eq!(r["code"], json!("VERIFY_WATCH_ONLY"), "{r:#?}");
    }

    // Event feed records VERIFY_WATCH_ONLY; frame stream never upgrades to 1.0x
    // even when the scenario's tap produces a new frame while WS is open.
    let evs = sse.wait_until(
        |evs| {
            evs.iter().any(|e| e["status"] == "VERIFY_WATCH_ONLY")
                && evs.iter().any(|e| e["cmd"] == "tap" && e["status"] == "ok")
        },
        Duration::from_secs(15),
    );
    assert!(evs.iter().any(|e| e["status"] == "VERIFY_WATCH_ONLY"));
    let tap_frame_seq = evs
        .iter()
        .find(|e| e["cmd"] == "tap" && e["status"] == "ok")
        .and_then(|e| e.get("frame_seq"))
        .and_then(|v| v.as_u64());
    let min_tap_seq = tap_frame_seq
        .map(|s| s.max(f0.seq + 1))
        .unwrap_or(f0.seq + 1);
    let f_tap = frames.wait_match(
        |p| p.stream_idx > f0.stream_idx && p.seq >= min_tap_seq && p.bytes != f0.bytes,
        Duration::from_secs(12),
        "scenario tap-produced verify frame while WS is open",
    );
    assert_eq!(
        f_tap.content_type, "image/png",
        "verify stream must stay PNG on tap-produced frame while WS is open"
    );
    assert_eq!(
        f_tap.w, 540,
        "verify stream must never upgrade to 1.0x resolution on tap-produced frame while WS is open"
    );
    for part in frames.drain_seen() {
        assert_eq!(
            part.content_type, "image/png",
            "verify stream must stay PNG"
        );
        assert_eq!(
            part.w, 540,
            "verify stream must never upgrade to 1.0x resolution"
        );
    }

    drop(ws);
    drop(sse);
    drop(frames);

    let output = child.wait_with_output().expect("wait acos verify");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "verify failed:\nstdout={stdout}\nstderr={stderr}"
    );
    let env: Value = serde_json::from_str(stdout.trim()).expect("verify JSON");
    assert_eq!(env["ok"], json!(true), "{env:#?}");
    assert_eq!(env["status"], json!("pass"), "{env:#?}");

    assert_eq!(
        hub.get_json("/api/sessions")["sessions"],
        json!([]),
        "verify session must vanish after exit"
    );
    drop(hub);
    let _ = std::fs::remove_dir_all(&dir);
}
