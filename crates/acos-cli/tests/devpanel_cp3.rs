//! DEVPANEL1 CP3: event-feed `source` labels, watch-only verify runs, the 16-slot
//! cap and crash cleanup (docs/DEVICE_PANEL_SPEC.md §3, §6, §8.5-8.8).
//!
//! Run live ART test under the shared guard:
//!   env -u TMPDIR tools/art-runtime/run-exclusive.sh --task DEVPANEL1 -- \
//!     tools/guard/guarded.sh --mem 12G --time 1500 -- \
//!     cargo test --offline -p acos-cli --test devpanel_cp3 -- --ignored --test-threads=1

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

mod common;

use common::ShortSocketDir;

/// The vendored hub parsers: `ACOS_HUB_DIST`, this checkout's `.cache/ref`, or
/// the shared cache of the main checkout (worktrees do not copy `.cache/ref`).
fn hub_dist_dir() -> PathBuf {
    if let Ok(p) = std::env::var("ACOS_HUB_DIST") {
        return PathBuf::from(p);
    }
    let rel = ".cache/ref/expo-device-hub-0.22.1/serve-emu-dist";
    let local = repo_root().join(rel);
    if local.join("adb.js").is_file() {
        return local;
    }
    PathBuf::from("/home/Qui/Projects/acos").join(rel)
}

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
        .join(format!("cp3-test-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("runtime")).unwrap();
    std::fs::create_dir_all(dir.join("cache")).unwrap();
    std::fs::create_dir_all(dir.join("avd-running")).unwrap();
    dir
}

struct Daemon {
    child: Child,
    socket: PathBuf,
    dir: PathBuf,
    avd_dir: PathBuf,
    _sockets: ShortSocketDir,
}

impl Daemon {
    fn start(tag: &str, extra_args: &[&str], extra_envs: &[(&str, &str)]) -> Self {
        let dir = scratch(tag);
        let lock_dir = dir.join("adb-locks");
        let avd_dir = dir.join("avd-running");
        let sockets = ShortSocketDir::new(tag);
        let socket = sockets.socket("serve.sock");
        let mut cmd = Command::new(acos_bin());
        cmd.arg("serve")
            .arg("--socket")
            .arg(&socket)
            .arg("--idle-timeout")
            .arg("180")
            .args(extra_args)
            .env("ACOS_RUNTIME_DIR", dir.join("runtime"))
            .env("ACOS_CACHE_DIR", dir.join("cache"))
            .env("ACOS_ADB_LOCK_DIR", &lock_dir)
            .env("ACOS_AVD_RUNNING_DIR", &avd_dir)
            .env("ACOS_MEMORY_GUARD", "off")
            .env("ACOS_VIEW_REGISTRY_DIR", dir.join("viewreg"))
            .env_remove("ACOS_SESSION_SOCKET")
            .stdout(Stdio::null())
            .stderr(Stdio::from(std::fs::File::create(dir.join("serve.err")).unwrap()));
        if let Some(art) = find_bin("acos-art", "ACOS_ART_BIN") {
            cmd.env("ACOS_ART_BIN", art);
        }
        if let Some(bridge) = find_bin("acos-devbridge", "ACOS_DEVBRIDGE_BIN") {
            cmd.env("ACOS_DEVBRIDGE_BIN", bridge);
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
            avd_dir,
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

struct PrivateAdbServer {
    port: u16,
}

impl PrivateAdbServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral adb port");
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let p = port.to_string();
        let _ = Command::new("adb")
            .args(["-P", &p, "kill-server"])
            .env("ANDROID_ADB_SERVER_PORT", &p)
            .output();
        let status = Command::new("adb")
            .args(["-P", &p, "start-server"])
            .env("ANDROID_ADB_SERVER_PORT", &p)
            .status()
            .expect("adb start-server");
        assert!(status.success(), "adb -P {p} start-server failed");
        Self { port }
    }
}

impl Drop for PrivateAdbServer {
    fn drop(&mut self) {
        let p = self.port.to_string();
        let _ = Command::new("adb")
            .args(["-P", &p, "kill-server"])
            .env("ANDROID_ADB_SERVER_PORT", &p)
            .output();
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

fn find_node_bounds(node: &Value, needle_text: Option<&str>, needle_class: Option<&str>) -> Option<(i32, i32, i32, i32)> {
    let text = node.get("text").and_then(Value::as_str).unwrap_or("");
    let class = node.get("class").and_then(Value::as_str).unwrap_or("");
    let text_ok = needle_text.map_or(true, |t| text == t);
    let class_ok = needle_class.map_or(true, |c| class.contains(c));
    if text_ok && class_ok {
        if let Some(b) = node.get("bounds").and_then(parse_bounds_val) {
            return Some(b);
        }
    }
    if let Some(children) = node.get("children").and_then(Value::as_array) {
        for ch in children {
            if let Some(found) = find_node_bounds(ch, needle_text, needle_class) {
                return Some(found);
            }
        }
    }
    None
}

fn find_bounds_in_tree(resp: &Value, needle_text: Option<&str>, needle_class: Option<&str>) -> Option<(i32, i32, i32, i32)> {
    let nodes = resp["result"]["tree"]["nodes"].as_array()?;
    for n in nodes {
        if let Some(found) = find_node_bounds(n, needle_text, needle_class) {
            return Some(found);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// CP3 helpers
// ---------------------------------------------------------------------------

fn adb_devices(port: u16) -> String {
    let p = port.to_string();
    let out = Command::new("adb")
        .args(["-P", &p, "devices"])
        .env("ANDROID_ADB_SERVER_PORT", &p)
        .output()
        .expect("adb devices");
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn emulator_lines(port: u16) -> Vec<String> {
    adb_devices(port)
        .lines()
        .filter(|l| l.starts_with("emulator-") && l.contains("device"))
        .map(str::to_string)
        .collect()
}

/// Poll `adb devices` until `serial` is (or is not) listed; returns the elapsed time.
fn wait_serial(port: u16, serial: &str, present: bool, limit: Duration) -> Duration {
    let start = Instant::now();
    loop {
        let listed = adb_devices(port)
            .lines()
            .any(|l| l.starts_with(serial) && l.contains("\tdevice"));
        if listed == present {
            return start.elapsed();
        }
        assert!(
            start.elapsed() < limit,
            "{serial} {} in `adb devices` after {limit:?}:\n{}",
            if present { "never appeared" } else { "still listed" },
            adb_devices(port)
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

const PANEL_SCRIPT: &str = r#"
import path from "node:path";
import { pathToFileURL } from "node:url";
const dist = process.env.HUB_DIST;
const avdDir = process.env.ACOS_AVD_RUNNING_DIR;
const serial = process.env.ACOS_SERIAL;
const ops = JSON.parse(process.env.PANEL_OPS);
const { findEmulatorGrpcEndpoint, EmulatorGrpcClient } = await import(
  pathToFileURL(path.join(dist, "emulator-grpc.js")).href
);
const endpoint = findEmulatorGrpcEndpoint(serial, { discoveryDirs: () => [avdDir] });
if (!endpoint) { console.log("NO_ENDPOINT"); process.exit(3); }
const client = new EmulatorGrpcClient(endpoint);
const wait = (ms) => new Promise((r) => setTimeout(r, ms));
for (const op of ops) {
  try {
    if (op.touch) {
      const [x, y] = op.touch;
      await client.sendTouch([{ identifier: 0, x, y, pressure: 1 }]);
      await wait(25);
      await client.sendTouch([{ identifier: 0, x, y, pressure: 0 }]);
    } else if (op.text) {
      await client.sendKey({ text: op.text });
    }
    console.log("OK " + JSON.stringify(op));
  } catch (e) {
    console.log("ERR " + JSON.stringify(op) + " " + String(e && e.message || e));
  }
  await wait(150);
}
client.close();
"#;

/// Send panel input through the hub's own gRPC client. Returns stdout lines.
fn panel_input(dir: &Path, avd_dir: &Path, serial: &str, ops: &Value) -> Vec<String> {
    let script = dir.join("panel.mjs");
    std::fs::write(&script, PANEL_SCRIPT).unwrap();
    let out = Command::new("node")
        .arg(&script)
        .env("HUB_DIST", hub_dist_dir())
        .env("ACOS_AVD_RUNNING_DIR", avd_dir)
        .env("ACOS_SERIAL", serial)
        .env("PANEL_OPS", ops.to_string())
        .output()
        .expect("run node panel input");
    assert!(
        out.status.success(),
        "node panel input failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

/// Subscribe to a session's live-view feed (events only) with the reference probe.
fn start_probe(socket: &str, out: &Path, seconds: u32) -> Child {
    std::fs::create_dir_all(out).unwrap();
    Command::new("python3")
        .arg("-I")
        .arg(repo_root().join("tools/view/stream_probe.py"))
        .args(["--socket", socket, "--no-frames", "--seconds"])
        .arg(seconds.to_string())
        .arg("--out")
        .arg(out)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn stream_probe.py")
}

fn read_events(out: &Path) -> Vec<Value> {
    std::fs::read_to_string(out.join("events.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

fn registry_stream_socket(dir: &Path, session_id: &str) -> String {
    let reg = dir.join("viewreg");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(rd) = std::fs::read_dir(&reg) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if name.starts_with(session_id) && name.ends_with(".json") {
                    if let Ok(text) = std::fs::read_to_string(e.path()) {
                        if let Ok(v) = serde_json::from_str::<Value>(&text) {
                            if let Some(s) = v["stream_socket"].as_str() {
                                return s.to_string();
                            }
                        }
                    }
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "no stream_socket in the live registry {} for {session_id}",
            reg.display()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn wait_events(out: &Path, want: impl Fn(&[Value]) -> bool, secs: u64) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let ev = read_events(out);
        if want(&ev) || Instant::now() > deadline {
            return ev;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

// ---------------------------------------------------------------------------
// §8.5: event feed `source: agent|panel`
// ---------------------------------------------------------------------------

#[test]
#[ignore = "boots a real ART guest; panel input through the hub's gRPC client"]
fn cp3_event_feed_source_labels() {
    let text_apk = find_fixture_apk("text-input.apk").expect("text-input.apk not found");
    let adb_srv = PrivateAdbServer::start();
    let adb_port = adb_srv.port.to_string();
    let daemon = Daemon::start("cp3-src", &["--adb"], &[("ANDROID_ADB_SERVER_PORT", &adb_port)]);
    let mut conn = daemon.connect();
    let base = json!({ "session_id": "cp3-src" });
    let launch = conn.call("acos_launch", json!({ "base": base, "apk": text_apk }));
    assert_eq!(launch["ok"], json!(true), "{launch:#?}");
    let serial = launch["result"]["device_panel"]["serial"].as_str().unwrap().to_string();

    let obs = conn.call("acos_observe", json!({ "base": base, "tree": true }));
    let (l, t, r, b) = find_bounds_in_tree(&obs, None, Some("EditText")).expect("EditText bounds");
    let (cx, cy) = ((l + r) / 2, (t + b) / 2);

    let socket = registry_stream_socket(&daemon.dir, "cp3-src");
    let out = daemon.dir.join("probe-src");
    let mut probe = start_probe(&socket, &out, 40);
    std::thread::sleep(Duration::from_millis(800));

    // Agent input through the ACOS tools.
    let tap = conn.call("acos_tap", json!({ "base": base, "x": cx, "y": cy }));
    assert_eq!(tap["ok"], json!(true), "{tap:#?}");
    let typed = conn.call("acos_text", json!({ "base": base, "text": "agent" }));
    assert_eq!(typed["ok"], json!(true), "{typed:#?}");
    // Panel input through the gRPC bridge.
    let lines = panel_input(
        &daemon.dir,
        &daemon.avd_dir,
        &serial,
        &json!([{ "touch": [cx, cy] }, { "text": "panel" }]),
    );
    assert!(lines.iter().all(|l| l.starts_with("OK ")), "panel input failed: {lines:?}");

    let events = wait_events(
        &out,
        |ev| {
            ev.iter().any(|e| e["source"] == "panel" && e["cmd"] == "text")
                && ev.iter().any(|e| e["source"] == "panel" && e["phase"] == "up")
        },
        15,
    );
    let _ = probe.kill();
    let _ = probe.wait();
    let find = |cmd: &str, src: &str| {
        events.iter().find(|e| e["cmd"] == cmd && e["source"] == src).cloned()
    };
    assert!(find("tap", "agent").is_some(), "no source=agent tap in {events:#?}");
    assert!(find("text", "agent").is_some(), "no source=agent text in {events:#?}");
    assert!(find("touch", "panel").is_some(), "no source=panel touch in {events:#?}");
    assert!(find("text", "panel").is_some(), "no source=panel text in {events:#?}");
    // Every input event carries a source; none is mislabelled.
    for e in &events {
        if e["kind"] == "action" && matches!(e["cmd"].as_str(), Some("tap" | "touch" | "key" | "text" | "swipe")) {
            assert!(
                e["source"] == "agent" || e["source"] == "panel",
                "input event without a source: {e}"
            );
        }
    }
    // Typed text is masked in the feed whoever typed it.
    for e in events.iter().filter(|e| e["cmd"] == "text") {
        assert_ne!(e["text"], json!("panel"), "text leaked: {e}");
    }
    // A drag's moves never reach the feed: only the down and up edges.
    let panel_touch = events.iter().filter(|e| e["source"] == "panel" && e["cmd"] == "touch").count();
    assert_eq!(panel_touch, 2, "expected down+up only: {events:#?}");

    assert_eq!(conn.call("acos_destroy", json!({ "base": base }))["ok"], json!(true));
    drop(adb_srv);
}

// ---------------------------------------------------------------------------
// §8.6: a verify run under --adb is watch-only
// ---------------------------------------------------------------------------

#[test]
#[ignore = "boots a real ART guest running `acos verify --adb`"]
fn cp3_verify_run_is_watch_only() {
    let counter_apk = find_fixture_apk("counter.apk").expect("counter.apk not found");
    let adb_srv = PrivateAdbServer::start();
    let adb_port = adb_srv.port.to_string();
    let dir = scratch("cp3-verify");
    let avd_dir = dir.join("avd-running");
    let scenario = dir.join("watch-only.yaml");
    std::fs::write(
        &scenario,
        format!(
            r#"version: 1
name: watch-only
description: Launch counter, hold 14 s while a panel tries to tap Increment, and assert the count is still 0.
app: {apk}
engine: native-preferred
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
      ms: 14000
  - assert_text:
      selector: {{id: count}}
      text: {{exact: "0"}}
"#,
            apk = counter_apk.display()
        ),
    )
    .unwrap();
    let artifacts = dir.join("artifacts");
    let mut child = Command::new(acos_bin())
        .args(["verify", "--adb", "--engine", "art", "--overwrite", "--json"])
        .arg(&scenario)
        .arg("--artifacts")
        .arg(&artifacts)
        .env("ACOS_RUNTIME_DIR", dir.join("runtime"))
        .env("ACOS_CACHE_DIR", dir.join("cache"))
        .env("ACOS_ADB_LOCK_DIR", dir.join("adb-locks"))
        .env("ACOS_AVD_RUNNING_DIR", &avd_dir)
        .env("ACOS_MEMORY_GUARD", "off")
        .env("ANDROID_ADB_SERVER_PORT", &adb_port)
        .env("ACOS_ART_BIN", find_bin("acos-art", "ACOS_ART_BIN").expect("acos-art"))
        .env("ACOS_DEVBRIDGE_BIN", find_bin("acos-devbridge", "ACOS_DEVBRIDGE_BIN").expect("acos-devbridge"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn acos verify --adb");
    // The exposure is announced as one JSON line on stderr.
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });
    let announce: Value = {
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            let line = rx.recv_timeout(deadline.saturating_duration_since(Instant::now())).expect("verify never announced device_panel");
            if let Some(rest) = line.strip_prefix("acos verify: device_panel ") {
                break serde_json::from_str(rest).expect("device_panel JSON");
            }
        }
    };
    assert_eq!(announce["device_panel"]["exposed"], json!(true), "{announce:#?}");
    let serial = announce["device_panel"]["serial"].as_str().unwrap().to_string();
    let socket = announce["stream_socket"].as_str().expect("stream_socket").to_string();
    wait_serial(adb_srv.port, &serial, true, Duration::from_secs(10));

    let out = dir.join("probe-verify");
    let mut probe = start_probe(&socket, &out, 40);
    std::thread::sleep(Duration::from_millis(800));
    // The Increment button sits at about (157, 431) on the 1080x2400 reference.
    let lines = panel_input(&dir, &avd_dir, &serial, &json!([{ "touch": [157, 431] }, { "text": "x" }]));
    assert!(
        lines.iter().all(|l| l.starts_with("ERR ")),
        "panel input during a verify run must be rejected, got {lines:?}"
    );
    let events = wait_events(&out, |ev| ev.iter().any(|e| e["status"] == "VERIFY_WATCH_ONLY"), 10);
    let _ = probe.kill();
    let _ = probe.wait();
    assert!(
        events.iter().any(|e| e["source"] == "panel"
            && e["status"] == "VERIFY_WATCH_ONLY"
            && e["note"] == "panel input ignored: verify run"),
        "feed does not say the panel input was ignored: {events:#?}"
    );

    let status = child.wait().expect("verify exit");
    let mut stdout = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take().unwrap(), &mut stdout).unwrap();
    assert!(status.success(), "verify did not pass (a human influenced the verdict?):\n{stdout}");
    let verdict: Value = serde_json::from_str(stdout.trim()).unwrap_or(Value::Null);
    assert_eq!(verdict["status"], json!("pass"), "{stdout}");
    // The device is gone with the run and no discovery file is left behind.
    wait_serial(adb_srv.port, &serial, false, Duration::from_secs(5));
    let left: Vec<_> = std::fs::read_dir(&avd_dir).unwrap().flatten().collect();
    assert!(left.is_empty(), "ini left after the verify run: {left:?}");
    drop(adb_srv);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// §8.7: 16-slot cap
// ---------------------------------------------------------------------------

#[test]
#[ignore = "boots 17 real ART guests"]
fn cp3_sixteen_slot_cap() {
    let apk = find_fixture_apk("counter.apk").expect("counter.apk not found");
    let adb_srv = PrivateAdbServer::start();
    let adb_port = adb_srv.port.to_string();
    let daemon = Daemon::start(
        "cp3-cap",
        &["--adb", "--max-sessions", "20"],
        &[("ANDROID_ADB_SERVER_PORT", &adb_port)],
    );
    let mut conn = daemon.connect();
    let mut serials = Vec::new();
    let mut seventeenth = Value::Null;
    for i in 0..17 {
        let base = json!({ "session_id": format!("cap-{i:02}") });
        let launch = conn.call("acos_launch", json!({ "base": base, "apk": apk }));
        assert_eq!(launch["ok"], json!(true), "session {i} must run even without a slot: {launch:#?}");
        let dp = launch["result"]["device_panel"].clone();
        if i < 16 {
            assert_eq!(dp["exposed"], json!(true), "session {i}: {dp:#?}");
            serials.push(dp["serial"].as_str().unwrap().to_string());
        } else {
            seventeenth = dp;
        }
    }
    assert_eq!(
        seventeenth,
        json!({"exposed": false, "reason": "all 16 adb emulator slots in use"}),
        "the 17th session must report exposed:false"
    );
    serials.sort();
    let mut want: Vec<String> = (0..16).map(|k| format!("emulator-{}", 5554 + 2 * k)).collect();
    want.sort();
    assert_eq!(serials, want, "16 distinct slots 5554..5584");
    // adb sees all 16 (its scan range is exactly these ports).
    for s in &serials {
        wait_serial(adb_srv.port, s, true, Duration::from_secs(15));
    }
    assert_eq!(emulator_lines(adb_srv.port).len(), 16, "{}", adb_devices(adb_srv.port));
    // The 17th is a normal live session.
    let obs = conn.call("acos_observe", json!({ "base": {"session_id": "cap-16"}, "tree": true }));
    assert_eq!(obs["ok"], json!(true), "17th session must work: {obs:#?}");
    // Freeing a slot lets a later session take it.
    assert_eq!(conn.call("acos_destroy", json!({ "base": {"session_id": "cap-03"} }))["ok"], json!(true));
    let again = conn.call("acos_launch", json!({ "base": {"session_id": "cap-17"}, "apk": apk }));
    assert_eq!(again["result"]["device_panel"]["exposed"], json!(true), "{again:#?}");
    for i in 0..18 {
        if i == 3 {
            continue;
        }
        let _ = conn.call("acos_destroy", json!({ "base": {"session_id": format!("cap-{i:02}")} }));
    }
    drop(adb_srv);
}

// ---------------------------------------------------------------------------
// §8.8: crash cleanup
// ---------------------------------------------------------------------------

fn children_of(pid: u32) -> Vec<u32> {
    let out = Command::new("pgrep").args(["-P", &pid.to_string()]).output().expect("pgrep");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect()
}

fn pid_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[test]
#[ignore = "boots a real ART guest and kills `acos serve` with SIGKILL"]
fn cp3_crash_cleanup() {
    let apk = find_fixture_apk("counter.apk").expect("counter.apk not found");
    let adb_srv = PrivateAdbServer::start();
    let adb_port = adb_srv.port.to_string();
    let mut daemon = Daemon::start("cp3-crash", &["--adb"], &[("ANDROID_ADB_SERVER_PORT", &adb_port)]);
    let mut conn = daemon.connect();
    let base = json!({ "session_id": "crash-1" });
    let launch = conn.call("acos_launch", json!({ "base": base, "apk": apk }));
    assert_eq!(launch["ok"], json!(true), "{launch:#?}");
    let serial = launch["result"]["device_panel"]["serial"].as_str().unwrap().to_string();
    wait_serial(adb_srv.port, &serial, true, Duration::from_secs(10));
    let inis: Vec<PathBuf> = std::fs::read_dir(&daemon.avd_dir).unwrap().flatten().map(|e| e.path()).collect();
    assert_eq!(inis.len(), 1, "{inis:?}");

    let serve_pid = daemon.child.id();
    let descendants = children_of(serve_pid);
    assert!(!descendants.is_empty(), "serve has no children?");
    drop(conn);

    // kill -9 the serve (not a clean shutdown).
    unsafe { libc::kill(serve_pid as i32, libc::SIGKILL) };
    let _ = daemon.child.wait();
    let t0 = Instant::now();
    let gone = wait_serial(adb_srv.port, &serial, false, Duration::from_secs(5));
    eprintln!("cp3_crash_cleanup: device gone from adb devices after {gone:?} (total {:?})", t0.elapsed());

    // Children are not left behind (PDEATHSIG / process-group teardown).
    let deadline = Instant::now() + Duration::from_secs(10);
    while descendants.iter().any(|p| pid_alive(*p)) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    let survivors: Vec<u32> = descendants.iter().copied().filter(|p| pid_alive(*p)).collect();
    for p in &survivors {
        unsafe { libc::kill(*p as i32, libc::SIGKILL) };
    }
    assert!(survivors.is_empty(), "children of the killed serve survived: {survivors:?}");

    // A stale ini may remain; the next serve start sweeps it.
    let stale = std::fs::read_dir(&daemon.avd_dir).unwrap().flatten().count();
    eprintln!("cp3_crash_cleanup: {stale} stale ini file(s) after the crash");
    // A real emulator's ini in the same directory must survive the sweep.
    let foreign = daemon.avd_dir.join("pid_999999.ini");
    std::fs::write(&foreign, "port.serial=5554\navd.name=Pixel_8_API_35\n").unwrap();

    let socket2 = daemon._sockets.socket("serve2.sock");
    let mut serve2 = Command::new(acos_bin())
        .arg("serve")
        .arg("--socket")
        .arg(&socket2)
        .args(["--adb", "--idle-timeout", "60"])
        .env("ACOS_RUNTIME_DIR", daemon.dir.join("runtime"))
        .env("ACOS_CACHE_DIR", daemon.dir.join("cache"))
        .env("ACOS_ADB_LOCK_DIR", daemon.dir.join("adb-locks"))
        .env("ACOS_AVD_RUNNING_DIR", &daemon.avd_dir)
        .env("ACOS_MEMORY_GUARD", "off")
        .env("ANDROID_ADB_SERVER_PORT", &adb_port)
        .env_remove("ACOS_SESSION_SOCKET")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn second serve");
    let deadline = Instant::now() + Duration::from_secs(20);
    while !socket2.exists() {
        assert!(Instant::now() < deadline, "second serve never bound");
        std::thread::sleep(Duration::from_millis(25));
    }
    let left: Vec<String> = std::fs::read_dir(&daemon.avd_dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(left, vec!["pid_999999.ini".to_string()], "stale ACOS ini not swept (or a foreign one was): {left:?}");

    // The slot's flock died with the process: the same serial is free again.
    let mut conn2 = Conn::new(&socket2);
    let relaunch = conn2.call("acos_launch", json!({ "base": {"session_id": "crash-2"}, "apk": apk }));
    assert_eq!(relaunch["ok"], json!(true), "{relaunch:#?}");
    assert_eq!(relaunch["result"]["device_panel"]["serial"], json!(serial), "{relaunch:#?}");
    let _ = conn2.call("acos_destroy", json!({ "base": {"session_id": "crash-2"} }));
    let _ = serve2.kill();
    let _ = serve2.wait();
    drop(adb_srv);
}

// ---------------------------------------------------------------------------
// CR #11: deferred ART launch (`dev_server: start` before `launch`) under --adb
// ---------------------------------------------------------------------------

#[test]
fn cp3_deferred_verify_adb_exposes_only_after_launch() {
    use std::os::unix::fs::PermissionsExt;

    let dir = scratch("cp3-deferred-adb");
    let avd_dir = dir.join("avd-running");
    let sockets = ShortSocketDir::new("cp3-def");

    // Unused TCP port for ANDROID_ADB_SERVER_PORT so notify_adb_server never touches host port 5037.
    let unused_adb_port = {
        let l = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral unused port");
        l.local_addr().unwrap().port()
    };
    let dev_server_port = {
        let l = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral metro port");
        l.local_addr().unwrap().port()
    };

    // Fake react-native/cli.js + metro/ dir that records whether any pid_*.ini existed at dev_server start.
    let nm_dir = dir.join("node_modules");
    std::fs::create_dir_all(nm_dir.join("react-native")).unwrap();
    std::fs::create_dir_all(nm_dir.join("metro")).unwrap();
    std::fs::write(nm_dir.join("metro/index.js"), "module.exports = {};\n").unwrap();
    let metro_saw_ini = dir.join("metro_saw_ini.txt");
    std::fs::write(
        nm_dir.join("react-native/cli.js"),
        r#"
const fs = require("fs");
const http = require("http");
const path = require("path");
const args = process.argv.slice(2);
const portIdx = args.indexOf("--port");
const port = portIdx >= 0 ? Number(args[portIdx + 1]) : 8081;
const avdDir = process.env.ACOS_AVD_RUNNING_DIR;
const entries = fs.existsSync(avdDir) ? fs.readdirSync(avdDir).filter((f) => f.endsWith(".ini")) : [];
fs.writeFileSync(process.env.METRO_SAW_INI, JSON.stringify(entries));
const srv = http.createServer((req, res) => {
  if (req.url && req.url.startsWith("/status")) {
    res.writeHead(200, { "Content-Type": "text/plain" });
    res.end("packager-status:running\n");
    return;
  }
  res.writeHead(404);
  res.end();
});
srv.listen(port, "127.0.0.1");
process.on("SIGTERM", () => srv.close(() => process.exit(0)));
"#,
    )
    .unwrap();

    let project_dir = dir.join("project");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(project_dir.join("index.js"), "// dummy\n").unwrap();

    // Fake acos-art binary speaking the session line-JSON protocol and recording avd_dir on `tree`.
    let fake_art = dir.join("fake_art.py");
    let tree_saw_ini = dir.join("tree_saw_ini.txt");
    let frames_dir = dir.join("frames");
    std::fs::create_dir_all(&frames_dir).unwrap();
    std::fs::write(
        &fake_art,
        r##"#!/usr/bin/env python3
import base64, hashlib, json, os, sys

PNG = base64.b64decode(
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4//8/AAX+Av4N70a4AAAAAElFTkSuQmCC"
)
out = os.environ["FAKE_ART_OUT"]
avd_dir = os.environ["ACOS_AVD_RUNNING_DIR"]
seq = 0

def reply_frame(cmd, with_tree):
    global seq
    seq += 1
    p = os.path.join(out, f"f_{seq}.png")
    data = PNG + f"#seq={seq}".encode()
    with open(p, "wb") as f:
        f.write(data)
    rep = {
        "ok": True,
        "cmd": cmd,
        "frame_seq": seq,
        "png": p,
        "md5": hashlib.md5(data).hexdigest(),
    }
    if with_tree:
        rep["tree"] = {
            "nodes": [{
                "class": "android.widget.TextView",
                "id": "dev.acos.fixtures.counter:id/count",
                "text": "0",
                "bounds": "[42,317][63,368]",
                "visibility": 0,
            }]
        }
    return rep

print(json.dumps(reply_frame("ready", True)), flush=True)
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    cmd = req.get("cmd", "")
    if cmd == "close":
        print(json.dumps({"ok": True, "cmd": "close", "frame_seq": seq}), flush=True)
        break
    if cmd == "tree":
        inis = sorted(f for f in os.listdir(avd_dir) if f.endswith(".ini")) if os.path.isdir(avd_dir) else []
        with open(os.environ["TREE_SAW_INI"], "w") as f:
            json.dump(inis, f)
        print(json.dumps(reply_frame("tree", True)), flush=True)
    elif cmd == "screenshot":
        print(json.dumps(reply_frame("screenshot", False)), flush=True)
    elif cmd == "reset":
        print(json.dumps(reply_frame("reset", True)), flush=True)
    else:
        print(json.dumps({"ok": False, "cmd": cmd, "code": "BAD_CMD", "error": "unknown"}), flush=True)
"##,
    )
    .unwrap();
    std::fs::set_permissions(&fake_art, std::fs::Permissions::from_mode(0o755)).unwrap();

    // Use real acos-devbridge when built alongside, else fall back to a minimal mock.
    let devbridge_bin = find_bin("acos-devbridge", "ACOS_DEVBRIDGE_BIN").unwrap_or_else(|| {
        let mock = dir.join("fake_devbridge.py");
        std::fs::write(
            &mock,
            r##"#!/usr/bin/env python3
import argparse, json, os, signal, socket

p = argparse.ArgumentParser()
p.add_argument("--socket", required=True)
p.add_argument("--callback", required=True)
args = p.parse_args()
if os.path.exists(args.socket):
    os.unlink(args.socket)
srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
srv.bind(args.socket)
srv.listen(8)
running = True
def stop(_s, _f):
    global running
    running = False
    try:
        srv.close()
    except Exception:
        pass
signal.signal(signal.SIGTERM, stop)
while running:
    try:
        conn, _ = srv.accept()
    except OSError:
        break
    with conn:
        line = conn.makefile("r").readline()
        if not line:
            continue
        req = json.loads(line)
        if req.get("cmd") == "register":
            resp = {"ok": True, "grpc_port": 50051}
        else:
            resp = {"ok": True}
        conn.sendall((json.dumps(resp) + "\n").encode())
"##,
        )
        .unwrap();
        std::fs::set_permissions(&mock, std::fs::Permissions::from_mode(0o755)).unwrap();
        mock
    });

    let apk = find_fixture_apk("counter.apk").unwrap_or_else(|| {
        let p = dir.join("dummy.apk");
        std::fs::write(&p, b"PK\x05\x06").unwrap();
        p
    });

    let scenario = dir.join("deferred-watch.yaml");
    std::fs::write(
        &scenario,
        format!(
            r#"version: 1
name: deferred-watch
description: Verify --adb registers WatchViewOps after deferred launch.
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
capabilities: [lifecycle, ui.offscreen, ui.text]
needs_env: [metro]
expectation: pass
expectation_reason: exercises deferred ART session boot under --adb
budgets:
  memory_mib: 512
  wall_ms: 60000
steps:
  - dev_server:
      op: start
      project: {project}
      port: {port}
      expose_to_guest: false
      timeout_ms: 15000
  - launch:
      activity: dev.acos.fixtures.counter/.MainActivity
  - wait_for:
      selector: {{id: count}}
      timeout_ms: 5000
  - dev_server:
      op: stop
"#,
            apk = apk.display(),
            project = project_dir.display(),
            port = dev_server_port,
        ),
    )
    .unwrap();

    let artifacts = dir.join("artifacts");
    let out = Command::new(acos_bin())
        .args(["verify", "--adb", "--engine", "art", "--overwrite", "--json"])
        .arg(&scenario)
        .arg("--artifacts")
        .arg(&artifacts)
        .env("ACOS_NO_DAEMON", "1")
        .env("ACOS_RUNTIME_DIR", dir.join("runtime"))
        .env("ACOS_CACHE_DIR", dir.join("cache"))
        .env("ACOS_ADB_LOCK_DIR", dir.join("adb-locks"))
        .env("ACOS_AVD_RUNNING_DIR", &avd_dir)
        .env("ACOS_DEV_SERVER_DIR", dir.join("dev-server"))
        .env("ACOS_METRO_NODE_MODULES", &nm_dir)
        .env("ACOS_MEMORY_GUARD", "off")
        .env("XDG_RUNTIME_DIR", sockets.dir())
        .env("ANDROID_ADB_SERVER_PORT", unused_adb_port.to_string())
        .env("ACOS_ART_BIN", &fake_art)
        .env("ACOS_DEVBRIDGE_BIN", &devbridge_bin)
        .env("FAKE_ART_OUT", &frames_dir)
        .env("METRO_SAW_INI", &metro_saw_ini)
        .env("TREE_SAW_INI", &tree_saw_ini)
        .output()
        .expect("run acos verify --adb on deferred scenario");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "verify failed:\nstdout={stdout}\nstderr={stderr}");
    let verdict: Value = serde_json::from_str(stdout.trim()).expect("verdict JSON");
    assert_eq!(verdict["status"], json!("pass"), "verdict={stdout}");

    // 1. Before `launch` (while dev_server starts), no device_panel exposure / .ini exists yet.
    let at_metro: Vec<String> = serde_json::from_str(
        &std::fs::read_to_string(&metro_saw_ini).expect("metro_saw_ini written"),
    )
    .unwrap();
    assert!(
        at_metro.is_empty(),
        "device_panel exposed too early during dev_server start: {at_metro:?}"
    );

    // 2. Once `launch` boots the deferred ART session, stderr announces exposure and pid_*.ini exists.
    let announce_line = stderr
        .lines()
        .find_map(|l| l.strip_prefix("acos verify: device_panel "))
        .unwrap_or_else(|| {
            panic!("verify never announced device_panel on deferred launch.\nstderr={stderr}")
        });
    let announce: Value = serde_json::from_str(announce_line).expect("device_panel JSON");
    assert_eq!(announce["device_panel"]["exposed"], json!(true), "{announce:#?}");

    let at_tree: Vec<String> = serde_json::from_str(
        &std::fs::read_to_string(&tree_saw_ini).expect("tree_saw_ini written"),
    )
    .unwrap();
    assert_eq!(
        at_tree.len(),
        1,
        "expected exactly 1 pid_*.ini visible after deferred launch, saw {at_tree:?}"
    );

    // 3. After verify exits, shutdown_all cleaned up the .ini.
    let left: Vec<_> = std::fs::read_dir(&avd_dir).unwrap().flatten().collect();
    assert!(left.is_empty(), "ini left after deferred verify run: {left:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

