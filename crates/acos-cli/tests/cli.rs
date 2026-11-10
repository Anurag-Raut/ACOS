//! Integration tests for the `acos` binary.

use std::path::PathBuf;

use assert_cmd::Command;
use serde_json::Value;

fn workspace_path(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn parse_json(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).expect("stdout should be valid JSON")
}

#[test]
fn version_flag_prints_version() {
    let out = Command::cargo_bin("acos")
        .unwrap()
        .arg("--version")
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("acos"), "version output: {stdout}");
    assert!(stdout.contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn inspect_hello_views_json() {
    let apk = workspace_path("../acos-apk/tests/data/hello-views.apk");
    let out = Command::cargo_bin("acos")
        .unwrap()
        .args(["inspect"])
        .arg(&apk)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let v = parse_json(&out.stdout);
    assert_eq!(v["ok"], true);
    assert_eq!(v["status"], "pass");
    assert_eq!(v["fidelity"], "static-analysis");
    assert_eq!(v["backend"], "acos-native");
    assert!(v["error"].is_null());
    assert!(v["request_id"]
        .as_str()
        .is_some_and(|s| s.starts_with("req_")));

    let result = &v["result"];
    assert_eq!(result["package"], "dev.acos.fixtures.helloviews");
    assert_eq!(result["version_code"], 1);
    assert_eq!(result["version_name"], "1.0");
    assert_eq!(result["min_sdk"], 26);
    assert_eq!(result["target_sdk"], 34);
    assert_eq!(result["label"], "Hello Views");
    assert_eq!(
        result["launcher_activity"],
        "dev.acos.fixtures.helloviews.MainActivity"
    );
    assert_eq!(result["dex_files"][0]["name"], "classes.dex");
    assert!(result["dex_files"][0]["size"].as_u64().unwrap() > 0);
    assert_eq!(result["native_abis"].as_array().unwrap().len(), 0);
    assert_eq!(result["signing"]["v2"], true);
    assert!(result["signing"]["note"]
        .as_str()
        .unwrap()
        .contains("not cryptographically verified"));
    assert_eq!(result["resources"]["present"], true);

    let perms = result["permissions"].as_array().unwrap();
    assert!(perms
        .iter()
        .any(|p| p["name"] == "android.permission.INTERNET"));
}

#[test]
fn inspect_hello_jni_json() {
    let apk = workspace_path("../acos-apk/tests/data/hello-jni.apk");
    let out = Command::cargo_bin("acos")
        .unwrap()
        .args(["inspect"])
        .arg(&apk)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let v = parse_json(&out.stdout);
    let result = &v["result"];
    assert_eq!(result["package"], "dev.acos.fixtures.hellojni");
    assert_eq!(result["version_code"], 2);
    assert_eq!(result["label"], "Hello JNI");

    let abis: Vec<String> = result["native_abis"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap().to_string())
        .collect();
    assert_eq!(abis, vec!["arm64-v8a".to_string(), "x86_64".to_string()]);

    let libs = result["native_libs"].as_array().unwrap();
    assert_eq!(libs.len(), 2);
    assert!(libs.iter().any(|l| l["abi"] == "x86_64"
        && l["name"] == "libhellojni.so"
        && l["size"].as_u64().unwrap() > 0));
}

#[test]
fn inspect_non_apk_returns_error_envelope() {
    let not_apk = workspace_path("Cargo.toml");
    let out = Command::cargo_bin("acos")
        .unwrap()
        .args(["inspect"])
        .arg(&not_apk)
        .arg("--json")
        .output()
        .unwrap();
    assert!(!out.status.success(), "expected nonzero exit");

    let v = parse_json(&out.stdout);
    assert_eq!(v["ok"], false);
    assert_eq!(v["status"], "error");
    assert_eq!(v["fidelity"], "static-analysis");
    assert_eq!(v["error"]["code"], "APK_NOT_ZIP");
    assert!(v["error"]["message"]
        .as_str()
        .is_some_and(|s| !s.is_empty()));
    assert!(v["result"].is_null());
}

#[test]
fn inspect_non_apk_text_mode_reports_error() {
    let not_apk = workspace_path("Cargo.toml");
    let out = Command::cargo_bin("acos")
        .unwrap()
        .args(["inspect"])
        .arg(&not_apk)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("APK_NOT_ZIP"), "stderr: {stderr}");
}

#[test]
fn inspect_text_mode_lists_key_fields() {
    let apk = workspace_path("../acos-apk/tests/data/hello-views.apk");
    let out = Command::cargo_bin("acos")
        .unwrap()
        .args(["inspect"])
        .arg(&apk)
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("dev.acos.fixtures.helloviews"));
    assert!(stdout.contains("Hello Views"));
    assert!(stdout.contains("classes.dex"));
}
