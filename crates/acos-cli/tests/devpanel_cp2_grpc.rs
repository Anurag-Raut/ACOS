//! DEVPANEL1 CP2: `acos-devbridge` gRPC (`EmulatorController`), `pid_<pid>.ini`
//! discovery with `expo-device-hub/emulator-grpc.js`, auth, `getScreenshot` pixel
//! equality, change-driven `streamScreenshot`, `sendTouch`, and `sendKey`.
//!
//! Run live ART test under the shared guard:
//!   env -u TMPDIR tools/art-runtime/run-exclusive.sh --task DEVPANEL1 -- \
//!     tools/guard/guarded.sh --mem 12G --time 1500 -- \
//!     cargo test --offline -p acos-cli --test devpanel_cp2_grpc -- --ignored --test-threads=1

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
        .join(format!("cp2-test-{tag}-{}", std::process::id()));
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
            .env_remove("ACOS_SESSION_SOCKET")
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
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

fn tree_contains_text(node: &Value, needle: &str) -> bool {
    if node.get("text").and_then(Value::as_str).unwrap_or("").contains(needle) {
        return true;
    }
    if let Some(children) = node.get("children").and_then(Value::as_array) {
        for ch in children {
            if tree_contains_text(ch, needle) {
                return true;
            }
        }
    }
    false
}

#[test]
fn serve_with_adb_writes_no_ini_without_sessions() {
    let daemon = Daemon::start("idle-no-ini", &["--adb"], &[]);
    let count = std::fs::read_dir(&daemon.avd_dir).unwrap().count();
    assert_eq!(count, 0, "idle serve --adb should write zero ini files");
}

#[test]
#[ignore = "boots real ART guests and tests gRPC video, screenshots, touch, and keyboard via expo-device-hub"]
fn cp2_live_grpc_stream_touch_key_and_auth() {
    let counter_apk = find_fixture_apk("counter.apk").expect("counter.apk not found");
    let text_apk = find_fixture_apk("text-input.apk").expect("text-input.apk not found");
    let hub_dist = hub_dist_dir();
    assert!(hub_dist.join("emulator-grpc.js").is_file());

    let adb_srv = PrivateAdbServer::start();
    let adb_port_str = adb_srv.port.to_string();
    let daemon = Daemon::start(
        "live-cp2",
        &["--adb"],
        &[("ANDROID_ADB_SERVER_PORT", &adb_port_str)],
    );
    let mut conn = daemon.connect();

    // 1. Launch counter.apk and verify INI discovery + device_panel JSON.
    let base_counter = json!({ "session_id": "cp2-counter" });
    let launch = conn.call("acos_launch", json!({ "base": base_counter, "apk": counter_apk }));
    assert_eq!(launch["ok"], json!(true), "{launch:#?}");
    let dp = &launch["result"]["device_panel"];
    assert_eq!(dp["exposed"], json!(true), "{dp:#?}");
    assert!(dp["grpc_port"].as_u64().unwrap_or(0) > 0, "missing grpc_port");
    assert!(dp.get("grpc_token").is_none(), "token must never be in RPC reply");
    let serial = dp["serial"].as_str().unwrap().to_string();
    let launcher_pid = launch["result"]["launcher_pid"]
        .as_u64()
        .or_else(|| dp["launcher_pid"].as_u64());
    let ini_files: Vec<PathBuf> = std::fs::read_dir(&daemon.avd_dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    assert_eq!(ini_files.len(), 1, "expected 1 ini file, got {ini_files:?}");
    if let Some(pid) = launcher_pid {
        assert_eq!(
            ini_files[0].file_name().unwrap().to_str().unwrap(),
            format!("pid_{pid}.ini")
        );
    }

    // Take an agent screenshot (`acos_observe` PNG) to compare pixel-for-pixel with `getScreenshot`.
    let obs_png_path = daemon.dir.join("agent-observe.png");
    let obs = conn.call(
        "acos_observe",
        json!({ "base": base_counter, "screenshot": true, "tree": true }),
    );
    assert_eq!(obs["ok"], json!(true), "{obs:#?}");
    let obs_src = PathBuf::from(
        obs["result"]["screenshot"]["path"]
            .as_str()
            .expect("observe screenshot path"),
    );
    std::fs::copy(&obs_src, &obs_png_path).unwrap();

    // Locate "Increment" button center from the real UI tree.
    let (l, t, r, b) =
        find_bounds_in_tree(&obs, Some("Increment"), None).expect("Increment bounds in tree");
    let inc_x = (l + r) / 2;
    let inc_y = (t + b) / 2;

    // 2. Run Node check against vendored `emulator-grpc.js` (`findEmulatorGrpcEndpoint` + `EmulatorGrpcClient`).
    let node_script = daemon.dir.join("check_grpc.mjs");
    std::fs::write(
        &node_script,
        r#"
import assert from "node:assert/strict";
import fs from "node:fs";
import http2 from "node:http2";
import path from "node:path";
import zlib from "node:zlib";
import { pathToFileURL } from "node:url";

const dist = process.env.HUB_DIST;
const avdDir = process.env.ACOS_AVD_RUNNING_DIR;
const serial = process.env.ACOS_SERIAL;
const obsPngPath = process.env.OBS_PNG_PATH;
const incX = Number(process.env.INC_X);
const incY = Number(process.env.INC_Y);

const grpcMod = await import(pathToFileURL(path.join(dist, "emulator-grpc.js")).href);
const {
  findEmulatorGrpcEndpoint,
  EmulatorGrpcClient,
  IMG_FORMAT_RGB888,
  IMG_FORMAT_PNG,
} = grpcMod;

// Discovery via findEmulatorGrpcEndpoint
const endpoint = findEmulatorGrpcEndpoint(serial, { discoveryDirs: () => [avdDir] });
assert(endpoint, `findEmulatorGrpcEndpoint returned null for ${serial}`);
assert(endpoint.port > 0);
assert(endpoint.token && endpoint.token.length === 32, "expected 32-hex bearer token");
assert(endpoint.avdName.startsWith("ACOS dev.acos.fixtures.counter"), `bad avdName: ${endpoint.avdName}`);

// Helper to verify UNAUTHENTICATED / UNIMPLEMENTED raw gRPC status
function rawUnary(method, bodyBuf, tokenHeader) {
  return new Promise((resolve, reject) => {
    const client = http2.connect(`http://127.0.0.1:${endpoint.port}`);
    client.on("error", reject);
    const headers = {
      ":method": "POST",
      ":path": `/android.emulation.control.EmulatorController/${method}`,
      "content-type": "application/grpc",
      te: "trailers",
    };
    if (tokenHeader !== undefined) headers.authorization = tokenHeader;
    const req = client.request(headers);
    let grpcStatus = null;
    let grpcMessage = "";
    const chunks = [];
    req.on("response", (h) => {
      if (h["grpc-status"] !== undefined) grpcStatus = String(h["grpc-status"]);
      if (h["grpc-message"] !== undefined) grpcMessage = decodeURIComponent(String(h["grpc-message"]));
    });
    req.on("trailers", (h) => {
      if (h["grpc-status"] !== undefined) grpcStatus = String(h["grpc-status"]);
      if (h["grpc-message"] !== undefined) grpcMessage = decodeURIComponent(String(h["grpc-message"]));
    });
    req.on("data", (c) => chunks.push(c));
    req.on("close", () => {
      client.close();
      resolve({ grpcStatus, grpcMessage, data: Buffer.concat(chunks) });
    });
    const frame = Buffer.allocUnsafe(5 + bodyBuf.length);
    frame.writeUInt8(0, 0);
    frame.writeUInt32BE(bodyBuf.length, 1);
    bodyBuf.copy(frame, 5);
    req.end(frame);
  });
}

// 2a. Auth check: missing and wrong tokens fail with UNAUTHENTICATED (16)
for (const bad of [undefined, "Bearer wrongtoken"]) {
  const r = await rawUnary("getStatus", Buffer.alloc(0), bad);
  assert.equal(r.grpcStatus, "16", `expected UNAUTHENTICATED(16), got ${r.grpcStatus}`);
}

// 2b. Honest UNIMPLEMENTED (12) for unsupported RPC and unsupported key
const unimpl = await rawUnary("getClipboard", Buffer.alloc(0), `Bearer ${endpoint.token}`);
assert.equal(unimpl.grpcStatus, "12", `expected UNIMPLEMENTED(12), got ${unimpl.grpcStatus}`);

const statusRes = await rawUnary("getStatus", Buffer.alloc(0), `Bearer ${endpoint.token}`);
assert.equal(statusRes.grpcStatus, "0", `getStatus failed: ${statusRes.grpcStatus} ${statusRes.grpcMessage}`);

// 2c. getScreenshot RGB888 & pixel equality vs acos_observe PNG
const client = new EmulatorGrpcClient(endpoint);
const shot = await client.getScreenshot({ format: IMG_FORMAT_RGB888 });
assert.equal(shot.format, IMG_FORMAT_RGB888);
assert.equal(shot.width, 1080);
assert.equal(shot.height, 2400);
assert.equal(shot.image.length, 1080 * 2400 * 3);

// Minimal PNG RGBA/RGB decoder in Node using built-in zlib to verify exact pixels
function decodePngToRgb(pngBuf) {
  let off = 8;
  let w = 0, h = 0, colorType = 0;
  const idat = [];
  while (off < pngBuf.length) {
    const len = pngBuf.readUInt32BE(off);
    const type = pngBuf.subarray(off + 4, off + 8).toString("ascii");
    const data = pngBuf.subarray(off + 8, off + 8 + len);
    off += 12 + len;
    if (type === "IHDR") {
      w = data.readUInt32BE(0);
      h = data.readUInt32BE(4);
      colorType = data[9];
    } else if (type === "IDAT") {
      idat.push(data);
    } else if (type === "IEND") {
      break;
    }
  }
  const bpp = colorType === 6 ? 4 : 3;
  const raw = zlib.inflateSync(Buffer.concat(idat));
  const stride = w * bpp;
  const rgb = Buffer.allocUnsafe(w * h * 3);
  const prev = Buffer.alloc(stride);
  const cur = Buffer.alloc(stride);
  const paeth = (a, b, c) => {
    const p = a + b - c;
    const pa = Math.abs(p - a), pb = Math.abs(p - b), pc = Math.abs(p - c);
    return (pa <= pb && pa <= pc) ? a : (pb <= pc ? b : c);
  };
  let pos = 0;
  for (let y = 0; y < h; y++) {
    const f = raw[pos++];
    for (let i = 0; i < stride; i++) {
      const x = raw[pos++];
      const a = i >= bpp ? cur[i - bpp] : 0;
      const b = prev[i];
      const c = i >= bpp ? prev[i - bpp] : 0;
      let val;
      if (f === 0) val = x;
      else if (f === 1) val = (x + a) & 0xff;
      else if (f === 2) val = (x + b) & 0xff;
      else if (f === 3) val = (x + ((a + b) >> 1)) & 0xff;
      else val = (x + paeth(a, b, c)) & 0xff;
      cur[i] = val;
    }
    for (let px = 0; px < w; px++) {
      rgb[(y * w + px) * 3 + 0] = cur[px * bpp + 0];
      rgb[(y * w + px) * 3 + 1] = cur[px * bpp + 1];
      rgb[(y * w + px) * 3 + 2] = cur[px * bpp + 2];
    }
    cur.copy(prev);
  }
  return { w, h, rgb };
}

const obsDecoded = decodePngToRgb(fs.readFileSync(obsPngPath));
assert.equal(obsDecoded.w, 1080);
assert.equal(obsDecoded.h, 2400);
assert.equal(Buffer.compare(shot.image, obsDecoded.rgb), 0, "getScreenshot RGB888 pixels differ from acos_observe screenshot");

// Also verify scaled PNG getScreenshot works
const shotScaledPng = await client.getScreenshot({ format: IMG_FORMAT_PNG, width: 540, height: 1200 });
assert.equal(shotScaledPng.width, 540);
assert.equal(shotScaledPng.height, 1200);
assert.equal(shotScaledPng.image.subarray(0, 4).toString("hex"), "89504e47");

// 2d. streamScreenshot: initial frame on static screen, silent while idle, new frame after sendTouch
const ac = new AbortController();
const frames = [];
const streamDone = client.streamScreenshot(
  { format: IMG_FORMAT_RGB888 },
  (img) => frames.push(img),
  ac.signal,
).catch((e) => {
  if (!ac.signal.aborted) throw e;
});

const startWait = Date.now();
while (frames.length === 0 && Date.now() - startWait < 3000) {
  await new Promise((r) => setTimeout(r, 20));
}
assert(frames.length >= 1, "streamScreenshot should deliver initial frame on idle screen");
const countAfterInitial = frames.length;

// Verify silence while idle (wait 400ms)
await new Promise((r) => setTimeout(r, 400));
assert.equal(frames.length, countAfterInitial, `streamScreenshot emitted ${frames.length - countAfterInitial} extra frames while screen was idle`);

// 2e. Tap Increment via gRPC sendTouch
await client.sendTouch([{ identifier: 0, x: incX, y: incY, pressure: 1 }]);
await new Promise((r) => setTimeout(r, 25));
await client.sendTouch([{ identifier: 0, x: incX, y: incY, pressure: 0 }]);

const changeWait = Date.now();
while (frames.length === countAfterInitial && Date.now() - changeWait < 3000) {
  await new Promise((r) => setTimeout(r, 20));
}
assert(frames.length > countAfterInitial, "streamScreenshot did not emit a new frame after sendTouch updated the counter UI");
assert.equal(frames[frames.length - 1].width, 1080);
assert.equal(frames[frames.length - 1].height, 2400);

ac.abort();
await streamDone;
client.close();
"#,
    )
    .unwrap();

    let node_out = Command::new("node")
        .arg(&node_script)
        .env("HUB_DIST", &hub_dist)
        .env("ACOS_AVD_RUNNING_DIR", &daemon.avd_dir)
        .env("ACOS_SERIAL", &serial)
        .env("OBS_PNG_PATH", &obs_png_path)
        .env("INC_X", inc_x.to_string())
        .env("INC_Y", inc_y.to_string())
        .output()
        .expect("run node grpc check");
    assert!(
        node_out.status.success(),
        "node grpc test failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&node_out.stdout),
        String::from_utf8_lossy(&node_out.stderr)
    );

    // Confirm Increment actually incremented counter to "1" in UI tree.
    let obs_after = conn.call("acos_observe", json!({ "base": base_counter, "tree": true }));
    assert_eq!(obs_after["ok"], json!(true), "{obs_after:#?}");
    assert!(
        obs_after["result"]["tree"]["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| tree_contains_text(n, "1")),
        "counter did not increment to 1 after sendTouch: {obs_after:#?}"
    );

    // 3. Test sendKey typing text on text-input.apk.
    let base_text = json!({ "session_id": "cp2-text" });
    let launch_txt = conn.call("acos_launch", json!({ "base": base_text, "apk": text_apk }));
    assert_eq!(launch_txt["ok"], json!(true), "{launch_txt:#?}");
    let serial_txt = launch_txt["result"]["device_panel"]["serial"]
        .as_str()
        .unwrap()
        .to_string();

    let obs_txt = conn.call("acos_observe", json!({ "base": base_text, "tree": true }));
    assert_eq!(obs_txt["ok"], json!(true), "{obs_txt:#?}");
    let (el, et, er, bb) = find_bounds_in_tree(&obs_txt, None, Some("EditText"))
        .expect("EditText bounds in text-input.apk");
    let et_x = (el + er) / 2;
    let et_y = (et + bb) / 2;

    let text_script = daemon.dir.join("check_text.mjs");
    std::fs::write(
        &text_script,
        r#"
import assert from "node:assert/strict";
import path from "node:path";
import { pathToFileURL } from "node:url";

const dist = process.env.HUB_DIST;
const avdDir = process.env.ACOS_AVD_RUNNING_DIR;
const serial = process.env.ACOS_SERIAL;
const etX = Number(process.env.ET_X);
const etY = Number(process.env.ET_Y);

const { findEmulatorGrpcEndpoint, EmulatorGrpcClient } = await import(
  pathToFileURL(path.join(dist, "emulator-grpc.js")).href
);
const endpoint = findEmulatorGrpcEndpoint(serial, { discoveryDirs: () => [avdDir] });
assert(endpoint);
const client = new EmulatorGrpcClient(endpoint);
await client.sendTouch([{ identifier: 0, x: etX, y: etY, pressure: 1 }]);
await new Promise((r) => setTimeout(r, 25));
await client.sendTouch([{ identifier: 0, x: etX, y: etY, pressure: 0 }]);
await new Promise((r) => setTimeout(r, 150));

await client.sendKey({ text: "hello panel" });
await new Promise((r) => setTimeout(r, 200));
client.close();
"#,
    )
    .unwrap();

    let text_out = Command::new("node")
        .arg(&text_script)
        .env("HUB_DIST", &hub_dist)
        .env("ACOS_AVD_RUNNING_DIR", &daemon.avd_dir)
        .env("ACOS_SERIAL", &serial_txt)
        .env("ET_X", et_x.to_string())
        .env("ET_Y", et_y.to_string())
        .output()
        .expect("run node text key test");
    assert!(
        text_out.status.success(),
        "node text test failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&text_out.stdout),
        String::from_utf8_lossy(&text_out.stderr)
    );

    let obs_typed = conn.call("acos_observe", json!({ "base": base_text, "tree": true }));
    assert!(
        obs_typed["result"]["tree"]["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| tree_contains_text(n, "hello panel")),
        "EditText did not receive 'hello panel': {obs_typed:#?}"
    );

    // 4. Destroy sessions and confirm INI files are cleaned up.
    assert_eq!(
        conn.call("acos_destroy", json!({ "base": base_counter }))["ok"],
        json!(true)
    );
    assert_eq!(
        conn.call("acos_destroy", json!({ "base": base_text }))["ok"],
        json!(true)
    );
    let remaining: Vec<_> = std::fs::read_dir(&daemon.avd_dir).unwrap().flatten().collect();
    assert_eq!(remaining.len(), 0, "expected all ini files removed after destroy");
}
