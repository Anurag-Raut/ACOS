//! T-30 acceptance: `android.view.DisplayEventReceiver` + `Choreographer`.
//!
//! Compiles `tests/java/VsyncMain.java` with the host `javac`/`d8` (build tools
//! only, never the execution path) and runs it on the real Android 14 boot
//! classpath (`--corelib real`) through the standard `Runtime` (so the real
//! `DisplayManagerGlobal`/`IDisplayManager` service is installed). Skips loudly
//! when the SDK or the boot-classpath cache is absent.
//!
//! What is asserted:
//!
//! * a real `Choreographer.postFrameCallback` chain receives exactly three
//!   frame callbacks whose `frameTimeNanos` are strictly monotonic;
//! * after the chain ends the virtual source is idle (no pending receiver, so
//!   no tick can be generated);
//! * with a `Choreographer` created but nothing scheduled, the looper runs its
//!   timer out and zero frames are delivered;
//! * `nativeGetLatestVsyncEventData` returns the last monotonic vsync id.

mod common;

use std::path::Path;

use acos_vm::javabridge::JavaValue;
use acos_vm::natives::fwk_vsync;
use acos_vm::RunOutcome;

fn run(dex: &Path, args: &[&str]) -> Option<(acos_runtime::Runtime, RunOutcome)> {
    let mut rt = common::new_runtime()?;
    rt.vm_mut()
        .add_dex(acos_dex::DexFile::parse(std::fs::read(dex).unwrap()).unwrap());
    let args: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
    let outcome = rt.run("VsyncMain", &args);
    Some((rt, outcome))
}

fn field(stdout: &str, key: &str) -> String {
    stdout
        .lines()
        .find_map(|l| l.strip_prefix(&format!("{key}=")))
        .unwrap_or_else(|| panic!("missing `{key}=` in:\n{stdout}"))
        .trim()
        .to_string()
}

#[test]
fn choreographer_frames_are_monotonic_and_stop_when_idle() {
    let Some(dex) = acos_test_support::require_some(common::compile_case(&["VsyncMain"]), "choreographer_frames_are_monotonic_and_stop_when_idle", "vsync natives: javac/d8/android.jar unavailable") else {
        return;
    };
    fwk_vsync::reset();
    let Some((mut rt, outcome)) = run(&dex, &[]) else {
        return;
    };
    assert_eq!(
        outcome.exit_code, 0,
        "VsyncMain failed: vm_error={:?}\nstdout:\n{}\nstderr:\n{}",
        outcome.vm_error, outcome.stdout, outcome.stderr
    );
    assert!(outcome.vm_error.is_none(), "{:?}", outcome.vm_error);
    assert_eq!(
        field(&outcome.stdout, "frames"),
        "3",
        "stdout:\n{}",
        outcome.stdout
    );
    assert_eq!(
        field(&outcome.stdout, "monotonic"),
        "true",
        "frame times not strictly monotonic:\n{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.lines().any(|l| l == "done"),
        "VsyncMain did not finish:\n{}",
        outcome.stdout
    );

    // The Java chain received real ticks; the source is now idle, so no
    // receiver has an outstanding request.
    assert!(
        fwk_vsync::tick_count() >= 3,
        "expected >=3 generated ticks, got {}",
        fwk_vsync::tick_count()
    );
    assert!(
        fwk_vsync::last_tick() > 0,
        "last tick timestamp must be set"
    );
    assert!(
        fwk_vsync::is_idle(),
        "vsync source must be idle after the callback chain ends"
    );
    assert_eq!(fwk_vsync::pending_count(), 0);

    // `nativeGetLatestVsyncEventData` returns the last monotonic vsync id.
    let latest = latest_vsync_id(rt.vm_mut()).expect("nativeGetLatestVsyncEventData");
    assert!(
        latest >= 3,
        "latest vsync id {latest} must cover the three delivered frames"
    );
    let again = latest_vsync_id(rt.vm_mut()).expect("nativeGetLatestVsyncEventData again");
    assert_eq!(again, latest, "vsync id must not go backwards while idle");
}

#[test]
fn no_ticks_when_nothing_is_scheduled() {
    let Some(dex) = acos_test_support::require_some(common::compile_case(&["VsyncMain"]), "no_ticks_when_nothing_is_scheduled", "vsync natives: javac/d8/android.jar unavailable") else {
        return;
    };
    fwk_vsync::reset();
    let Some((_rt, outcome)) = run(&dex, &["none"]) else {
        return;
    };
    assert_eq!(
        outcome.exit_code, 0,
        "VsyncMain none failed: vm_error={:?}\nstdout:\n{}\nstderr:\n{}",
        outcome.vm_error, outcome.stdout, outcome.stderr
    );
    assert!(outcome.vm_error.is_none(), "{:?}", outcome.vm_error);
    assert_eq!(
        field(&outcome.stdout, "frames"),
        "0",
        "stdout:\n{}",
        outcome.stdout
    );
    assert!(
        fwk_vsync::is_idle(),
        "idle source must remain idle with nothing scheduled"
    );
    assert_eq!(
        fwk_vsync::tick_count(),
        0,
        "an unscheduled DisplayEventReceiver must not generate ticks"
    );
}

/// The deterministic virtual-time scheduler drives the same real
/// `Choreographer`/`DisplayEventReceiver` chain without a real 16.6 ms sleep
/// per frame. This is the pump the lifecycle driver uses to reach the first
/// `performTraversals`.
#[test]
fn choreographer_frames_run_under_virtual_time() {
    let Some(dex) = acos_test_support::require_some(common::compile_case(&["VsyncMain"]), "choreographer_frames_run_under_virtual_time", "choreographer frames under virtual time: javac/d8 unavailable") else {
        return;
    };
    fwk_vsync::reset();
    let Some(mut rt) = common::new_runtime() else {
        return;
    };
    rt.vm_mut()
        .add_dex(acos_dex::DexFile::parse(std::fs::read(&dex).unwrap()).unwrap());
    let start = std::time::Instant::now();
    let outcome = rt.vm_mut().run_virtual("VsyncMain", &[]);
    let wall = start.elapsed();
    assert_eq!(
        outcome.exit_code, 0,
        "VsyncMain virtual failed: vm_error={:?}\nstdout:\n{}\nstderr:\n{}",
        outcome.vm_error, outcome.stdout, outcome.stderr
    );
    assert_eq!(
        field(&outcome.stdout, "frames"),
        "3",
        "stdout:\n{}",
        outcome.stdout
    );
    assert_eq!(
        field(&outcome.stdout, "monotonic"),
        "true",
        "frame times not strictly monotonic:\n{}",
        outcome.stdout
    );
    assert!(
        wall < std::time::Duration::from_secs(3),
        "virtual time must not sleep one 16.6 ms interval per vsync: {wall:?}"
    );
    assert!(fwk_vsync::tick_count() >= 3, "expected >=3 ticks");
    assert!(fwk_vsync::is_idle(), "vsync source idle after the chain");
}

/// Read `Choreographer.getInstance().mDisplayEventReceiver.getLatestVsyncEventData().frameTimelines[0].vsyncId`.
fn latest_vsync_id(vm: &mut acos_vm::Vm) -> Option<i64> {
    let ch = vm
        .call_static(
            "android.view.Choreographer",
            "getInstance",
            "()Landroid/view/Choreographer;",
            &[],
        )
        .ok()?;
    let ch = match ch {
        JavaValue::Object(h) if h != 0 => h,
        _ => return None,
    };
    let receiver = match vm.get_field(ch, "mDisplayEventReceiver").ok()? {
        JavaValue::Object(h) if h != 0 => h,
        _ => return None,
    };
    let event = match vm
        .call_method(
            receiver,
            "getLatestVsyncEventData",
            "()Landroid/view/DisplayEventReceiver$VsyncEventData;",
            &[],
        )
        .ok()?
    {
        JavaValue::Object(h) if h != 0 => h,
        _ => return None,
    };
    let timelines = match vm.get_field(event, "frameTimelines").ok()? {
        JavaValue::Object(h) if h != 0 => h,
        _ => return None,
    };
    let timeline = match vm.get_array_element(timelines, 0).ok()? {
        JavaValue::Object(h) if h != 0 => h,
        _ => return None,
    };
    match vm.get_field(timeline, "vsyncId").ok()? {
        JavaValue::Long(v) => Some(v),
        _ => None,
    }
}
