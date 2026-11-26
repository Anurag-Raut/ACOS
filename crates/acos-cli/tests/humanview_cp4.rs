//! HUMANVIEW1 Checkpoint 4 latency & zero-cost verification tests:
//!
//! 1. `cp4_latency_tap_to_changed_frame_50_reps` (`#[ignore]`) — boots a real
//!    ART `counter.apk` session under `acos serve` + `acos view`, upgrades to
//!    full-size 1.0x PNG @ 30fps via WebSocket `/api/input/<id>`, sends 50
//!    measured taps, decodes MJPEG frames from `/api/frames/<id>`, waits for
//!    each frame whose decoded pixels actually change, and asserts p50 <= 100 ms
//!    and p95 <= 200 ms (§4.2, §6 item 6).

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
        .join(format!("hv-cp4-{tag}-{}", std::process::id()));
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
            .env("ACOS_VIEW_STATS_FILE", dir.join("view-stats.json"))
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
    fn connect(port: u16, token: &str, session_id: &str) -> Self {
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).expect("connect ws");
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let _ = stream.set_nodelay(true);
        write!(
            stream,
            "GET /api/input/{session_id}?t={token} HTTP/1.1\r\n\
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
struct MjpegPart {
    stream_idx: u64,
    content_type: String,
    w: u32,
    h: u32,
    seq: u64,
    received_at: Instant,
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
        let _ = stream.set_nodelay(true);
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
                            let boundary_after = b"\r\n--acosframe\r\n";
                            let body_start = hdr_end + 4;
                            let total_need = body_start + content_length + boundary_after.len();
                            if buf.len() < total_need {
                                break;
                            }
                            assert_eq!(
                                &buf[body_start + content_length..total_need],
                                boundary_after,
                                "MJPEG part must be immediately followed by closing boundary"
                            );
                            let bytes = buf[body_start..body_start + content_length].to_vec();
                            buf.drain(..body_start + content_length + 2);
                            if !content_type.is_empty() && !bytes.is_empty() {
                                let received_at = Instant::now();
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
                                    received_at,
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

    fn drain_all(&self) -> Vec<MjpegPart> {
        let (lock, _) = &*self.state;
        lock.lock().unwrap().drain(..).collect()
    }

    fn wait_match(
        &self,
        pred: impl Fn(&MjpegPart) -> bool,
        timeout: Duration,
        label: &str,
    ) -> MjpegPart {
        self.wait_match_until(pred, Instant::now() + timeout, label)
    }

    fn wait_match_until(
        &self,
        pred: impl Fn(&MjpegPart) -> bool,
        deadline: Instant,
        label: &str,
    ) -> MjpegPart {
        let (lock, cv) = &*self.state;
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

fn decode_rgb(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
    let img = image::load_from_memory(bytes)
        .expect("decode MJPEG frame")
        .into_rgb8();
    let (w, h) = img.dimensions();
    (w, h, img.into_raw())
}

fn changed_pixels(a: &[u8], b: &[u8]) -> usize {
    if a.len() != b.len() {
        return a.len().max(b.len()) / 3;
    }
    a.chunks_exact(3)
        .zip(b.chunks_exact(3))
        .filter(|(pa, pb)| {
            pa[0].abs_diff(pb[0]) >= 16
                || pa[1].abs_diff(pb[1]) >= 16
                || pa[2].abs_diff(pb[2]) >= 16
        })
        .count()
}

fn percentile_sorted(sorted: &[f64], p: f64) -> f64 {
    assert!(!sorted.is_empty());
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    let idx = rank.saturating_sub(1).min(sorted.len() - 1);
    sorted[idx]
}

#[test]
#[ignore]
fn cp4_latency_tap_to_changed_frame_50_reps() {
    let daemon = Daemon::start("cp4-latency");
    let mut conn = daemon.connect();
    let apk = find_fixture_apk("counter.apk");
    let session_id = "s_hv_cp4_lat";

    let launch = conn.call(
        "acos_launch",
        json!({
            "base": { "session_id": session_id },
            "apk": apk,
            "animations": "off",
            "settle": "strict",
        }),
    );
    assert_eq!(launch["ok"], json!(true), "launch: {launch}");

    let obs = conn.call(
        "acos_observe",
        json!({ "base": { "session_id": session_id }, "tree": true, "settle": "strict" }),
    );
    assert_eq!(obs["ok"], json!(true), "observe: {obs}");
    let inc_node =
        find_in_tree(&obs, Some("Increment"), Some("Button")).expect("Increment button");
    let (cx, cy) = center_of(&inc_node);

    let hub = ViewHubProc::start(&daemon.viewreg);
    let frames = MjpegStream::connect(hub.port, &hub.token, session_id);
    let _initial_png = frames.wait_match(
        |f| f.content_type == "image/png" && f.w == 540 && f.h == 1200,
        Duration::from_secs(10),
        "initial 0.5x PNG",
    );

    let mut ws = WsClient::connect(hub.port, &hub.token, session_id);
    let mut current_part = frames.wait_match(
        |f| f.content_type == "image/png" && f.w == 1080 && f.h == 2400,
        Duration::from_secs(10),
        "interactive 1.0x PNG",
    );
    let (_, _, mut baseline_rgb) = decode_rgb(&current_part.bytes);

    let send_tap_wait_changed_frame =
        |_idx: usize,
         ws: &mut WsClient,
         frames: &MjpegStream,
         base_idx: &mut u64,
         base_seq: &mut u64,
         base_rgb: &mut Vec<u8>|
         -> (f64, usize, u64) {
            // Drain any pre-tap buffered frames so `base_idx`/`base_seq`/`base_rgb`
            // reflect the latest frame already on the wire before `t0`.
            let drained = frames.drain_all();
            if let Some(last) = drained
                .iter()
                .rev()
                .find(|f| f.content_type == "image/png" && f.w == 1080 && f.h == 2400)
            {
                if last.stream_idx > *base_idx {
                    *base_idx = last.stream_idx;
                    *base_seq = (*base_seq).max(last.seq);
                    let (w, h, rgb) = decode_rgb(&last.bytes);
                    assert_eq!((w, h), (1080, 2400));
                    *base_rgb = rgb;
                }
            }

            let t0 = Instant::now();
            let deadline = t0 + Duration::from_secs(5);
            ws.send_only(&json!({ "t": "touch", "phase": "down", "id": 0, "x": cx, "y": cy }));
            let ack1 = ws.read_json();
            ws.send_only(&json!({ "t": "touch", "phase": "up", "id": 0, "x": cx, "y": cy }));
            let ack2 = ws.read_json();
            assert_eq!(ack1["ok"], json!(true), "down ack: {ack1}");
            assert_eq!(ack2["ok"], json!(true), "up ack: {ack2}");

            let (next_part, changed_count) = loop {
                let candidate = frames.wait_match_until(
                    |f| {
                        f.content_type == "image/png"
                            && f.w == 1080
                            && f.h == 2400
                            && f.stream_idx > *base_idx
                            && f.seq > *base_seq
                            && f.received_at >= t0
                    },
                    deadline,
                    "changed PNG frame after tap",
                );
                *base_idx = candidate.stream_idx;
                *base_seq = candidate.seq;
                let (w, h, rgb) = decode_rgb(&candidate.bytes);
                assert_eq!((w, h), (1080, 2400));
                let diff = changed_pixels(base_rgb, &rgb);
                if diff >= 16 {
                    *base_rgb = rgb;
                    break (candidate, diff);
                }
            };
            let elapsed_ms = next_part.received_at.duration_since(t0).as_secs_f64() * 1000.0;
            (elapsed_ms, changed_count, next_part.seq)
        };

    let mut base_idx = current_part.stream_idx;
    let mut base_seq = current_part.seq;
    // 3 warmup taps so PNG encoder / WS / C1 / guest render paths are warm.
    for i in 0..3 {
        let _ = send_tap_wait_changed_frame(
            100 + i,
            &mut ws,
            &frames,
            &mut base_idx,
            &mut base_seq,
            &mut baseline_rgb,
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    let mut samples_ms = Vec::with_capacity(50);
    let mut pixel_diffs = Vec::with_capacity(50);
    for i in 0..50 {
        // Space taps beyond one 30fps frame interval (33.3 ms) so a previous
        // sample's frame does not trigger the rate limiter on the next tap.
        std::thread::sleep(Duration::from_millis(50));
        let (dt_ms, diff, _seq) = send_tap_wait_changed_frame(
            i,
            &mut ws,
            &frames,
            &mut base_idx,
            &mut base_seq,
            &mut baseline_rgb,
        );
        samples_ms.push(dt_ms);
        pixel_diffs.push(diff);
    }

    if let Ok(stats_raw) = std::fs::read_to_string(daemon.dir.join("view-stats.json")) {
        eprintln!("CP4_PRODUCER_STATS {stats_raw}");
    }

    // Also probe final count in guest tree to verify all 53 taps landed.
    let final_obs = conn.call(
        "acos_observe",
        json!({ "base": { "session_id": session_id }, "tree": true, "settle": "strict" }),
    );
    assert_eq!(final_obs["ok"], json!(true), "final_obs: {final_obs}");
    fn find_count_node(node: &Value) -> Option<Value> {
        if node.is_object() {
            let is_count_id = ["resource_id", "resource-id", "id"].iter().any(|field| {
                node.get(field)
                    .and_then(Value::as_str)
                    .is_some_and(|rid| rid == "count" || rid.ends_with(":id/count"))
            });
            let is_text_view = node
                .get("class")
                .and_then(Value::as_str)
                .is_some_and(|s| s.ends_with("TextView"));
            if is_count_id && is_text_view {
                return Some(node.clone());
            }
        }
        for key in ["result", "tree", "nodes", "children", "root"] {
            if let Some(child) = node.get(key) {
                if let Some(found) = find_count_node(child) {
                    return Some(found);
                }
            }
        }
        if let Some(arr) = node.as_array() {
            for item in arr {
                if let Some(found) = find_count_node(item) {
                    return Some(found);
                }
            }
        }
        None
    }
    let count_node = find_count_node(&final_obs)
        .unwrap_or_else(|| panic!("count TextView missing in {final_obs:#?}"));
    assert_eq!(
        count_node.get("text").and_then(Value::as_str),
        Some("53"),
        "count mismatch after 53 taps: {count_node:#?}"
    );

    let mut sorted = samples_ms.clone();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let min_ms = sorted[0];
    let max_ms = sorted[sorted.len() - 1];
    let p50_ms = percentile_sorted(&sorted, 50.0);
    let p95_ms = percentile_sorted(&sorted, 95.0);
    let mean_ms = sorted.iter().sum::<f64>() / sorted.len() as f64;
    let min_diff = *pixel_diffs.iter().min().unwrap();
    let max_diff = *pixel_diffs.iter().max().unwrap();

    let summary = json!({
        "n": samples_ms.len(),
        "min_ms": (min_ms * 100.0).round() / 100.0,
        "p50_ms": (p50_ms * 100.0).round() / 100.0,
        "p95_ms": (p95_ms * 100.0).round() / 100.0,
        "max_ms": (max_ms * 100.0).round() / 100.0,
        "mean_ms": (mean_ms * 100.0).round() / 100.0,
        "min_changed_pixels": min_diff,
        "max_changed_pixels": max_diff,
        "samples_ms": samples_ms.iter().map(|v| (v * 100.0).round() / 100.0).collect::<Vec<_>>(),
    });
    println!("CP4_LATENCY_JSON {}", summary);

    // Keep unused variable warning silent if `current_part` mutated later.
    let _ = &mut current_part;

    let _ = conn.call("acos_destroy", json!({ "base": { "session_id": session_id } }));

    assert!(
        p50_ms <= 100.0,
        "input-to-visible-frame p50 {:.2} ms exceeded 100 ms target",
        p50_ms
    );
    assert!(
        p95_ms <= 200.0,
        "input-to-visible-frame p95 {:.2} ms exceeded 200 ms target",
        p95_ms
    );
}
