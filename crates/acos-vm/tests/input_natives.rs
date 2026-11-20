//! T-36 Java-level acceptance for the `view.input` natives.
//!
//! Compiles `tests/java/InputMain.java` and `InputStressMain.java` with the host
//! `javac`/`d8` (build tools only, never the execution path) and runs them on the
//! real Android 14 boot classpath (`--corelib real`). Skips loudly when the SDK
//! or the boot-classpath cache is absent.
//!
//! What is asserted:
//!
//! * a real `MotionEvent.obtain` event round-trips through a real
//!   `InputChannel` pair (sender → receiver) preserving action/flags/coords;
//! * a real `InputEventReceiver` subclass receives injected DOWN/UP on a
//!   `Looper` and `finishInputEvent` is called once per dispatched event;
//! * the 16 required `MotionEvent` natives are all exercised;
//! * 1000 events leak no native handles, channels, receivers or fds.

use std::path::{Path, PathBuf};
use std::process::Command;

use acos_dex::DexFile;
use acos_vm::javabridge::JavaValue;
use acos_vm::natives::fwk_input;
use acos_vm::{CoreLib, Vm, VmError, VmOptions};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let p = dir.join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

struct Tools {
    javac: PathBuf,
    d8: PathBuf,
    jar: PathBuf,
    android_jar: PathBuf,
}

fn tools() -> Option<Tools> {
    let javac = which("javac")?;
    let jar = which("jar")?;
    let sdk = std::env::var("ANDROID_HOME")
        .or_else(|_| std::env::var("ANDROID_SDK_ROOT"))
        .unwrap_or_else(|_| "/home/Qui/Android/Sdk".to_string());
    let d8 = PathBuf::from(format!("{sdk}/build-tools/35.0.0/d8"));
    let android_jar = PathBuf::from(format!("{sdk}/platforms/android-34/android.jar"));
    if !d8.is_file() || !android_jar.is_file() {
        return None;
    }
    Some(Tools {
        javac,
        d8,
        jar,
        android_jar,
    })
}

/// The `@hide` `android.view` input classes the public SDK stub omits.
const INPUT_STUBS: &[&str] = &["InputChannel", "InputEventReceiver", "InputEventSender"];

fn compile_cases(names: &[&str]) -> Option<PathBuf> {
    let tools = tools()?;
    let root = repo_root();
    // Cache per fixture set: the two Java programs are different DEXes.
    let out = root
        .join("target/tmp/acos-input-natives")
        .join(names.join("-"));
    let dex = out.join("classes.dex");
    if dex.is_file() {
        return Some(dex);
    }
    let classes = out.join("classes");
    let stubs = out.join("stubs");
    std::fs::create_dir_all(&classes).ok()?;
    std::fs::create_dir_all(&stubs).ok()?;

    // 1. Compile the signature-only stubs for the hidden classes and jar them.
    let stub_srcs: Vec<PathBuf> = INPUT_STUBS
        .iter()
        .map(|n| {
            root.join("crates/acos-vm/tests/java/stubs/android/view")
                .join(format!("{n}.java"))
        })
        .collect();
    let ok = Command::new(&tools.javac)
        .args(["--release", "11", "-encoding", "UTF-8", "-cp"])
        .arg(&tools.android_jar)
        .args(["-d"])
        .arg(&stubs)
        .args(&stub_srcs)
        .status()
        .ok()?
        .success();
    if !ok {
        return None;
    }
    let stub_jar = out.join("input-stubs.jar");
    let ok = Command::new(&tools.jar)
        .arg("cf")
        .arg(&stub_jar)
        .args(["-C"])
        .arg(&stubs)
        .arg("android")
        .status()
        .ok()?
        .success();
    if !ok {
        return None;
    }

    // 2. Compile the fixtures against the stubs + the public SDK jar.
    let sources: Vec<PathBuf> = names
        .iter()
        .map(|n| {
            root.join("crates/acos-vm/tests/java")
                .join(format!("{n}.java"))
        })
        .collect();
    let ok = Command::new(&tools.javac)
        .args(["--release", "11", "-encoding", "UTF-8", "-cp"])
        .arg(format!(
            "{}:{}",
            stub_jar.display(),
            tools.android_jar.display()
        ))
        .args(["-d"])
        .arg(&classes)
        .args(&sources)
        .status()
        .ok()?
        .success();
    if !ok {
        return None;
    }
    let mut class_files: Vec<PathBuf> = std::fs::read_dir(&classes)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "class").unwrap_or(false))
        .collect();
    class_files.sort();
    let ok = Command::new(&tools.d8)
        .arg("--lib")
        .arg(&tools.android_jar)
        .arg("--lib")
        .arg(&stub_jar)
        .args(["--min-api", "26", "--output"])
        .arg(&out)
        .args(&class_files)
        .status()
        .ok()?
        .success();
    if !ok || !dex.is_file() {
        return None;
    }
    Some(dex)
}

fn run(dex: &Path, main: &str) -> Option<(Vm, i32, String, String, Option<VmError>)> {
    let options = VmOptions {
        corelib: CoreLib::Real,
        ..VmOptions::default()
    };
    let mut vm = match Vm::new(options) {
        Ok(vm) => vm,
        Err(VmError::BootClasspathMissing(m)) => {
            acos_test_support::require_prereq(false, "run", &format!("input natives: {m}"));
            return None;
        }
        Err(e) => panic!("Vm::new failed: {e}"),
    };
    vm.add_dex(DexFile::parse(std::fs::read(dex).unwrap()).unwrap());
    let outcome = vm.run(main, &[]);
    Some((
        vm,
        outcome.exit_code,
        outcome.stdout,
        outcome.stderr,
        outcome.vm_error,
    ))
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
fn motion_event_round_trips_and_receiver_finishes() {
    let Some(dex) = acos_test_support::require_some(compile_cases(&["InputMain"]), "motion_event_round_trips_and_receiver_finishes", "input natives: javac/d8/android.jar unavailable") else {
        return;
    };
    fwk_input::reset_hits();
    let Some((_vm, code, stdout, _stderr, err)) = run(&dex, "InputMain") else {
        return;
    };
    assert_eq!(code, 0, "run failed: {err:?}\nstdout={stdout}");
    assert!(err.is_none(), "{err:?}");
    assert_eq!(field(&stdout, "sent1"), "true");
    assert_eq!(field(&stdout, "sent2"), "true");
    assert_eq!(field(&stdout, "received"), "2", "stdout:\n{stdout}");
    assert_eq!(field(&stdout, "finished"), "2", "stdout:\n{stdout}");
    assert_eq!(field(&stdout, "downAction"), "0");
    assert_eq!(field(&stdout, "upAction"), "1");
    assert_eq!(
        field(&stdout, "downFlags"),
        field(&stdout, "sentFlags"),
        "flags not preserved"
    );
    let dx: f32 = field(&stdout, "downX").parse().unwrap();
    let dy: f32 = field(&stdout, "downY").parse().unwrap();
    assert!((dx - 540.125).abs() < 0.001, "downX={dx}");
    assert!((dy - 427.875).abs() < 0.001, "downY={dy}");
    let ux: f32 = field(&stdout, "upX").parse().unwrap();
    let uy: f32 = field(&stdout, "upY").parse().unwrap();
    assert!((ux - 540.125).abs() < 0.001, "upX={ux}");
    assert!((uy - 427.875).abs() < 0.001, "upY={uy}");
    // The native ack path saw exactly the two dispatches.
    assert_eq!(fwk_input::finished_events(), 2);
}

#[test]
fn sixteen_motion_event_natives_exercised() {
    let Some(dex) = acos_test_support::require_some(compile_cases(&["InputMain"]), "sixteen_motion_event_natives_exercised", "input natives: javac/d8/android.jar unavailable") else {
        return;
    };
    fwk_input::reset_hits();
    let Some((mut vm, code, stdout, _stderr, err)) = run(&dex, "InputMain") else {
        return;
    };
    assert_eq!(code, 0, "run failed: {err:?}");
    // The three required natives whose public Java entry points are `@hide`
    // (`getId`, `isTouchEvent`, `setTargetAccessibilityFocus`) are driven
    // through the real framework class via the bridge: the VM resolves them
    // from the boot DEX, not the public SDK stub.
    let ev = vm
        .call_static(
            "android.view.MotionEvent",
            "obtain",
            "(JJIFFFFIFFII)Landroid/view/MotionEvent;",
            &[
                JavaValue::Long(1),
                JavaValue::Long(2),
                JavaValue::Int(0),
                JavaValue::Float(1.0),
                JavaValue::Float(2.0),
                JavaValue::Float(1.0),
                JavaValue::Float(1.0),
                JavaValue::Int(0),
                JavaValue::Float(1.0),
                JavaValue::Float(1.0),
                JavaValue::Int(0),
                JavaValue::Int(0),
            ],
        )
        .expect("MotionEvent.obtain");
    let h = ev.as_handle().expect("MotionEvent handle");
    vm.call_method(h, "getId", "()I", &[]).expect("getId");
    vm.call_method(h, "isTouchEvent", "()Z", &[])
        .expect("isTouchEvent");
    vm.call_method(
        h,
        "setTargetAccessibilityFocus",
        "(Z)V",
        &[JavaValue::Boolean(true)],
    )
    .expect("setTargetAccessibilityFocus");
    let flags = vm.call_method(h, "getFlags", "()I", &[]).expect("getFlags");
    assert_ne!(
        flags,
        JavaValue::Int(0),
        "nativeSetFlags did not take effect"
    );
    vm.release_bridge_root(h);
    let hits = fwk_input::hits();
    let required = [
        "nativeGetAction",
        "nativeGetAxisValue",
        "nativeGetButtonState",
        "nativeGetDownTimeNanos",
        "nativeGetEventTimeNanos",
        "nativeGetFlags",
        "nativeGetId",
        "nativeGetPointerCount",
        "nativeGetPointerId",
        "nativeGetRawAxisValue",
        "nativeGetSource",
        "nativeGetToolType",
        "nativeIsTouchEvent",
        "nativeOffsetLocation",
        "nativeSetAction",
        "nativeSetFlags",
    ];
    let mut missing = Vec::new();
    for name in required {
        if hits.get(name).copied().unwrap_or(0) == 0 {
            missing.push(name);
        }
    }
    assert!(
        missing.is_empty(),
        "natives not exercised: {missing:?}\nstdout={stdout}"
    );
}

#[test]
fn thousand_events_leak_no_handles_or_fds() {
    let Some(dex) = acos_test_support::require_some(compile_cases(&["InputStressMain"]), "thousand_events_leak_no_handles_or_fds", "input natives: javac/d8/android.jar unavailable") else {
        return;
    };
    let Some((_vm, code, stdout, stderr, err)) = run(&dex, "InputStressMain") else {
        return;
    };
    assert_eq!(
        code, 0,
        "run failed: {err:?}\nstderr={stderr}\nstdout={stdout}"
    );
    assert_eq!(field(&stdout, "received"), "1000", "stdout:\n{stdout}");
    assert_eq!(field(&stdout, "finished"), "1000", "stdout:\n{stdout}");
    // Every native struct/channel/receiver was disposed by the Java program.
    // The channel descriptors and the native receivers are owned by ACOS and
    // are released by the Java program's `dispose()` calls: strictly zero.
    assert_eq!(fwk_input::live_channels(), 0, "InputChannels leaked");
    assert_eq!(fwk_input::live_receivers(), 0, "InputEventReceivers leaked");
    // `MotionEvent` native structs are owned by the framework's
    // `NativeAllocationRegistry` (freed by the finalizer), and the Java
    // recycler pool reuses a bounded number of objects (MAX_RECYCLED = 10), so
    // 1000 events must not grow the live set proportionally.
    assert_eq!(
        fwk_input::motion_created(),
        2000,
        "expected 2 x 1000 events"
    );
    assert!(
        fwk_input::live_motion_events() <= 16,
        "MotionEvent structs grew with the event count: {}",
        fwk_input::live_motion_events()
    );
}

#[test]
fn input_channel_round_trips_through_a_parcel() {
    let Some(dex) = acos_test_support::require_some(compile_cases(&["InputParcelMain"]), "input_channel_round_trips_through_a_parcel", "input natives: javac/d8/android.jar unavailable") else {
        return;
    };
    let Some((_vm, code, stdout, stderr, err)) = run(&dex, "InputParcelMain") else {
        return;
    };
    assert_eq!(
        code, 0,
        "run failed: {err:?}\nstderr={stderr}\nstdout={stdout}"
    );
    assert_eq!(field(&stdout, "name"), "acos-parcel-test (client)");
    assert_eq!(field(&stdout, "readName"), "acos-parcel-test (client)");
    assert_eq!(field(&stdout, "sent"), "true");
    assert_eq!(field(&stdout, "received"), "1", "stdout:\n{stdout}");
}
