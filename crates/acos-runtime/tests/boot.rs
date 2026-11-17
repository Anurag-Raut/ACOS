//! M2 acceptance: bring the real `android.app.ActivityThread` up and reach
//! `IActivityManager.attachApplication` with a valid `IApplicationThread`.
//!
//! The test runs the real Android 14 framework DEX (`framework.jar`) on the
//! Rust VM: `Looper.prepareMainLooper()`, `new ActivityThread()`, then the
//! private `attach(false, startSeq)` whose final handshake crosses the single
//! ACOS Binder native into the in-process `ServiceHost`. It skips loudly (never
//! silently passes) when the fixture APK or the boot-classpath cache is absent.

use std::path::{Path, PathBuf};
use std::time::Instant;

use acos_runtime::{BootError, Runtime};
use acos_vm::{CoreLib, VmError, VmOptions};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The hello-views fixture APK, if it has been built.
fn hello_views_apk() -> Option<PathBuf> {
    let candidates = [
        repo_root().join("fixtures/build/apps/hello-views.apk"),
        repo_root().join("fixtures/apps/hello-views/hello-views.apk"),
    ];
    candidates.into_iter().find(|p| p.is_file())
}

fn new_runtime() -> Option<Runtime> {
    match Runtime::standard_with(VmOptions {
        corelib: CoreLib::Real,
        trace_natives: true,
        ..VmOptions::default()
    }) {
        Ok(rt) => Some(rt),
        Err(VmError::BootClasspathMissing(msg)) => {
            eprintln!("SKIP boot test: {msg}");
            None
        }
        Err(e) => panic!("Runtime::standard_with failed: {e}"),
    }
}

/// The M2 gate: the real framework `attach` path reaches the Rust ATMS.
#[test]
fn activity_thread_attach_reaches_atms_attach_application() {
    let Some(apk) = hello_views_apk() else {
        eprintln!("SKIP activity_thread_attach_reaches_atms_attach_application: fixtures/build/apps/hello-views.apk not built");
        return;
    };
    let Some(mut rt) = new_runtime() else {
        return;
    };

    let started = Instant::now();
    let report = match rt.boot_app_process(&apk) {
        Ok(r) => r,
        Err(e) => {
            panic!(
                "framework boot failed at {e} (elapsed {:?})",
                started.elapsed()
            );
        }
    };

    // The exact M2 evidence: a non-null IApplicationThread binder arrived.
    assert!(
        acos_runtime::atms_boot::is_valid_app_thread(&report.attach),
        "attachApplication had no valid IApplicationThread: {:?}",
        report.attach
    );
    assert_eq!(report.attach.start_seq, 1, "startSeq round-trip");
    assert!(
        !report.attach.app_thread.unwrap().is_null(),
        "null binder id"
    );

    // Stage ordering and progress.
    assert_eq!(
        report.stages,
        vec![
            "open-apk",
            "load-app-dex",
            "prepare-main-looper",
            "instantiate-activity-thread",
            "attach-application",
        ]
    );
    assert!(
        report.classes_loaded > report.boot_classpath_classes,
        "the framework boot must load classes beyond the M1 core trio"
    );
    assert!(
        !report.natives_invoked.is_empty(),
        "native tracing should be on for the evidence log"
    );

    eprintln!(
        "M2 boot: attach={:?} wall={:?} classes={} (core trio {}) natives={}",
        report.attach,
        report.elapsed,
        report.classes_loaded,
        report.boot_classpath_classes,
        report.natives_invoked.len()
    );
}

/// A failed boot must name the exact missing API, never silently succeed.
#[test]
fn missing_apk_is_a_structured_error() {
    // A VM is not needed for this path; build one only if the cache exists.
    let Some(mut rt) = new_runtime() else {
        return;
    };
    let err = rt
        .boot_app_process(repo_root().join("fixtures/build/apps/does-not-exist.apk"))
        .expect_err("a missing APK must fail");
    assert!(
        matches!(err, BootError::Apk(_)),
        "expected an APK error, got {err}"
    );
}
