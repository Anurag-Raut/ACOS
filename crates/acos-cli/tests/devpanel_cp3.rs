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
