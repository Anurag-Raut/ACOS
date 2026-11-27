//! PR 2 (v0.6.0): Input Dispatch & Settle Fidelity.
//!
//! Covers all three deliverables without touching files owned by parallel PRs:
//! 1. Real Unicode `acos_text` via guest `InputConnection.commitText` (with
//!    structured `INPUT_UNAVAILABLE` when non-ASCII cannot commit).
//! 2. `settle: "content"` support on `acos_swipe {until: ...}` (while `fast` is
//!    still rejected).
//! 3. Session-level launch `settle: "content"` inheritance across subsequent
//!    verbs (`observe_tree`, `observe`, `tap`, `swipe`, `type_text`, `key`,
//!    `action`) + post-tap cached tree refresh when an input reply omits `tree`.
//!
//! Mock-guest tests run fast without booting ART; live-ART end-to-end tests
//! are `#[ignore]` and run under `tools/art-runtime/run-exclusive.sh` +
//! `tools/guard/guarded.sh`.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

mod common;

fn repo_root() -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.to_path_buf())
}

fn acos() -> &'static str {
    env!("CARGO_BIN_EXE_acos")
}

fn scratch(tag: &str) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    tag.hash(&mut hasher);
    let dir = repo_root().join(".cache").join(format!(
        "v060-is-{:08x}-{}",
        hasher.finish() as u32,
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("runtime")).unwrap();
    std::fs::create_dir_all(dir.join("cache")).unwrap();
    std::fs::create_dir_all(dir.join("art-home")).unwrap();
    std::fs::canonicalize(&dir).unwrap_or(dir)
}

fn fake_art(dir: &Path) -> PathBuf {
    let script = dir.join("fake-acos-art.py");
    let body = r#"#!/usr/bin/env python3
import json, os, struct, sys, zlib

out = os.environ["FAKE_ART_OUT"]
log = os.path.join(out, "requests.jsonl")
png = os.path.join(out, "frame.png")
os.makedirs(out, exist_ok=True)
def _chunk(tag, data):
    return struct.pack(">I", len(data)) + tag + data + struct.pack(">I", zlib.crc32(tag + data) & 0xffffffff)
with open(png, "wb") as handle:
    ihdr = struct.pack(">IIBBBBB", 1, 1, 8, 6, 0, 0, 0)
    idat = zlib.compress(b"\x00\x00\x00\x00\xff")
    handle.write(b"\x89PNG\r\n\x1a\n" + _chunk(b"IHDR", ihdr) + _chunk(b"IDAT", idat) + _chunk(b"IEND", b""))

state = {"seq": 0, "swipes": 0, "opened_details": False}

def tree():
    nodes = [
        {
            "id": "search",
            "class": "android.widget.EditText",
            "text": "Search",
            "hint": "Search",
            "content_description": "Search field",
            "bounds": "[40,120][680,220]",
            "bounds_on_screen": "[40,120][680,220]",
            "visible": True,
            "focused": True,
            "clickable": True,
            "password": False,
            "window": "main",
        },
        {
            "id": "open_details",
            "class": "android.widget.Button",
            "text": "Open",
            "content_description": "",
            "bounds": "[400,400][600,560]",
            "bounds_on_screen": "[400,400][600,560]",
            "visible": True,
            "clickable": True,
            "window": "main",
        },
    ]
    if state["swipes"] >= 2:
        nodes.append({
            "id": "deep_row",
            "class": "android.widget.TextView",
            "text": "Target",
            "content_description": "",
            "bounds": "[40,700][680,780]",
            "bounds_on_screen": "[40,700][680,780]",
            "visible": True,
            "clickable": True,
            "window": "main",
        })
    if state["opened_details"]:
        nodes.append({
            "id": "details_btn",
            "class": "android.widget.Button",
            "text": "Details",
            "content_description": "",
            "bounds": "[100,800][400,900]",
            "bounds_on_screen": "[100,800][400,900]",
            "visible": True,
            "clickable": True,
            "window": "main",
        })
    return {
        "nodes": nodes,
        "windows": [{"window": "main", "z": 0}],
        "window_count": 1,
        "node_budget": 2000,
        "truncated": False,
    }

def frame(cmd, settle="strict", include_tree=True):
    state["seq"] += 1
    out_obj = {
        "ok": True,
        "cmd": cmd,
        "frame_seq": state["seq"],
        "probe_seq": state["seq"],
        "md5": "m%d" % state["seq"],
        "png": png,
        "settle": {
            "mode": settle,
            "settled": True,
            "bound": "event",
            "first_change_ms": 1,
        },
        "gesture": {
            "seqs": [state["seq"]],
            "token": "0x1",
            "acks": [{"seq": state["seq"], "handled": True, "latency_us": 1000}],
        },
    }
    if include_tree:
        out_obj["tree"] = tree()
    return out_obj

print(json.dumps({
    "ok": True,
    "cmd": "ready",
    "package": "dev.acos.v060.mock",
    "activity": "dev.acos.v060.mock/.MainActivity",
    "width": 1080,
    "height": 2400,
    "density": 420,
}), flush=True)

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    try:
        request = json.loads(line)
    except ValueError:
        break
    with open(log, "a") as handle:
        handle.write(json.dumps(request, sort_keys=True) + "\n")
    cmd = request.get("cmd", "")
    settle = request.get("settle", "strict")
    if cmd == "tree":
        print(json.dumps(frame("tree", settle=settle, include_tree=True)), flush=True)
    elif cmd == "tap":
        x, y = request.get("x", 0), request.get("y", 0)
        if 400 <= x <= 600 and 400 <= y <= 560:
            state["opened_details"] = True
        omit_tree = os.path.exists(os.path.join(out, "omit_tap_tree"))
        print(json.dumps(frame("tap", settle=settle, include_tree=not omit_tree)), flush=True)
    elif cmd == "swipe":
        state["swipes"] += 1
        print(json.dumps(frame("swipe", settle=settle, include_tree=True)), flush=True)
    elif cmd == "type_text":
        if os.path.exists(os.path.join(out, "fail_input_unavailable")):
            print(json.dumps({
                "ok": False,
                "cmd": "type_text",
                "code": "INPUT_UNAVAILABLE",
                "error": "no focused view returned an InputConnection for commitText",
            }), flush=True)
        else:
            print(json.dumps(frame("type_text", settle=settle, include_tree=True)), flush=True)
    elif cmd == "key":
        print(json.dumps(frame("key", settle=settle, include_tree=True)), flush=True)
    elif cmd == "logs":
        print(json.dumps({
            "ok": True, "cmd": "logs", "total": 0, "kept": 0, "dropped": 0, "entries": []
        }), flush=True)
    elif cmd == "close":
        print(json.dumps({"ok": True, "cmd": "close"}), flush=True)
        break
    else:
        print(json.dumps({
            "ok": False, "cmd": cmd, "code": "BAD_REQUEST", "error": "unsupported"
        }), flush=True)
"#;
    std::fs::write(&script, body).unwrap();
    let mut perms = std::fs::metadata(&script).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script, perms).unwrap();
    script
}

struct Daemon {
    child: Child,
    dir: PathBuf,
    socket: PathBuf,
    art_home: PathBuf,
    _socket_dir: common::ShortSocketDir,
}

impl Daemon {
    fn start_with_art(tag: &str, art_bin: &Path, timeout_secs: u64) -> Daemon {
        let dir = scratch(tag);
        let art_home = dir.join("art-home");
        let socket_dir = common::ShortSocketDir::new(tag);
        let socket = socket_dir.socket("serve.sock");
        let child = Command::new(acos())
            .arg("serve")
            .arg("--socket")
            .arg(&socket)
            .arg("--idle-timeout")
            .arg(timeout_secs.to_string())
            .env("ACOS_RUNTIME_DIR", dir.join("runtime"))
            .env_remove("ACOS_SESSION_SOCKET")
            .env("ACOS_CACHE_DIR", dir.join("cache"))
            .env("ACOS_ART_BIN", art_bin)
            .env("FAKE_ART_OUT", &art_home)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn acos serve");
        let deadline = Instant::now() + Duration::from_secs(30);
        while !socket.exists() {
            assert!(Instant::now() < deadline, "serve socket never appeared");
            std::thread::sleep(Duration::from_millis(25));
        }
        Daemon {
            child,
            dir,
            socket,
            art_home,
            _socket_dir: socket_dir,
        }
    }

    fn start_fake(tag: &str) -> Daemon {
        let dir = scratch(tag);
        let art_home = dir.join("art-home");
        let fake = fake_art(&art_home);
        let socket_dir = common::ShortSocketDir::new(tag);
        let socket = socket_dir.socket("serve.sock");
        let child = Command::new(acos())
            .arg("serve")
            .arg("--socket")
            .arg(&socket)
            .arg("--idle-timeout")
            .arg("120")
            .env("ACOS_RUNTIME_DIR", dir.join("runtime"))
            .env_remove("ACOS_SESSION_SOCKET")
            .env("ACOS_CACHE_DIR", dir.join("cache"))
            .env("ACOS_ART_BIN", &fake)
            .env("FAKE_ART_OUT", &art_home)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn acos serve");
        let deadline = Instant::now() + Duration::from_secs(30);
        while !socket.exists() {
            assert!(Instant::now() < deadline, "serve socket never appeared");
            std::thread::sleep(Duration::from_millis(25));
        }
        Daemon {
            child,
            dir,
            socket,
            art_home,
            _socket_dir: socket_dir,
        }
    }

    fn connect(&self, read_timeout_secs: u64) -> Conn {
        Conn::new(&self.socket, read_timeout_secs)
    }

    fn fake_apk(&self) -> PathBuf {
        let apk = self.dir.join("fake.apk");
        if !apk.exists() {
            std::fs::write(&apk, b"not-a-real-apk").unwrap();
        }
        apk
    }

    fn guest_requests(&self) -> Vec<Value> {
        let path = self.art_home.join("requests.jsonl");
        match std::fs::read_to_string(path) {
            Ok(text) => text
                .lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect(),
            Err(_) => Vec::new(),
        }
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
    fn new(path: &Path, read_timeout_secs: u64) -> Conn {
        let stream = UnixStream::connect(path).expect("connect to serve socket");
        stream
            .set_read_timeout(Some(Duration::from_secs(read_timeout_secs)))
            .unwrap();
        Conn {
            writer: stream.try_clone().unwrap(),
            reader: BufReader::new(stream),
            next: 0,
        }
    }

    fn call(&mut self, tool: &str, args: Value) -> Value {
        self.next += 1;
        let request = json!({"id": self.next, "tool": tool, "args": args});
        self.writer
            .write_all((request.to_string() + "\n").as_bytes())
            .unwrap();
        self.writer.flush().unwrap();
        let mut line = String::new();
        self.reader.read_line(&mut line).unwrap();
        let value: Value = serde_json::from_str(&line).expect("response is JSON");
        assert_eq!(value["id"], json!(self.next));
        value["envelope"].clone()
    }
}

fn base(id: &str) -> Value {
    json!({"request_id": format!("v060-{id}"), "session_id": id})
}

fn live_art_bin() -> PathBuf {
    if let Some(path) = std::env::var_os("ACOS_ART_BIN") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return path;
        }
    }
    for rel in [
        ".cache/target/release/acos-art",
        ".cache/target/debug/acos-art",
        "target/release/acos-art",
        "target/debug/acos-art",
    ] {
        let path = repo_root().join(rel);
        if path.is_file() {
            return path;
        }
    }
    repo_root().join(".cache/target/debug/acos-art")
}

fn live_apk(name: &str) -> PathBuf {
    let local = repo_root().join("fixtures/build/apps").join(format!("{name}.apk"));
    if local.is_file() {
        return local;
    }
    PathBuf::from("/home/Qui/Projects/acos/fixtures/build/apps")
        .join(format!("{name}.apk"))
}

fn collect_texts(nodes: &Value, out: &mut Vec<String>) {
    let nodes = match nodes {
        Value::Array(_) => nodes,
        Value::Object(map) => match map.get("nodes") {
            Some(nodes) => nodes,
            None => return,
        },
        _ => return,
    };
    if let Some(array) = nodes.as_array() {
        for node in array {
            if let Some(text) = node.get("text").and_then(Value::as_str) {
                if !text.is_empty() {
                    out.push(text.to_string());
                }
            }
            if let Some(children) = node.get("children") {
                collect_texts(children, out);
            }
        }
    }
}

fn find_node_center(nodes: &Value, needle_text: &str) -> Option<(i64, i64)> {
    let array = match nodes {
        Value::Array(arr) => arr,
        Value::Object(map) => map.get("nodes").and_then(Value::as_array)?,
        _ => return None,
    };
    for node in array {
        if node.get("text").and_then(Value::as_str) == Some(needle_text) {
            if let Some(bounds) = node.get("bounds").and_then(Value::as_str) {
                if let Some((l, t, r, b)) = parse_bounds(bounds) {
                    return Some(((l + r) / 2, (t + b) / 2));
                }
            }
        }
        if let Some(children) = node.get("children") {
            if let Some(pt) = find_node_center(children, needle_text) {
                return Some(pt);
            }
        }
    }
    None
}

fn parse_bounds(bounds: &str) -> Option<(i64, i64, i64, i64)> {
    let inner = bounds.strip_prefix('[')?.strip_suffix(']')?;
    let (lt, rb) = inner.split_once("][")?;
    let (l, t) = lt.split_once(',')?;
    let (r, b) = rb.split_once(',')?;
    Some((
        l.parse().ok()?,
        t.parse().ok()?,
        r.parse().ok()?,
        b.parse().ok()?,
    ))
}

#[test]
fn launch_settle_content_inherited_across_verbs_and_overridable() {
    let daemon = Daemon::start_fake("inherit-content");
    let mut conn = daemon.connect(120);
    let b = base("s_inherit_content");

    let launch = conn.call(
        "acos_launch",
        json!({"base": b, "apk": daemon.fake_apk(), "settle": "content"}),
    );
    assert_eq!(launch["ok"], json!(true), "{launch:#?}");

    // observe_tree without settle -> content_settle (result reports mode: "content",
    // bound: "content-stable", backed by host fast tree-only polls + pixel bind)
    let before = daemon.guest_requests().len();
    let tree = conn.call("acos_observe_tree", json!({"base": b}));
    assert_eq!(tree["ok"], json!(true), "{tree:#?}");
    assert_eq!(tree["result"]["settle"]["mode"], json!("content"), "{tree:#?}");
    assert_eq!(
        tree["result"]["settle"]["bound"],
        json!("content-stable"),
        "{tree:#?}"
    );
    let after = daemon.guest_requests();
    assert!(
        after[before..]
            .iter()
            .any(|req| req["cmd"] == "tree" && req["settle"] == "fast" && req["pixels"] == false),
        "expected content_settle host fast tree-only probe: {:?}",
        &after[before..]
    );

    // Regression test for tree_with_settle_pixels coercing "content" -> "strict"
    // when screenshot: true is passed!
    let tree_shot = conn.call("acos_observe_tree", json!({"base": b, "screenshot": true}));
    assert_eq!(tree_shot["ok"], json!(true), "{tree_shot:#?}");
    assert_eq!(
        tree_shot["result"]["settle"]["mode"],
        json!("content"),
        "screenshot:true must preserve settle:content: {tree_shot:#?}"
    );
    assert_eq!(
        tree_shot["result"]["settle"]["bound"],
        json!("content-stable"),
        "screenshot:true must run content_settle rather than coercing to strict: {tree_shot:#?}"
    );

    // observe {tree: true} and observe {text: true} inherit content
    let obs_tree = conn.call("acos_observe", json!({"base": b, "tree": true}));
    assert_eq!(obs_tree["ok"], json!(true), "{obs_tree:#?}");
    assert_eq!(obs_tree["result"]["settle"]["mode"], json!("content"), "{obs_tree:#?}");

    let obs_text = conn.call("acos_observe", json!({"base": b, "text": true}));
    assert_eq!(obs_text["ok"], json!(true), "{obs_text:#?}");
    assert_eq!(obs_text["result"]["settle"]["mode"], json!("content"), "{obs_text:#?}");

    // expect_change: true must be rejected when inheriting settle: "content"
    let rejected = conn.call(
        "acos_tap",
        json!({"base": b, "text": "Open", "expect_change": true}),
    );
    assert_eq!(rejected["ok"], json!(false), "{rejected:#?}");
    assert_eq!(rejected["error"]["code"], json!("INVALID_ARGUMENT"));

    // tap, text, key, swipe, action all inherit "content" when settle is omitted
    let tap = conn.call("acos_tap", json!({"base": b, "text": "Open"}));
    assert_eq!(tap["ok"], json!(true), "{tap:#?}");
    assert_eq!(tap["result"]["settle"]["mode"], json!("content"), "{tap:#?}");

    let text = conn.call("acos_text", json!({"base": b, "text": "hello"}));
    assert_eq!(text["ok"], json!(true), "{text:#?}");
    assert_eq!(text["result"]["settle"]["mode"], json!("content"), "{text:#?}");

    let key = conn.call("acos_key", json!({"base": b, "code": "enter"}));
    assert_eq!(key["ok"], json!(true), "{key:#?}");
    assert_eq!(key["result"]["settle"]["mode"], json!("content"), "{key:#?}");

    let swipe = conn.call("acos_swipe", json!({"base": b, "direction": "up"}));
    assert_eq!(swipe["ok"], json!(true), "{swipe:#?}");
    assert_eq!(swipe["result"]["settle"]["mode"], json!("content"), "{swipe:#?}");

    let action_tap = conn.call(
        "acos_action",
        json!({"base": b, "action": {"kind": "tap", "x": 100, "y": 100}}),
    );
    assert_eq!(action_tap["ok"], json!(true), "{action_tap:#?}");
    assert_eq!(action_tap["result"]["settle"]["mode"], json!("content"), "{action_tap:#?}");

    // Explicit settle: "strict" overrides the session default
    let strict_tap = conn.call(
        "acos_tap",
        json!({"base": b, "x": 100, "y": 100, "settle": "strict", "expect_change": false}),
    );
    assert_eq!(strict_tap["ok"], json!(true), "{strict_tap:#?}");
    assert_eq!(strict_tap["result"]["settle"]["mode"], json!("strict"), "{strict_tap:#?}");

    let strict_tree = conn.call(
        "acos_observe_tree",
        json!({"base": b, "settle": "strict"}),
    );
    assert_eq!(strict_tree["ok"], json!(true), "{strict_tree:#?}");
    assert_eq!(strict_tree["result"]["settle"]["mode"], json!("strict"), "{strict_tree:#?}");

    let _ = conn.call("acos_destroy", json!({"base": b}));
}

#[test]
fn launch_settle_fast_not_inherited_by_subsequent_verbs() {
    let daemon = Daemon::start_fake("no-inherit-fast");
    let mut conn = daemon.connect(120);
    let b = base("s_fast");

    let launch = conn.call(
        "acos_launch",
        json!({"base": b, "apk": daemon.fake_apk(), "settle": "fast"}),
    );
    assert_eq!(launch["ok"], json!(true), "{launch:#?}");

    let tree = conn.call("acos_observe_tree", json!({"base": b}));
    assert_eq!(tree["ok"], json!(true), "{tree:#?}");
    assert_eq!(tree["result"]["settle"]["mode"], json!("strict"), "{tree:#?}");

    let tap = conn.call("acos_tap", json!({"base": b, "x": 100, "y": 100}));
    assert_eq!(tap["ok"], json!(true), "{tap:#?}");
    assert_eq!(tap["result"]["settle"]["mode"], json!("strict"), "{tap:#?}");

    let _ = conn.call("acos_destroy", json!({"base": b}));
}

#[test]
fn swipe_until_allows_content_settle_and_rejects_fast() {
    let daemon = Daemon::start_fake("swipe-until-content");
    let mut conn = daemon.connect(120);
    let b = base("s_swipe_until");

    let launch = conn.call("acos_launch", json!({"base": b, "apk": daemon.fake_apk()}));
    assert_eq!(launch["ok"], json!(true), "{launch:#?}");

    // settle: "fast" with `until` is still rejected
    let rejected = conn.call(
        "acos_swipe",
        json!({
            "base": b,
            "direction": "up",
            "until": {"text": "Target"},
            "settle": "fast",
        }),
    );
    assert_eq!(rejected["ok"], json!(false), "{rejected:#?}");
    assert_eq!(rejected["error"]["code"], json!("INVALID_ARGUMENT"));

    // settle: "content" with `until` is accepted and dispatches content-settled tree checks
    let before = daemon.guest_requests().len();
    let found = conn.call(
        "acos_swipe",
        json!({
            "base": b,
            "direction": "up",
            "until": {"text": "Target"},
            "settle": "content",
            "max_attempts": 5,
        }),
    );
    assert_eq!(found["ok"], json!(true), "{found:#?}");
    assert_eq!(found["result"]["until"]["found"], json!(true), "{found:#?}");
    assert_eq!(found["result"]["until"]["settle"], json!("content"), "{found:#?}");
    assert_eq!(
        found["result"]["settle"]["mode"],
        json!("content"),
        "{found:#?}"
    );
    let after = daemon.guest_requests();
    assert!(
        after[before..]
            .iter()
            .any(|req| req["cmd"] == "tree" && req["settle"] == "fast" && req["pixels"] == false),
        "scroll_until with settle:content must run host content_settle polls: {:?}",
        &after[before..]
    );

    let _ = conn.call("acos_destroy", json!({"base": b}));
}

#[test]
fn coordinate_tap_without_inline_tree_refreshes_cached_tree() {
    let daemon = Daemon::start_fake("coord-tap-refresh");
    std::fs::write(daemon.art_home.join("omit_tap_tree"), b"1").unwrap();
    let mut conn = daemon.connect(120);
    let b = base("s_coord_refresh");

    let launch = conn.call("acos_launch", json!({"base": b, "apk": daemon.fake_apk()}));
    assert_eq!(launch["ok"], json!(true), "{launch:#?}");

    // Tap coordinates of the "Open" button [400,400][600,560].
    // The mock omits "tree" from the tap reply, so ArtSession must issue a
    // follow-up "tree" request to refresh its cache.
    let before = daemon.guest_requests().len();
    let tap = conn.call("acos_tap", json!({"base": b, "x": 500, "y": 480}));
    assert_eq!(tap["ok"], json!(true), "{tap:#?}");
    let after = daemon.guest_requests();
    assert!(
        after[before..].iter().any(|req| req["cmd"] == "tree"),
        "tap reply without tree must trigger follow-up tree refresh: {:?}",
        &after[before..]
    );

    // Immediately tap the newly revealed "Details" button by selector WITHOUT
    // calling acos_observe_tree in between.
    let details = conn.call("acos_tap", json!({"base": b, "text": "Details"}));
    assert_eq!(
        details["ok"],
        json!(true),
        "selector tap must resolve against refreshed cached tree: {details:#?}"
    );

    let _ = conn.call("acos_destroy", json!({"base": b}));
}

#[test]
fn unicode_text_dispatches_to_guest_and_surfaces_input_unavailable() {
    let daemon = Daemon::start_fake("unicode-text");
    let mut conn = daemon.connect(120);
    let b = base("s_unicode");

    let launch = conn.call("acos_launch", json!({"base": b, "apk": daemon.fake_apk()}));
    assert_eq!(launch["ok"], json!(true), "{launch:#?}");

    let typed = conn.call("acos_text", json!({"base": b, "text": "腹立つ"}));
    assert_eq!(typed["ok"], json!(true), "{typed:#?}");
    assert_eq!(typed["result"]["target"]["text"], json!("腹立つ"));

    let requests = daemon.guest_requests();
    assert!(
        requests
            .iter()
            .any(|req| req["cmd"] == "type_text" && req["text"] == "腹立つ"),
        "expected Unicode type_text request: {requests:#?}"
    );

    // When the guest reports INPUT_UNAVAILABLE for non-ASCII text, serve
    // surfaces it via failure::map_art_code("INPUT_UNAVAILABLE") -> UNSUPPORTED_BACKEND
    // with the InputConnection failure message intact.
    std::fs::write(daemon.art_home.join("fail_input_unavailable"), b"1").unwrap();
    let failed = conn.call("acos_text", json!({"base": b, "text": "腹立つ"}));
    assert_eq!(failed["ok"], json!(false), "{failed:#?}");
    assert_eq!(failed["error"]["code"], json!("UNSUPPORTED_BACKEND"));
    assert!(
        failed["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("InputConnection")),
        "expected InputConnection detail in error: {failed:#?}"
    );

    let _ = conn.call("acos_destroy", json!({"base": b}));
}

#[test]
#[ignore = "boots a real ART guest; run under tools/art-runtime/run-exclusive.sh + tools/guard/guarded.sh"]
fn live_unicode_text_input_commits_non_ascii_to_edittext() {
    let apk = live_apk("text-input");
    let art = live_art_bin();
    if !apk.is_file() || !art.is_file() {
        acos_test_support::require_prereq(
            false,
            "live_unicode_text_input_commits_non_ascii_to_edittext",
            &format!("{} or {} missing", apk.display(), art.display()),
        );
        return;
    }

    let daemon = Daemon::start_with_art("live-unicode", &art, 240);
    let mut conn = daemon.connect(300);
    let b = base("s_live_unicode");

    let launch = conn.call("acos_launch", json!({"base": b, "apk": apk}));
    assert_eq!(launch["ok"], json!(true), "{launch:#?}");

    let initial = conn.call("acos_observe_tree", json!({"base": b}));
    assert_eq!(initial["ok"], json!(true), "{initial:#?}");

    // Focus the EditText by tapping it, then type Japanese text via commitText.
    let focus_tap = conn.call("acos_tap", json!({"base": b, "id": "input"}));
    assert_eq!(focus_tap["ok"], json!(true), "{focus_tap:#?}");

    let jp = conn.call("acos_text", json!({"base": b, "text": "腹立つ"}));
    assert_eq!(jp["ok"], json!(true), "{jp:#?}");

    let save = conn.call("acos_tap", json!({"base": b, "id": "save"}));
    assert_eq!(save["ok"], json!(true), "{save:#?}");

    let tree = conn.call("acos_observe_tree", json!({"base": b}));
    assert_eq!(tree["ok"], json!(true), "{tree:#?}");
    let mut texts = Vec::new();
    collect_texts(&tree["result"]["tree"], &mut texts);
    assert!(
        texts.iter().any(|t| t == "Saved: 腹立つ"),
        "expected 'Saved: 腹立つ' in tree, got: {texts:?}"
    );

    // Also verify accented Latin and supplementary-plane UTF-16 emoji commitText.
    let extra = conn.call("acos_text", json!({"base": b, "text": " café 🎉"}));
    assert_eq!(extra["ok"], json!(true), "{extra:#?}");
    let save2 = conn.call("acos_tap", json!({"base": b, "id": "save"}));
    assert_eq!(save2["ok"], json!(true), "{save2:#?}");

    let tree2 = conn.call("acos_observe_tree", json!({"base": b}));
    assert_eq!(tree2["ok"], json!(true), "{tree2:#?}");
    let mut texts2 = Vec::new();
    collect_texts(&tree2["result"]["tree"], &mut texts2);
    assert!(
        texts2.iter().any(|t| t == "Saved: 腹立つ café 🎉"),
        "expected 'Saved: 腹立つ café 🎉' in tree, got: {texts2:?}"
    );

    let destroy = conn.call("acos_destroy", json!({"base": b}));
    assert_eq!(destroy["ok"], json!(true), "{destroy:#?}");
}

#[test]
#[ignore = "boots a real ART guest; run under tools/art-runtime/run-exclusive.sh + tools/guard/guarded.sh"]
fn live_settle_content_inherited_and_coord_tap_refreshes_tree() {
    let apk = live_apk("settle-lab");
    let art = live_art_bin();
    if !apk.is_file() || !art.is_file() {
        acos_test_support::require_prereq(
            false,
            "live_settle_content_inherited_and_coord_tap_refreshes_tree",
            &format!("{} or {} missing", apk.display(), art.display()),
        );
        return;
    }

    let daemon = Daemon::start_with_art("live-settle-lab", &art, 240);
    let mut conn = daemon.connect(300);
    let b = base("s_live_settle");

    // Launch with settle: "content" so subsequent verbs inherit content settle.
    let launch = conn.call(
        "acos_launch",
        json!({"base": b, "apk": apk, "settle": "content"}),
    );
    assert_eq!(launch["ok"], json!(true), "{launch:#?}");

    // Start the infinite indeterminate ProgressBar; strict settle would never
    // settle while pixels spin, whereas inherited content settle completes.
    let start_prog = conn.call("acos_tap", json!({"base": b, "id": "progress_start"}));
    assert_eq!(start_prog["ok"], json!(true), "{start_prog:#?}");
    assert_eq!(start_prog["result"]["settle"]["mode"], json!("content"), "{start_prog:#?}");

    let obs = conn.call("acos_observe_tree", json!({"base": b}));
    assert_eq!(obs["ok"], json!(true), "{obs:#?}");
    assert_eq!(obs["result"]["settle"]["mode"], json!("content"), "{obs:#?}");
    assert_eq!(obs["result"]["settle"]["settled"], json!(true), "{obs:#?}");

    // Inherited settle: "content" on delay40 button updates delay40-done.
    let delay = conn.call("acos_tap", json!({"base": b, "id": "delay40"}));
    assert_eq!(delay["ok"], json!(true), "{delay:#?}");
    assert_eq!(delay["result"]["settle"]["mode"], json!("content"), "{delay:#?}");

    // Stop the infinite progress bar so we can test coordinate tap -> selector tap.
    let stop_prog = conn.call("acos_tap", json!({"base": b, "id": "progress_stop"}));
    assert_eq!(stop_prog["ok"], json!(true), "{stop_prog:#?}");

    // Coordinate tap test: find "Fade dialog" coordinates from the tree, tap
    // by raw (x, y), and immediately tap "Close" inside the opened Dialog
    // by selector WITHOUT calling acos_observe_tree in between.
    let tree = conn.call("acos_observe_tree", json!({"base": b, "settle": "strict"}));
    assert_eq!(tree["ok"], json!(true), "{tree:#?}");
    let (dx, dy) = find_node_center(&tree["result"]["tree"], "Fade dialog")
        .expect("Fade dialog button center");
    let coord_tap = conn.call(
        "acos_tap",
        json!({"base": b, "x": dx, "y": dy, "settle": "strict"}),
    );
    assert_eq!(coord_tap["ok"], json!(true), "{coord_tap:#?}");

    let close_tap = conn.call(
        "acos_tap",
        json!({"base": b, "text": "Close", "settle": "strict"}),
    );
    assert_eq!(
        close_tap["ok"],
        json!(true),
        "selector tap right after coordinate tap must see the Dialog: {close_tap:#?}"
    );

    let destroy = conn.call("acos_destroy", json!({"base": b}));
    assert_eq!(destroy["ok"], json!(true), "{destroy:#?}");
}

#[test]
#[ignore = "boots a real ART guest; run under tools/art-runtime/run-exclusive.sh + tools/guard/guarded.sh"]
fn live_swipe_until_content_settle_on_scroll_list() {
    let apk = live_apk("scroll-list");
    let art = live_art_bin();
    if !apk.is_file() || !art.is_file() {
        acos_test_support::require_prereq(
            false,
            "live_swipe_until_content_settle_on_scroll_list",
            &format!("{} or {} missing", apk.display(), art.display()),
        );
        return;
    }

    let daemon = Daemon::start_with_art("live-scroll-content", &art, 240);
    let mut conn = daemon.connect(300);
    let b = base("s_live_scroll_content");

    let launch = conn.call("acos_launch", json!({"base": b, "apk": apk}));
    assert_eq!(launch["ok"], json!(true), "{launch:#?}");

    let scroll = conn.call(
        "acos_swipe",
        json!({
            "base": b,
            "direction": "up",
            "duration_ms": 250,
            "fling": false,
            "until": {"text": "Row 25"},
            "settle": "content",
            "max_attempts": 10,
        }),
    );
    assert_eq!(scroll["ok"], json!(true), "{scroll:#?}");
    assert_eq!(scroll["result"]["until"]["found"], json!(true), "{scroll:#?}");
    assert_eq!(scroll["result"]["until"]["settle"], json!("content"), "{scroll:#?}");
    assert_eq!(
        scroll["result"]["settle"]["mode"],
        json!("content"),
        "{scroll:#?}"
    );

    let destroy = conn.call("acos_destroy", json!({"base": b}));
    assert_eq!(destroy["ok"], json!(true), "{destroy:#?}");
}
