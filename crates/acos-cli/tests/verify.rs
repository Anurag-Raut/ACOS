//! `acos verify` CLI integration: real process, real artifacts, envelope schema.
//!
//! The reference-replay backend is used so the test is deterministic and does not
//! need the framework boot classpath. The output is validated against the
//! checked-in `envelope.json` JSON Schema (a small subset interpreter) and then
//! through the protocol crate's validating deserializer.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use assert_cmd::Command;
use serde_json::Value;

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn scratch(name: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("acos-cli-verify-{}-{n}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn json_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// A deliberately small JSON Schema check: `type`, `enum`, `required` and
/// `properties`. It is enough to catch an envelope that drifts from the contract.
fn assert_matches_schema(schema: &Value, value: &Value, path: &str) {
    if let Some(enum_values) = schema.get("enum").and_then(Value::as_array) {
        assert!(
            enum_values.iter().any(|candidate| candidate == value),
            "{path}: {value} is not one of {enum_values:?}"
        );
    }
    if let Some(schema_type) = schema.get("type") {
        let allowed: Vec<&str> = match schema_type {
            Value::String(s) => vec![s.as_str()],
            Value::Array(items) => items.iter().filter_map(Value::as_str).collect(),
            _ => Vec::new(),
        };
        if !allowed.is_empty() {
            assert!(
                allowed.contains(&json_type(value)),
                "{path}: expected type {allowed:?}, got {}",
                json_type(value)
            );
        }
    }
    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        for key in required.iter().filter_map(Value::as_str) {
            assert!(
                value.get(key).is_some(),
                "{path}: missing required property {key:?}"
            );
        }
    }
    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        for (key, subschema) in properties {
            if let Some(child) = value.get(key) {
                assert_matches_schema(subschema, child, &format!("{path}.{key}"));
            }
        }
    }
}

#[test]
fn verify_json_matches_the_envelope_schema() {
    let artifacts = scratch("envelope");
    let output = Command::cargo_bin("acos")
        .unwrap()
        .arg("verify")
        .arg(workspace().join("scenarios/negative/wrong-text.yaml"))
        .arg("--apk")
        .arg(workspace().join("fixtures/build/apps/hello-views.apk"))
        .arg("--reference")
        .arg(workspace().join("fixtures/reference/hello-views/art-api34-x86_64"))
        .arg("--artifacts")
        .arg(&artifacts)
        .arg("--json")
        .output()
        .expect("run acos verify");

    // A fail verdict is still a successful command invocation with JSON output.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let value: Value = serde_json::from_str(&stdout).expect("stdout is JSON");
    assert_eq!(value["status"], "fail");
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "ASSERTION_FAILED");

    let schema: Value = serde_json::from_str(
        &std::fs::read_to_string(
            workspace().join("crates/acos-protocol/schema/envelope.json"),
        )
        .expect("read schema"),
    )
    .expect("schema JSON");
    assert_matches_schema(&schema, &value, "$");

    // The protocol crate's validating deserializer is the authoritative check.
    let envelope: acos_protocol::Envelope =
        serde_json::from_value(value.clone()).expect("envelope invariants hold");
    assert_eq!(envelope.status(), acos_protocol::Status::Fail);
    assert!(envelope.result().is_some());

    // The artifacts directory must be internally consistent.
    assert!(artifacts.join("verdict.json").is_file());
    assert!(artifacts.join("steps/00-launch.json").is_file());

    let _ = std::fs::remove_dir_all(&artifacts);
}

#[test]
fn verify_without_apk_uses_the_scenario_app() {
    let artifacts = scratch("no-apk");
    let output = Command::cargo_bin("acos")
        .unwrap()
        .current_dir(workspace())
        .arg("verify")
        .arg("scenarios/negative/wrong-text.yaml")
        .arg("--reference")
        .arg("fixtures/reference/hello-views/art-api34-x86_64")
        .arg("--artifacts")
        .arg(&artifacts)
        .arg("--json")
        .output()
        .expect("run acos verify");
    // Run from the workspace root so the scenario's relative `app` resolves.
    let value: Value = serde_json::from_slice(&output.stdout).expect("stdout is JSON");
    assert_eq!(value["status"], "fail");
    let _ = std::fs::remove_dir_all(&artifacts);
}
