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
#[ignore = "M2 not reached: bindApplication + Application.onCreate + the launch ClientTransaction all run (app_created=true, transaction_scheduled=true) and performLaunchActivity reaches MainActivity.onCreate -> setContentView -> PhoneWindow.installDecor -> DecorView.onResourcesLoaded inflating android:layout/screen_toolbar. W110-A landed two launch-chain fixes: `nativeResolveAttrs` chases a theme-resolved @resid to its concrete value (`?attr/colorControlNormal` -> res/color/text_color_secondary_device_default_light.xml), so VectorDrawable.applyTheme no longer raises `Can't convert to ComplexColor: type=0x1`; and the static `nativeAsset*` natives now take only the AssetInputStream pointer (their real Java signatures) with the owning manager encoded in the high 32 bits, so opening a real asset stream no longer panics with an out-of-bounds register read. android:drawable/ic_ab_back_material inflates. The launch now blocks in PhoneWindow.installDecor -> ApplicationPackageManager.getDefaultActivityIcon() decoding the default adaptive launcher icon: `Landroid/graphics/ImageDecoder;->nCreate(Ljava/io/InputStream;[BZLandroid/graphics/ImageDecoder$Source;)Landroid/graphics/ImageDecoder;` is not implemented (image/render, W104-A). W116-A: with W104-A's real ImageDecoder the counter.apk launch passes activity_on_create and reaches ViewRootImpl.setView -> IWindowSession.addToDisplayAsUser (its reply decodes in the real Stub$Proxy), and the bounded run's main-looper quit bug is fixed; the M2 gate fixture hello-views is still blocked earlier at android.widget.EditText inflate, and the first performTraversals never reaches relayout because the lifecycle driver does not pump the looper. results/m2/M2_LOG.md §W110-A, §W116-A. W134-A: the settings provider now answers `LIST_config` with a well-formed empty DeviceConfig `HashMap` (parcel `VAL_MAP`) and `BitmapFactory`/`ImageDecoder` extract the PNG `npTc` nine-patch chunk, so hello-views gets past the `IContentProvider LIST_config unsupported` and `<nine-patch> requires a valid 9-patch source image` failures; it now blocks at a bare `NullPointerException` in the `android.widget.EditText` constructor (`LayoutInflater.createView`), the next framework gap. The gate stays ignored. results/m2/M2_LOG.md §W134-A"]
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
