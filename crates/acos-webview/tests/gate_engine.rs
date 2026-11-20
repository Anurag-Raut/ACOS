//! WV5b acceptance: the **lazy gate** the sessions run, in front of the **real**
//! shared broker and the **real** pinned engine, over the **real** wire.
//!
//! It starts two sessions exactly the way a launch does — one small per-session
//! listener each, no engine — connects like the guest provider (WIRE v1 `hello`
//! → `view.create` → `view.loadData` → `render`), and asserts a PNG frame comes
//! back from Chromium. The second session must get **its own browser context**
//! from the same one-per-host broker, and the broker must idle-stop once no
//! session is left.
//!
//! Heavy (it starts Chromium), so it is guarded and opt-in:
//!
//! ```text
//! tools/art-runtime/run-exclusive.sh --task WV5b -- tools/guarded.sh --mem 8G --time 420 -- \
//!   env ACOS_WEBVIEW_TEST=1 CARGO_TARGET_DIR=$PWD/.cache/wv5b-target \
//!   cargo test -p acos-webview --test gate_engine -- --nocapture --test-threads=1
//! ```
//!
//! Without `ACOS_WEBVIEW_TEST=1` it prints a loud SKIP and returns: it never
//! reports a pass it did not measure.

mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use acos_webview::gate::{BrokerSource, Gate, GateConfig};
use common::{assert_png, WireGuest};
use serde_json::json;

fn engine_test_enabled() -> bool {
    std::env::var("ACOS_WEBVIEW_TEST").as_deref() == Ok("1")
}

fn scratch(tag: &str) -> PathBuf {
    // Absolute and **short**: a session socket path must stay well inside
    // `sun_path`'s 107 bytes even in a deep worktree, and the broker endpoint a
    // test names must not depend on the daemon's own cwd.
    let base = std::env::var_os("ACOS_VIEW_TEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join(format!("acos-wv5b-{}", std::process::id())));
    let dir = base.join(format!("e{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    dir
}

fn gate(dir: &Path, session_id: &str, broker_socket: &Path, bin: &Path, idle_secs: u64) -> Gate {
    Gate::start(GateConfig {
        session_dir: dir.join(if session_id == "session-a" {
            "a"
        } else if session_id == "session-b" {
            "b"
        } else {
            session_id
        }),
        session_id: session_id.to_string(),
        broker_socket: broker_socket.to_path_buf(),
        broker: BrokerSource::Binary(bin.to_path_buf()),
        engine_cache: std::env::var_os("ACOS_WEBVIEW_CACHE").map(PathBuf::from),
        idle_shutdown: Some(Duration::from_secs(idle_secs)),
        log: Some(dir.join("broker.log")),
        spawn_wait: acos_webview::gate::SPAWN_WAIT,
    })
    .expect("start gate")
}

fn pid_alive(pid: u32) -> bool {
    pid != 0 && Path::new(&format!("/proc/{pid}")).exists()
}

/// The one-broker-per-host, one-context-per-session, lazy-start gate — measured
/// end to end with real Chromium behind it.
#[test]
fn two_sessions_get_their_own_context_and_a_real_png_frame_through_the_gate() {
    if acos_test_support::require_prereq(
        engine_test_enabled(),
        "two_sessions_get_their_own_context_and_a_real_png_frame_through_the_gate",
        "set ACOS_WEBVIEW_TEST=1 to run this heavy Chromium test (the release gate runs it with the env set; see tools/test-prereq-optouts.txt)",
    )
    .is_none()
    {
        return;
    }
    let root = scratch("two-sessions");
    let broker_socket = root.join("run").join("broker.sock");
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_acos-webview"));

    let gate_a = gate(&root, "session-a", &broker_socket, &bin, 10);
    let gate_b = gate(&root, "session-b", &broker_socket, &bin, 10);
    // Lazy: two gates exist, nothing was spawned, nothing listens but the two
    // session sockets.
    assert_eq!(
        gate_a.stats().broker_spawns,
        0,
        "start must not wake the broker"
    );
    assert_eq!(
        gate_b.stats().broker_spawns,
        0,
        "start must not wake the broker"
    );
    assert!(!broker_socket.exists(), "no broker before a guest connects");

    // --- session A: the first guest connection starts the shared broker -------
    let a = WireGuest::connect(gate_a.socket());
    let hello_a = a.hello();
    assert_eq!(hello_a["proto"], json!(2));
    assert_eq!(hello_a["session"], json!("session-a"));
    let engine = &hello_a["engine"];
    assert!(
        engine["product"]
            .as_str()
            .unwrap_or("")
            .contains("HeadlessChrome"),
        "the guest must be told the engine identity: {engine}"
    );
    let t_first = Instant::now();
    assert!(
        pid_alive(gate_a.spawned_broker_pid()),
        "the first connect must have spawned the shared broker"
    );
    let created = a
        .call(
            "view.create",
            json!({"view": 1, "width": 1080, "height": 2400, "density": 2.625}),
        )
        .expect("view.create");
    assert_eq!(created["view"], json!(1));
    let loaded = a
        .call(
            "view.loadData",
            json!({
                "view": 1,
                "mime": "text/html",
                "encoding": "utf8",
                "data": "<!doctype html><title>gate-a</title><body style=\"background:#0a0\">\
                         <h1 id=\"t\">session A</h1>",
            }),
        )
        .expect("view.loadData");
    assert_eq!(loaded["view"], json!(1));
    let (meta, blob) = a.render(1);
    assert_eq!(meta["format"], json!("png"));
    // WV7: the frame's real pixel size. This engine has no device-resolution
    // capture path under begin-frame control (WIRE.md §4.2), so 1080x2400 @2.625
    // renders 411x914; 720 wide renders 274. The guest scales into the View.
    assert_eq!(meta["width"], json!(411));
    assert_eq!(meta["height"], json!(914));
    // The BLOB header carries the same real size as the JSON (WIRE.md §1: the
    // blob's id is the JSON response's, and its width/height are the frame's).
    assert_eq!(blob.width, 411);
    assert_eq!(blob.height, 914);
    assert_eq!(
        blob.data.len(),
        meta["bytes"].as_u64().unwrap_or(0) as usize
    );
    assert_png(&blob.data);
    println!(
        "session A: {} PNG bytes, session {} ms, engine {} {}",
        blob.data.len(),
        t_first.elapsed().as_millis(),
        engine["product"].as_str().unwrap_or(""),
        engine["version"].as_str().unwrap_or("")
    );
    let title = a
        .call(
            "view.evaluateJavascript",
            json!({"view": 1, "script": "document.title"}),
        )
        .expect("evaluate");
    assert_eq!(title["valueJson"], json!("\"gate-a\""));

    // --- session B: same broker, its own context ------------------------------
    let b = WireGuest::connect(gate_b.socket());
    let hello_b = b.hello();
    assert_eq!(hello_b["session"], json!("session-b"));
    assert_eq!(
        gate_b.stats().broker_spawns,
        0,
        "the second session must reuse the host's broker, not start another"
    );
    b.call(
        "view.create",
        json!({"view": 7, "width": 720, "height": 1280, "density": 2.0}),
    )
    .expect("view.create B");
    b.call(
        "view.loadData",
        json!({
            "view": 7,
            "mime": "text/html",
            "encoding": "utf8",
            "data": "<!doctype html><title>gate-b</title><body>session B</body>",
        }),
    )
    .expect("view.loadData B");
    let (meta_b, blob_b) = b.render(7);
    // 720 @2.0 = 360 CSS px (same rule as session A's 411).
    assert_eq!(meta_b["width"], json!(360));
    assert_png(&blob_b.data);
    // Sessions are isolated: each browser context holds only its own target.
    let mem = b.call("memory", json!({})).expect("memory");
    assert_eq!(
        mem["contexts"],
        json!(2),
        "one browser context per session: {mem}"
    );
    assert_eq!(mem["targets"], json!(2));

    // The broker is shared: one pid behind both sessions.
    let pids = (gate_a.spawned_broker_pid(), gate_b.spawned_broker_pid());
    assert!(pid_alive(pids.0));
    assert!(
        pids.1 == 0 || pids.1 == pids.0,
        "one broker per host, got pids {pids:?}"
    );

    // --- session end: sockets and directories go away -------------------------
    a.close();
    b.close();
    drop(gate_a);
    drop(gate_b);
    assert!(!root.join("a").exists(), "the session dir must be removed");
    assert!(!root.join("b").exists(), "the session dir must be removed");

    // --- idle stop: the broker exits when no session is left ------------------
    let broker_pid = pids.0;
    let deadline = Instant::now() + Duration::from_secs(30);
    while pid_alive(broker_pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(
        !pid_alive(broker_pid),
        "the shared broker must idle-stop (pid {broker_pid})"
    );
    assert!(
        !broker_socket.exists(),
        "an exited broker removes its endpoint"
    );
    // `ACOS_WV_KEEP=1` keeps the scratch (and the daemon's log) for evidence.
    if std::env::var_os("ACOS_WV_KEEP").is_none() {
        let _ = std::fs::remove_dir_all(&root);
    } else {
        println!("kept: {}", root.display());
    }
}
