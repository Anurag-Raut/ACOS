//! HUMANVIEW1 Checkpoint 1 integration tests:
//!
//! 1. `cp1_device_name_sanitized_in_launch_and_registry` — fast mock-guest test
//!    verifying `device_name` sanitization, `watch_only: false`, `serve_socket`,
//!    and registry cleanup on `acos_destroy`.
//! 2. `cp1_event_feed_source_labels_and_app_label` (`#[ignore]`) — boots a real
//!    ART guest on `text-input.apk`, drives agent (`acos_tap`, `acos_text`) and
//!    panel (`panel_touch`, `panel_text`), and checks `source: agent|panel`,
//!    masked text, down+up touch edges only, `app_label`, and `EditText` state.
//! 3. `cp1_verify_run_publishes_watch_only_and_records_ignored_panel_input`
//!    (`#[ignore]`) — boots `acos verify --engine art` on `counter.apk`,
//!    verifies the published `LiveEntry` has `watch_only: true`,
//!    `serve_socket: null`, and `app_label`, sends `panel_input_ignored`, checks
//!    `VERIFY_WATCH_ONLY` in the event stream, checks `acos verify` passes, and
//!    checks the registry entry is removed on exit.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
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

fn find_fixture_apk(name: &str) -> Option<PathBuf> {
    for candidate in [
        repo_root().join("fixtures/build/apps").join(name),
        PathBuf::from("/home/Qui/Projects/acos/fixtures/build/apps").join(name),
    ] {
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn scratch(tag: &str) -> PathBuf {
    let dir = repo_root()
        .join(".cache")
        .join(format!("hv-cp1-{tag}-{}", std::process::id()));
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
    fn start(tag: &str, extra_envs: &[(&str, &str)]) -> Self {
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
        for (k, v) in extra_envs {
            cmd.env(k, v);
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

fn read_registry_entries(viewreg: &Path) -> Vec<Value> {
    let Ok(rd) = std::fs::read_dir(viewreg) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in rd.flatten() {
        let p = entry.path();
        if p.extension().and_then(|e| e.to_str()) == Some("json") {
            if let Ok(raw) = std::fs::read_to_string(&p) {
                if let Ok(val) = serde_json::from_str::<Value>(&raw) {
                    out.push(val);
                }
            }
        }
    }
    out
}

fn wait_registry_entry(viewreg: &Path, timeout: Duration) -> Value {
    let deadline = Instant::now() + timeout;
    loop {
        let entries = read_registry_entries(viewreg);
        if let Some(first) = entries.into_iter().next() {
            return first;
        }
        assert!(
            Instant::now() < deadline,
            "no registry entry appeared in {}",
            viewreg.display()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

struct EventProbe {
    stop: Arc<AtomicBool>,
    events: Arc<Mutex<Vec<Value>>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl EventProbe {
    fn start(socket_path: &str) -> Self {
        let mut stream = UnixStream::connect(socket_path).expect("connect to stream_socket");
        stream
            .set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        stream
            .write_all(
                b"{\"v\":1,\"subscribe\":{\"frames\":false,\"events\":true,\"max_fps\":15,\"scale\":0.5,\"format\":\"png\"}}\n",
            )
            .unwrap();
        stream.flush().unwrap();

        let stop = Arc::new(AtomicBool::new(false));
        let events = Arc::new(Mutex::new(Vec::new()));
        let stop_t = Arc::clone(&stop);
        let events_t = Arc::clone(&events);

        let handle = std::thread::spawn(move || {
            let mut len_buf = [0u8; 4];
            while !stop_t.load(Ordering::Relaxed) {
                match stream.read_exact(&mut len_buf) {
                    Ok(()) => {}
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
                let total_len = u32::from_be_bytes(len_buf) as usize;
                if total_len == 0 || total_len > 16 * 1024 * 1024 {
                    break;
                }
                let mut buf = vec![0u8; total_len];
                if stream.read_exact(&mut buf).is_err() {
                    break;
                }
                if buf[0] == b'E' {
                    if let Ok(val) = serde_json::from_slice::<Value>(&buf[1..]) {
                        events_t.lock().unwrap().push(val);
                    }
                }
            }
        });

        Self {
            stop,
            events,
            handle: Some(handle),
        }
    }

    fn wait_until<F>(&self, pred: F, timeout: Duration) -> Vec<Value>
    where
        F: Fn(&[Value]) -> bool,
    {
        let deadline = Instant::now() + timeout;
        loop {
            let snap = self.events.lock().unwrap().clone();
            if pred(&snap) || Instant::now() >= deadline {
                return snap;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for EventProbe {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn parse_bounds_val(val: &Value) -> Option<(i32, i32, i32, i32)> {
    if let Some(s) = val.as_str() {
        let s = s.trim();
        let rest = s.strip_prefix('[')?;
        let mid = rest.find("][")?;
        let first = &rest[..mid];
        let second = rest[mid + 2..].strip_suffix(']')?;
        let (l, t) = first.split_once(',')?;
        let (r, b) = second.split_once(',')?;
        return Some((
            l.trim().parse().ok()?,
            t.trim().parse().ok()?,
            r.trim().parse().ok()?,
            b.trim().parse().ok()?,
        ));
    }
    if let Some(b) = val.as_array() {
        if b.len() == 4 {
            return Some((
                b[0].as_i64()? as i32,
                b[1].as_i64()? as i32,
                b[2].as_i64()? as i32,
                b[3].as_i64()? as i32,
            ));
        }
    }
    None
}

fn find_node(node: &Value, needle_text: Option<&str>, needle_class: Option<&str>) -> Option<Value> {
    let text = node.get("text").and_then(Value::as_str).unwrap_or("");
    let class = node.get("class").and_then(Value::as_str).unwrap_or("");
    let text_ok = needle_text.map_or(true, |t| text == t);
    let class_ok = needle_class.map_or(true, |c| class.contains(c));
    if text_ok && class_ok {
        return Some(node.clone());
    }
    if let Some(children) = node.get("children").and_then(Value::as_array) {
        for ch in children {
            if let Some(found) = find_node(ch, needle_text, needle_class) {
                return Some(found);
            }
        }
    }
    None
}

fn find_in_tree(resp: &Value, needle_text: Option<&str>, needle_class: Option<&str>) -> Option<Value> {
    let nodes = resp["result"]["tree"]["nodes"].as_array()?;
    for n in nodes {
        if let Some(found) = find_node(n, needle_text, needle_class) {
            return Some(found);
        }
    }
    None
}

#[test]
fn cp1_device_name_sanitized_in_launch_and_registry() {
    let prep = scratch("prep-fake");
    let fake_art = prep.join("fake_art.py");
    let frames_dir = prep.join("frames");
    std::fs::create_dir_all(&frames_dir).unwrap();
    std::fs::write(
        &fake_art,
        r##"#!/usr/bin/env python3
import base64, hashlib, json, os, sys

PNG = base64.b64decode(
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4//8/AAX+Av4N70a4AAAAAElFTkSuQmCC"
)
out = os.environ["FAKE_ART_OUT"]
seq = 0

def reply_frame(cmd):
    global seq
    seq += 1
    p = os.path.join(out, f"f_{seq}.png")
    data = PNG + f"#seq={seq}".encode()
    with open(p, "wb") as f:
        f.write(data)
    return {
        "ok": True,
        "cmd": cmd,
        "frame_seq": seq,
        "png": p,
        "md5": hashlib.md5(data).hexdigest(),
        "activity": "dev.acos.fixtures.counter/.MainActivity",
        "width": 1080,
        "height": 2400,
        "tree": {
            "nodes": [{
                "class": "android.widget.TextView",
                "id": "dev.acos.fixtures.counter:id/count",
                "text": "0",
                "bounds": "[42,317][63,368]",
                "visibility": 0,
            }]
        },
    }

print(json.dumps(reply_frame("ready")), flush=True)
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    cmd = req.get("cmd", "")
    if cmd == "close":
        print(json.dumps({"ok": True, "cmd": "close", "frame_seq": seq}), flush=True)
        break
    print(json.dumps(reply_frame(cmd)), flush=True)
"##,
    )
    .unwrap();
    std::fs::set_permissions(&fake_art, std::fs::Permissions::from_mode(0o755)).unwrap();

    let apk = find_fixture_apk("counter.apk").unwrap_or_else(|| {
        let p = prep.join("dummy.apk");
        std::fs::write(&p, b"PK\x05\x06").unwrap();
        p
    });

    let fake_art_str = fake_art.display().to_string();
    let frames_str = frames_dir.display().to_string();
    let daemon = Daemon::start(
        "cp1-devname",
        &[("ACOS_ART_BIN", &fake_art_str), ("FAKE_ART_OUT", &frames_str)],
    );
    let mut conn = daemon.connect();
    let base = json!({ "session_id": "cp1-devname" });

    let launch = conn.call(
        "acos_launch",
        json!({
            "base": base,
            "apk": apk,
            "device_name": "Pixel\x00=Pro  \n 9 ",
        }),
    );
    assert_eq!(launch["ok"], json!(true), "{launch:#?}");
    assert_eq!(launch["result"]["device_name"], json!("Pixel Pro 9"));

    let entry = wait_registry_entry(&daemon.viewreg, Duration::from_secs(5));
    assert_eq!(entry["session_id"], json!("cp1-devname"));
    assert_eq!(entry["device_name"], json!("Pixel Pro 9"));
    assert_eq!(entry["watch_only"], json!(false));
    let expected_sock = std::fs::canonicalize(&daemon.socket)
        .unwrap_or_else(|_| daemon.socket.clone())
        .display()
        .to_string();
    assert_eq!(entry["serve_socket"], json!(expected_sock));

    let destroy = conn.call("acos_destroy", json!({ "base": base }));
    assert_eq!(destroy["ok"], json!(true), "{destroy:#?}");
    assert!(
        read_registry_entries(&daemon.viewreg).is_empty(),
        "registry entry must be removed on acos_destroy"
    );

    let _ = std::fs::remove_dir_all(&prep);
}

#[test]
#[ignore = "boots a real ART guest"]
fn cp1_event_feed_source_labels_and_app_label() {
    let text_apk = find_fixture_apk("text-input.apk").expect("text-input.apk not found");
    let daemon = Daemon::start("cp1-src", &[]);
    let mut conn = daemon.connect();
    let base = json!({ "session_id": "cp1-src" });

    let launch = conn.call(
        "acos_launch",
        json!({ "base": base, "apk": text_apk, "device_name": "Text Rig" }),
    );
    assert_eq!(launch["ok"], json!(true), "{launch:#?}");
    assert_eq!(launch["result"]["device_name"], json!("Text Rig"));

    let entry = wait_registry_entry(&daemon.viewreg, Duration::from_secs(5));
    assert_eq!(entry["watch_only"], json!(false));
    assert_eq!(entry["device_name"], json!("Text Rig"));
    assert!(
        entry["app_label"]
            .as_str()
            .is_some_and(|s| !s.is_empty()),
        "expected non-empty app_label in registry: {entry:#?}"
    );
    assert!(
        entry["serve_socket"]
            .as_str()
            .is_some_and(|s| !s.is_empty()),
        "expected serve_socket in registry: {entry:#?}"
    );
    let stream_socket = entry["stream_socket"]
        .as_str()
        .expect("stream_socket missing from registry")
        .to_string();

    let obs = conn.call("acos_observe", json!({ "base": base, "tree": true }));
    let edit_node = find_in_tree(&obs, None, Some("EditText")).expect("EditText node");
    let (l, t, r, b) = parse_bounds_val(&edit_node["bounds"]).expect("EditText bounds");
    let (cx, cy) = ((l + r) / 2, (t + b) / 2);

    let probe = EventProbe::start(&stream_socket);
    std::thread::sleep(Duration::from_millis(300));

    // Agent input via normal ACOS tools.
    let tap = conn.call("acos_tap", json!({ "base": base, "x": cx, "y": cy }));
    assert_eq!(tap["ok"], json!(true), "{tap:#?}");
    let typed = conn.call("acos_text", json!({ "base": base, "text": "agent" }));
    assert_eq!(typed["ok"], json!(true), "{typed:#?}");

    // Panel input via internal panel_* dispatch on serve socket (down, moves, up, text).
    let now_ms: i64 = 10_000;
    let pt_down = conn.call(
        "panel_touch",
        json!({
            "base": base,
            "action": 0,
            "down_time": now_ms,
            "event_time": now_ms,
            "pointers": [[0, cx, cy]],
        }),
    );
    assert_eq!(pt_down["ok"], json!(true), "{pt_down:#?}");
    for step in 1i32..=2 {
        let pt_move = conn.call(
            "panel_touch",
            json!({
                "base": base,
                "action": 2,
                "down_time": now_ms,
                "event_time": now_ms + i64::from(step) * 16,
                "pointers": [[0, cx + step, cy]],
            }),
        );
        assert_eq!(pt_move["ok"], json!(true), "{pt_move:#?}");
    }
    let pt_up = conn.call(
        "panel_touch",
        json!({
            "base": base,
            "action": 1,
            "down_time": now_ms,
            "event_time": now_ms + 48,
            "pointers": [[0, cx + 2, cy]],
        }),
    );
    assert_eq!(pt_up["ok"], json!(true), "{pt_up:#?}");
    let pt_text = conn.call("panel_text", json!({ "base": base, "text": "panel" }));
    assert_eq!(pt_text["ok"], json!(true), "{pt_text:#?}");

    let events = probe.wait_until(
        |ev| {
            ev.iter().any(|e| e["source"] == "panel" && e["cmd"] == "text")
                && ev.iter().any(|e| e["source"] == "panel" && e["phase"] == "up")
        },
        Duration::from_secs(10),
    );
    drop(probe);

    let find = |cmd: &str, src: &str| {
        events
            .iter()
            .find(|e| e["cmd"] == cmd && e["source"] == src)
            .cloned()
    };
    assert!(find("tap", "agent").is_some(), "no source=agent tap in {events:#?}");
    assert!(find("text", "agent").is_some(), "no source=agent text in {events:#?}");
    assert!(find("touch", "panel").is_some(), "no source=panel touch in {events:#?}");
    assert!(find("text", "panel").is_some(), "no source=panel text in {events:#?}");

    for e in &events {
        if e["kind"] == "action"
            && matches!(
                e["cmd"].as_str(),
                Some("tap" | "touch" | "key" | "text" | "swipe")
            )
        {
            assert!(
                e["source"] == "agent" || e["source"] == "panel",
                "input event without valid source: {e}"
            );
        }
    }
    for e in events.iter().filter(|e| e["cmd"] == "text" && e["source"] == "panel") {
        assert_ne!(e["text"], json!("panel"), "panel text leaked into feed: {e}");
        assert_eq!(e["text"], json!("[masked]"), "panel text must be masked: {e}");
    }
    let panel_touch = events
        .iter()
        .filter(|e| e["source"] == "panel" && e["cmd"] == "touch")
        .count();
    assert_eq!(
        panel_touch, 2,
        "expected only down+up edges in feed (moves suppressed): {events:#?}"
    );

    let obs_after = conn.call("acos_observe", json!({ "base": base, "tree": true }));
    let edit_after = find_in_tree(&obs_after, None, Some("EditText")).expect("EditText after");
    let text_after = edit_after["text"].as_str().unwrap_or("");
    assert!(
        text_after.contains("agent") && text_after.contains("panel"),
        "expected EditText to receive both agent and panel text, got {text_after:?}"
    );

    assert_eq!(
        conn.call("acos_destroy", json!({ "base": base }))["ok"],
        json!(true)
    );
}

#[test]
#[ignore = "boots a real ART guest running acos verify"]
fn cp1_verify_run_publishes_watch_only_and_records_ignored_panel_input() {
    let counter_apk = find_fixture_apk("counter.apk").expect("counter.apk not found");
    let dir = scratch("cp1-verify");
    let viewreg = dir.join("viewreg");
    let scenario = dir.join("watch-only.yaml");
    std::fs::write(
        &scenario,
        format!(
            r#"version: 1
name: watch-only
description: Launch counter, hold 10 s while stream receives panel_input_ignored, and assert count stays 0.
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
      ms: 10000
  - assert_text:
      selector: {{id: count}}
      text: {{exact: "0"}}
"#,
            apk = counter_apk.display()
        ),
    )
    .unwrap();

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

    let child = cmd.spawn().expect("spawn acos verify");
    let entry = wait_registry_entry(&viewreg, Duration::from_secs(60));
    assert_eq!(entry["watch_only"], json!(true), "{entry:#?}");
    assert!(
        entry["serve_socket"].is_null(),
        "verify entry must have serve_socket: null: {entry:#?}"
    );
    assert!(
        entry["app_label"]
            .as_str()
            .is_some_and(|s| !s.is_empty()),
        "expected non-empty app_label in verify registry: {entry:#?}"
    );
    let stream_socket = entry["stream_socket"]
        .as_str()
        .expect("stream_socket in verify entry")
        .to_string();

    let probe = EventProbe::start(&stream_socket);
    std::thread::sleep(Duration::from_millis(300));

    let mut ping = UnixStream::connect(&stream_socket).expect("connect stream_socket for ignored ping");
    ping.write_all(b"{\"v\":1,\"panel_input_ignored\":true}\n")
        .unwrap();
    ping.flush().unwrap();
    drop(ping);

    let events = probe.wait_until(
        |ev| ev.iter().any(|e| e["status"] == "VERIFY_WATCH_ONLY"),
        Duration::from_secs(10),
    );
    assert!(
        events.iter().any(|e| e["status"] == "VERIFY_WATCH_ONLY"),
        "expected VERIFY_WATCH_ONLY event in {events:#?}"
    );
    drop(probe);

    let output = child.wait_with_output().expect("wait acos verify");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "verify failed:\nstdout={stdout}\nstderr={stderr}"
    );
    let env: Value = serde_json::from_str(stdout.trim()).expect("verify output JSON");
    assert_eq!(env["ok"], json!(true), "{env:#?}");
    assert_eq!(env["status"], json!("pass"), "{env:#?}");
    let verdict_status = env["result"]["verdict_status"]
        .as_str()
        .or_else(|| env["result"]["verdict"]["status"].as_str());
    assert_eq!(verdict_status, Some("pass"), "{env:#?}");

    assert!(
        read_registry_entries(&viewreg).is_empty(),
        "verify registry entry must be cleaned up on exit"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
