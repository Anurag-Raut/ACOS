//! DEVPANEL1 CP1: `acos serve --adb` endpoint, slot allocation, host shell
//! translator, and real `/usr/bin/adb` + `expo-device-hub` parser compatibility.
//!
//! Run live ART test under the shared guard:
//!   env -u TMPDIR tools/art-runtime/run-exclusive.sh --task DEVPANEL1 -- \
//!     tools/guard/guarded.sh --mem 12G --time 1500 -- \
//!     cargo test --offline -p acos-cli --test devpanel_cp1_adb -- --ignored --test-threads=1

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

mod common;

use common::ShortSocketDir;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn acos_bin() -> &'static str {
    env!("CARGO_BIN_EXE_acos")
}

fn find_art_bin() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("ACOS_ART_BIN") {
        let pb = PathBuf::from(p);
        if pb.is_file() {
            return Some(pb);
        }
    }
    let sibling = Path::new(acos_bin()).with_file_name("acos-art");
    if sibling.is_file() {
        return Some(sibling);
    }
    None
}

fn find_counter_apk() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("ACOS_TEST_APK") {
        let pb = PathBuf::from(p);
        if pb.is_file() {
            return Some(pb);
        }
    }
    for candidate in [
        repo_root().join("fixtures/build/apps/counter.apk"),
        PathBuf::from("/home/Qui/Projects/acos/fixtures/build/apps/counter.apk"),
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
        .join(format!("cp1-test-{tag}-{}", std::process::id()));
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
    lock_dir: PathBuf,
    _sockets: ShortSocketDir,
}

impl Daemon {
    fn start(tag: &str, extra_args: &[&str], extra_envs: &[(&str, &str)]) -> Self {
        let dir = scratch(tag);
        let lock_dir = dir.join("adb-locks");
        let sockets = ShortSocketDir::new(tag);
        let socket = sockets.socket("serve.sock");
        let mut cmd = Command::new(acos_bin());
        cmd.arg("serve")
            .arg("--socket")
            .arg(&socket)
            .arg("--idle-timeout")
            .arg("120")
            .args(extra_args)
            .env("ACOS_RUNTIME_DIR", dir.join("runtime"))
            .env("ACOS_CACHE_DIR", dir.join("cache"))
            .env("ACOS_ADB_LOCK_DIR", &lock_dir)
            .env("ACOS_AVD_RUNNING_DIR", dir.join("avd-running"))
            .env("ACOS_MEMORY_GUARD", "off")
            .env_remove("ACOS_SESSION_SOCKET")
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        if let Some(art) = find_art_bin() {
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
            lock_dir,
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
        // Reserve a free port in 5200..5500 or ephemeral loopback port outside 5037.
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

#[test]
fn serve_without_adb_creates_no_adb_artifacts() {
    let daemon = Daemon::start("no-adb", &[], &[]);
    let mut conn = daemon.connect();
    let pong = conn.call("ping", json!({}));
    assert_eq!(pong["ok"], json!(true));
    assert!(
        !daemon.lock_dir.exists(),
        "ACOS_ADB_LOCK_DIR should not exist when --adb is off"
    );
}

#[test]
fn serve_with_adb_starts_cleanly_when_idle() {
    let daemon = Daemon::start("idle-adb", &["--adb"], &[]);
    let mut conn = daemon.connect();
    let pong = conn.call("ping", json!({}));
    assert_eq!(pong["ok"], json!(true));
    let listed = conn.call("session_list", json!({}));
    assert_eq!(listed["ok"], json!(true), "{listed:#?}");
    assert_eq!(listed["result"]["sessions"], json!([]));
}

#[test]
#[ignore = "boots a real ART guest and exercises real /usr/bin/adb + expo-device-hub parsers"]
fn cp1_live_art_adb_and_hub_parsers() {
    let Some(apk) = find_counter_apk() else {
        panic!("counter.apk not found; set ACOS_TEST_APK or build fixtures");
    };
    let hub_dist = repo_root().join(".cache/ref/expo-device-hub-0.22.1/serve-emu-dist");
    assert!(
        hub_dist.join("adb.js").is_file(),
        "vendored expo-device-hub missing at {}",
        hub_dist.display()
    );

    let adb_srv = PrivateAdbServer::start();
    let adb_port_str = adb_srv.port.to_string();
    let daemon = Daemon::start(
        "live-cp1",
        &["--adb"],
        &[("ANDROID_ADB_SERVER_PORT", &adb_port_str)],
    );
    let mut conn = daemon.connect();
    let base = json!({ "session_id": "cp1-int" });

    let launch = conn.call("acos_launch", json!({ "base": base, "apk": apk }));
    assert_eq!(launch["ok"], json!(true), "{launch:#?}");
    let dp = &launch["result"]["device_panel"];
    assert_eq!(dp["exposed"], json!(true), "{dp:#?}");
    let serial = dp["serial"].as_str().expect("serial").to_string();
    assert!(serial.starts_with("emulator-55"), "unexpected serial {serial}");

    let script = daemon.dir.join("check_hub.mjs");
    std::fs::write(
        &script,
        r#"
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import path from "node:path";
import { pathToFileURL } from "node:url";

const dist = process.env.HUB_DIST;
const serial = process.env.ACOS_SERIAL;
const adbPort = process.env.ANDROID_ADB_SERVER_PORT;
const apkPath = process.env.ACOS_APK;

const adbMod = await import(pathToFileURL(path.join(dist, "adb.js")).href);
const accMod = await import(pathToFileURL(path.join(dist, "accessibility.js")).href);
const appMod = await import(pathToFileURL(path.join(dist, "app-info.js")).href);

let found = false;
for (let i = 0; i < 40; i++) {
  const devs = await adbMod.listDevices();
  if (devs.some((d) => d.serial === serial && d.state === "device")) {
    found = true;
    break;
  }
  await new Promise((r) => setTimeout(r, 100));
}
assert(found, `${serial} not discovered in adb devices`);

const size = await adbMod.getDeviceSize(serial);
assert.equal(size.width, 1080);
assert.equal(size.height, 2400);

const density = await adbMod.getDisplayDensity(serial);
assert.equal(density.scale, 1);

assert.equal(await adbMod.getDisplayRotation(serial), 0);
const urot = await adbMod.getUserRotation(serial);
assert.equal(urot.mode, "free");

const font = await adbMod.getFontScale(serial);
assert.equal(font.scale, 1);

const night = await adbMod.getNightMode(serial);
assert.equal(night.mode, "light");

const netStat = await adbMod.getNetworkStatus(serial);
assert.equal(netStat.wifi, "enabled");

await adbMod.getReduceMotion(serial);
await adbMod.getHighTextContrast(serial);
await adbMod.getFontWeight(serial);
const kb = await adbMod.getSoftwareKeyboard(serial);
assert.equal(kb.hardwareKeyboard, true);

const png = await adbMod.screencapPng(serial);
assert.equal(png.subarray(0, 4).toString("hex"), "89504e47");

const fg = await appMod.getForegroundApp(serial);
assert.equal(fg.packageName, "dev.acos.fixtures.counter");
assert(fg.pid > 0);

const acc = await accMod.getAccessibilitySnapshot(serial);
assert(acc.ok);
assert(acc.nodes.some((n) => n.text === "Increment"));

const rotRes = await adbMod.setUserRotation(serial, "landscape");
assert.equal(rotRes.mode, "lock");
assert.equal(await adbMod.getDisplayRotation(serial), 1);

const unsup = spawnSync("adb", ["-P", adbPort, "-s", serial, "shell", "logcat"], { encoding: "utf8" });
assert.notEqual(unsup.status, 0);
assert((unsup.stderr + unsup.stdout).includes("acos: unsupported"));

const inst = spawnSync("adb", ["-P", adbPort, "-s", serial, "install", apkPath], { encoding: "utf8" });
assert.notEqual(inst.status, 0);
assert((inst.stderr + inst.stdout).includes("acos: unsupported"));
"#,
    )
    .unwrap();

    let node_out = Command::new("node")
        .arg(&script)
        .env("HUB_DIST", &hub_dist)
        .env("ACOS_SERIAL", &serial)
        .env("ANDROID_ADB_SERVER_PORT", &adb_port_str)
        .env("ACOS_APK", &apk)
        .output()
        .expect("run node hub parser test");
    assert!(
        node_out.status.success(),
        "node parser verification failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&node_out.stdout),
        String::from_utf8_lossy(&node_out.stderr)
    );

    let destroy = conn.call("acos_destroy", json!({ "base": base }));
    assert_eq!(destroy["ok"], json!(true), "{destroy:#?}");

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let out = Command::new("adb")
            .args(["-P", &adb_port_str, "devices"])
            .env("ANDROID_ADB_SERVER_PORT", &adb_port_str)
            .output()
            .expect("adb devices");
        let text = String::from_utf8_lossy(&out.stdout);
        if !text.lines().any(|l| l.starts_with(&serial)) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "{serial} still listed by adb devices 5s after destroy:\n{text}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}
