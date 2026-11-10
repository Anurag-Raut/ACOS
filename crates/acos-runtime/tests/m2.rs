//! M2 acceptance: the real Android 14 framework runs
//! `Application.onCreate` before `MainActivity.onCreate`, then `onStart` and
//! `onResume`, matching the ART oracle's `lifecycle-summary.txt`.
//!
//! The test never uses recorded mock calls: every event is an `AcosFixture`
//! log line emitted by the real `MainActivity` bytecode, and
//! `app_created` is read from the real `ActivityThread.mInitialApplication`.
//!
//! State as of W147-A: `bindApplication` is delivered to the real
//! `ApplicationThread`, `Application.onCreate` runs (`app_created=true`), the
//! launch `ClientTransaction` reaches `ActivityThread.performLaunchActivity`
//! and the real `MainActivity` emits `onCreate`, `onStart`, `onResume`, exactly
//! the ART oracle's prefix. The gate is enabled. The first traversal after
//! `onResume` still raises a `SurfaceControl` NPE owned by the W144-A
//! lifecycle/render work (see `results/m2/M2_LOG.md` §W147-A); the gate asserts
//! the lifecycle prefix, and reports any later framework error through the
//! printed `M2[normal] ... error=...` evidence line.

use std::path::{Path, PathBuf};
use std::time::Instant;

use acos_runtime::Runtime;
use acos_vm::{CoreLib, VmError, VmOptions};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn hello_views_apk() -> Option<PathBuf> {
    let candidates = [
        repo_root().join("fixtures/build/apps/hello-views.apk"),
        repo_root().join("fixtures/apps/hello-views/hello-views.apk"),
    ];
    if let Some(found) = candidates.iter().find(|p| p.is_file()) {
        return Some(found.clone());
    }
    // Hard-fail under ACOS_REQUIRE_FIXTURES; otherwise a loud skip.
    acos_test_support::have_fixture("fixtures/build/apps/hello-views.apk");
    None
}

/// The reference lifecycle, one event per line (`AcosFixture: <event>`).
fn reference_lifecycle() -> Option<Vec<String>> {
    let rel = "fixtures/reference/hello-views/art-api34-x86_64/lifecycle-summary.txt";
    if !acos_test_support::have_fixture(rel) {
        return None;
    }
    let path = repo_root().join(rel);
    let text = std::fs::read_to_string(path).ok()?;
    Some(
        text.lines()
            .filter_map(|l| l.strip_prefix("AcosFixture: ").map(str::to_string))
            .collect(),
    )
}

fn new_runtime() -> Option<Runtime> {
    match Runtime::standard_with(VmOptions {
        corelib: CoreLib::Real,
        trace_natives: true,
        ..VmOptions::default()
    }) {
        Ok(rt) => Some(rt),
        Err(VmError::BootClasspathMissing(msg)) => {
            // Hard-fail under ACOS_REQUIRE_FIXTURES; otherwise a loud skip.
            acos_test_support::have_fixture(".cache/boot/android-34");
            eprintln!("SKIP m2 test: {msg}");
            None
        }
        Err(e) => panic!("Runtime::standard_with failed: {e}"),
    }
}

/// Peak resident set size in KiB (Linux `/proc/self/status` `VmHWM`).
fn peak_rss_kb() -> u64 {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return 0;
    };
    status
        .lines()
        .find_map(|l| l.strip_prefix("VmHWM:"))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|kb| kb.parse().ok())
        .unwrap_or(0)
}

/// Run the lifecycle once and print the evidence. Returns `None` when the
/// fixtures are absent (loud skip, never a silent pass).
fn run_lifecycle(label: &str) -> Option<acos_runtime::LifecycleReport> {
    // The `AcosFixture` log sink is process-wide (`acos-vm::natives::os_log`),
    // so the three tests in this file must not run their lifecycle
    // concurrently or they observe each other's events. Cargo runs test
    // functions in parallel by default; this serializes them.
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let apk = hello_views_apk()?;
    let mut rt = new_runtime()?;
    let started = Instant::now();
    let report = rt.boot_and_run_lifecycle(&apk);
    let wall = started.elapsed();
    eprintln!(
        "M2[{label}] wall={wall:?} peak_rss={}kB classes={} bind={} app={} txscheduled={} tx={} quit={} events={:?} error={:?}",
        peak_rss_kb(),
        report.classes_loaded,
        report.bind_delivered,
        report.app_created,
        report.transaction_scheduled,
        report.transaction_delivered,
        report.looper_quit,
        report.events,
        report.error,
    );
    Some(report)
}

/// The M2 gate: the real framework runs `onCreate` -> `onStart` -> `onResume`
/// in order, matching the ART oracle's prefix. See `results/m2/M2_LOG.md`
/// §W147-A for the current state (the first traversal after `onResume` still
/// hits a `SurfaceControl` NPE owned by the W144-A lifecycle/render work).
#[test]
fn m2_lifecycle_create_start_resume_matches_reference() {
    let Some(reference) = reference_lifecycle() else {
        eprintln!("SKIP m2_lifecycle_create_start_resume_matches_reference: reference missing");
        return;
    };
    let Some(report) = run_lifecycle("normal") else {
        return;
    };
    assert!(
        report.bind_delivered,
        "bindApplication was not delivered to the real ApplicationThread"
    );
    // Application.onCreate must run before the first Activity callback.
    assert!(
        report.app_created,
        "ActivityThread.mInitialApplication is null: Application.onCreate did not run"
    );
    // M2 is create/start/resume; compare exactly that prefix with the oracle. A
    // framework error that occurred *after* `onResume` (the first traversal;
    // §W147-A / W144-A) would leave `events` complete and is reported through
    // `report.error` but does not invalidate the lifecycle milestone. An error
    // that prevented a callback would shorten `events` and fail here.
    let expected = &reference[..3.min(reference.len())];
    assert_eq!(
        report.events, expected,
        "lifecycle callbacks/order differ from the ART oracle (error={:?})",
        report.error
    );
}

/// Progress guard: the real `IApplicationThread.bindApplication` receivership
/// still works. This is the part of M2 that is done, and it must not regress.
#[test]
fn bind_application_is_delivered_to_the_real_application_thread() {
    let Some(report) = run_lifecycle("bind-progress") else {
        return;
    };
    assert!(
        report.bind_delivered,
        "bindApplication was not delivered; error={:?}",
        report.error
    );
}

/// Honesty invariant (advisor finding): a transaction may be reported as
/// delivered only if the framework actually ran the Activity callbacks.
#[test]
fn lifecycle_never_claims_an_undelivered_transaction() {
    let Some(report) = run_lifecycle("honesty") else {
        return;
    };
    if report.transaction_delivered {
        assert!(
            report.app_created,
            "transaction delivered before Application.onCreate: {:?}",
            report.error
        );
        assert_eq!(
            report.events.first().map(String::as_str),
            Some("onCreate"),
            "transaction delivered without MainActivity.onCreate: {:?}",
            report.error
        );
    }
}
