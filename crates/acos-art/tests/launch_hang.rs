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
//!
//! **Cold caches are setup, not the property** (LHFLAKE). The S6 property is
//! "once the guest is dead, the launch returns promptly", and the promptness
//! bound (`PROMPT_RETURN_BOUND`) is measured on a *warm* launch. On a cold
//! checkout the first launch in a run also pays for the launch-guest javac/d8
//! build (`build-cache STORE`, ~1.6 s), the guest seccomp/artshim clang builds
//! (~0.2 s) and the shared boot image (`dex2oat`, `prep_ms=3784`). Measured on
//! main 2026-11-14: the first run after a build took 4.3-11 s (which failed
//! this bound), every later run 0.25-0.29 s. Those costs are announced on
//! stderr (`build-cache MISS/STORE`, `prep_ms=`) and say nothing about how
//! long the launcher takes to notice a dead guest, so `warm_fixture` drives
//! the same fixture once, untimed, and the timed run that follows measures the
//! return only. The hard per-run deadline and the bound both still fail a real
//! S6 hang: `promptness_bound_still_bites` proves a >5 s return is rejected.

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

/// The warm `/data` used to seed a private copy (one rule for every ART test,
/// HYG2): `ACOS_ART_TEST_DATA`/`ACOS_ART_WARM_DATA`/a warm `ACOS_ART_DATA`, then
/// the checkout's productised cache — `run-exclusive.sh`'s per-invocation
/// `ACOS_ART_DATA` is empty, which is why the warm cache has its own name.
fn warm_data() -> PathBuf {
    acos_test_support::art_warm_data(&repo_root())
}

/// HYG2: a missing ART input fails the test by name (or is a counted skip under
/// `ACOS_ALLOW_ART_SKIP=1`), never a silent pass.
fn require_art() -> Option<()> {
    acos_test_support::require_art(&repo_root(), "launch hang test").map(|_| ())
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
    assert!(
        st.success(),
        "could not seed /data from {}",
        warm_data().display()
    );
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
    Run {
        status,
        stdout,
        stderr,
        elapsed: started.elapsed(),
        timed_out,
    }
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
    let Ok(rd) = std::fs::read_dir("/proc") else {
        return found;
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !name.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let path = e.path();
        let Ok(raw) = std::fs::read(path.join("cmdline")) else {
            continue;
        };
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
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path().display().to_string())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// The S6 property: after the guest has already ended, the launch must report
/// that outcome promptly instead of sitting on the peer thread. Applied to a
/// warm launch (see the module docs); `WARM_DEADLINE` still catches a hang
/// during cold prep, it just does not charge prep time to this bound.
const PROMPT_RETURN_BOUND: Duration = Duration::from_secs(5);

/// Warm-up runs only have to return; they are not timed. A genuinely hung
/// launcher still fails here, just against a deadline that leaves room for the
/// cold-cache work above.
const WARM_DEADLINE: Duration = Duration::from_secs(180);

/// `None` when the return was prompt enough, else the failure message.
///
/// Kept pure so `promptness_bound_still_bites` can prove that a slow return
/// still fails without the suite having to wait five seconds to find out.
fn prompt_bound_violation(elapsed: Duration) -> Option<String> {
    (elapsed > PROMPT_RETURN_BOUND).then(|| {
        format!(
            "launch took {elapsed:?} to report a guest that had already ended \
             (bound: {PROMPT_RETURN_BOUND:?})"
        )
    })
}

/// The cold-cache work a launch announces on stderr, quoted in a bound failure
/// so the next reader can tell "this was cold prep" from "the launcher waited"
/// without re-deriving it.
fn cold_cache_evidence(stderr: &str) -> String {
    let hits: Vec<&str> = stderr
        .lines()
        .filter(|l| {
            l.contains("build-cache MISS")
                || l.contains("build-cache STORE")
                || l.contains("prep_ms=")
                || l.contains("dex2oat")
        })
        .collect();
    if hits.is_empty() {
        "(no build-cache/prep lines on stderr: the launch was warm)".to_string()
    } else {
        hits.join("\n")
    }
}

/// Drive the same launch once as setup. Its whole job is to populate the
/// on-disk caches (build cache, shared boot image, page cache for the sysroot)
/// so the run that follows times the return and not the first launch on a cold
/// checkout. No timing assertion: only that the launcher returned at all.
///
/// The warm-up runs in its **own** seeded `/data` copy. That matters for the
/// `CC` fixture: the guest seccomp build's cache key covers the `CC` path and
/// the Landlock plan's contents, so warming in the timed run's own directory
/// would let the timed run reuse a guard `CC` never compiled — the fixture would
/// silently disarm itself instead of warming. A separate directory (with a
/// wrapper of its own) keeps the timed run's guard a genuine `CC` build while
/// the shared javac/d8, clang and boot-image caches still get warmed.
fn warm_fixture(tag: &str, timeout_secs: u64, envs: &[(&str, &str)], cc_deletes_plan: bool) {
    let data = seed_data(&format!("warm-{tag}"));
    let scratch = data.parent().expect("warm scratch root").to_path_buf();
    let out = scratch.join("out");
    let mut owned: Vec<(&str, String)> = envs.iter().map(|(k, v)| (*k, (*v).to_string())).collect();
    if cc_deletes_plan {
        let wrapper = scratch.join("cc-warm.sh");
        std::fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\nclang \"$@\" || exit $?\nrm -f '{}'\n",
                data.join("seccomp/landlock.plan").display()
            ),
        )
        .expect("write warm CC wrapper");
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755))
            .expect("chmod warm CC wrapper");
        owned.retain(|(k, _)| *k != "CC");
        owned.push(("CC", wrapper.to_string_lossy().to_string()));
    }
    // Mirror the timed run's re-arm: build the guard rather than reuse the
    // blob that came in with the seeded `/data`.
    let _ = std::fs::remove_file(data.join("seccomp/libacos_seccomp_guest.so"));
    let refs: Vec<(&str, &str)> = owned.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let run = run_launch(&data, &out, timeout_secs, &refs, WARM_DEADLINE);
    assert!(
        !run.timed_out,
        "the untimed warm-up launch did not return within {WARM_DEADLINE:?} — a launcher \
         that never returns is a hang even before the bound is applied.\nstdout:\n{}\nstderr:\n{}",
        run.stdout, run.stderr
    );
    // Diagnostic only (`--nocapture`): the cold-cache cost this setup absorbs.
    eprintln!(
        "LHFLAKE warm-up {tag}: {:?} (untimed, cold caches land here)",
        run.elapsed
    );
    let _ = std::fs::remove_dir_all(&scratch);
}

fn assert_clean_after(run: &Run, data: &Path) {
    // Diagnostic only (`--nocapture`): what the bound actually measured.
    eprintln!(
        "LHFLAKE measured return: {:?} (bound {PROMPT_RETURN_BOUND:?})",
        run.elapsed
    );
    assert!(
        !run.timed_out,
        "launch did not return within its test deadline ({:?}) — the S6 hang is back.\n\
         stdout:\n{}\nstderr:\n{}",
        run.elapsed, run.stdout, run.stderr
    );
    if let Some(why) = prompt_bound_violation(run.elapsed) {
        panic!(
            "{why}\n\
             the run was warmed first, so this is not cold-cache prep; the launcher itself \
             was slow to report a dead guest.\n\
             cold-cache lines (expected: none on a warmed run):\n{}\n\
             stdout:\n{}\nstderr:\n{}",
            cold_cache_evidence(&run.stderr),
            run.stdout,
            run.stderr
        );
    }
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
    //
    // LHFLAKE: the first launch in a cold run also builds the launch guest
    // (javac/d8) and the shared boot image, which took 4.3-11 s and is not what
    // this test is about. Warm those caches first (untimed, in a throwaway
    // scratch copy), then time this run.
    let _ = std::fs::remove_file(data.join("seccomp/libacos_seccomp_guest.so"));
    warm_fixture("early-exit", 10, &[], true);

    let run = run_launch(
        &data,
        &out,
        10,
        &[("CC", wrapper.to_str().unwrap())],
        Duration::from_secs(20),
    );
    assert_clean_after(&run, &data);
    assert_eq!(
        run.status.and_then(|s| s.code()),
        Some(1),
        "launch must fail, not fake ok"
    );
    let j = result_json(&run);
    assert_eq!(j["outcome"], "crash", "{j}");
    assert_eq!(
        j["code"], "GUEST_EXITED",
        "a dead guest needs its own outcome: {j}"
    );
    assert_eq!(
        j["guest_exit"], 126,
        "the guard's fail-closed exit code is the guest's: {j}"
    );
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
    warm_fixture("signal", 10, &[("ACOS_ART_SECCOMP_AUDIT", "kill")], false);
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
    assert_eq!(
        j["guest_signal"],
        libc::SIGSYS,
        "whole-list KILL dies of SIGSYS: {j}"
    );
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
    warm_fixture("timeout", 0, &[], false);
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
    warm_fixture("normal", 30, &[], false);
    let run = run_launch(&data, &out, 30, &[], Duration::from_secs(30));
    assert!(
        !run.timed_out,
        "ordinary launch hung:\n{}\n{}",
        run.stdout, run.stderr
    );
    assert_eq!(run.status.and_then(|s| s.code()), Some(0), "{}", run.stderr);
    let j = result_json(&run);
    assert_eq!(j["outcome"], "ok", "{j}");
    assert_eq!(j["application_created"], true, "{j}");
    assert_eq!(j["guest_exit"], 0, "{j}");
    assert!(j["guest_signal"].is_null(), "{j}");
    let wall = j["wall_ms"].as_u64().expect("wall_ms");
    assert!(
        wall > 0 && wall <= 2000,
        "ordinary launch wall was {wall} ms (142 ms class)"
    );
    assert_clean_after(&run, &data);
}

// ---------------------------------------------------------------------------
// 5. The bound itself still bites (sanity check on the timing helper)
// ---------------------------------------------------------------------------

/// LHFLAKE guard: the cold/warm split above must not have turned the 5 s bound
/// into a no-op. A return past the bound — including the 11 s a *cold* first run
/// measured before this change — is still a failure, so a real S6 hang (return
/// only after the outer deadline) cannot pass silently.
#[test]
fn promptness_bound_still_bites() {
    for ok in [
        Duration::ZERO,
        Duration::from_millis(229),
        Duration::from_millis(4290),
    ] {
        assert!(
            prompt_bound_violation(ok).is_none(),
            "a {ok:?} return is inside the {PROMPT_RETURN_BOUND:?} bound"
        );
    }
    let just_over = PROMPT_RETURN_BOUND + Duration::from_millis(1);
    assert!(
        prompt_bound_violation(just_over).is_some(),
        "a return just past the bound must fail"
    );
    for late in [just_over, Duration::from_secs(11), Duration::from_secs(20)] {
        let why = prompt_bound_violation(late).expect("a late return must fail");
        assert!(
            why.contains("already ended"),
            "the failure must name the property: {why}"
        );
    }
    // The failure message quotes cold-cache evidence when there is any, so a
    // regressed bound is not mistaken for a slow host.
    assert!(
        cold_cache_evidence("acos-art: build-cache MISS abc built in 84 ms")
            .contains("build-cache MISS")
    );
    assert!(
        cold_cache_evidence("acos-art: launch: boot_image: prep_ms=3784").contains("prep_ms=3784")
    );
    assert!(cold_cache_evidence("acos-art: launch wall=24ms").contains("warm"));
}
