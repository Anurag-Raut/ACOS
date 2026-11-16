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

/// The gitignored fixture APK the verify tests need; fail loudly (F-11) with
/// the exact build command rather than a confusing downstream error.
fn require_hello_views_apk() -> PathBuf {
    let apk = workspace().join("fixtures/build/apps/hello-views.apk");
    assert!(
        apk.exists(),
        "build fixtures first: `tools/build-apps.sh` (missing {})",
        apk.display()
    );
    apk
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
    let apk = require_hello_views_apk();
    let output = Command::cargo_bin("acos")
        .unwrap()
        .arg("verify")
        .arg(workspace().join("scenarios/negative/wrong-text.yaml"))
        .arg("--apk")
        .arg(&apk)
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

/// Run the same reference-replay verify twice into one `--artifacts` directory.
fn run_reference_verify(artifacts: &Path, extra: &[&str]) -> std::process::Output {
    let mut command = Command::cargo_bin("acos").unwrap();
    command
        .arg("verify")
        .arg(workspace().join("scenarios/negative/wrong-text.yaml"))
        .arg("--apk")
        .arg(require_hello_views_apk())
        .arg("--reference")
        .arg(workspace().join("fixtures/reference/hello-views/art-api34-x86_64"))
        .arg("--artifacts")
        .arg(artifacts)
        .arg("--json");
    for arg in extra {
        command.arg(arg);
    }
    command.output().expect("run acos verify")
}

#[test]
fn a_second_run_into_the_same_artifacts_dir_is_refused_until_overwrite() {
    let artifacts = scratch("owned");
    let first = run_reference_verify(&artifacts, &[]);
    assert!(!first.status.success(), "the negative scenario fails by design");
    let first_verdict: Value =
        serde_json::from_slice(&std::fs::read(artifacts.join("verdict.json")).unwrap())
            .expect("first run wrote a verdict");
    let first_run = first_verdict["request_id"]
        .as_str()
        .expect("a run id")
        .to_string();
    // The marker names the owner before any step runs.
    let marker: Value =
        serde_json::from_slice(&std::fs::read(artifacts.join("run.json")).unwrap())
            .expect("run.json");
    assert_eq!(marker["request_id"], first_run.as_str());

    // A second run into the same directory is a structured refusal, not a
    // silent interleave of two runs' steps/frames.
    let second = run_reference_verify(&artifacts, &[]);
    assert!(!second.status.success(), "the second run must fail");
    let envelope: Value = serde_json::from_slice(&second.stdout).expect("JSON envelope");
    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["error"]["code"], "INVALID_ARGUMENT");
    let details = &envelope["error"]["details"];
    assert_eq!(details["previous_run_id"], first_run.as_str());
    assert_eq!(details["overwrite"], false);
    assert!(
        envelope["error"]["message"]
            .as_str()
            .unwrap()
            .contains(&first_run),
        "the refusal names the previous run: {}",
        envelope["error"]["message"]
    );
    // The refused run wrote nothing: the first run's evidence is intact and
    // still the directory's verdict.
    let after_refusal: Value =
        serde_json::from_slice(&std::fs::read(artifacts.join("verdict.json")).unwrap()).unwrap();
    assert_eq!(after_refusal["request_id"], first_run.as_str());

    // `--overwrite` is the explicit consent: the old evidence is replaced and
    // the new marker records which run it replaced.
    let third = run_reference_verify(&artifacts, &["--overwrite"]);
    let third_envelope: Value = serde_json::from_slice(&third.stdout).expect("JSON envelope");
    assert_eq!(third_envelope["status"], "fail"); // the scenario fails by design
    let third_verdict: Value =
        serde_json::from_slice(&std::fs::read(artifacts.join("verdict.json")).unwrap()).unwrap();
    assert_ne!(third_verdict["request_id"], first_run.as_str());
    assert_eq!(
        third_verdict["artifacts_overwrote_run"],
        first_run.as_str()
    );
    let marker: Value =
        serde_json::from_slice(&std::fs::read(artifacts.join("run.json")).unwrap()).unwrap();
    assert_eq!(marker["overwrote_run_id"], first_run.as_str());
    assert_eq!(
        marker["request_id"],
        third_verdict["request_id"].as_str().unwrap()
    );
    let _ = std::fs::remove_dir_all(&artifacts);
}

#[test]
fn json_stdout_is_one_document_and_human_diagnostics_go_to_stderr() {
    let artifacts = scratch("stdout-only");
    let output = run_reference_verify(&artifacts, &[]);
    // stdout: exactly one JSON document, nothing else.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let envelope: Value = serde_json::from_str(stdout.trim()).expect("stdout is exactly JSON");
    assert_eq!(envelope["status"], "fail");
    assert!(
        !stdout.contains("verify: fail"),
        "no human summary on stdout in --json mode: {stdout}"
    );
    // stderr: the human line a triaging agent reads, including the human
    // selector form (never a Rust Debug dump).
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("verify: ASSERTION_FAILED"), "{stderr}");
    assert!(
        !stderr.contains("Selector {") && !stderr.contains("TextMatch {"),
        "no Rust Debug dump on stderr: {stderr}"
    );
    let _ = std::fs::remove_dir_all(&artifacts);
}

#[test]
fn verify_without_apk_uses_the_scenario_app() {
    let artifacts = scratch("no-apk");
    let _ = require_hello_views_apk();
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

/// FB4 follow-up: without `--artifacts` the run writes `out/<scenario stem>`,
/// and the rerun loop replaces the previous run there (recorded), instead of
/// being refused on the second iteration.
#[test]
fn a_rerun_into_the_default_artifacts_dir_replaces_the_previous_run() {
    let cwd = scratch("default-dir");
    std::fs::create_dir_all(&cwd).unwrap();
    let run = || {
        Command::cargo_bin("acos")
            .unwrap()
            .current_dir(&cwd)
            .arg("verify")
            .arg(workspace().join("scenarios/negative/wrong-text.yaml"))
            .arg("--apk")
            .arg(require_hello_views_apk())
            .arg("--reference")
            .arg(workspace().join("fixtures/reference/hello-views/art-api34-x86_64"))
            .arg("--json")
            .output()
            .expect("run acos verify")
    };
    let dir = cwd.join("out/wrong-text");
    let first = run();
    assert!(!first.status.success(), "the negative scenario fails by design");
    let first_run = serde_json::from_slice::<Value>(&std::fs::read(dir.join("verdict.json")).unwrap())
        .unwrap()["request_id"]
        .as_str()
        .unwrap()
        .to_string();
    let second = run();
    let envelope: Value = serde_json::from_slice(&second.stdout).expect("JSON envelope");
    assert_eq!(envelope["status"], "fail", "a rerun is a real run, not a refusal: {envelope}");
    assert_ne!(envelope["error"]["code"], "INVALID_ARGUMENT");
    let verdict: Value = serde_json::from_slice(&std::fs::read(dir.join("verdict.json")).unwrap()).unwrap();
    assert_ne!(verdict["request_id"], first_run.as_str());
    assert_eq!(verdict["artifacts_overwrote_run"], first_run.as_str());
    let _ = std::fs::remove_dir_all(&cwd);
}

/// A `wait_for` timeout must name the selector in the human form
/// (`text=="Nonexistent"`), never the Rust `Debug` dump
/// (`Selector { id: None, text: Some(TextMatch { ... }) }`) that an agent first
/// read as an internal object it could not act on. The same message is in the
/// envelope's `error.message`, so stdout and stderr agree.
#[test]
fn a_wait_for_timeout_names_the_selector_humanly() {
    let scratch = scratch("wait-for-human-selector");
    std::fs::create_dir_all(&scratch).expect("scratch");
    let scenario = scratch.join("wait-for-human-selector.yaml");
    std::fs::write(
        &scenario,
        "version: 1\n\
         name: wait-for-human-selector\n\
         description: wait_for on a text the fixture never renders\n\
         app: fixtures/build/apps/hello-views.apk\n\
         engine: reference-only\n\
         fallback: forbid\n\
         api_level: 34\n\
         isolation: process\n\
         display: {mode: offscreen, width: 1080, height: 2400, density_dpi: 420}\n\
         steps:\n\
         \x20 - launch:\n\
         \x20     activity: dev.acos.fixtures.helloviews/.MainActivity\n\
         \x20     clear_state: true\n\
         \x20 - wait_for:\n\
         \x20     selector: {text: {exact: Nonexistent}}\n\
         \x20     timeout_ms: 50\n",
    )
    .expect("write scenario");
    let artifacts = scratch.join("artifacts");
    let output = Command::cargo_bin("acos")
        .unwrap()
        .current_dir(workspace())
        .arg("verify")
        .arg(&scenario)
        .arg("--apk")
        .arg(require_hello_views_apk())
        .arg("--reference")
        .arg(workspace().join("fixtures/reference/hello-views/art-api34-x86_64"))
        .arg("--artifacts")
        .arg(&artifacts)
        .arg("--json")
        .output()
        .expect("run acos verify");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let envelope: Value = serde_json::from_str(stdout.trim()).expect("stdout is exactly JSON");
    assert_ne!(envelope["status"], "pass", "{envelope:#?}");
    let message = envelope
        .pointer("/error/message")
        .and_then(Value::as_str)
        .expect("a wait_for timeout carries an error message");
    assert!(
        message.contains("text==\"Nonexistent\""),
        "the selector must be human: {message}"
    );
    assert!(
        !message.contains("Selector {") && !message.contains("TextMatch {"),
        "no Rust Debug dump in the error: {message}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("text==\"Nonexistent\""),
        "the human summary names the selector: {stderr}"
    );
    assert!(
        !stderr.contains("Selector {") && !stderr.contains("TextMatch {"),
        "no Rust Debug dump on stderr: {stderr}"
    );
    let _ = std::fs::remove_dir_all(&scratch);
}
