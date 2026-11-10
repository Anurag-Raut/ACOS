//! M2 acceptance: the real Android 14 framework runs
//! `Application.onCreate` before `MainActivity.onCreate`, then `onStart` and
//! `onResume`, matching the ART oracle's `lifecycle-summary.txt`.
//!
//! The test never uses recorded mock calls: every event is an `AcosFixture`
//! log line emitted by the real `MainActivity` bytecode, and
//! `app_created` is read from the real `ActivityThread.mInitialApplication`.
//!
//! State as of this commit: `bindApplication` is delivered to the real
//! `ApplicationThread`, `handleBindApplication` runs
//! `Application.onCreate` (`app_created=true`), and the launch
//! `ClientTransaction` reaches `ActivityThread.performLaunchActivity` ->
//! `Activity.attach` -> `PhoneWindow.<init>`. It then blocks in
//! `Settings.getPublicSettingsForClass` (a `Field.get` static-slot bug), before
//! `MainActivity.onCreate` (see `results/m2/M2_LOG.md`, W29-A section).
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

/// The M2 gate. Ignored while the framework blocks before `Activity.onCreate`;
/// run with `cargo test -p acos-runtime --test m2 -- --ignored --nocapture`.
#[test]
#[ignore = "M2 not reached: bindApplication + Application.onCreate + the launch ClientTransaction all run (app_created=true, transaction_scheduled=true) and performLaunchActivity reaches MainActivity.onCreate -> setContentView -> PhoneWindow.installDecor. W65-A fixed two blockers: (1) `ActivityManager$TaskDescription` is now a conditional-aware codec (`FieldOp::Cond`) matching the real API-34 writeToParcel, so `IActivityClientController.setTaskDescription` no longer fails with `bad string: String16 is not NUL terminated`; (2) a theme attribute whose value is `@null` (reference id 0) is materialised as `TYPE_NULL` instead of `TYPE_REFERENCE` to 0, so `ActionBarOverlayLayout.init` -> `TypedArray.getDrawable(windowContentOverlay)` no longer throws `Resources$NotFoundException: Unable to find resource ID #0x0`. The launch now blocks in `DecorView.onResourcesLoaded` inflating `android:layout/screen_toolbar`: `ToolbarWidgetWrapper` loads a VectorDrawable and `VectorDrawable.nCreateGroup` is unimplemented (W47-A render/gfx owns the VectorDrawable native tree); results/m2/M2_LOG.md §W65-A"]
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
