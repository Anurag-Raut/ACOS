//! `acos-art launch` must return when the guest dies before its milestone.
//!
//! S6 found the hang with two independent triggers: a missing Landlock plan
//! (the guest's guard exits 126 before it ever connects to the peer) and the
//! whole-list KILL seccomp audit (SIGSYS at ART's first denied syscall). In
//! both cases the launcher blocked forever on the peer thread and left its
//! per-run root behind; the outer guard reported rc 124, not a guest outcome.
//!
//! These tests drive the shipped binary as a black box. The hang is visible
//! from outside as "the process did not exit", so every run has its own hard
//! deadline and is killed (process group) if it exceeds it. The assertions are
//! on the JSON outcome (`guest_exit` / `guest_signal` / `code`), the run-root
//! janitor view (`<data>/runs` empty) and `/proc` (no process still names the
//! run's data tree). One mutex serialises ART guests, as everywhere in this
//! crate's tests.

use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

static ART_LOCK: Mutex<()> = Mutex::new(());

fn art_guard() -> MutexGuard<'static, ()> {
    ART_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the crate lives at <repo>/crates/acos-art")
}

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_acos-art")
}

fn sysroot() -> PathBuf {
    std::env::var_os("ACOS_ART_SYSROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_root().join(".cache/art/sysroot"))
}

/// The warm `/data` used to seed a private copy. Under `run-exclusive.sh`
/// `ACOS_ART_DATA` is an empty per-invocation dir, so prefer an explicitly
/// seeded dir, then a warm `ACOS_ART_DATA`, then the productised cache.
fn warm_data() -> PathBuf {
    if let Some(p) = std::env::var_os("ACOS_ART_TEST_DATA") {
        return PathBuf::from(p);
    }
    let from_env = std::env::var_os("ACOS_ART_DATA").map(PathBuf::from);
    if let Some(p) = from_env.filter(|p| p.join("app/guest/classes.dex").is_file()) {
        return p;
    }
    repo_root().join(".cache/art/data")
}

fn require_art() -> Option<()> {
    if !sysroot().join("system/apex").is_dir() {
        eprintln!(
            "SKIP launch hang test: no ART sysroot at {} — run tools/art-sysroot.sh",
            sysroot().display()
        );
        return None;
    }
    if !warm_data().join("app/guest/classes.dex").is_file() {
        eprintln!(
            "SKIP launch hang test: no cached guest dex under {} — run \
             `acos-art render-app --apk fixtures/build/apps/counter.apk` once",
            warm_data().display()
        );
        return None;
    }
    Some(())
}

fn apk() -> PathBuf {
    repo_root().join("fixtures/build/apps/counter.apk")
}

fn seed_data(tag: &str) -> PathBuf {
    // Short on purpose: the guest's importer socket lives under
    // `<data>/runs/<id>/sock/`, and `sun_path` is 108 bytes. A deep test name
    // must not make the launcher fail with "path must be shorter than SUN_LEN".
    let short = format!(
        "{}{:03}",
        tag.chars().next().unwrap_or('x'),
        std::process::id() % 1000
    );
    let scratch = repo_root().join(".cache").join(format!("l{short}"));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).expect("create test scratch under .cache/");
    let data = scratch.join("data");
    let st = Command::new("cp")
        .arg("-a")
        .arg(warm_data())
        .arg(&data)
        .status()
        .expect("cp is available");
    assert!(st.success(), "could not seed /data from {}", warm_data().display());
    // The test measures *this* run's cleanup; start from an empty run root.
    let runs = data.join("runs");
    let _ = std::fs::remove_dir_all(&runs);
    std::fs::create_dir_all(&runs).expect("create runs dir");
    data
}

struct Run {
    status: Option<std::process::ExitStatus>,
    stdout: String,
    stderr: String,
    elapsed: Duration,
    timed_out: bool,
}

/// Run the binary with a hard wall-clock bound; kill its process group if the
/// bound is crossed (the very regression this suite exists for).
fn run_launch(
    data: &Path,
    out: &Path,
    timeout_secs: u64,
    envs: &[(&str, &str)],
    deadline: Duration,
) -> Run {
    let mut cmd = Command::new(bin());
    cmd.arg("launch")
        .arg("--data")
        .arg(data)
        .arg("--apk")
        .arg(apk())
        .arg("--until")
        .arg("application-created")
        .arg("--out")
        .arg(out)
        .arg("--timeout")
        .arg(timeout_secs.to_string())
        .env("ACOS_ART_QUIET_GUARD", "1")
        .env_remove("ACOS_ART_SECCOMP_AUDIT")
        .env_remove("ACOS_ART_KEEP_RUN")
        .env_remove("CC")
        .env_remove("ACOSART_PROBE");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let started = Instant::now();
    let mut child = cmd.spawn().expect("spawn acos-art launch");
    let pid = child.id() as i32;
    let mut stdout_pipe = child.stdout.take().expect("piped stdout");
    let mut stderr_pipe = child.stderr.take().expect("piped stderr");
    let out_thread = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = stdout_pipe.read_to_end(&mut v);
        v
    });
    let err_thread = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = stderr_pipe.read_to_end(&mut v);
        v
    });
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) => {}
            Err(e) => panic!("try_wait on launch: {e}"),
        }
        if started.elapsed() >= deadline {
            timed_out = true;
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
                libc::kill(pid, libc::SIGKILL);
            }
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let stdout = String::from_utf8_lossy(&out_thread.join().unwrap()).to_string();
    let stderr = String::from_utf8_lossy(&err_thread.join().unwrap()).to_string();
    Run { status, stdout, stderr, elapsed: started.elapsed(), timed_out }
}

fn result_json(run: &Run) -> serde_json::Value {
    let start = run
        .stdout
        .find('{')
        .unwrap_or_else(|| panic!("launch printed no JSON.\nstderr:\n{}", run.stderr));
    let end = run
        .stdout
        .rfind('}')
        .unwrap_or_else(|| panic!("launch JSON is not closed.\nstdout:\n{}", run.stdout));
    serde_json::from_str(&run.stdout[start..=end])
        .unwrap_or_else(|e| panic!("bad launch JSON: {e}\n{}", &run.stdout[start..=end]))
}

fn matching_processes(needle: &str) -> Vec<String> {
    let mut found = Vec::new();
    let Ok(rd) = std::fs::read_dir("/proc") else { return found };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !name.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let path = e.path();
        let Ok(raw) = std::fs::read(path.join("cmdline")) else { continue };
        let cmd = raw
            .iter()
            .map(|&b| if b == 0 { ' ' } else { b as char })
            .collect::<String>();
        if cmd.contains(needle) {
            let comm = std::fs::read_to_string(path.join("comm")).unwrap_or_default();
            found.push(format!("pid={name} comm={} cmd={cmd}", comm.trim()));
        }
    }
    found
}

fn wait_until_no_processes(data: &Path, within: Duration) -> Vec<String> {
    let needle = data.to_string_lossy().to_string();
    let deadline = Instant::now() + within;
    loop {
        let survivors = matching_processes(&needle);
        if survivors.is_empty() || Instant::now() >= deadline {
            return survivors;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn run_roots(data: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(data.join("runs"))
        .map(|rd| rd.flatten().map(|e| e.path().display().to_string()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

fn assert_clean_after(run: &Run, data: &Path) {
    assert!(
        !run.timed_out,
        "launch did not return within its test deadline ({:?}) — the S6 hang is back.\n\
         stdout:\n{}\nstderr:\n{}",
        run.elapsed,
        run.stdout,
        run.stderr
    );
    assert!(
        run.elapsed <= Duration::from_secs(5),
        "launch took {:?} to report a guest that had already ended (bound: 5 s)",
        run.elapsed
    );
    let roots = run_roots(data);
    assert!(roots.is_empty(), "launch left per-run root(s): {roots:?}");
    let survivors = wait_until_no_processes(data, Duration::from_secs(3));
    assert!(
        survivors.is_empty(),
        "processes still name the finished run's data tree after 3 s: {survivors:#?}"
    );
    // Green path: remove the per-test scratch copy (a failure keeps it).
    if let Some(scratch) = data.parent() {
        let _ = std::fs::remove_dir_all(scratch);
    }
}

// ---------------------------------------------------------------------------
// 1. Guest exits 126 before it ever connects: missing Landlock plan (S6)
// ---------------------------------------------------------------------------

#[test]
fn guest_exits_before_milestone_returns_exit_code_and_cleans_up() {
    let _serial = art_guard();
    let Some(()) = require_art() else { return };
    let data = seed_data("early-exit");
    let out = data.parent().unwrap().join("out");
    // A CC wrapper that deletes the plan *after* the guard blob is compiled is
    // a faithful reproduction of S6's missing plan: the launcher wrote it, the
    // guest's constructor finds no plan and exits 126 before connecting.
    let wrapper = data.parent().unwrap().join("cc-delete-plan.sh");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nclang \"$@\" || exit $?\nrm -f '{}'\n",
            data.join("seccomp/landlock.plan").display()
        ),
    )
    .expect("write CC wrapper");
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755))
        .expect("chmod CC wrapper");
    // Force build_guest to invoke CC instead of reusing the warm blob.
    let _ = std::fs::remove_file(data.join("seccomp/libacos_seccomp_guest.so"));

    let run = run_launch(
        &data,
        &out,
        10,
        &[("CC", wrapper.to_str().unwrap())],
        Duration::from_secs(20),
    );
    assert_clean_after(&run, &data);
    assert_eq!(run.status.and_then(|s| s.code()), Some(1), "launch must fail, not fake ok");
    let j = result_json(&run);
    assert_eq!(j["outcome"], "crash", "{j}");
    assert_eq!(j["code"], "GUEST_EXITED", "a dead guest needs its own outcome: {j}");
    assert_eq!(j["guest_exit"], 126, "the guard's fail-closed exit code is the guest's: {j}");
    assert!(j["guest_signal"].is_null(), "{j}");
    assert_eq!(j["application_created"], false, "{j}");
    let notes = j["notes"].to_string();
    assert!(
        notes.contains("landlock=plan-missing"),
        "the fail-closed reason must be in the structured notes: {notes}"
    );
}

// ---------------------------------------------------------------------------
// 2. Guest killed by a signal: whole-list KILL seccomp audit (S6)
// ---------------------------------------------------------------------------

#[test]
fn guest_killed_by_signal_returns_signal_and_cleans_up() {
    let _serial = art_guard();
    let Some(()) = require_art() else { return };
    let data = seed_data("signal");
    let out = data.parent().unwrap().join("out");
    let run = run_launch(
        &data,
        &out,
        10,
        &[("ACOS_ART_SECCOMP_AUDIT", "kill")],
        Duration::from_secs(20),
    );
    // R3 retention: a tree is kept only for a crash the guest raised itself
    // *and* whose crash record was parsed. This death is SIGSYS from the seccomp
    // audit filter, which the guest's handler does not cover and for which it
    // prints no `ACOSCRASH` line, so there is nothing to keep: S4's cleanup
    // guarantee stands unchanged here (a host kill and a signal death without
    // evidence both remove the tree).
    assert_clean_after(&run, &data);
    assert_eq!(run.status.and_then(|s| s.code()), Some(1));
    let j = result_json(&run);
    assert_eq!(j["outcome"], "crash", "{j}");
    assert_eq!(
        j["code"], "GUEST_KILLED_BY_SIGNAL",
        "a signal death must not be folded into a milestone code: {j}"
    );
    assert_eq!(j["guest_signal"], libc::SIGSYS, "whole-list KILL dies of SIGSYS: {j}");
    assert!(j["guest_exit"].is_null(), "{j}");
    assert!(
        !j["stderr"].as_str().unwrap_or_default().is_empty(),
        "the structured result carries the guest stderr tail: {j}"
    );
}

// ---------------------------------------------------------------------------
// 3. Milestone timeout is its own outcome (and does not hang)
// ---------------------------------------------------------------------------

#[test]
fn milestone_timeout_returns_timeout_and_cleans_up() {
    let _serial = art_guard();
    let Some(()) = require_art() else { return };
    let data = seed_data("timeout");
    let out = data.parent().unwrap().join("out");
    // Zero budget: the guest is killed in the wait loop before it can reach
    // `application-created`. That is exactly the "milestone timeout" branch.
    let run = run_launch(&data, &out, 0, &[], Duration::from_secs(20));
    assert_clean_after(&run, &data);
    assert_eq!(run.status.and_then(|s| s.code()), Some(1));
    let j = result_json(&run);
    assert_eq!(j["outcome"], "timeout", "{j}");
    assert_eq!(j["code"], "TIMEOUT", "{j}");
    assert!(j["guest_exit"].is_null(), "{j}");
    assert!(j["guest_signal"].is_null(), "{j}");
    assert_eq!(j["application_created"], false, "{j}");
}

// ---------------------------------------------------------------------------
// 4. The ordinary path is unchanged and stays in the 142 ms class
// ---------------------------------------------------------------------------

#[test]
fn ordinary_launch_to_application_created_stays_fast() {
    let _serial = art_guard();
    let Some(()) = require_art() else { return };
    let data = seed_data("normal");
    let out = data.parent().unwrap().join("out");
    let run = run_launch(&data, &out, 30, &[], Duration::from_secs(30));
    assert!(!run.timed_out, "ordinary launch hung:\n{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.status.and_then(|s| s.code()), Some(0), "{}", run.stderr);
    let j = result_json(&run);
    assert_eq!(j["outcome"], "ok", "{j}");
    assert_eq!(j["application_created"], true, "{j}");
    assert_eq!(j["guest_exit"], 0, "{j}");
    assert!(j["guest_signal"].is_null(), "{j}");
    let wall = j["wall_ms"].as_u64().expect("wall_ms");
    assert!(wall > 0 && wall <= 2000, "ordinary launch wall was {wall} ms (142 ms class)");
    assert_clean_after(&run, &data);
}
