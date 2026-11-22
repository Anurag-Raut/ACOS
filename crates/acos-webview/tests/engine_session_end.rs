//! WVKILL: ending one session must never kill the host's shared engine.
//!
//! Symptom (WVTAIL §4.3, WVSCOPE §5): when a session that used a WebView ends,
//! the broker logs `engine pid=… died (watcher restart): exit status signal: 9
//! (SIGKILL)` and every *other* live session loses its views.
//!
//! The test: two sessions on one real broker with the real engine, each with a
//! rendered view. Session A ends (guest closed, gate dropped, as a launch's
//! teardown does). Session B must still render, on the **same engine pid**, in
//! the **same generation**, with **no** restart.
//!
//! ```text
//! tools/art-runtime/run-exclusive.sh --task WVKILL -- tools/guarded.sh --mem 8G --time 420 -- \
//!   env ACOS_WEBVIEW_TEST=1 CARGO_TARGET_DIR=$PWD/.cache/wvkill-target \
//!   cargo test -p acos-webview --test engine_session_end -- --nocapture --test-threads=1
//! ```

mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use acos_webview::gate::{BrokerSource, Gate, GateConfig};
use common::{assert_png, WireGuest};
use serde_json::{json, Value};

fn scratch(tag: &str) -> PathBuf {
    let base = std::env::var_os("ACOS_VIEW_TEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join(format!("acos-wvkill-{}", std::process::id())));
    let dir = base.join(format!("k{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    dir
}

fn gate(dir: &Path, id: &str, broker_socket: &Path, bin: &Path) -> Gate {
    Gate::start(GateConfig {
        session_dir: dir.join(id),
        session_id: id.to_string(),
        broker_socket: broker_socket.to_path_buf(),
        broker: BrokerSource::Binary(bin.to_path_buf()),
        engine_cache: std::env::var_os("ACOS_WEBVIEW_CACHE").map(PathBuf::from),
        idle_shutdown: Some(Duration::from_secs(60)),
        log: Some(dir.join("broker.log")),
        spawn_wait: acos_webview::gate::SPAWN_WAIT,
    })
    .expect("start gate")
}

fn make_view(g: &WireGuest, view: i64, body: &str) {
    g.call(
        "view.create",
        json!({"view": view, "width": 720, "height": 1280, "density": 2.0}),
    )
    .expect("view.create");
    g.call(
        "view.loadData",
        json!({"view": view, "mime": "text/html", "encoding": "utf8",
               "data": format!("<!doctype html><title>{body}</title><body>{body}</body>")}),
    )
    .expect("view.loadData");
    let (meta, blob) = g.render(view);
    assert_eq!(meta["format"], json!("png"));
    assert_png(&blob.data);
}

fn status(g: &WireGuest) -> Value {
    g.call("engineStatus", json!({})).expect("engineStatus")
}

#[test]
fn ending_one_session_does_not_kill_the_shared_engine_of_the_other() {
    if acos_test_support::require_prereq(
        std::env::var("ACOS_WEBVIEW_TEST").as_deref() == Ok("1"),
        "ending_one_session_does_not_kill_the_shared_engine_of_the_other",
        "set ACOS_WEBVIEW_TEST=1 to run this heavy Chromium test (the release gate runs it with the env set; see tools/test-prereq-optouts.txt)",
    )
    .is_none()
    {
        return;
    }
    let root = scratch("end");
    let broker_socket = root.join("run").join("broker.sock");
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_acos-webview"));

    let gate_a = gate(&root, "session-a", &broker_socket, &bin);
    let gate_b = gate(&root, "session-b", &broker_socket, &bin);

    let a = WireGuest::connect(gate_a.socket());
    a.hello();
    make_view(&a, 1, "session A");
    let b = WireGuest::connect(gate_b.socket());
    b.hello();
    make_view(&b, 7, "session B");

    let before = status(&b);
    let pid = before["engine"]["pid"].as_u64().expect("engineStatus carries the engine pid");
    assert!(Path::new(&format!("/proc/{pid}")).exists(), "engine pid {pid} must be alive");
    assert_eq!(before["restarts"], json!(0));
    let generation = before["generation"].clone();
    assert_eq!(before["openViews"], json!(2));

    // Session A ends the way a launch's teardown ends it.
    a.close();
    drop(a);
    drop(gate_a);
    // The broker's watcher ticks about every 250 ms; the old bug killed the
    // engine right after the spawning session's connection ended. Watch for
    // long enough to see a death *and* its restart.
    let until = Instant::now() + Duration::from_secs(3);
    while Instant::now() < until {
        assert!(
            Path::new(&format!("/proc/{pid}")).exists(),
            "the shared engine (pid {pid}) died after session A ended"
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    // Session B still has its view and still gets pixels.
    let (meta, blob) = b.render(7);
    assert_eq!(meta["format"], json!("png"));
    assert_png(&blob.data);
    let after = status(&b);
    assert_eq!(after["engine"]["pid"].as_u64(), Some(pid), "same engine pid: {after}");
    assert_eq!(after["generation"], generation, "no new generation: {after}");
    assert_eq!(after["restarts"], json!(0), "no restart: {after}");
    assert_eq!(after["openViews"], json!(1), "A's view is closed, B's is not: {after}");

    // And a new session C can still join the same engine.
    let gate_c = gate(&root, "session-c", &broker_socket, &bin);
    let c = WireGuest::connect(gate_c.socket());
    c.hello();
    make_view(&c, 3, "session C");
    assert_eq!(status(&c)["engine"]["pid"].as_u64(), Some(pid));

    b.close();
    c.close();
    drop(gate_b);
    drop(gate_c);
    let _ = std::fs::remove_dir_all(&root);
}
