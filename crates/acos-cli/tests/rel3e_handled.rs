//! REL3E: a reply says whether the app **handled** the input, not only that it
//! was sent.
//!
//! The feedback case: `acos_tap {"text": "Floating button"}` on a settings row
//! whose title is not the pressable (the switch is) was accepted and changed
//! nothing, and the reply said only `state_changed: false` — so an agent could
//! not tell "nothing tappable there / no handler consumed it" from "the app
//! handled it but nothing changed".
//!
//! `handled` is the bit the app window passes to Android's
//! `InputEventReceiver.finishInputEvent(handled)`; it reaches the reply through
//! the real `InputChannel` acks. This test drives it on a real ART guest with
//! the `tap-targets` fixture:
//!
//! * a tap on the non-clickable label `title` → `handled: false`,
//!   `state_changed: false`, and a `target.hit` block that names the label and
//!   the clickable `toggle` switch to tap instead;
//! * a tap on the clickable `inc` button → `handled: true`, `state_changed: true`
//!   and the counter moves.
//!
//! **Ignored by default** because it boots a real ART guest. Run it under the
//! shared lock and the host guard:
//!
//! ```text
//! tools/art-runtime/run-exclusive.sh --task REL3E -- \
//!   tools/guarded.sh --mem 8G --time 600 -- \
//!   cargo test -p acos-cli --release --test rel3e_handled -- --ignored --test-threads=1
//! ```
//!
//! The fixture must be built first (`ACOS_APP_ONLY=tap-targets tools/build-apps.sh`).

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
    // The daemon refuses an APK path with a `..` component, and a git worktree
    // root is reached through one from `crates/acos-cli`.
    std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.to_path_buf())
}

fn acos() -> &'static str {
    env!("CARGO_BIN_EXE_acos")
}

/// Scratch under this checkout's `.cache`; the socket lives in a short dir so a
/// deep worktree path cannot overflow `sun_path` (see `common`).
fn scratch(tag: &str) -> PathBuf {
    let dir = repo_root()
        .join(".cache")
        .join(format!("rel3e-test-{tag}-{}", std::process::id()));
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
        // REL3E runs its own release build under `.cache/rel3e-target`; point
        // the launcher at that `acos-art` so the guest is the binary this
        // branch built, never a stale one from another target dir.
        let art_bin = repo_root().join(".cache/rel3e-target/release/acos-art");
        let mut command = Command::new(acos());
        command
            .arg("serve")
            .arg("--socket")
            .arg(&socket)
            .arg("--idle-timeout")
            .arg("240")
            .env("ACOS_RUNTIME_DIR", dir.join("runtime"))
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
    json!({"request_id": format!("rel3e-{id}"), "session_id": id})
}

/// `true` when a node's `resource_id` is `...:id/<resource>` (the tree reports
/// the fully qualified id).
fn is_resource(node: &Value, resource: &str) -> bool {
    node["resource_id"]
        .as_str()
        .is_some_and(|rid| rid == resource || rid.ends_with(&format!(":id/{resource}")))
}

/// Find a node by resource id in either tree shape: the `observe_tree` array or
/// the `{nodes: [...], max_nodes, truncated}` object `tree_json` produces.
fn find_node<'a>(nodes: &'a Value, resource: &str) -> Option<&'a Value> {
    let nodes = match nodes {
        Value::Array(_) => nodes,
        Value::Object(map) => map.get("nodes")?,
        _ => return None,
    };
    for node in nodes.as_array()? {
        let rid = node
            .get("resource_id")
            .and_then(Value::as_str)
            .unwrap_or("");
        if rid == resource || rid.ends_with(&format!(":id/{resource}")) {
            return Some(node);
        }
        if let Some(found) = find_node(node.get("children").unwrap_or(&Value::Null), resource) {
            return Some(found);
        }
    }
    None
}

#[test]
#[ignore = "boots a real ART guest; run under tools/guarded.sh + run-exclusive.sh"]
fn tap_on_a_label_is_reported_not_handled_and_names_the_switch() {
    let apk = repo_root().join("fixtures/build/apps/tap-targets.apk");
    let art = repo_root().join(".cache/rel3e-target/release/acos-art");
    if !apk.is_file() || !art.is_file() {
        eprintln!(
            "SKIP: {} or {} missing (build with ACOS_APP_ONLY=tap-targets tools/build-apps.sh \
             and cargo build --release -p acos-art)",
            apk.display(),
            art.display()
        );
        return;
    }
    let daemon = Daemon::start("handled");
    let mut conn = daemon.connect();
    let b = base("s_rel3e");

    let launch = conn.call("acos_launch", json!({"base": b, "apk": apk}));
    assert_eq!(launch["ok"], json!(true), "{launch:#?}");
    assert_eq!(launch["result"]["engine"], json!("art"));

    // ---- the feedback case: a tap on the non-clickable label ----------------
    let label = conn.call("acos_tap", json!({"base": b, "id": "title"}));
    assert_eq!(label["ok"], json!(true), "{label:#?}");
    let result = &label["result"];
    assert_eq!(
        result["handled"],
        json!(false),
        "a tap on the label must report the app window's own handled=false: {label:#?}"
    );
    assert_eq!(result["state_changed"], json!(false));
    assert_eq!(result["handled_evidence"]["source"], json!("finishInputEvent"));
    assert_eq!(
        result["handled_evidence"]["published"],
        result["handled_evidence"]["acked"],
        "both InputMessages were acked: {label:#?}"
    );
    assert_eq!(result["target"]["selector"], json!({"id": "title"}));
    assert!(
        is_resource(&result["target"]["node"], "title"),
        "the resolved node is the label: {label:#?}"
    );
    assert!(
        is_resource(&result["target"]["hit"]["node"], "title"),
        "the hit-test names the label: {label:#?}"
    );
    assert_eq!(
        result["target"]["hit"]["node"]["clickable"],
        json!(false),
        "the label itself takes no click: {label:#?}"
    );
    assert_eq!(
        result["target"]["hit"]["consumer"],
        Value::Null,
        "no ancestor of the label is clickable: {label:#?}"
    );
    assert!(
        is_resource(&result["target"]["hit"]["suggest"], "toggle"),
        "the hit-test must name the clickable sibling: {label:#?}"
    );
    assert_eq!(result["target"]["hit"]["suggest"]["clickable"], json!(true));
    let hint = result["handled_hint"].as_str().unwrap_or_default();
    assert!(hint.contains("nothing tappable there"), "{hint}");
    assert!(hint.contains("Switch"), "the hint names the switch class: {hint}");
    assert!(
        hint.contains("nearest clickable"),
        "the hint says what to tap instead: {hint}"
    );

    // ---- and a control that really is clickable ----------------------------
    let button = conn.call("acos_tap", json!({"base": b, "id": "inc"}));
    assert_eq!(button["ok"], json!(true), "{button:#?}");
    let result = &button["result"];
    assert_eq!(
        result["handled"],
        json!(true),
        "a tap on the button must report handled=true: {button:#?}"
    );
    assert_eq!(result["state_changed"], json!(true));
    assert!(
        is_resource(&result["target"]["hit"]["node"], "inc"),
        "the hit-test names the button: {button:#?}"
    );
    assert_eq!(result["target"]["hit"]["clickable"], json!(true));
    assert_eq!(result["target"]["hit"]["suggest"], Value::Null);
    assert!(
        result.get("handled_hint").is_none(),
        "a handled tap that changed the screen needs no hint: {button:#?}"
    );

    let tree = conn.call("acos_observe_tree", json!({"base": b, "max_nodes": 2000}));
    assert_eq!(tree["ok"], json!(true), "{tree:#?}");
    let count = find_node(&tree["result"]["tree"], "count").expect("count node");
    assert_eq!(count["text"], json!("1"), "the handled tap really incremented");

    // REL3S gives ART a real swipe publisher; the same `handled` contract that
    // governed the tap above governs every event of the gesture.
    let swipe = conn.call(
        "acos_swipe",
        json!({"base": b, "from": {"x": 1, "y": 2}, "to": {"x": 3, "y": 4}}),
    );
    assert_eq!(swipe["ok"], json!(true), "{swipe:#?}");
    assert_eq!(
        swipe["result"]["handled_evidence"]["source"],
        json!("finishInputEvent"),
        "a swipe reports the window's own handled bit: {swipe:#?}"
    );

    let destroy = conn.call("acos_destroy", json!({"base": b}));
    assert_eq!(destroy["ok"], json!(true), "{destroy:#?}");
}
