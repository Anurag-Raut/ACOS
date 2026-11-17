//! GL1: a kept ART session must outlive the client connection that launched it,
//! and must not outlive the daemon.
//!
//! `PR_SET_PDEATHSIG` is per parent **thread**. `acos serve` created sessions on
//! the per-connection thread, so the guest was SIGKILLed (`signal 9`) the
//! instant the `acos launch --keep` client disconnected; the next
//! `acos session status/tap/observe` from another process answered
//! `GUEST_EXITED`. The engine now spawns every guest from one process-lifetime
//! thread (`engine/art.rs`, `spawner`), which keeps the death signal meaning
//! exactly "the `acos` process died".
//!
//! Both tests boot a real ART guest, so they are ignored by default and must run
//! under `tools/guarded.sh` (ADR-0018), through the host-wide ART lock:
//!
//! ```text
//! tools/art-runtime/run-exclusive.sh --task GL1 -- \
//!   tools/guarded.sh --mem 8G --time 300 -- \
//!   cargo test --release -p acos-cli --test gl1_kept_session -- --ignored --test-threads=1
//! ```
//!
//! Scratch lives under `ACOS_GL1_SCRATCH` (default `<repo>/.cache/gl1-tests`).
//! The daemon socket does not: a Unix socket path must fit `SUN_LEN` (108
//! bytes), which a worktree path does not, so it is bound in `ShortSocketDir`
//! (a short `$XDG_RUNTIME_DIR` directory, removed on drop).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
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

/// Scratch root for everything that is not a socket (runtime dir, cache dir,
/// daemon logs).
fn scratch_root() -> PathBuf {
    if let Some(dir) = std::env::var_os("ACOS_GL1_SCRATCH") {
        return PathBuf::from(dir);
    }
    repo_root().join(".cache").join("gl1-tests")
}

struct Scratch {
    dir: PathBuf,
    sockets: ShortSocketDir,
}

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let dir = scratch_root().join(format!("t-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("runtime")).unwrap();
        std::fs::create_dir_all(dir.join("cache")).unwrap();
        let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
        let sockets = ShortSocketDir::new(tag);
        Scratch { dir, sockets }
    }

    fn socket(&self) -> PathBuf {
        self.sockets.socket("s.sock")
    }

    fn log(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The daemon under test, with the socket pinned for both sides
/// (`ACOS_SESSION_SOCKET` is what every `acos` client resolves).
struct Daemon {
    child: Child,
    scratch: Scratch,
}

impl Daemon {
    fn start(tag: &str) -> Daemon {
        Daemon::start_with_env(tag, &[])
    }

    fn start_with_env(tag: &str, env: &[(&str, &str)]) -> Daemon {
        let scratch = Scratch::new(tag);
        let mut command = Command::new(acos());
        command
            .arg("serve")
            .arg("--socket")
            .arg(scratch.socket())
            .arg("--idle-timeout")
            .arg("300")
            .env("ACOS_RUNTIME_DIR", scratch.dir.join("runtime"))
            .env("ACOS_CACHE_DIR", scratch.dir.join("cache"))
            .env("ACOS_SESSION_SOCKET", scratch.socket())
            .envs(env.iter().copied())
            .stdout(Stdio::from(
                std::fs::File::create(scratch.log("daemon.json")).unwrap(),
            ))
            .stderr(Stdio::from(
                std::fs::File::create(scratch.log("daemon.log")).unwrap(),
            ));
        let child = command.spawn().expect("spawn acos serve");
        let deadline = Instant::now() + Duration::from_secs(20);
        while !scratch.socket().exists() {
            assert!(
                Instant::now() < deadline,
                "serve socket {} never appeared",
                scratch.socket().display()
            );
            std::thread::sleep(Duration::from_millis(25));
        }
        Daemon { child, scratch }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    /// One `acos` client, in its own process, talking to this daemon.
    fn client(&self, args: &[&str]) -> Output {
        Command::new(acos())
            .args(args)
            .env("ACOS_RUNTIME_DIR", self.scratch.dir.join("runtime"))
            .env("ACOS_CACHE_DIR", self.scratch.dir.join("cache"))
            .env("ACOS_SESSION_SOCKET", self.scratch.socket())
            .env("ACOS_NO_DAEMON", "0")
            .stdin(Stdio::null())
            .output()
            .expect("run an acos client")
    }

    /// One client call that must be a JSON envelope on stdout.
    fn call(&self, args: &[&str]) -> Value {
        let output = self.client(args);
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let envelope: Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
            panic!(
                "acos {args:?} did not answer a JSON envelope ({e}): stdout={stdout} stderr={}",
                String::from_utf8_lossy(&output.stderr)
            )
        });
        envelope
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn ok_calls(envelope: &Value) -> &Value {
    assert_eq!(envelope["ok"], Value::Bool(true), "{envelope:#?}");
    envelope["result"]
        .as_object()
        .map(|_| &envelope["result"])
        .expect("a passing envelope carries a result")
}

/// Locate the counter's `count` TextView in a `session observe --tree` reply.
fn counter_text(tree: &Value) -> Option<String> {
    fn walk(nodes: &Value, found: &mut Option<String>) {
        let Some(array) = nodes.as_array() else {
            return;
        };
        for node in array {
            let rid = node["resource_id"].as_str().unwrap_or("");
            if rid == "count" || rid.ends_with(":id/count") {
                *found = Some(node["text"].as_str().unwrap_or("").to_string());
            }
            walk(&node["children"], found);
        }
    }
    let mut found = None;
    walk(&tree["result"]["tree"]["nodes"], &mut found);
    found
}

/// Live (non-zombie) processes whose command line names `acos-art` and `dir`.
fn guest_processes(dir: &Path) -> Vec<u32> {
    let needle = dir.display().to_string();
    let mut pids = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return pids;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|n| n.parse::<u32>().ok()) else {
            continue;
        };
        let Ok(raw) = std::fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        let cmdline = String::from_utf8_lossy(&raw).replace('\0', " ");
        if !cmdline.contains("acos-art") || !cmdline.contains(&needle) {
            continue;
        }
        // A zombie is not executing code; it is reaped by init/the subreaper.
        let state = std::fs::read_to_string(entry.path().join("status"))
            .ok()
            .and_then(|status| {
                status
                    .lines()
                    .find(|line| line.starts_with("State:"))
                    .and_then(|line| line.split_whitespace().nth(1).map(str::to_string))
            });
        if state.as_deref() == Some("Z") {
            continue;
        }
        pids.push(pid);
    }
    pids.sort_unstable();
    pids
}

fn wait_until<F: FnMut() -> bool>(budget: Duration, mut check: F) -> bool {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if check() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    check()
}

fn counter_apk() -> Option<PathBuf> {
    let apk = repo_root().join("fixtures/build/apps/counter.apk");
    apk.is_file().then_some(apk)
}

/// GL1/1: `launch --keep` in one process, then status/tree/tap/screenshot in
/// other processes, then `destroy` leaves no guest behind.
#[test]
#[ignore = "boots a real ART guest; run under tools/guarded.sh"]
fn a_kept_session_survives_the_launching_client() {
    let Some(apk) = counter_apk() else {
        eprintln!("SKIP: fixtures/build/apps/counter.apk is missing");
        return;
    };
    let daemon = Daemon::start("kept");
    let apk_arg = apk.display().to_string();

    // Client A: the launch runs in its own process and *exits*, which closes the
    // connection that created the session. Before GL1 the guest died here.
    let launch = daemon.call(&["launch", &apk_arg, "--keep", "--json"]);
    let launched = ok_calls(&launch);
    assert_eq!(
        launched["engine"],
        Value::Bool(false).as_bool().map(|_| "art").unwrap()
    );
    assert_eq!(launched["engine"], "art", "{launch:#?}");

    // The guest must be alive right now, i.e. the launch's connection thread is
    // already gone and nothing was killed with it.
    assert!(
        !guest_processes(&daemon.scratch.dir).is_empty(),
        "the launched guest is not running after the launch client exited"
    );

    // Client B: another process, its own connection.
    let status = daemon.call(&["session", "status", "--json"]);
    if status["ok"] != Value::Bool(true) {
        assert_ne!(
            status["error"]["code"], "GUEST_EXITED",
            "the kept guest died with the launch connection (GL1): {status:#?}"
        );
    }
    let status = ok_calls(&status);
    assert_eq!(status["engine"], "art", "{status:#?}");
    assert_eq!(status["launched"], Value::Bool(true), "{status:#?}");

    let tree = daemon.call(&["session", "observe", "--tree", "--json"]);
    assert_eq!(
        tree["error"]["code"],
        Value::Null,
        "tree after the launch client exited must work: {tree:#?}"
    );
    ok_calls(&tree);
    assert_eq!(
        counter_text(&tree).as_deref(),
        Some("0"),
        "the fresh guest must show count 0: {tree:#?}"
    );

    let tap = daemon.call(&["session", "tap", "--id", "inc", "--json"]);
    ok_calls(&tap);

    let after = daemon.call(&["session", "observe", "--tree", "--json"]);
    ok_calls(&after);
    assert_eq!(
        counter_text(&after).as_deref(),
        Some("1"),
        "the tap from a second process must increment the counter: {after:#?}"
    );

    let shot = daemon.scratch.log("kept.png");
    let shot_arg = shot.display().to_string();
    let observe = daemon.call(&["session", "observe", "--screenshot", &shot_arg, "--json"]);
    ok_calls(&observe);
    let mut head = [0u8; 8];
    let mut file = std::fs::File::open(&shot).expect("the screenshot exists");
    file.read_exact(&mut head)
        .expect("the screenshot is readable");
    assert_eq!(&head[..4], &[0x89, b'P', b'N', b'G'], "not a PNG: {head:?}");
    let bytes = std::fs::metadata(&shot).unwrap().len();
    assert!(bytes > 1000, "PNG too small: {bytes} bytes");

    // Teardown: the guest is gone, and provably so (no leftover acos-art).
    let before = guest_processes(&daemon.scratch.dir);
    assert!(
        !before.is_empty(),
        "the guest should still be running: {before:?}"
    );
    let destroy = daemon.call(&["session", "destroy", "--json"]);
    let destroyed = ok_calls(&destroy);
    assert_eq!(destroyed["destroyed"], Value::Bool(true), "{destroy:#?}");
    assert!(
        wait_until(Duration::from_secs(5), || guest_processes(
            &daemon.scratch.dir
        )
        .is_empty()),
        "guest(s) survived `acos session destroy`: {:?}",
        guest_processes(&daemon.scratch.dir)
    );
}

/// GL1/2: `kill -9` the daemon with a live session; the guest must go with it
/// (that is the orphan-cleanup guarantee `PR_SET_PDEATHSIG` still provides).
#[test]
#[ignore = "boots a real ART guest; run under tools/guarded.sh"]
fn a_sigkilled_daemon_takes_its_guests_with_it() {
    let Some(apk) = counter_apk() else {
        eprintln!("SKIP: fixtures/build/apps/counter.apk is missing");
        return;
    };
    let mut daemon = Daemon::start("sigkill");
    let apk_arg = apk.display().to_string();
    let launch = daemon.call(&["launch", &apk_arg, "--keep", "--json"]);
    ok_calls(&launch);

    // The whole guest tree lives under the daemon's scratch (runtime dir).
    let guests = guest_processes(&daemon.scratch.dir);
    assert!(!guests.is_empty(), "no live guest to kill: {guests:?}");
    eprintln!("live guests before SIGKILL: {guests:?}");

    let pid = daemon.pid();
    // SIGKILL, exactly what a benchmark cleanup or a crashed harness does.
    unsafe {
        libc::kill(pid as i32, libc::SIGKILL);
    }
    let started = Instant::now();
    assert!(
        wait_until(Duration::from_secs(2), || guest_processes(
            &daemon.scratch.dir
        )
        .is_empty()),
        "guest(s) survived the daemon SIGKILL: {:?}",
        guest_processes(&daemon.scratch.dir)
    );
    eprintln!(
        "guests gone {} ms after the daemon SIGKILL",
        started.elapsed().as_millis()
    );
    let _ = daemon.child.wait();
    // Every guest pid the test saw must be gone (or a reaped zombie).
    for guest in guests {
        let alive = std::fs::read_to_string(format!("/proc/{guest}/status"))
            .ok()
            .and_then(|status| {
                status
                    .lines()
                    .find(|line| line.starts_with("State:"))
                    .and_then(|line| line.split_whitespace().nth(1).map(str::to_string))
            })
            .map(|state| state != "Z")
            .unwrap_or(false);
        assert!(
            !alive,
            "guest {guest} is still running after the daemon died"
        );
    }
}

// ---------------------------------------------------------------------------
// GL1/3: explicit destruction is bounded and provably complete (lifecycle vs
// kill). Android delivers the activity lifecycle and then SIGKILLs; it never
// joins an app's threads. A thread-heavy fixture (`thread-heavy.apk`: 12
// non-daemon threads that never exit) is the stand-in for a React Native app.
// ---------------------------------------------------------------------------

fn thread_heavy_apk() -> Option<PathBuf> {
    let apk = repo_root().join("fixtures/build/apps/thread-heavy.apk");
    apk.is_file().then_some(apk)
}

/// Launch, then destroy, returning the teardown report and the wall time.
fn launch_and_destroy(daemon: &Daemon, apk: &Path) -> (Value, Duration) {
    let apk_arg = apk.display().to_string();
    let launch = daemon.call(&["launch", &apk_arg, "--keep", "--json"]);
    let launched = ok_calls(&launch);
    assert_eq!(launched["engine"], "art", "{launch:#?}");
    assert!(
        !guest_processes(&daemon.scratch.dir).is_empty(),
        "the guest must be running before the destroy"
    );
    let started = Instant::now();
    let destroy = daemon.call(&["session", "destroy", "--json"]);
    let elapsed = started.elapsed();
    let destroyed = ok_calls(&destroy);
    assert_eq!(destroyed["destroyed"], Value::Bool(true), "{destroy:#?}");
    let teardown = destroyed["teardown"].clone();
    assert_eq!(teardown["verified"], Value::Bool(true), "{destroy:#?}");
    assert_eq!(teardown["survivors"], json!([]), "{destroy:#?}");
    assert_eq!(teardown["session_leftovers"], json!([]), "{destroy:#?}");
    (teardown, elapsed)
}

/// GL1/3a: counter and the thread-heavy fixture both tear down in < 1 s with no
/// leaked process, and the session is no longer listed.
#[test]
#[ignore = "boots a real ART guest; run under tools/guarded.sh"]
fn explicit_destruction_is_bounded_and_leaves_nothing_behind() {
    let Some(counter) = counter_apk() else {
        eprintln!("SKIP: fixtures/build/apps/counter.apk is missing");
        return;
    };
    let Some(thread_heavy) = thread_heavy_apk() else {
        eprintln!("SKIP: fixtures/build/apps/thread-heavy.apk is missing (tools/build-apps.sh)");
        return;
    };
    let daemon = Daemon::start("destroy");
    for apk in [&counter, &thread_heavy] {
        let (teardown, elapsed) = launch_and_destroy(&daemon, apk);
        eprintln!(
            "{}: destroy {} ms (reported teardown_ms {}, path {})",
            apk.file_name().unwrap().to_string_lossy(),
            elapsed.as_millis(),
            teardown["teardown_ms"],
            teardown["lifecycle"]
        );
        assert!(
            elapsed < Duration::from_secs(1),
            "explicit destruction of {} took {elapsed:?}",
            apk.display()
        );
        assert!(
            teardown["teardown_ms"].as_u64().unwrap() < 1000,
            "the engine's own teardown timer says {teardown:#?}"
        );
        assert!(
            matches!(
                teardown["lifecycle"].as_str(),
                Some("lifecycle") | Some("killed")
            ),
            "the destroy reply must name the path: {teardown:#?}"
        );
        // No process of that session may survive, and the id must be gone.
        assert!(
            wait_until(Duration::from_secs(2), || guest_processes(
                &daemon.scratch.dir
            )
            .is_empty()),
            "guest(s) survived the destroy: {:?}",
            guest_processes(&daemon.scratch.dir)
        );
        let listed = daemon.call(&["session", "list", "--json"]);
        assert_eq!(
            listed["result"]["count"].as_u64().unwrap_or(1),
            0,
            "the destroyed session must not be listed: {listed:#?}"
        );
    }
}

/// GL1/3b: when the app does not end by itself, destruction must not wait for
/// its threads — the grace expires, the tree is killed, and the reply says so.
#[test]
#[ignore = "boots a real ART guest; run under tools/guarded.sh"]
fn a_guest_that_never_ends_is_killed_after_the_bounded_grace() {
    let Some(thread_heavy) = thread_heavy_apk() else {
        eprintln!("SKIP: fixtures/build/apps/thread-heavy.apk is missing");
        return;
    };
    // A zero grace forces the kill path: no waiting on the app's threads.
    let daemon = Daemon::start_with_env("destroy-kill", &[("ACOS_ART_TEARDOWN_GRACE_MS", "0")]);
    let (teardown, elapsed) = launch_and_destroy(&daemon, &thread_heavy);
    eprintln!(
        "thread-heavy with a 0 ms grace: destroy {} ms (reported teardown_ms {}, path {})",
        elapsed.as_millis(),
        teardown["teardown_ms"],
        teardown["lifecycle"]
    );
    assert_eq!(teardown["lifecycle"], "killed", "{teardown:#?}");
    assert!(
        elapsed < Duration::from_secs(1),
        "the kill path must still be bounded: {elapsed:?}"
    );
    assert!(
        wait_until(Duration::from_secs(2), || guest_processes(
            &daemon.scratch.dir
        )
        .is_empty()),
        "guest(s) survived the kill path: {:?}",
        guest_processes(&daemon.scratch.dir)
    );
}
