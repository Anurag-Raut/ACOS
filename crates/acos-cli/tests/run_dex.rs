//! CLI-level tests for `acos run-dex`.

use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::prelude::*;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn compile_unsupported() -> Option<PathBuf> {
    let root = repo_root();
    let out = root.join("target/tmp/acos-java-cases/UnsupportedMain");
    let dex = out.join("classes.dex");
    if dex.is_file() {
        return Some(dex);
    }
    let javac = which("javac")?;
    let sdk = std::env::var("ANDROID_HOME").unwrap_or_else(|_| "/home/Qui/Android/Sdk".to_string());
    let d8 = PathBuf::from(format!("{sdk}/build-tools/35.0.0/d8"));
    let android_jar = PathBuf::from(format!("{sdk}/platforms/android-34/android.jar"));
    if !d8.is_file() || !android_jar.is_file() {
        return None;
    }
    let src = root.join("crates/acos-vm/tests/java/UnsupportedMain.java");
    let classes = out.join("classes");
    std::fs::create_dir_all(&classes).ok()?;
    if !Command::new(javac)
        .args(["--release", "11", "-d"])
        .arg(&classes)
        .arg(&src)
        .status()
        .ok()?
        .success()
    {
        return None;
    }
    let mut class_files: Vec<PathBuf> = std::fs::read_dir(&classes)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "class").unwrap_or(false))
        .collect();
    class_files.sort();
    if !Command::new(d8)
        .arg("--lib")
        .arg(&android_jar)
        .args(["--min-api", "26", "--output"])
        .arg(&out)
        .args(&class_files)
        .status()
        .ok()?
        .success()
    {
        return None;
    }
    Some(dex)
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

#[test]
fn unsupported_run_reports_ok_false() {
    let Some(dex) = compile_unsupported() else {
        eprintln!("SKIP unsupported_run_reports_ok_false: javac/d8 unavailable");
        return;
    };
    let output = Command::cargo_bin("acos")
        .unwrap()
        .args(["run-dex"])
        .arg(&dex)
        .args(["--main", "UnsupportedMain", "--json"])
        .output()
        .unwrap();
    assert!(!output.status.success(), "must not exit zero");
    let v: serde_json::Value = serde_json::from_slice(&output.stdout).expect("json envelope");
    assert_eq!(v["ok"], serde_json::json!(false));
    assert_eq!(v["status"], serde_json::json!("unsupported"));
    assert_eq!(
        v["error"]["code"],
        serde_json::json!("UNSUPPORTED_JAVA_API")
    );
    let api = v["error"]["details"]["api"].as_str().unwrap_or("");
    assert!(api.contains("java/util/concurrent/Executors"), "api={api}");
}

#[test]
fn fixture_run_json_is_ok() {
    let dex = repo_root().join("fixtures/build/dexprogs/13_exceptions/classes.dex");
    if !dex.is_file() {
        eprintln!("SKIP fixture_run_json_is_ok: fixtures/build absent");
        return;
    }
    let output = Command::cargo_bin("acos")
        .unwrap()
        .args(["run-dex"])
        .arg(&dex)
        .args(["--main", "Main", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let v: serde_json::Value = serde_json::from_slice(&output.stdout).expect("json envelope");
    assert_eq!(v["ok"], serde_json::json!(true));
    assert_eq!(v["status"], serde_json::json!("pass"));
    assert_eq!(v["backend"], serde_json::json!("acos-native"));
    assert_eq!(v["fidelity"], serde_json::json!("native-runtime"));
    assert_eq!(v["result"]["exit_code"], serde_json::json!(0));
    assert!(v["result"]["stdout"].as_str().unwrap().contains("done"));
    assert!(v["metrics"]["instructions"].as_u64().unwrap() > 0);
}
