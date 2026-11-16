//! REL3S: swipe/scroll on the real-ART engine.
//!
//! Before this track `acos_swipe` / `acos swipe` / a scenario `swipe` step on
//! ART answered `UNSUPPORTED_BACKEND` (`input.swipe`): the live input channel
//! had a tap publisher and nothing else, so an agent could not scroll a list in
//! a real app session. The fix publishes a real interpolated `MotionEvent`
//! stream (DOWN, MOVEs at the display refresh cadence with real timestamps, UP)
//! through the same channel taps use, reports each event's
//! `finishInputEvent(handled)` bit (REL3E), settles with the existing rules, and
//! adds a bounded `until` search whose "found" can only come from a settled
//! traversal.
//!
//! **Ignored by default** because it boots a real ART guest. Run it under the
//! shared lock and the host guard:
//!
//! ```text
//! ACOS_APP_ONLY=scroll-list tools/build-apps.sh
//! tools/art-runtime/run-exclusive.sh --task REL3S -- \
//!   tools/guarded.sh --mem 10G --time 900 -- \
//!   cargo test -p acos-cli --release --test rel3s_swipe -- --ignored --test-threads=1
//! ```

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

mod common;

use common::ShortSocketDir;

fn repo_root() -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.to_path_buf())
}

fn acos() -> &'static str {
    env!("CARGO_BIN_EXE_acos")
}

/// The release `acos-art` launcher this branch built.
///
/// The track's own target dir is preferred; an explicit `ACOS_ART_BIN` wins, so
/// the test can be pointed at a launcher built elsewhere without editing it.
fn art_bin() -> PathBuf {
    if let Some(path) = std::env::var_os("ACOS_ART_BIN") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return path;
        }
    }
    for target in [
        ".cache/rel3h-target",
        ".cache/rel3s-target",
        ".cache/rel3s2-target",
    ] {
        let path = repo_root().join(target).join("release/acos-art");
        if path.is_file() {
            return path;
        }
    }
    repo_root().join(".cache/rel3s2-target/release/acos-art")
}

fn scratch(tag: &str) -> PathBuf {
    let dir = repo_root()
        .join(".cache")
        .join(format!("rel3s-test-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("runtime")).unwrap();
    std::fs::create_dir_all(dir.join("cache")).unwrap();
    std::fs::canonicalize(&dir).unwrap_or(dir)
}

struct Daemon {
    child: Child,
    socket: PathBuf,
    dir: PathBuf,
    _sockets: ShortSocketDir,
}

impl Daemon {
    fn start(tag: &str) -> Daemon {
        let dir = scratch(tag);
        let sockets = ShortSocketDir::new(tag);
        let socket = sockets.socket("serve.sock");
        let art_bin = art_bin();
        let mut command = Command::new(acos());
        command
            .arg("serve")
            .arg("--socket")
            .arg(&socket)
            .arg("--idle-timeout")
            .arg("240")
            .env("ACOS_RUNTIME_DIR", dir.join("runtime"))
            // The suite may export ACOS_SESSION_SOCKET; it wins over the runtime
            // dir, so this test must not inherit it.
            .env_remove("ACOS_SESSION_SOCKET")
            .env("ACOS_CACHE_DIR", dir.join("cache"))
            .env("ACOS_ART_BIN", &art_bin)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = command.spawn().expect("spawn acos serve");
        let deadline = Instant::now() + Duration::from_secs(30);
        while !socket.exists() {
            assert!(
                Instant::now() < deadline,
                "serve socket {} never appeared",
                socket.display()
            );
            std::thread::sleep(Duration::from_millis(25));
        }
        Daemon {
            child,
            socket,
            dir,
            _sockets: sockets,
        }
    }

    fn connect(&self) -> Conn {
        Conn::new(&self.socket)
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // Only the process this test started.
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
            .set_read_timeout(Some(Duration::from_secs(300)))
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
    json!({"request_id": format!("rel3s-{id}"), "session_id": id})
}

/// Rows are plain TextViews: `text` is the row label, and the tree only reports
/// nodes the user can see (an off-screen row is absent or `visible:false`).
fn node_texts(nodes: &Value, out: &mut Vec<String>) {
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
                node_texts(children, out);
            }
        }
    }
}

fn texts(tree: &Value) -> Vec<String> {
    let mut out = Vec::new();
    node_texts(tree, &mut out);
    out
}

fn visible_texts(tree: &Value) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(nodes: &Value, out: &mut Vec<String>) {
        let nodes = match nodes {
            Value::Array(_) => nodes,
            Value::Object(map) => match map.get("nodes") {
                Some(nodes) => nodes,
                None => return,
            },
            _ => return,
        };
        let Some(array) = nodes.as_array() else {
            return;
        };
        for node in array {
            if node.get("visible").and_then(Value::as_bool) == Some(true) {
                if let Some(text) = node.get("text").and_then(Value::as_str) {
                    if !text.is_empty() {
                        out.push(text.to_string());
                    }
                }
            }
            if let Some(children) = node.get("children") {
                walk(children, out);
            }
        }
    }
    walk(tree, &mut out);
    out
}

#[test]
#[ignore = "boots a real ART guest; run under tools/guarded.sh + run-exclusive.sh"]
fn swipe_scrolls_a_long_list_and_scroll_until_finds_a_deep_row() {
    let apk = repo_root().join("fixtures/build/apps/scroll-list.apk");
    let art = art_bin();
    if !apk.is_file() || !art.is_file() {
        eprintln!(
            "SKIP: {} or {} missing (build with ACOS_APP_ONLY=scroll-list tools/build-apps.sh \
             and cargo build --release -p acos-art/acos-cli)",
            apk.display(),
            art.display()
        );
        return;
    }
    let daemon = Daemon::start("swipe");
    let mut conn = daemon.connect();
    let b = base("s_rel3s");

    let launch = conn.call("acos_launch", json!({"base": b, "apk": apk}));
    assert_eq!(launch["ok"], json!(true), "{launch:#?}");
    // REL3S: the live session advertises the real gesture command. `acos_status`
    // is the session's own capability report (the launch reply carries the
    // `capabilities_used` envelope list, not this map).
    let status = conn.call("acos_status", json!({"base": b}));
    assert_eq!(status["ok"], json!(true), "{status:#?}");
    assert_eq!(
        status["result"]["capabilities"]["swipe"],
        json!(true),
        "the ART session must advertise its swipe command: {status:#?}"
    );

    let tree = conn.call("acos_observe_tree", json!({"base": b, "max_nodes": 4000}));
    assert_eq!(tree["ok"], json!(true), "{tree:#?}");
    let before = texts(&tree["result"]["tree"]);
    assert!(
        before.iter().any(|text| text == "Row 0"),
        "the fixture starts at the top of the list: {before:?}"
    );

    // ---- one real swipe up: the list moves --------------------------------
    let swipe = conn.call(
        "acos_swipe",
        json!({
            "base": b,
            "from": {"x": 300, "y": 1100},
            "to": {"x": 300, "y": 400},
            "duration_ms": 300,
            "fling": true,
        }),
    );
    assert_eq!(swipe["ok"], json!(true), "{swipe:#?}");
    let result = &swipe["result"];
    assert_eq!(
        result["accepted"],
        json!(true),
        "the live input channel accepted the gesture: {swipe:#?}"
    );
    assert_eq!(
        result["handled_evidence"]["source"],
        json!("finishInputEvent"),
        "REL3E's handled contract reaches swipe: {swipe:#?}"
    );
    let published = result["handled_evidence"]["published"].as_u64().unwrap_or(0);
    let acked = result["handled_evidence"]["acked"].as_u64().unwrap_or(0);
    assert!(
        published > 10,
        "a 300 ms gesture publishes DOWN + ~19 MOVEs + UP, not a tap: {swipe:#?}"
    );
    assert_eq!(
        acked, published,
        "every published event carries the window's own finishInputEvent bit: {swipe:#?}"
    );
    assert_eq!(
        result["handled"],
        json!(true),
        "a scroll gesture is consumed by the list: {swipe:#?}"
    );
    assert_eq!(
        result["state_changed"],
        json!(true),
        "the settled tree shows the list moved: {swipe:#?}"
    );

    let after = conn.call("acos_observe_tree", json!({"base": b, "max_nodes": 4000}));
    assert_eq!(after["ok"], json!(true), "{after:#?}");
    let visible = visible_texts(&after["result"]["tree"]);
    assert!(
        !visible.iter().any(|text| text == "Row 0"),
        "row 0 must have left the viewport after a real swipe: {visible:?}"
    );
    let deep = visible
        .iter()
        .filter_map(|text| text.strip_prefix("Row ").and_then(|n| n.parse::<u32>().ok()))
        .max()
        .unwrap_or(0);
    assert!(
        deep >= 5,
        "a real swipe must reveal later rows (highest visible: Row {deep}) {visible:?}"
    );

    // ---- scroll_until: a bounded search on settled observations ------------
    let found = conn.call(
        "acos_swipe",
        json!({
            "base": b,
            "direction": "up",
            "duration_ms": 200,
            "fling": true,
            "until": {"text": "Row 80"},
            "max_attempts": 12,
        }),
    );
    assert_eq!(found["ok"], json!(true), "{found:#?}");
    assert_eq!(found["result"]["until"]["found"], json!(true), "{found:#?}");
    let attempts = found["result"]["until"]["attempts"].as_u64().unwrap_or(0);
    assert!(
        (1..=12).contains(&attempts),
        "the search is bounded and reports its attempts: {found:#?}"
    );
    assert!(
        found["result"]["until"]["frame_seq"].as_u64().unwrap_or(0) > 0,
        "found is backed by a settled traversal's frame: {found:#?}"
    );
    let tree = conn.call("acos_observe_tree", json!({"base": b, "max_nodes": 4000}));
    let visible = visible_texts(&tree["result"]["tree"]);
    assert!(
        visible.iter().any(|text| text == "Row 80"),
        "the found row is really on screen after the search: {visible:?}"
    );

    // ---- and a miss is honest ---------------------------------------------
    // Two gestures cannot reach the end of a 100-row list from here, so this is
    // the "used every attempt" miss. If a gesture did reach the end first the
    // answer is `END_OF_LIST` (REL3H, pinned by the case below) - both are
    // structured non-passes naming the same facts, and neither is a pass.
    let missing = conn.call(
        "acos_swipe",
        json!({
            "base": b,
            "direction": "up",
            "duration_ms": 200,
            "until": {"text": "Row 9999"},
            "max_attempts": 2,
        }),
    );
    assert_eq!(missing["ok"], json!(false), "{missing:#?}");
    let code = missing["error"]["code"].as_str().unwrap_or_default();
    assert!(
        code == "ELEMENT_NOT_FOUND" || code == "END_OF_LIST",
        "a scroll_until miss is a structured failure, never a pass: {missing:#?}"
    );
    let details = &missing["error"]["details"];
    assert_eq!(details["max_attempts"], json!(2), "{missing:#?}");
    assert!(
        (1..=2).contains(&details["attempts"].as_u64().unwrap_or(0)),
        "the failure says how many gestures it took: {missing:#?}"
    );
    assert!(
        details["last_tree_texts"]
            .as_array()
            .is_some_and(|texts| !texts.is_empty()),
        "the failure names what the last tree actually showed: {missing:#?}"
    );

    // ---- REL3H/REL3H2: the end of the list ends the search ------------------
    // Row 9999 does not exist. With enough attempts the list reaches its end;
    // the next gesture then moves nothing, and the search must stop there with
    // END_OF_LIST - in *fewer* attempts than `max_attempts`, not by burning
    // every remaining gesture and reporting ELEMENT_NOT_FOUND. REL3H2: one
    // unchanged gesture is not enough (a loading list is unchanged too), so the
    // end needs two *consumed* gestures that each moved nothing.
    let end = conn.call(
        "acos_swipe",
        json!({
            "base": b,
            "direction": "up",
            "duration_ms": 200,
            "fling": true,
            "until": {"text": "Row 9999"},
            "max_attempts": 12,
        }),
    );
    assert_eq!(end["ok"], json!(false), "{end:#?}");
    assert_eq!(
        end["error"]["code"],
        json!("END_OF_LIST"),
        "the end of the list is its own structured answer: {end:#?}"
    );
    let details = &end["error"]["details"];
    assert_eq!(details["end_of_list"], json!(true), "{end:#?}");
    assert_eq!(
        details["unchanged_gestures"],
        json!(2),
        "the end is two unchanged consumed gestures (REL3H2): {end:#?}"
    );
    assert_eq!(
        details["gesture_consumed"],
        json!(true),
        "the boundaries of the list really consumed the swipes: {end:#?}"
    );
    let attempts = details["attempts"].as_u64().unwrap_or(0);
    assert!(
        attempts >= 2 && attempts < 12,
        "the search stops at the end instead of spending every attempt: {end:#?}"
    );
    assert_eq!(details["max_attempts"], json!(12), "{end:#?}");
    assert!(
        details["last_tree_texts"]
            .as_array()
            .is_some_and(|texts| texts.iter().any(|text| text == "Row 99")),
        "the last settled tree is the end of the list: {end:#?}"
    );

    let destroy = conn.call("acos_destroy", json!({"base": b}));
    assert_eq!(destroy["ok"], json!(true), "{destroy:#?}");
}
