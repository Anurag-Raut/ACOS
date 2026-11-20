//! ADR-0029 item 4: `acos launch --service` validates its spec before a session
//! exists and refuses on a route with no service table instead of pretending.
//!
//! Both cases are decided before any guest starts (no ART session, no daemon),
//! so this is a fast, deterministic test of the CLI contract; the attached set
//! on a live ART session is covered by the ART evidence run.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn counter_apk() -> PathBuf {
    workspace().join("fixtures/build/apps/counter.apk")
}

fn run(args: &[&str]) -> (i32, Value, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_acos"))
        .args(args)
        .current_dir(workspace())
        .output()
        .expect("run acos");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let json = serde_json::from_str::<Value>(&stdout).unwrap_or(Value::Null);
    (
        output.status.code().unwrap_or(-1),
        json,
        format!("stdout={stdout} stderr={stderr}"),
    )
}

#[test]
fn a_bad_service_spec_is_invalid_argument_before_any_session() {
    let apk = counter_apk();
    assert!(apk.is_file(), "fixture missing: {}", apk.display());
    let (code, json, log) = run(&[
        "launch",
        apk.to_str().unwrap(),
        "--service",
        "feedback.vibrator:rela",
        "--json",
    ]);
    assert_ne!(code, 0, "{log}");
    assert_eq!(
        json.pointer("/error/code").and_then(Value::as_str),
        Some("INVALID_ARGUMENT"),
        "{log}"
    );
    assert!(
        json.pointer("/error/details/field").and_then(Value::as_str) == Some("service"),
        "{log}"
    );
}

#[test]
fn a_service_on_the_in_process_route_is_refused_not_ignored() {
    let apk = counter_apk();
    assert!(apk.is_file(), "fixture missing: {}", apk.display());
    let (code, json, log) = run(&[
        "launch",
        apk.to_str().unwrap(),
        "--service",
        "feedback.vibrator:stub",
        "--json",
    ]);
    assert_ne!(code, 0, "{log}");
    assert_eq!(
        json.pointer("/error/code").and_then(Value::as_str),
        Some("UNSUPPORTED_BACKEND"),
        "{log}"
    );
    let message = json
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(message.contains("session host"), "{log}");
}
