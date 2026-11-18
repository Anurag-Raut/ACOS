//! R3: a guest that dies of a native signal must leave a diagnosable record.
//!
//! The defect this pins down: `results/render/R2.md` §2 lost 1 in 10 Gaten
//! launches to `SIGSEGV` and had nothing to read — no fault address, no signal
//! code, no faulting thread, no stack, and the run's own directory removed by
//! teardown. The guest had no crash handler at all (this image's
//! `ld-android.so` links bionic's debuggerd *stub*, not the fallback handler,
//! and ART only installs its own unexpected-signal dumper when `ANDROID_ROOT`
//! is not exactly `/system`).
//!
//! This test faults a real ART guest on purpose (`ACOSLAUNCH_CRASH_TEST=segv`,
//! one store to an unmapped page after `activity-resumed`) and requires the
//! shipped `session-exit.json` to carry:
//!
//!   * the terminating signal and the fault address (`native_crash.fault_addr`),
//!   * the `si_code` name (`SEGV_MAPERR`),
//!   * the faulting thread's name and tid,
//!   * at least one native frame with a mapped library path, and
//!   * the raw ART dump on disk (`native-crash.txt`), and
//!   * the per-run tree kept, not deleted, for the crash.
//!
//! Self-skipping: without the ART sysroot and a warm `/data` (see
//! `tests/launch_hang.rs`) there is no guest to fault.

use std::io::Read;
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

fn warm_data() -> PathBuf {
    acos_test_support::art_warm_data(&repo_root())
}

/// HYG2: a missing ART input fails the test by name (or is a counted skip under
/// `ACOS_ALLOW_ART_SKIP=1`), never a silent pass.
fn require_art() -> Option<()> {
    acos_test_support::require_art(&repo_root(), "R3 crash evidence").map(|_| ())
}

fn apk() -> PathBuf {
    let views = repo_root().join("fixtures/build/apps/hello-views.apk");
    if views.is_file() {
        views
    } else {
        repo_root().join("fixtures/build/apps/counter.apk")
    }
}

fn seed_data(tag: &str) -> PathBuf {
    let short = format!(
        "{}{:03}",
        tag.chars().next().unwrap_or('x'),
        std::process::id() % 1000
    );
    let scratch = repo_root().join(".cache").join(format!("c{short}"));
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
    let runs = data.join("runs");
    let _ = std::fs::remove_dir_all(&runs);
    std::fs::create_dir_all(&runs).expect("create runs dir");
    data
}

/// Launch the shipped binary with the deliberate-crash seam and a hard bound.
fn run_crash_launch(data: &Path, out: &Path) -> (String, String, bool) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_acos-art"));
    cmd.arg("launch")
        .arg("--data")
        .arg(data)
        .arg("--apk")
        .arg(apk())
        .arg("--until")
        .arg("activity-resumed")
        .arg("--out")
        .arg(out)
        .arg("--timeout")
        .arg("60")
        .env("ACOS_ART_QUIET_GUARD", "1")
        .env("ACOSLAUNCH_CRASH_TEST", "segv")
        .env_remove("ACOS_ART_KEEP_RUN")
        .env_remove("ACOSART_CRASH_DUMP")
        .env_remove("ACOS_ART_SECCOMP_AUDIT")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let started = Instant::now();
    let mut child = cmd.spawn().expect("spawn acos-art launch");
    let pid = child.id() as i32;
    let mut out_pipe = child.stdout.take().expect("piped stdout");
    let mut err_pipe = child.stderr.take().expect("piped stderr");
    let out_thread = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = out_pipe.read_to_end(&mut v);
        v
    });
    let err_thread = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = err_pipe.read_to_end(&mut v);
        v
    });
    let mut timed_out = false;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {}
            Err(e) => panic!("try_wait on launch: {e}"),
        }
        if started.elapsed() >= Duration::from_secs(180) {
            timed_out = true;
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
                libc::kill(pid, libc::SIGKILL);
            }
            let _ = child.wait();
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    (
        String::from_utf8_lossy(&out_thread.join().unwrap()).to_string(),
        String::from_utf8_lossy(&err_thread.join().unwrap()).to_string(),
        timed_out,
    )
}

#[test]
fn a_guest_killed_by_a_signal_leaves_fault_addr_code_thread_and_a_native_frame() {
    let _guard = art_guard();
    if require_art().is_none() {
        return;
    }
    let data = seed_data("r3");
    let out = data.parent().unwrap().join("out");
    let _ = std::fs::remove_dir_all(&out);
    let t0 = Instant::now();
    let (stdout, stderr, timed_out) = run_crash_launch(&data, &out);
    eprintln!(
        "R3: launch wall={:?} out={} timed_out={timed_out}",
        t0.elapsed(),
        out.display()
    );
    assert!(
        !timed_out,
        "the crash launch never returned.\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );

    let record = std::fs::read_to_string(out.join("session-exit.json")).unwrap_or_else(|e| {
        panic!(
            "no session-exit.json in {}: {e}\nstderr:\n{stderr}",
            out.display()
        )
    });
    let doc: serde_json::Value = serde_json::from_str(&record).expect("session-exit.json is JSON");

    assert_eq!(
        doc["guest_signal"],
        serde_json::json!(11),
        "the guest must still die of its own SIGSEGV (taxonomy unchanged):\n{record}"
    );
    assert_eq!(doc["code"], "GUEST_KILLED_BY_SIGNAL", "{record}");
    let crash = &doc["native_crash"];
    assert!(
        !crash.is_null(),
        "a signal death must carry native_crash evidence:\n{record}"
    );
    assert_eq!(crash["signal"], serde_json::json!(11), "{record}");
    assert_eq!(crash["signal_name"], "SIGSEGV", "{record}");
    assert_eq!(crash["code_name"], "SEGV_MAPERR", "si_code name:\n{record}");
    assert_eq!(
        crash["fault_addr"], "0x0",
        "the fault address must be the one the kernel delivered:\n{record}"
    );
    let thread = crash["thread"]["name"].as_str().unwrap_or_default();
    assert!(
        !thread.is_empty(),
        "the faulting thread must be named:\n{record}"
    );
    assert!(crash["thread"]["tid"].as_i64().unwrap_or(0) > 0, "{record}");
    let frames = crash["frames"].as_array().cloned().unwrap_or_default();
    assert!(
        !frames.is_empty(),
        "the native backtrace must carry at least the faulting frame:\n{record}"
    );
    let first = &frames[0];
    assert!(
        first["map"].as_str().unwrap_or_default().ends_with(".so"),
        "frame #00 must name a mapped library, got {first}"
    );

    // The raw runtime dump is kept verbatim next to the record, and the run
    // tree of a crash is *not* deleted by teardown.
    let raw_path = doc["native_crash_path"]
        .as_str()
        .map(PathBuf::from)
        .expect("native_crash_path");
    let raw = std::fs::read_to_string(&raw_path)
        .unwrap_or_else(|e| panic!("no raw dump at {}: {e}", raw_path.display()));
    assert!(
        raw.contains("Fatal signal 11"),
        "the raw ART dump must carry the runtime's own tombstone:\n{raw}"
    );
    assert!(
        raw.contains("Backtrace:"),
        "the raw ART dump must carry the backtrace section:\n{raw}"
    );
    let kept = doc["kept_run_dir"]
        .as_str()
        .map(PathBuf::from)
        .expect("a kept crash tree must be recorded in session-exit.json");
    assert!(kept.is_dir(), "kept_run_dir must exist: {}", kept.display());
    assert!(
        kept.join("kept-for-crash.json").is_file(),
        "the kept tree carries the retention marker: {}",
        kept.display()
    );
    let runs: Vec<_> = std::fs::read_dir(data.join("runs"))
        .map(|rd| rd.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    assert_eq!(
        runs,
        vec![kept.clone()],
        "exactly the kept crash tree remains under <data>/runs"
    );
}

/// The counterpart the review asked for: a signal **the host sends** is a
/// teardown decision, never crash evidence. `--timeout` makes the launcher
/// SIGKILL the guest's process group; nothing may be kept.
#[test]
fn a_host_sigkill_does_not_keep_the_run_tree() {
    let _guard = art_guard();
    if require_art().is_none() {
        return;
    }
    let data = seed_data("r3kill");
    let out = data.parent().unwrap().join("out");
    let _ = std::fs::remove_dir_all(&out);
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_acos-art"));
    cmd.arg("launch")
        .arg("--data")
        .arg(&data)
        .arg("--apk")
        .arg(apk())
        .arg("--until")
        .arg("activity-resumed")
        .arg("--out")
        .arg(&out)
        .arg("--timeout")
        .arg("1")
        .env("ACOS_ART_QUIET_GUARD", "1")
        .env_remove("ACOS_ART_KEEP_RUN")
        .env_remove("ACOS_ART_KEEP_SESSION")
        .env_remove("ACOS_ART_SECCOMP_AUDIT")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = cmd.spawn().expect("spawn acos-art launch");
    let pid = child.id() as i32;
    let mut out_pipe = child.stdout.take().expect("piped stdout");
    let mut err_pipe = child.stderr.take().expect("piped stderr");
    let out_thread = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = out_pipe.read_to_end(&mut v);
        v
    });
    let err_thread = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = err_pipe.read_to_end(&mut v);
        v
    });
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {}
            Err(e) => panic!("try_wait on launch: {e}"),
        }
        if started.elapsed() >= Duration::from_secs(120) {
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
                libc::kill(pid, libc::SIGKILL);
            }
            let _ = child.wait();
            panic!("the SIGKILL launch never returned");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let stderr = String::from_utf8_lossy(&err_thread.join().unwrap()).to_string();
    let _stdout = String::from_utf8_lossy(&out_thread.join().unwrap()).to_string();
    let record = std::fs::read_to_string(out.join("session-exit.json"))
        .unwrap_or_else(|e| panic!("no session-exit.json: {e}\nstderr:\n{stderr}"));
    let doc: serde_json::Value = serde_json::from_str(&record).expect("session-exit.json is JSON");
    assert!(
        doc["kept_run_dir"].is_null(),
        "a host SIGKILL must not keep a tree:\n{record}"
    );
    let runs: Vec<_> = std::fs::read_dir(data.join("runs"))
        .map(|rd| rd.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    assert!(
        runs.is_empty(),
        "a host SIGKILL leaves no run tree (S4 keeps holding): {runs:?}"
    );
}
