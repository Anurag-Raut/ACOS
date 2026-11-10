//! M2 acceptance: the real Android 14 framework runs
//! `Application.onCreate` before `MainActivity.onCreate`, then `onStart` and
//! `onResume`, matching the ART oracle's `lifecycle-summary.txt`.
//!
//! The test never uses recorded mock calls: every event is an `AcosFixture`
//! log line emitted by the real `MainActivity` bytecode, and
//! `app_created` is read from the real `ActivityThread.mInitialApplication`.
//!
//! State as of this commit: `bindApplication` is delivered to the real
//! `ApplicationThread` and `handleBindApplication` starts on the main looper,
//! but it blocks inside `android.graphics.Typeface.<clinit>` because the font
//! config parse needs file I/O, the `/system` path mapping and ICU regex, none
//! of which are wired yet (see `results/m2/M2_LOG.md`).
//! The full gate is therefore `#[ignore]`d with that exact reason rather than
//! silently passing; the two non-ignored tests pin the progress and the
//! honesty invariant (never report a transaction as delivered when it was
//! not).

use std::path::{Path, PathBuf};
use std::time::Instant;

use acos_runtime::Runtime;
use acos_vm::{CoreLib, VmError, VmOptions};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn hello_views_apk() -> Option<PathBuf> {
    [
        repo_root().join("fixtures/build/apps/hello-views.apk"),
        repo_root().join("fixtures/apps/hello-views/hello-views.apk"),
    ]
    .into_iter()
    .find(|p| p.is_file())
}

/// The reference lifecycle, one event per line (`AcosFixture: <event>`).
fn reference_lifecycle() -> Option<Vec<String>> {
    let path = repo_root().join(
        "fixtures/reference/hello-views/art-api34-x86_64/lifecycle-summary.txt",
    );
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

/// The M2 gate. Ignored while the framework blocks in `Typeface.<clinit>`
/// before `Application.onCreate`; run with
/// `cargo test -p acos-runtime --test m2 -- --ignored --nocapture`.
#[test]
#[ignore = "M2 not reached: display reply + boot classpath are fixed and handleBindApplication now reaches ContextImpl.setupGraphicsSupport, which needs libcore.io.Linux.setxattr (guest VFS has no xattrs; should raise ErrnoException ENOTSUP); results/m2/M2_LOG.md"]
fn m2_lifecycle_create_start_resume_matches_reference() {
    let Some(reference) = reference_lifecycle() else {
        eprintln!("SKIP m2_lifecycle_create_start_resume_matches_reference: reference missing");
        return;
    };
    let Some(report) = run_lifecycle("normal") else {
        return;
    };
    assert!(
        report.error.is_none(),
        "lifecycle failed before the callbacks: {:?}",
        report.error
    );
    assert!(
        report.bind_delivered,
        "bindApplication was not delivered to the real ApplicationThread"
    );
    // Application.onCreate must run before the first Activity callback.
    assert!(
        report.app_created,
        "ActivityThread.mInitialApplication is null: Application.onCreate did not run"
    );
    // M2 is create/start/resume; compare that prefix with the oracle.
    let expected = &reference[..3.min(reference.len())];
    assert_eq!(
        &report.events[..report.events.len().min(expected.len())],
        expected,
        "lifecycle order differs from the ART oracle"
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
