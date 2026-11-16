//! FB1: the live-channel selectors and typing, over the `acos serve` socket.
//!
//! Three claims, each driven exactly as an agent drives them (`acos_tap` /
//! `acos_text` / `acos_assert` requests on the daemon socket):
//!
//! * **`content_desc` selectors** reach a React Native `TextInput` whose node
//!   text is its placeholder (`猫, ねこ, neko...`) and whose *name* lives in the
//!   content description (`Search Japanese dictionary`).
//! * **typing is the scenario path.** The old live gate refused with
//!   `UNSUPPORTED_BACKEND text.input` whenever no node ended in `EditText`
//!   (every RN `TextInput`: an obfuscated subclass) — so an agent could focus
//!   the field and still not type. The verb now forwards the guest's own
//!   `type_text` command, the same one a scenario `type` step sends, and this
//!   test asserts the guest received exactly that command with the typed text.
//! * **ambiguity is refused, never silently picked**: two visible nodes named
//!   `Continue` are `AMBIGUOUS_SELECTOR` with both candidates listed, and
//!   nothing is injected.
//!
//! A fake `acos-art` (`ACOS_ART_BIN`) keeps this fast and guest-free; the real
//! end-to-end proof on Gaten is the release smoke (`tools/release/mcp_smoke.py
//! --mode gaten-search`) plus `results/handoff/FB1.md`.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn scratch(tag: &str) -> PathBuf {
    // The daemon's `serve.sock` lives in here, so the directory name is part of
    // a unix-socket path and must fit `sun_path` (108 bytes) even from a long
    // worktree root. A short stable digest of the tag keeps the tests distinct
    // without spending the budget on prose.
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    tag.hash(&mut hasher);
    let dir = repo_root().join(".cache").join(format!(
        "fb1-{:08x}-{}",
        hasher.finish() as u32,
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("runtime")).unwrap();
    std::fs::create_dir_all(dir.join("cache")).unwrap();
    std::fs::create_dir_all(dir.join("art-home")).unwrap();
    std::fs::canonicalize(&dir).unwrap_or(dir)
}

/// A fake interactive `acos-art` session.
///
/// It answers `ready`/`tree`/`tap`/`type_text`/`logs`/`close` and appends every
/// request line it receives to `requests.jsonl`, so the test can prove *what*
/// the live channel actually sent to the guest (routing), not just what the
/// envelope said. Its tree is the Gaten home screen's shape: an RN `TextInput`
/// whose text is the placeholder and whose name is its content description, two
/// buttons that share the label `Continue`, and — when the `password` marker
/// file exists — a focused password field.
fn fake_art(dir: &Path) -> PathBuf {
    let script = dir.join("fake-acos-art.py");
    let body = format!(
        r#"#!/usr/bin/env python3
import json, os, sys

out = os.environ["FAKE_ART_OUT"]
log = os.path.join(out, "requests.jsonl")
png = os.path.join(out, "frame.png")
os.makedirs(out, exist_ok=True)
with open(png, "wb") as handle:
    handle.write(b"\x89PNG\r\n\x1a\n-fake-frame")

state = {{"seq": 0, "focused": False, "typed": ""}}

def tree():
    nodes = [
        {{
            "id": "search", "class": "Z4.g",
            "text": "猫, ねこ, neko...", "hint": "猫, ねこ, neko...",
            "content_description": "Search Japanese dictionary",
            "bounds": "[189,170][831,286]", "bounds_on_screen": "[189,170][831,286]",
            "visible": True, "focused": state["focused"], "clickable": True,
            "password": False, "window": "main",
        }},
        {{
            "id": "continue-a", "class": "android.widget.Button", "text": "Continue",
            "content_description": "", "bounds": "[0,0][100,60]",
            "bounds_on_screen": "[0,0][100,60]", "visible": True, "clickable": True,
            "window": "main",
        }},
        {{
            "id": "continue-b", "class": "android.widget.Button", "text": "Continue",
            "content_description": "", "bounds": "[0,60][100,120]",
            "bounds_on_screen": "[0,60][100,120]", "visible": True, "clickable": True,
            "window": "main",
        }},
    ]
    if os.path.exists(os.path.join(out, "password")):
        nodes.append({{
            "id": "token", "class": "android.widget.EditText", "text": "",
            "hint": "Paste your token", "content_description": "API token",
            "bounds": "[0,120][100,180]", "bounds_on_screen": "[0,120][100,180]",
            "visible": True, "focused": True, "password": True, "window": "main",
        }})
    return {{
        "nodes": nodes, "windows": [{{"window": "main", "z": 0}}],
        "window_count": 1, "node_budget": 2000, "truncated": False,
    }}

def frame(cmd):
    state["seq"] += 1
    return {{
        "ok": True, "cmd": cmd, "frame_seq": state["seq"], "md5": "m%d" % state["seq"],
        "png": png, "tree": tree(),
        "settle": {{"mode": "strict", "settled": True, "bound": "event",
                   "first_change_ms": 1}},
        "gesture": {{"seqs": [state["seq"]], "token": "0x1",
                    "acks": [{{"seq": state["seq"], "handled": True,
                              "latency_us": 1000}}]}},
    }}

print(json.dumps({{"ok": True, "cmd": "ready", "package": "com.gaten",
                  "activity": "com.gaten.app/.MainActivity", "width": 1080,
                  "height": 2400, "density": 420}}), flush=True)

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
    if cmd == "tree":
        print(json.dumps(frame("tree")), flush=True)
    elif cmd == "tap":
        x, y = request.get("x", 0), request.get("y", 0)
        # The field is the only target in its band; anything inside its bounds
        # means the selector resolved to the text input.
        if 189 <= x <= 831 and 170 <= y <= 286:
            state["focused"] = True
        print(json.dumps(frame("tap")), flush=True)
    elif cmd == "type_text":
        state["typed"] += request.get("text", "")
        print(json.dumps(frame("type_text")), flush=True)
    elif cmd == "key":
        print(json.dumps(frame("key")), flush=True)
    elif cmd == "logs":
        print(json.dumps({{"ok": True, "cmd": "logs", "total": 0, "kept": 0,
                          "dropped": 0, "entries": []}}), flush=True)
    elif cmd == "close":
        print(json.dumps({{"ok": True, "cmd": "close"}}), flush=True)
        break
    else:
        print(json.dumps({{"ok": False, "cmd": cmd, "code": "BAD_REQUEST",
                          "error": "unsupported"}}), flush=True)
"#
    );
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
}

impl Daemon {
    fn start(tag: &str) -> Daemon {
        let dir = scratch(tag);
        let art_home = dir.join("art-home");
        let fake = fake_art(&art_home);
        let socket = dir.join("serve.sock");
        let child = Command::new(env!("CARGO_BIN_EXE_acos"))
            .arg("serve")
            .arg("--socket")
            .arg(&socket)
            .arg("--idle-timeout")
            .arg("120")
            .env("ACOS_RUNTIME_DIR", dir.join("runtime"))
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
        }
    }

    fn connect(&self) -> Conn {
        Conn::new(&self.socket)
    }

    fn apk(&self) -> PathBuf {
        let apk = self.dir.join("fake.apk");
        if !apk.exists() {
            std::fs::write(&apk, b"not-a-real-apk").unwrap();
        }
        apk
    }

    /// Every request line the fake guest has received so far.
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
    fn new(path: &Path) -> Conn {
        let stream = UnixStream::connect(path).expect("connect to serve socket");
        stream
            .set_read_timeout(Some(Duration::from_secs(120)))
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
    json!({"request_id": format!("fb1-{id}"), "session_id": id})
}

fn launch(daemon: &Daemon, conn: &mut Conn, id: &str) -> Value {
    let launched = conn.call(
        "acos_launch",
        json!({"base": base(id), "apk": daemon.apk()}),
    );
    assert_eq!(launched["ok"], json!(true), "{launched:#?}");
    launched
}

/// (1) Typing on a live session is the scenario path: the guest receives the
/// same `{"cmd":"type_text"}` a `type` step sends, and the reply is a pass. The
/// field is an RN `TextInput` (obfuscated class), which the removed
/// `EditText`-suffix gate used to refuse.
#[test]
fn live_text_reaches_the_guest_type_text_command() {
    let daemon = Daemon::start("live-text");
    let mut conn = daemon.connect();
    let b = base("s_text");
    launch(&daemon, &mut conn, "s_text");

    let tapped = conn.call(
        "acos_tap",
        json!({"base": b, "content_desc": "Search Japanese dictionary"}),
    );
    assert_eq!(tapped["ok"], json!(true), "{tapped:#?}");
    assert_eq!(tapped["result"]["accepted"], json!(true));

    // The tap landed on the text input (the fake only focuses it inside its
    // bounds), so the tree now says the field is focused.
    let tree = conn.call("acos_observe_tree", json!({"base": b}));
    assert_eq!(tree["ok"], json!(true), "{tree:#?}");
    let nodes = tree
        .pointer("/result/tree/nodes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let search = nodes
        .iter()
        .find(|node| node["content_desc"] == json!("Search Japanese dictionary"))
        .expect("the search field is in the tree");
    assert_eq!(search["focused"], json!(true), "{search:#?}");
    assert_eq!(search["text"], json!("猫, ねこ, neko..."));
    assert_eq!(search["password"], json!(false));

    let typed = conn.call("acos_text", json!({"base": b, "text": "kayou"}));
    assert_eq!(typed["ok"], json!(true), "{typed:#?}");
    assert_eq!(typed["status"], json!("pass"));
    assert_eq!(typed["result"]["action"], json!("text"));
    assert_eq!(typed["result"]["target"]["text"], json!("kayou"));

    // Routing, not just the envelope: the guest was asked to type, exactly as a
    // scenario `type` step asks it.
    let requests = daemon.guest_requests();
    let type_requests: Vec<&Value> = requests
        .iter()
        .filter(|request| request["cmd"] == json!("type_text"))
        .collect();
    assert_eq!(
        type_requests.len(),
        1,
        "the live verb must send one type_text command: {requests:#?}"
    );
    assert_eq!(type_requests[0]["text"], json!("kayou"));
    assert!(
        type_requests[0].get("settle").is_some(),
        "the settle mode travels with the command, as on the scenario path"
    );
    let _ = conn.call("acos_destroy", json!({"base": b}));
}

/// (2) An explicit `content_desc` selector is field-exact, and a selector that
/// names several user-visible nodes is refused with the candidates listed —
/// never silently picked, and nothing is injected.
#[test]
fn ambiguous_selector_is_refused_with_candidates() {
    let daemon = Daemon::start("ambiguous");
    let mut conn = daemon.connect();
    let b = base("s_amb");
    launch(&daemon, &mut conn, "s_amb");

    let before = daemon.guest_requests().len();
    let refused = conn.call("acos_tap", json!({"base": b, "text": "Continue"}));
    assert_eq!(refused["ok"], json!(false), "{refused:#?}");
    assert_eq!(refused["error"]["code"], json!("AMBIGUOUS_SELECTOR"));
    assert_eq!(refused["error"]["class"], json!("assertion"));
    let details = refused
        .pointer("/error/details")
        .cloned()
        .expect("the refusal carries structured details");
    assert_eq!(details["count"], json!(2), "{details:#?}");
    let candidates = details["candidates"]
        .as_array()
        .cloned()
        .expect("candidates");
    assert_eq!(candidates.len(), 2);
    for candidate in &candidates {
        assert_eq!(candidate["text"], json!("Continue"));
        assert!(
            candidate["bounds"].as_str().is_some_and(|b| b.contains("][")),
            "each candidate names its bounds: {candidate:#?}"
        );
        assert!(candidate["id"].as_str().is_some());
    }
    // Nothing reached the guest: an ambiguous request never injects input.
    let after = daemon.guest_requests();
    assert_eq!(
        after.len(),
        before,
        "an ambiguous tap must not reach the guest: {:?}",
        &after[before..]
    );

    // The escape hatch: `index` picks one occurrence explicitly.
    let second = conn.call(
        "acos_tap",
        json!({"base": b, "text": "Continue", "index": 1}),
    );
    assert_eq!(second["ok"], json!(true), "{second:#?}");

    // Assertions are field-exact: `text` never falls back to a content
    // description, so the RN field's accessibility label is not its text.
    let wrong = conn.call(
        "acos_assert",
        json!({"base": b, "text": "Search Japanese dictionary", "expect": "x"}),
    );
    assert_eq!(wrong["error"]["code"], json!("ELEMENT_NOT_FOUND"), "{wrong:#?}");
    // ... and the explicit content_desc is the field-exact way to name it.
    let ok = conn.call(
        "acos_assert",
        json!({"base": b, "content_desc": "Search Japanese dictionary",
               "expect": "猫, ねこ, neko..."}),
    );
    assert_eq!(ok["ok"], json!(true), "{ok:#?}");
    assert_eq!(ok["result"]["assert"], json!(true));
    let _ = conn.call("acos_destroy", json!({"base": b}));
}

/// (2b) An assertion that names several nodes is refused too, with the same
/// structured candidate list.
#[test]
fn ambiguous_assertion_is_refused_with_candidates() {
    let daemon = Daemon::start("ambiguous-assert");
    let mut conn = daemon.connect();
    let b = base("s_amb_assert");
    launch(&daemon, &mut conn, "s_amb_assert");

    let refused = conn.call(
        "acos_assert",
        json!({"base": b, "text": "Continue", "expect": "Continue"}),
    );
    assert_eq!(refused["error"]["code"], json!("AMBIGUOUS_SELECTOR"), "{refused:#?}");
    assert_eq!(
        refused.pointer("/error/details/count"),
        Some(&json!(2)),
        "{refused:#?}"
    );
    let _ = conn.call("acos_destroy", json!({"base": b}));
}

/// (1b) A password field never echoes the typed characters: the reply masks
/// them and says how many there were.
#[test]
fn password_typing_is_masked_in_the_reply() {
    let daemon = Daemon::start("masked");
    std::fs::write(daemon.art_home.join("password"), b"1").unwrap();
    let mut conn = daemon.connect();
    let b = base("s_mask");
    launch(&daemon, &mut conn, "s_mask");

    let typed = conn.call("acos_text", json!({"base": b, "text": "s3cret"}));
    assert_eq!(typed["ok"], json!(true), "{typed:#?}");
    let target = typed.pointer("/result/target").cloned().expect("target");
    assert_eq!(target["text_masked"], json!(true), "{target:#?}");
    assert_eq!(target["chars"], json!(6));
    assert_eq!(target["text"], json!("*"));
    let envelope = typed.to_string();
    assert!(
        !envelope.contains("s3cret"),
        "the reply must not carry the password: {envelope}"
    );
    let _ = conn.call("acos_destroy", json!({"base": b}));
}
