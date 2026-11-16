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

/// PROVENANCE + the single engine default: `acos verify` from a cwd with a
/// **relative** scenario path prints a summary line and writes a verdict that
/// both name the absolute scenario and APK with their 12-character digests, the
/// engine and the fidelity. Before this, `verify: pass` named neither input, so
/// a mistyped scenario path could run an older scenario against a stale APK and
/// look correct (feedback 2026-11-14).
#[test]
fn the_summary_line_and_verdict_name_the_scenario_and_apk() {
    let apk = require_hello_views_apk();
    let artifacts = scratch("provenance");
    let output = Command::cargo_bin("acos")
        .unwrap()
        .current_dir(workspace())
        .arg("verify")
        // Deliberately relative: the tool must report it absolutely.
        .arg("scenarios/negative/wrong-text.yaml")
        .arg("--apk")
        .arg(&apk)
        .arg("--reference")
        .arg("fixtures/reference/hello-views/art-api34-x86_64")
        .arg("--artifacts")
        .arg(&artifacts)
        .output()
        .expect("run acos verify");

    let scenario = workspace().join("scenarios/negative/wrong-text.yaml");
    let scenario = std::fs::canonicalize(&scenario).unwrap_or(scenario);
    let apk = std::fs::canonicalize(&apk).unwrap_or(apk);

    // 1. The human summary line (text output, no --json).
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&format!("provenance: scenario {}", scenario.display())),
        "the summary must name the scenario: {stdout}"
    );
    assert!(
        stdout.contains(&format!("apk {}", apk.display())),
        "the summary must name the APK: {stdout}"
    );
    assert!(stdout.contains("(sha "), "the summary must carry digests: {stdout}");
    assert!(stdout.contains("engine rust-vm"), "{stdout}");
    assert!(stdout.contains("fidelity "), "{stdout}");

    // 2. verdict.json carries the same block, with absolute paths.
    let verdict: Value =
        serde_json::from_slice(&std::fs::read(artifacts.join("verdict.json")).unwrap())
            .expect("verdict.json");
    let provenance = &verdict["provenance"];
    assert_eq!(
        provenance["scenario"]["path"],
        serde_json::json!(scenario.display().to_string()),
        "{provenance}"
    );
    assert_eq!(
        provenance["apk"]["path"],
        serde_json::json!(apk.display().to_string()),
        "{provenance}"
    );
    for file in ["scenario", "apk"] {
        let sha = provenance[file]["sha256"].as_str().expect("sha256");
        assert_eq!(sha.len(), 64);
        assert_eq!(
            provenance[file]["sha256_12"],
            serde_json::json!(&sha[..12]),
            "{provenance}"
        );
    }
    assert_eq!(provenance["engine"], "rust-vm", "{provenance}");
    assert_eq!(verdict["reproducer"]["scenario_path"], provenance["scenario"]["path"]);
    assert!(
        std::path::Path::new(
            verdict["reproducer"]["scenario_path"]
                .as_str()
                .expect("scenario path")
        )
        .is_absolute(),
        "repro.json must not store a relative scenario path: {}",
        verdict["reproducer"]
    );
    let _ = std::fs::remove_dir_all(&artifacts);
}

/// The CLI already rejects an unknown flag (clap), which the MCP side now also
/// does for arguments: one rule for both front doors.
#[test]
fn an_unknown_flag_is_a_usage_error_not_silently_ignored() {
    let output = Command::cargo_bin("acos")
        .unwrap()
        .arg("verify")
        .arg("scenarios/counter.yaml")
        .arg("--screenshot")
        .output()
        .expect("run acos verify");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--screenshot"), "{stderr}");
    assert!(
        stderr.contains("unexpected argument") || stderr.contains("unknown"),
        "{stderr}"
    );
    assert!(
        stderr.contains("--scenario"),
        "clap suggests a real flag: {stderr}"
    );
}

/// The `--engine` default in `--help` is the constant the code uses.
#[test]
fn the_engine_default_in_help_is_the_one_constant() {
    let output = Command::cargo_bin("acos")
        .unwrap()
        .args(["verify", "--help"])
        .output()
        .expect("run acos verify --help");
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(
        help.contains(&format!(
            "[default: {}]",
            acos_protocol::commands::DEFAULT_VERIFY_ENGINE_NAME
        )),
        "help must print the constant default: {help}"
    );
    assert!(
        !help.contains("[default: rust-vm]"),
        "help must not claim a default the code does not use: {help}"
    );
}

/// `--reference` without `--engine` implies the replay engine; the run is then
/// **not** an explicit engine choice, and `engine.json` must say so.
///
/// The old `explicit` test compared the engine name with the build default, so
/// `--reference` (which selects `rust-vm` by implication) looked as if the caller
/// had asked for it. An explicit `--engine rust-vm` on the same command is
/// explicit, and the two runs must differ in exactly that field.
#[test]
fn reference_without_engine_is_not_an_explicit_engine_choice() {
    let apk = require_hello_views_apk();
    let reference = workspace().join("fixtures/reference/hello-views/art-api34-x86_64");
    let run = |artifacts: &Path, engine: Option<&str>| {
        let mut command = Command::cargo_bin("acos").unwrap();
        command
            .current_dir(workspace())
            .arg("verify")
            .arg("scenarios/negative/wrong-text.yaml")
            .arg("--apk")
            .arg(&apk)
            .arg("--reference")
            .arg(&reference)
            .arg("--artifacts")
            .arg(artifacts)
            .arg("--json");
        if let Some(engine) = engine {
            command.arg("--engine").arg(engine);
        }
        let output = command.output().expect("run acos verify");
        let engine_json: Value = serde_json::from_slice(
            &std::fs::read(artifacts.join("engine.json"))
                .unwrap_or_else(|error| panic!("engine.json: {error}")),
        )
        .expect("engine.json parses");
        (output, engine_json)
    };

    let implied_dir = scratch("engine-implied");
    let (output, engine_json) = run(&implied_dir, None);
    assert!(
        !output.status.success(),
        "the negative scenario fails by design"
    );
    assert_eq!(engine_json["engine"]["engine"], "rust-vm", "{engine_json}");
    assert_eq!(
        engine_json["engine"]["explicit"], false,
        "--reference implies the replay engine; the caller did not choose it: {engine_json}"
    );

    let explicit_dir = scratch("engine-explicit");
    let (_, engine_json) = run(&explicit_dir, Some("rust-vm"));
    assert_eq!(engine_json["engine"]["engine"], "rust-vm", "{engine_json}");
    assert_eq!(
        engine_json["engine"]["explicit"], true,
        "an explicit --engine is explicit: {engine_json}"
    );

    let _ = std::fs::remove_dir_all(&implied_dir);
    let _ = std::fs::remove_dir_all(&explicit_dir);
}
