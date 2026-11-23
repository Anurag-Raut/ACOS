//! W50: `acos serve` drives real ART by default — engine selection, labels,
//! structured refusals and (ignored by default) the real agent loop.
//!
//! The socket is the real transport (newline JSON, `acos_mcp::daemon`), so these
//! tests drive the same path an agent uses. They are black-box: `acos` is the
//! binary under test (`CARGO_BIN_EXE_acos`).
//!
//! * `daemon_defaults_to_art_and_refuses_unknown_engines` — no guest needed.
//! * `an_unknown_session_is_not_routed_to_the_frozen_rust_vm` — no guest needed.
//! * `serve_flag_rejects_an_unknown_engine` — no guest needed.
//! * `art_agent_loop_over_the_socket` — **ignored by default**: it boots a real
//!   ART guest, so it must run under `tools/guard/guarded.sh` (ADR-0018), e.g.
//!     tools/guard/guarded.sh --mem 8G --time 600 -- \
//!       cargo test -p acos-cli --release --test w50_serve_art -- \
//!         --ignored --test-threads=1
//!   The W50 benchmark run (`bench-agent-loop.sh`) exercises the same path.

use std::io::{BufRead, BufReader, Write};
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

fn acos() -> &'static str {
    env!("CARGO_BIN_EXE_acos")
}

fn scratch(tag: &str) -> PathBuf {
    // A unix socket path must fit SUN_LEN (108 bytes including the NUL), i.e. at
    // most 107 characters. `repo_root()` is built from `CARGO_MANIFEST_DIR`, so a
    // git worktree under `.t3/worktrees/<repo>/<uuid>/` adds ~50 characters and
    // the readable directory name below overflows the limit: the daemon then
    // cannot bind and the socket never appears. Keep the readable name when it
    // fits and fall back to a short hash of it when it does not (still inside
    // this checkout's `.cache`, never /tmp).
    let pid = std::process::id();
    let mut dir = repo_root()
        .join(".cache")
        .join(format!("w50-test-{tag}-{pid}"));
    let budget = 107 - "/serve.sock".len();
    if dir.as_os_str().len() > budget {
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for byte in format!("{tag}-{pid}").bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100_0000_01b3);
        }
        dir = repo_root().join(".cache").join(format!("w50-{hash:016x}"));
    }
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("runtime")).unwrap();
    std::fs::create_dir_all(dir.join("cache")).unwrap();
    // Resolve it (a worktree root can carry a `..` segment) before it is used as
    // a socket path, and prove the resolved path still fits.
    let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
    let socket = dir.join("serve.sock");
    assert!(
        socket.as_os_str().len() <= 107,
        "the test's own socket path {} is {} characters; SUN_LEN allows 107",
        socket.display(),
        socket.as_os_str().len()
    );
    dir
}

struct Daemon {
    child: Child,
    socket: PathBuf,
    dir: PathBuf,
    // Declared last so the socket dir outlives every use of `socket`; dropped
    // (and removed) with the daemon.
    sockets: ShortSocketDir,
}

impl Daemon {
    fn start(tag: &str, extra: &[&str]) -> Daemon {
        let dir = scratch(tag);
        // `sun_path` is 108 bytes: the socket never lives under the worktree,
        // whose path length is not under this test's control.
        let sockets = ShortSocketDir::new(tag);
        let socket = sockets.socket("serve.sock");
        let mut command = Command::new(acos());
        command
            .arg("serve")
            .arg("--socket")
            .arg(&socket)
            .arg("--idle-timeout")
            .arg("120")
            .env("ACOS_RUNTIME_DIR", dir.join("runtime"))
            // The suite may export ACOS_SESSION_SOCKET; it wins over the runtime
            // dir, so this test must not inherit it.
            .env_remove("ACOS_SESSION_SOCKET")
            .env("ACOS_CACHE_DIR", dir.join("cache"))
            .args(extra)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = command.spawn().expect("spawn acos serve");
        let deadline = Instant::now() + Duration::from_secs(20);
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
            sockets,
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
            .set_read_timeout(Some(Duration::from_secs(240)))
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
    json!({"request_id": format!("w50-{id}"), "session_id": id})
}

fn details(envelope: &Value) -> Value {
    envelope
        .pointer("/error/details")
        .cloned()
        .unwrap_or(Value::Null)
}

#[test]
fn daemon_defaults_to_art_and_refuses_unknown_engines() {
    let daemon = Daemon::start("defaults", &[]);
    let mut conn = daemon.connect();

    // The transport answers, and it says which engine a launch would drive.
    let pong = conn.call("ping", json!({}));
    assert_eq!(pong["ok"], json!(true));
    assert_eq!(pong["result"]["default_engine"], json!("art"));

    // An unknown engine is a structured refusal naming the supported engines,
    // never a fallback to the Rust VM.
    let unknown = conn.call(
        "acos_launch",
        json!({"base": base("s_unknown_engine"), "apk": "/nonexistent/app.apk", "engine": "banana"}),
    );
    assert_eq!(unknown["ok"], json!(false));
    assert_eq!(unknown["error"]["code"], json!("UNSUPPORTED_BACKEND"));
    assert_eq!(unknown["error"]["class"], json!("unsupported"));
    let details = details(&unknown);
    assert_eq!(details["fallback_used"], json!(false));
    assert_eq!(details["default_engine"], json!("art"));
    assert_eq!(details["engine"], json!("art"));
    assert_eq!(details["supported_engines"], json!(["art", "rust-vm"]));

    // A non-string engine is also refused, not coerced.
    let wrong_type = conn.call(
        "acos_launch",
        json!({"base": base("s_engine_type"), "apk": "/nonexistent/app.apk", "engine": 7}),
    );
    assert_eq!(wrong_type["error"]["code"], json!("UNSUPPORTED_BACKEND"));

    // Every reply carries the engine, trust and evidence labels.
    assert_eq!(unknown["error"]["details"]["trust"], json!("trusted"));
    assert_eq!(unknown["error"]["details"]["evidence_label"], json!("none"));
    assert_eq!(pong["result"]["trust"], json!("trusted"));
}

#[test]
fn an_unknown_session_is_not_routed_to_the_frozen_rust_vm() {
    let daemon = Daemon::start("unknown-session", &[]);
    let mut conn = daemon.connect();

    // Before W50 this created a Rust VM session and returned its state. The
    // default engine is ART now, and an unlaunched id is refused by name.
    let tree = conn.call(
        "acos_observe_tree",
        json!({"base": base("s_never_launched"), "max_nodes": 100}),
    );
    assert_eq!(tree["ok"], json!(false));
    assert_eq!(tree["error"]["code"], json!("SESSION_NOT_FOUND"));
    assert_eq!(details(&tree)["engine"], json!("art"));
    assert!(tree["error"]["hint"]
        .as_str()
        .unwrap()
        .contains("acos_launch"));

    // An explicit rust-vm request reaches the frozen legacy route and every
    // reply says so. (No tree exists there; the point is the label.)
    let frozen = conn.call(
        "acos_observe_tree",
        json!({"base": base("s_frozen"), "engine": "rust-vm"}),
    );
    let label = frozen
        .pointer("/result/engine")
        .or_else(|| frozen.pointer("/error/details/engine"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    assert_eq!(label, "rust-vm, frozen", "{frozen:#?}");
    assert_eq!(frozen["ok"], json!(false));
    let frozen_details = details(&frozen);
    assert_eq!(frozen_details["engine_frozen"], json!(true));
    assert_eq!(frozen_details["engine"], json!("rust-vm, frozen"));
    let _ = conn.call("acos_destroy", json!({"base": base("s_frozen")}));

    // An unknown tool is a structured refusal as well.
    let unknown = conn.call("acos_teleport", json!({"base": base("s_x")}));
    assert_eq!(unknown["error"]["code"], json!("SESSION_NOT_FOUND"));
}

#[test]
fn serve_flag_rejects_an_unknown_engine() {
    let dir = scratch("flag");
    let sockets = ShortSocketDir::new("flag");
    let output = Command::new(acos())
        .args(["serve", "--socket"])
        .arg(sockets.socket("s.sock"))
        .args(["--engine", "native"])
        .env("ACOS_RUNTIME_DIR", dir.join("runtime"))
        // The suite may export ACOS_SESSION_SOCKET; it wins over the runtime
        // dir, so this test must not inherit it.
        .env_remove("ACOS_SESSION_SOCKET")
        .output()
        .expect("run acos serve");
    assert!(
        !output.status.success(),
        "an unknown engine must refuse to start"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unknown --engine"), "{stderr}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The real loop, through the socket, on real ART.
///
/// Ignored by default because it boots a guest: run it under
/// `tools/guard/guarded.sh` (ADR-0018). It is the functional counterpart of the
/// benchmark: tree reached, tap changes state, the assert verb passes, reset
/// produces a newer frame, and destroy leaves no process behind.
#[test]
#[ignore = "boots a real ART guest; run under tools/guard/guarded.sh"]
fn art_agent_loop_over_the_socket() {
    let apk = repo_root().join("fixtures/build/apps/counter.apk");
    let art = repo_root().join(".cache/w50-target/release/acos-art");
    if !apk.is_file() || !art.is_file() {
        acos_test_support::require_prereq(false, "art_agent_loop_over_the_socket", "counter.apk or release acos-art missing");
        return;
    }
    let daemon = Daemon::start("art-loop", &[]);
    let mut conn = daemon.connect();
    let b = base("s_w50_art_loop");

    let launch = conn.call("acos_launch", json!({"base": b, "apk": apk}));
    assert_eq!(launch["ok"], json!(true), "{launch:#?}");
    assert_eq!(launch["result"]["engine"], json!("art"));
    assert_eq!(launch["result"]["evidence_label"], json!("host-pixel"));
    assert_eq!(launch["result"]["engine_frozen"], json!(false));
    let launched_tree = launch["result"]["tree"].clone();
    assert!(find_node(&launched_tree, "inc").is_some(), "{launch:#?}");

    let tree = conn.call("acos_observe_tree", json!({"base": b, "max_nodes": 2000}));
    assert_eq!(tree["ok"], json!(true), "{tree:#?}");
    assert_eq!(tree["result"]["engine"], json!("art"));
    assert_eq!(tree["result"]["evidence_label"], json!("guest-asserted"));
    assert!(tree["result"]["frame"]["frame_seq"].as_u64().unwrap() >= 1);
    let count = find_node(&tree["result"]["tree"], "count").expect("count node");
    assert_eq!(count["text"], json!("0"));

    let tap = conn.call("acos_tap", json!({"base": b, "id": "inc"}));
    assert_eq!(tap["ok"], json!(true), "{tap:#?}");
    assert_eq!(tap["result"]["state_changed"], json!(true));

    let after = conn.call("acos_observe_tree", json!({"base": b}));
    let count = find_node(&after["result"]["tree"], "count").expect("count node");
    assert_eq!(
        count["text"],
        json!("1"),
        "the tap must increment the counter"
    );

    let asserted = conn.call(
        "acos_assert",
        json!({"base": b, "id": "count", "expect": "1", "mode": "exact"}),
    );
    assert_eq!(asserted["ok"], json!(true), "{asserted:#?}");
    assert_eq!(asserted["result"]["assert"], json!(true));

    // The server-side assert must fail, not pass, on the wrong expectation.
    let refused = conn.call(
        "acos_assert",
        json!({"base": b, "id": "count", "expect": "99", "mode": "exact"}),
    );
    assert_eq!(refused["error"]["code"], json!("ASSERTION_FAILED"));

    let shot_path = daemon.dir.join("shot.png");
    let shot = conn.call(
        "acos_observe",
        json!({"base": b, "tree": false, "png": true, "screenshot": shot_path}),
    );
    assert_eq!(shot["ok"], json!(true), "{shot:#?}");
    assert_eq!(shot["result"]["evidence_label"], json!("host-pixel"));
    let bytes = std::fs::metadata(&shot_path)
        .expect("screenshot written")
        .len();
    assert!(bytes > 1000, "PNG too small: {bytes}");
    assert_eq!(
        std::fs::read(&shot_path).unwrap()[..4],
        [0x89, b'P', b'N', b'G']
    );

    let logs = conn.call("acos_logs", json!({"base": b}));
    assert_eq!(logs["ok"], json!(true), "{logs:#?}");
    assert_eq!(logs["result"]["engine"], json!("art"));
    assert_eq!(logs["result"]["evidence"]["logs"], json!("guest-asserted"));

    // REL3S: ART delivers a real swipe (an interpolated MotionEvent stream),
    // so the verb is no longer a refusal.
    let swipe = conn.call(
        "acos_swipe",
        json!({"base": b, "from": {"x": 1, "y": 2}, "to": {"x": 3, "y": 4}}),
    );
    assert_eq!(swipe["ok"], json!(true), "{swipe:#?}");
    assert_eq!(swipe["result"]["engine"], json!("art"));
    assert!(
        swipe["result"]["handled_evidence"]["published"]
            .as_u64()
            .unwrap_or(0)
            > 2,
        "a swipe publishes a real event stream: {swipe:#?}"
    );

    let status = conn.call("acos_status", json!({"base": b}));
    assert_eq!(status["ok"], json!(true), "{status:#?}");
    assert_eq!(status["result"]["stage_reached"], json!("first_frame"));
    assert_eq!(status["result"]["capabilities"]["swipe"], json!(true));

    let before_reset = status["result"]["published_frames"].as_u64().unwrap();
    let reset = conn.call("acos_reset", json!({"base": b}));
    assert_eq!(reset["ok"], json!(true), "{reset:#?}");
    assert_eq!(reset["result"]["data_cleared"], json!(true));
    assert!(
        reset["result"]["frame_seq"].as_u64().unwrap() > before_reset,
        "reset must produce a provably newer frame: {reset:#?}"
    );

    let fresh = conn.call("acos_observe_tree", json!({"base": b}));
    let count = find_node(&fresh["result"]["tree"], "count").expect("count node");
    assert_eq!(count["text"], json!("0"), "reset must clear the counter");

    let destroy = conn.call("acos_destroy", json!({"base": b}));
    assert_eq!(destroy["ok"], json!(true), "{destroy:#?}");
    assert_eq!(destroy["result"]["teardown"]["survivors"], json!([]));
    assert_eq!(destroy["result"]["teardown"]["verified"], json!(true));

    // The guest is gone, and the id is no longer addressable.
    let gone = conn.call("acos_observe_tree", json!({"base": b}));
    assert_eq!(gone["error"]["code"], json!("SESSION_NOT_FOUND"));
}

fn find_node<'a>(nodes: &'a Value, resource: &str) -> Option<&'a Value> {
    let array = nodes.as_array()?;
    for node in array {
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
