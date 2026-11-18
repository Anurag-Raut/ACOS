//! WVSCOPE decision 1: the host's WebView broker must not die with the session
//! that happened to spawn it.
//!
//! WVTAIL §4.3 found the mechanism: the daemon was `setsid`-detached, which
//! changes the session but **not the cgroup**, so a run harness that tears its
//! session scope down (a scope-wide SIGKILL, which `guarded.sh` and every run
//! script do) killed the shared engine for *every* session of the host — an
//! unanswered `render` and `NO_VIEW` for the rest of another agent's run.
//!
//! The test runs both arms of the same mechanism, with the real engine:
//!
//! 1. a **child** process runs a gate inside its own transient scope
//!    (`systemd-run --user --scope --unit=acos-wvscope-<pid>`), connects a guest
//!    and creates a real WebView — the first connect spawns the shared broker;
//! 2. the parent SIGKILLs the child's scope's cgroup (the child dies);
//! 3. **scope arm** (the fix): the broker lands in a transient scope of its own,
//!    survives, and a second session gets a real PNG frame from *that* broker
//!    (no spawn, no `engine.restarted`);
//!    **fallback arm** (`ACOS_WEBVIEW_SCOPE=0`, today's behaviour): the broker is
//!    in the session's cgroup, dies with it, and the second session has to start
//!    a new engine (`broker_spawns == 1`) — the bug reproduced, so the test fails
//!    if the scope is ever removed.
//!
//! Opt-in (`ACOS_WEBVIEW_TEST=1`, it starts Chromium) and skipped, loudly, where
//! `systemd --user` is not usable:
//!
//! ```text
//! tools/art-runtime/run-exclusive.sh --task WVSCOPE -- tools/guarded.sh --mem 8G --time 420 -- \
//!   env ACOS_WEBVIEW_TEST=1 CARGO_TARGET_DIR=$PWD/.cache/wvscope-target \
//!   cargo test -p acos-webview --test broker_scope -- --nocapture --test-threads=1
//! ```

#[allow(dead_code)]
mod common;

use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use acos_webview::gate::{BrokerSource, Gate, GateConfig, SpawnScope};
use common::{assert_png, WireGuest};
use serde_json::{json, Value};

/// Set in the in-scope child: the test function then *is* the child.
const CHILD_ENV: &str = "ACOS_WV_SCOPE_CHILD";
/// Root directory the child reports into.
const ROOT_ENV: &str = "ACOS_WV_SCOPE_ROOT";
/// `scope` (default) or `fallback` — which spawn path the child's gate uses.
const ARM_ENV: &str = "ACOS_WV_SCOPE_ARM";

fn engine_test_enabled() -> bool {
    std::env::var("ACOS_WEBVIEW_TEST").as_deref() == Ok("1")
}

fn scratch(tag: &str) -> PathBuf {
    let base = std::env::var_os("ACOS_VIEW_TEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join(format!("acos-wvscope-{}", std::process::id())));
    let dir = base.join(format!("c{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    dir
}

/// The `0::` line of `/proc/<pid>/cgroup` — where systemd put the process.
fn read_cgroup(pid: u32) -> String {
    let text = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).unwrap_or_default();
    text.lines()
        .find(|l| l.starts_with("0::"))
        .unwrap_or("")
        .trim_start_matches("0::")
        .to_string()
}

fn pid_alive(pid: u32) -> bool {
    pid != 0 && Path::new(&format!("/proc/{pid}")).exists()
}

fn kill(pid: u32) {
    if pid == 0 {
        return;
    }
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    // SAFETY: a pid this test started (a broker of this build).
    unsafe {
        let _ = kill(pid as i32, 15);
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while pid_alive(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    if pid_alive(pid) {
        // SAFETY: as above, after SIGTERM did not do it.
        unsafe {
            let _ = kill(pid as i32, 9);
        }
    }
}

fn gate_config(session_dir: &Path, broker_socket: &Path, log: &Path) -> GateConfig {
    GateConfig {
        session_dir: session_dir.to_path_buf(),
        session_id: session_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "s".into()),
        broker_socket: broker_socket.to_path_buf(),
        broker: BrokerSource::Binary(PathBuf::from(env!("CARGO_BIN_EXE_acos-webview"))),
        engine_cache: std::env::var_os("ACOS_WEBVIEW_CACHE").map(PathBuf::from),
        idle_shutdown: Some(Duration::from_secs(300)),
        log: Some(log.to_path_buf()),
        spawn_wait: acos_webview::gate::SPAWN_WAIT,
    }
}

struct Arm {
    root: PathBuf,
    unit: String,
    broker_pid: u32,
    broker_cgroup: String,
    child_cgroup: String,
    child: Child,
}

/// Run the in-scope half of the test in a child process of its own, and kill that
/// child's scope's cgroup exactly like a run harness tears its session down.
fn run_child_arm(tag: &str, arm: &str) -> Arm {
    let root = scratch(tag);
    let unit = format!("acos-wvscope-{}-{}", std::process::id(), arm);
    let child = Command::new("systemd-run")
        .args([
            "--user",
            "--scope",
            "--quiet",
            "--collect",
            &format!("--unit={unit}"),
            "--",
        ])
        .arg(std::env::current_exe().expect("test binary"))
        .args([
            "--exact",
            "a_broker_in_its_own_scope_outlives_the_session_scope_that_spawned_it",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_ENV, "1")
        .env(ARM_ENV, arm)
        .env(ROOT_ENV, &root)
        .stdin(Stdio::null())
        .spawn()
        .expect("spawn the in-scope child");

    let status_file = root.join("child.json");
    let deadline = Instant::now() + Duration::from_secs(180);
    let mut child = child;
    while !status_file.exists() && Instant::now() < deadline {
        if let Ok(Some(status)) = child.try_wait() {
            panic!("the in-scope child died before reporting ({status}); see its output above");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let report: Value =
        serde_json::from_str(&std::fs::read_to_string(&status_file).expect("the child reported"))
            .expect("child report is JSON");
    Arm {
        root,
        unit,
        broker_pid: report["broker_pid"].as_u64().unwrap_or(0) as u32,
        broker_cgroup: report["broker_cgroup"].as_str().unwrap_or("").to_string(),
        child_cgroup: report["child_cgroup"].as_str().unwrap_or("").to_string(),
        child,
    }
}

impl Arm {
    fn kill_scope(&mut self) {
        let status = Command::new("systemctl")
            .args([
                "--user",
                "kill",
                "--signal=SIGKILL",
                &format!("{}.scope", self.unit),
            ])
            .status()
            .expect("systemctl --user kill");
        assert!(status.success(), "the session scope must be killed");
        let status = self.child.wait().expect("wait for the in-scope child");
        assert!(
            status.signal() == Some(9) || !status.success(),
            "the session scope's process must have died with it: {status:?}"
        );
    }

    fn second_session(&self) -> (Gate, WireGuest, Value) {
        let dir = self.root.join("b");
        let gate = Gate::start(gate_config(
            &dir,
            &self.root.join("run/broker.sock"),
            &self.root.join("broker.log"),
        ))
        .expect("second gate");
        let guest = WireGuest::connect(gate.socket());
        let hello = guest.hello();
        let spawns = gate.stats().broker_spawns;
        (gate, guest, json!({"hello": hello, "spawns": spawns}))
    }
}

#[test]
fn a_broker_in_its_own_scope_outlives_the_session_scope_that_spawned_it() {
    if std::env::var_os(CHILD_ENV).is_some() {
        // The in-scope half: spawn the broker, use a WebView, report, and stay
        // alive until the parent kills this scope.
        child_main();
        return;
    }
    if !engine_test_enabled() {
        println!(
            "SKIP broker_scope: set ACOS_WEBVIEW_TEST=1 (heavy: starts a Chromium) to run it \
             under tools/art-runtime/run-exclusive.sh"
        );
        return;
    }
    if acos_webview::gate::spawn_scope() != SpawnScope::TransientScope {
        println!(
            "SKIP broker_scope: systemd --user is not usable here, so the broker has no scope of \
             its own to survive in (that is the documented fallback, not a pass)"
        );
        return;
    }

    // --- arm 1, the fix: the broker has its own scope and survives ------------
    let mut arm = run_child_arm("scope", "scope");
    assert!(
        arm.child_cgroup.contains(&format!("{}.scope", arm.unit)),
        "the child must run in its session scope: {}",
        arm.child_cgroup
    );
    assert!(
        !arm.broker_cgroup.contains(&format!("{}.scope", arm.unit)),
        "the broker must not share the session's cgroup (that is the bug): {}",
        arm.broker_cgroup
    );
    assert!(
        arm.broker_cgroup
            .rsplit('/')
            .next()
            .unwrap_or("")
            .starts_with("acos-webview-"),
        "the broker must run in its own acos-webview-* scope: {}",
        arm.broker_cgroup
    );
    assert_ne!(arm.child_cgroup, arm.broker_cgroup);
    assert!(pid_alive(arm.broker_pid), "the broker is running");
    arm.kill_scope();
    assert!(
        pid_alive(arm.broker_pid),
        "the broker must survive the session scope that spawned it (pid {}, cgroup {})",
        arm.broker_pid,
        arm.broker_cgroup
    );
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        pid_alive(arm.broker_pid),
        "the broker must stay up after the teardown settles"
    );

    // … and a second session keeps working: same broker (no spawn), real frame.
    let (gate, guest, facts) = arm.second_session();
    assert_eq!(facts["hello"]["proto"], json!(2), "{facts}");
    assert_eq!(
        facts["spawns"],
        json!(0),
        "the second session must reuse the surviving broker: {facts} {:?}",
        gate.stats()
    );
    let created = guest
        .call(
            "view.create",
            json!({"view": 1, "width": 540, "height": 960, "density": 2.0}),
        )
        .expect("view.create on the surviving broker");
    assert_eq!(created["view"], json!(1));
    guest
        .call(
            "view.loadData",
            json!({
                "view": 1,
                "mime": "text/html",
                "encoding": "utf8",
                "data": "<!doctype html><title>after the teardown</title>\
                         <body style=\"background:#0a0\"><h1>still here</h1>",
            }),
        )
        .expect("view.loadData");
    let (_meta, blob) = guest.render(1);
    assert_png(&blob.data);
    assert!(blob.width > 0 && blob.height > 0, "{blob:?}");
    let seen = guest.seen();
    assert!(
        !seen
            .iter()
            .any(|(_, m)| m["event"] == json!("engine.restarted")),
        "the surviving broker must not have restarted: {seen:?}"
    );
    guest.close();
    drop(gate);
    kill(arm.broker_pid);

    // --- arm 2, the before: `setsid` only, and the broker dies with the scope --
    let mut before = run_child_arm("fallback", "fallback");
    assert!(
        before
            .broker_cgroup
            .contains(&format!("{}.scope", before.unit)),
        "the fallback arm must reproduce today's behaviour: the broker is in the \
         session's own cgroup ({})",
        before.broker_cgroup
    );
    before.kill_scope();
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        !pid_alive(before.broker_pid),
        "with ACOS_WEBVIEW_SCOPE=0 (today's behaviour) the broker must die with the session \
         scope — if this ever stops being true, the test no longer proves anything"
    );
    // The second session still works, but on a *new* engine: every view of the
    // killed session is gone, which is exactly the other agent's `NO_VIEW`.
    let (gate, guest, facts) = before.second_session();
    assert_eq!(
        facts["spawns"],
        json!(1),
        "the fallback arm replaces the engine: {facts} {:?}",
        gate.stats()
    );
    guest.close();
    let replacement = gate.spawned_broker_pid();
    drop(gate);
    kill(replacement);
    let _ = std::fs::remove_dir_all(&arm.root);
    let _ = std::fs::remove_dir_all(&before.root);
}

/// The in-scope half of the test: a session that creates a WebView, writes what
/// it learned about the process tree, and then waits to be killed with its scope.
fn child_main() {
    let root = PathBuf::from(std::env::var(ROOT_ENV).expect("root"));
    if std::env::var(ARM_ENV).as_deref() == Ok("fallback") {
        // The documented fallback = the behaviour before WVSCOPE.
        std::env::set_var(acos_webview::gate::SPAWN_SCOPE_ENV, "0");
    }
    let session = root.join("a");
    let gate = Gate::start(gate_config(
        &session,
        &root.join("run/broker.sock"),
        &root.join("broker.log"),
    ))
    .expect("child gate");

    let guest = WireGuest::connect(gate.socket());
    let hello = guest.hello();
    assert_eq!(hello["proto"], json!(2));
    guest
        .call(
            "view.create",
            json!({"view": 1, "width": 540, "height": 960, "density": 2.0}),
        )
        .expect("the in-scope session must get a view");
    guest
        .call(
            "view.loadData",
            json!({
                "view": 1,
                "mime": "text/html",
                "encoding": "utf8",
                "data": "<!doctype html><title>in scope</title>\
                         <body style=\"background:#00a\"><h1>in scope</h1>",
            }),
        )
        .expect("the in-scope session must load a page");
    let (_meta, blob) = guest.render(1);
    assert_png(&blob.data);

    let broker_pid = gate.spawned_broker_pid();
    assert!(broker_pid != 0, "the first connect must spawn the broker");
    let report = json!({
        "broker_pid": broker_pid,
        "broker_cgroup": read_cgroup(broker_pid),
        "child_cgroup": read_cgroup(std::process::id()),
        "session_socket": gate.socket().display().to_string(),
        "frame_bytes": blob.data.len(),
    });
    std::fs::write(root.join("child.json"), report.to_string()).expect("child report");
    // Hold the gate (and so the session scope) open until the parent kills it.
    loop {
        std::thread::sleep(Duration::from_millis(250));
    }
}
