//! F11: an MCP reply must stay small even when the run's error text is huge.
//!
//! A single failing `acos_verify` on a React Native APK produced a ~14k-token
//! reply: a 200+ frame java tail and a 400+ line native tail, repeated three
//! times (top-level `error`, `result.steps[0].error` and
//! `result.verdict.failure.error`). This file drives the whole server with a
//! fake `acos` binary (`tests/fixtures/bounded-error.sh`) that emits exactly
//! that shape, so the bound is proven end to end, not only in the unit tests of
//! `acos_mcp::bound`.

mod common;

use std::path::{Path, PathBuf};

use common::*;
use serde_json::{json, Value};

fn scratch(name: &str) -> PathBuf {
    let dir = repo_root().join("target/acos-mcp-bounded").join(format!(
        "{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    dir
}

fn huge_error_envelope() -> String {
    let mut java = String::from("java.lang.IllegalStateException: RN setup failed\n");
    for i in 0..220 {
        java.push_str(&format!(
            "\tat com.facebook.react.Frame{i}.run(Frame{i}.java:{i})\n"
        ));
    }
    let mut native = String::from("\n--- native tail ---\n");
    for i in 0..420 {
        native.push_str(&format!("#{i:02} 0x0000{i:04x} libhermes.so+0x{i:x}\n"));
    }
    let message = format!(
        "JAVA_EXCEPTION: the app threw during SoLoader.init\n{java}{native}\tcaused by: \
         java.lang.UnsatisfiedLinkError: no hermes in java.library.path\n"
    );
    json!({
        "request_id": "req_bounded",
        "ok": false,
        "status": "fail",
        "backend": "acos-native",
        "fidelity": "native-runtime",
        "capabilities_used": ["lifecycle"],
        "error": {"code": "JAVA_EXCEPTION", "message": message, "retryable": false},
        "result": {
            "verdict_path": "out/gaten/verdict.json",
            "artifacts_dir": "out/gaten",
            "steps": [{"index": 1, "status": "fail",
                       "error": {"code": "JAVA_EXCEPTION", "message": message}}],
            "verdict": {"failure": {"error": {"code": "JAVA_EXCEPTION", "message": message}}}
        },
        "metrics": {"wall_ms": 12}
    })
    .to_string()
}

/// Start a server whose `acos` is the huge-error stub, writing its envelope
/// into `dir`.
fn client_for_stub(dir: &Path) -> McpClient {
    let script = repo_root().join("crates/acos-mcp/tests/fixtures/bounded-error.sh");
    ensure_executable(&script);
    let envelope_file = dir.join("envelope.json");
    std::fs::write(&envelope_file, huge_error_envelope()).expect("envelope");
    let backend = backend_for(
        &script,
        vec![(
            "ACOS_STUB_JSON".to_string(),
            envelope_file.display().to_string(),
        )],
    );
    McpClient::start(backend, test_config())
}

fn verify_args(dir: &Path) -> Value {
    json!({
        "base": {"request_id": "t_bounded"},
        "scenario": fixture_path("scenarios/gaten-home.yaml"),
        "artifacts": format!("{}/artifacts", dir.display()),
    })
}

#[test]
fn a_huge_error_tail_is_capped_once_with_the_full_text_on_disk() {
    let dir = scratch("capped");
    let mut client = client_for_stub(&dir);
    client.initialize("2025-06-18");

    let response = client.call_tool("acos_verify", verify_args(&dir));
    assert!(is_error(&response), "a fail envelope is an error result");
    let structured = response
        .pointer("/result/structuredContent")
        .expect("structuredContent")
        .clone();
    let raw_text = response
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .expect("content text");

    // Before the bound this reply was ~150 KB of stack tails.
    assert!(
        raw_text.len() < 24_000,
        "the reply is {} bytes; the bound did not apply",
        raw_text.len()
    );

    // The head keeps the diagnosis; the tail keeps the root cause.
    let message = structured
        .pointer("/error/message")
        .and_then(Value::as_str)
        .expect("error.message");
    assert!(
        message.starts_with("JAVA_EXCEPTION: the app threw"),
        "{message}"
    );
    assert!(message.contains("caused by: java.lang.UnsatisfiedLinkError"));
    assert!(message.contains("ACOS MCP: /error/message bounded for size"));
    // ... and it names where the whole text still is.
    assert!(message.contains("out/gaten/verdict.json"), "{message}");
    assert!(
        message.len() < 6000,
        "bounded message is {} bytes",
        message.len()
    );

    // The full text appears once: the other two copies are back-references.
    for pointer in [
        "/result/steps/0/error/message",
        "/result/verdict/failure/error/message",
    ] {
        let copy = structured
            .pointer(pointer)
            .and_then(Value::as_str)
            .expect("repeated message");
        assert!(
            copy.starts_with("[ACOS MCP: identical "),
            "repeated tail must be a back-reference, got: {copy}"
        );
        assert!(copy.contains("/error/message"));
    }

    // The reply says it was bounded, and by how much.
    let bounds = response
        .pointer("/result/_meta/acos_mcp/reply_bounded")
        .expect("reply_bounded meta");
    assert_eq!(bounds["truncated_strings"], 1);
    assert_eq!(bounds["duplicate_strings"], 2);
    assert_eq!(bounds["cap_bytes"], 4000);
    assert!(
        bounds["saved_bytes"].as_u64().unwrap() > 40_000,
        "the reply should shrink by tens of KB: {}",
        bounds["saved_bytes"]
    );

    // The bound is an MCP-reply concern only: the CLI's own output is intact
    // (the full tails are still there for anyone reading the artifacts).
    let on_disk = std::fs::read_to_string(dir.join("envelope.json")).unwrap();
    let parsed: Value = serde_json::from_str(&on_disk).expect("envelope json");
    let disk_message = parsed
        .pointer("/error/message")
        .and_then(Value::as_str)
        .expect("disk error.message");
    assert!(!disk_message.contains("bounded for size"));
    assert!(disk_message.contains("caused by: java.lang.UnsatisfiedLinkError"));
    assert!(disk_message.len() > 20_000);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_cap_is_configurable_and_zero_disables_it() {
    let dir = scratch("unbounded");
    let script = repo_root().join("crates/acos-mcp/tests/fixtures/bounded-error.sh");
    ensure_executable(&script);
    let envelope_file = dir.join("envelope.json");
    std::fs::write(&envelope_file, huge_error_envelope()).expect("envelope");
    let backend = backend_for(
        &script,
        vec![(
            "ACOS_STUB_JSON".to_string(),
            envelope_file.display().to_string(),
        )],
    );
    // `ACOS_MCP_MAX_TEXT=0` and `max_text_bytes = 0` mean "do not bound" (the
    // CLI behaviour) — an escape hatch for a caller that wants everything.
    let mut config = test_config();
    config.max_text_bytes = 0;
    let mut client = McpClient::start(backend, config);
    client.initialize("2025-06-18");
    let response = client.call_tool("acos_verify", verify_args(&dir));
    assert!(
        response.pointer("/result/_meta").is_none(),
        "no bound, no meta"
    );
    let message = response
        .pointer("/result/structuredContent/error/message")
        .and_then(Value::as_str)
        .expect("error.message");
    assert!(!message.contains("bounded for size"));
    assert!(message.len() > 20_000);
    let _ = std::fs::remove_dir_all(&dir);
}

/// F10: `flags` are documented `NAME=VALUE` metadata that reach the CLI as
/// `--flag NAME=VALUE` and are recorded in the reproducer; a malformed entry
/// (`--native`, `engine=`) is a structured `INVALID_ARGUMENT` and the run never
/// starts.
#[test]
fn flags_are_name_value_metadata_and_malformed_ones_are_invalid_argument() {
    let dir = scratch("flags");
    let script = repo_root().join("crates/acos-mcp/tests/fixtures/bounded-error.sh");
    ensure_executable(&script);
    let envelope_file = dir.join("envelope.json");
    std::fs::write(&envelope_file, huge_error_envelope()).expect("envelope");
    let argv_file = dir.join("argv.log");
    let backend = backend_for(
        &script,
        vec![
            (
                "ACOS_STUB_JSON".to_string(),
                envelope_file.display().to_string(),
            ),
            ("ACOS_STUB_ARGV".to_string(), argv_file.display().to_string()),
        ],
    );
    let mut config = test_config();
    config.max_text_bytes = 0;
    let mut client = McpClient::start(backend, config);
    client.initialize("2025-03-26");

    // A documented value: the flag reaches the CLI unchanged.
    let mut args = verify_args(&dir);
    args["flags"] = json!(["engine=art", "variant.pre-fix=1"]);
    let response = client.call_tool("acos_verify", args);
    // The stub's own envelope is a `fail` (that is the point of the fixture):
    // what matters here is that the request was accepted and dispatched.
    let envelope = envelope_of(&response);
    assert_ne!(envelope["error"]["code"], "INVALID_ARGUMENT");
    let argv = std::fs::read_to_string(&argv_file).expect("argv recorded");
    assert!(argv.contains("--flag engine=art"), "{argv}");
    assert!(argv.contains("--flag variant.pre-fix=1"), "{argv}");
    std::fs::remove_file(&argv_file).ok();

    // Malformed values never reach the CLI.
    for (flag, needle) in [
        ("--native", "NAME=VALUE"),
        ("engine", "NAME=VALUE"),
        ("engine=", "empty value"),
        ("=art", "empty name"),
    ] {
        let mut args = verify_args(&dir);
        args["flags"] = json!([flag]);
        let response = client.call_tool("acos_verify", args);
        assert!(is_error(&response), "{flag} must be rejected");
        let envelope = envelope_of(&response);
        assert_eq!(envelope["error"]["code"], "INVALID_ARGUMENT", "{flag}");
        assert_eq!(envelope["status"], "error");
        assert_eq!(envelope["error"]["details"]["field"], "flags");
        assert_eq!(envelope["error"]["details"]["index"], 0);
        assert_eq!(envelope["error"]["details"]["value"], flag);
        assert!(
            envelope["error"]["details"]["reason"]
                .as_str()
                .unwrap()
                .contains(needle),
            "{flag}: {}",
            envelope["error"]["details"]["reason"]
        );
    }
    assert!(
        !argv_file.exists(),
        "a malformed flag must not start the backend"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
