//! End-to-end `acos launch --native` tests on the repository's own fixture APKs.
//!
//! These spawn the real `acos` binary, so the ADR-0022 confinement is entered in
//! a fresh single-threaded process before the session worker starts — exactly
//! the product path. Only ACOS-owned fixture libraries are executed.

use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::prelude::*;
use serde_json::Value;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn boot_available() -> bool {
    acos_test_support::have_fixture(".cache/boot/android-34/BOOTCLASSPATH")
}

fn fixture(name: &str) -> Option<PathBuf> {
    let rel = format!("fixtures/build/apps/{name}");
    let apk = repo_root().join(&rel);
    if acos_test_support::have_fixture(&rel) {
        Some(apk)
    } else {
        eprintln!("SKIP native_launch: missing {}", apk.display());
        None
    }
}

fn launch(apk: &Path, native: bool) -> Value {
    let mut cmd = Command::cargo_bin("acos").expect("acos binary");
    cmd.current_dir(repo_root())
        .arg("launch")
        .arg("--json");
    if native {
        cmd.arg("--native");
    }
    cmd.arg(apk);
    let output = cmd.output().expect("run acos launch");
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "launch output is not JSON: {e}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn events(value: &Value) -> Vec<String> {
    value["result"]["events"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn stage_status<'a>(value: &'a Value, stage: &str) -> &'a str {
    value["result"]["stages"]
        .as_array()
        .and_then(|stages| {
            stages
                .iter()
                .find(|s| s["stage"] == stage)
                .and_then(|s| s["status"].as_str())
        })
        .unwrap_or("<missing>")
}

#[test]
fn native_hello_loads_and_runs_apk_library() {
    if !boot_available() {
        eprintln!("SKIP native_hello_loads_and_runs_apk_library: no boot cache");
        return;
    }
    let Some(apk) = fixture("native-hello.apk") else {
        return;
    };
    let value = launch(&apk, true);

    let events = events(&value);
    let has = |needle: &str| events.iter().any(|e| e.contains(needle));
    assert!(
        has("nativeHello=hello-from-native"),
        "native String must reach Java; events: {events:#?}\nerror: {:#?}",
        value["error"]
    );
    // The app's AssetManager `mObject` handle is registered by the real
    // `nativeCreate`, so `AAssetManager_fromJava` resolves the live fwk_res
    // manager and the native code reads the APK's `assets/hello.txt` bytes.
    assert!(
        has("asset=hello-acos!!"),
        "AAsset_read must return the APK asset bytes; events: {events:#?}"
    );
    assert!(
        has("bitmapPixels=12"),
        "AndroidBitmap_lockPixels must fill the Java Bitmap; events: {events:#?}"
    );
    assert!(
        has("bitmapPixel11=ff112233"),
        "Java Bitmap.getPixel must see the native pixels; events: {events:#?}"
    );
    // Java passed a real Surface; the native ANativeWindow lock/fill/post went
    // through the production NDK host into the compositor's FrameStore.
    assert!(
        has("frame=1"),
        "ANativeWindow_unlockAndPost must publish a frame; events: {events:#?}"
    );
    assert_eq!(
        stage_status(&value, "first_frame"),
        "ok",
        "a FrameStore frame must be observable through the frame API; events: {events:#?}"
    );

    // `Application.onCreate` ran, so the native bridge works even though the
    // Activity lifecycle may still block later (W68-A).
    assert_eq!(
        stage_status(&value, "application_on_create"),
        "ok",
        "application_on_create must complete; error: {:#?}",
        value["error"]
    );
}

#[test]
fn native_hostile_actions_are_denied_by_confinement() {
    if !boot_available() {
        eprintln!("SKIP native_hostile_actions_are_denied_by_confinement: no boot cache");
        return;
    }
    let Some(apk) = fixture("native-hostile.apk") else {
        return;
    };
    let value = launch(&apk, true);

    let events = events(&value);
    assert!(
        events.iter().any(|e| e == "hostile=0"),
        "every denied action must fail and the session must survive; events: {events:#?}\nerror: {:#?}",
        value["error"]
    );
    assert_eq!(
        stage_status(&value, "application_on_create"),
        "ok",
        "the session must survive the hostile probe; error: {:#?}",
        value["error"]
    );
}

#[test]
fn native_launch_without_native_reports_unsupported_with_hint() {
    if !boot_available() {
        eprintln!("SKIP native_launch_without_native_reports_unsupported_with_hint: no boot cache");
        return;
    }
    let Some(apk) = fixture("native-hello.apk") else {
        return;
    };
    let value = launch(&apk, false);
    assert_eq!(
        stage_status(&value, "application_on_create"),
        "blocked",
        "without --native the native call must not run"
    );
    let blocker = &value["result"]["stages"]
        .as_array()
        .and_then(|s| s.iter().find(|s| s["stage"] == "application_on_create"))
        .map(|s| s["blocker"].clone())
        .unwrap_or(Value::Null);
    assert_eq!(blocker["code"], "UNSUPPORTED_JAVA_API");
    let message = blocker["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("--native"),
        "the default blocker must hint at `--native`: {message}"
    );
}

#[test]
fn wrong_abi_apk_is_rejected_before_any_execution() {
    let Some(apk) = fixture("rn-hello-arm64.apk") else {
        return;
    };
    let value = launch(&apk, true);
    assert_eq!(value["ok"], false);
    assert_eq!(
        value["error"]["code"], "UNSUPPORTED_ABI",
        "an arm64-only APK must be rejected by the gate: {value:#?}"
    );
}
