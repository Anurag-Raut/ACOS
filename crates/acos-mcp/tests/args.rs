//! ARGS: every MCP tool rejects an unknown argument, by name, with the valid
//! names in the reply (feedback 2026-11-15: `acos_observe_tree
//! {"screenshot": true}` looked like a success on a build that ignored it).
//!
//! Two rules, one source: the published schema says
//! `additionalProperties: false`, and the dispatcher rejects the call even when
//! a client did not validate. These tests drive the real server over pipes, so
//! both layers are exercised as an agent would hit them.

mod common;

use std::path::{Path, PathBuf};

use common::*;
use serde_json::{json, Value};

fn scratch(name: &str) -> PathBuf {
    let dir = repo_root()
        .join("target/acos-mcp-args")
        .join(format!("{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    dir
}

/// A server whose `acos` is the F11 stub: it answers the capability probe, so
/// `acos_verify` is advertised, and every tool call that needs the CLI is
/// irrelevant here because an unknown argument is refused before dispatch.
fn client(dir: &Path) -> McpClient {
    let script = repo_root().join("crates/acos-mcp/tests/fixtures/bounded-error.sh");
    ensure_executable(&script);
    let envelope = dir.join("envelope.json");
    std::fs::write(
        &envelope,
        r#"{"ok":true,"status":"pass","request_id":"stub"}"#,
    )
    .expect("stub");
    let mut server = McpClient::start(
        backend_for(
            &script,
            vec![("ACOS_STUB_JSON".to_string(), envelope.display().to_string())],
        ),
        test_config(),
    );
    server.initialize("2025-06-18");
    server
}

#[test]
fn every_advertised_tool_schema_is_a_closed_object() {
    let dir = scratch("closed");
    let mut server = client(&dir);
    let list = server.request("tools/list", json!({}));
    let tools = list
        .pointer("/result/tools")
        .and_then(Value::as_array)
        .expect("tools");
    assert!(!tools.is_empty());
    for tool in tools {
        let name = tool["name"].as_str().unwrap_or("?");
        assert_eq!(
            tool["inputSchema"]["additionalProperties"],
            json!(false),
            "`{name}` must publish additionalProperties: false: {}",
            tool["inputSchema"]
        );
        assert!(
            tool["inputSchema"]["properties"].is_object(),
            "`{name}` has no properties: {}",
            tool["inputSchema"]
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_unknown_argument_is_invalid_argument_not_a_silent_no_op() {
    let dir = scratch("unknown");
    let mut server = client(&dir);
    let response = server.call_tool(
        "acos_verify",
        json!({
            "scenario": "scenarios/counter.yaml",
            "artifacts": "target/acos-mcp-args/out",
            "screenshots": true,
        }),
    );
    assert!(is_error(&response), "{response}");
    let envelope = envelope_of(&response);
    let error = &envelope["error"];
    assert_eq!(error["code"], "INVALID_ARGUMENT", "{envelope}");
    assert!(
        error["message"]
            .as_str()
            .unwrap_or("")
            .contains("`screenshots`"),
        "the reply must name the argument: {error}"
    );
    let details = &error["details"];
    assert_eq!(details["unknown"], json!(["screenshots"]), "{details}");
    let valid = details["valid"].as_array().expect("valid arguments");
    let valid: Vec<&str> = valid.iter().filter_map(Value::as_str).collect();
    // The valid names come from the same schema `tools/list` publishes.
    for expected in ["scenario", "artifacts", "engine", "flags"] {
        assert!(valid.contains(&expected), "{valid:?}");
    }
    assert!(
        error["hint"].as_str().unwrap_or("").contains("`scenario`"),
        "the hint must list the valid names: {error}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_near_miss_that_started_this_is_named_in_one_retry() {
    let dir = scratch("near-miss");
    let mut server = client(&dir);
    // `screenshots` is one character from `screenshot` on the tool that has it.
    let response = server.call_tool("acos_observe_tree", json!({"screenshots": true}));
    assert!(is_error(&response), "{response}");
    let envelope = envelope_of(&response);
    assert_eq!(envelope["error"]["code"], "INVALID_ARGUMENT", "{envelope}");
    assert!(
        envelope["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("did you mean `screenshot`"),
        "{envelope}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_valid_call_reaches_the_tool_and_a_meta_tool_rejects_a_stray_argument() {
    let dir = scratch("valid");
    let mut server = client(&dir);
    // A meta tool needs no session: a valid topic still works …
    let ok = server.call_tool("acos_help", json!({"topic": "errors"}));
    assert!(!is_error(&ok), "{ok}");
    assert!(
        envelope_of(&ok)["result"]["errors"].is_object() || envelope_of(&ok)["result"].is_object(),
        "{ok}"
    );
    // … and the same tool refuses an argument it does not have.
    let bad = server.call_tool("acos_help", json!({"topic": "errors", "verbose": true}));
    assert!(is_error(&bad), "{bad}");
    let envelope = envelope_of(&bad);
    assert_eq!(envelope["error"]["code"], "INVALID_ARGUMENT", "{envelope}");
    assert_eq!(envelope["error"]["details"]["unknown"], json!(["verbose"]));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_verify_default_engine_schema_is_the_one_constant() {
    let dir = scratch("engine-default");
    let mut server = client(&dir);
    let list = server.request("tools/list", json!({}));
    let tool = tool(&list, "acos_verify").expect("acos_verify");
    assert_eq!(
        tool["inputSchema"]["properties"]["engine"]["default"],
        json!(acos_protocol::commands::DEFAULT_VERIFY_ENGINE_NAME),
        "{}",
        tool["inputSchema"]["properties"]["engine"]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn session_addressed_tools_do_not_require_base_and_destroy_accepts_empty_args() {
    let cli = acos_mcp::CliBackend::new(acos_mcp::BackendConfig::for_binary("/nonexistent/acos"));
    let mut server = McpClient::start(
        acos_mcp::SessionBackend::with_cli(cli, Default::default()),
        test_config(),
    );
    server.initialize("2025-06-18");
    let list = server.request("tools/list", json!({}));
    for name in acos_mcp::descriptors::SESSION_ADDRESSED_PROTOCOL_TOOLS {
        for schema in [
            acos_mcp::descriptors::tool_schema(name).expect("tool_schema"),
            tool(&list, name)
                .unwrap_or_else(|| panic!("{name} missing from tools/list"))["inputSchema"]
                .clone(),
        ] {
            let required: Vec<&str> = schema["required"]
                .as_array()
                .map(|items| items.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            assert!(
                !required.contains(&"base"),
                "{name} schema must not require `base`: {schema}"
            );
        }
    }
    let destroy_schema = acos_mcp::descriptors::tool_schema("acos_destroy").expect("schema");
    for args in [json!({}), json!({"session_id": "s_short"})] {
        assert!(
            acos_mcp::args::check("acos_destroy", &args, &destroy_schema).is_none(),
            "acos_destroy rejected {args}"
        );
        let reply = server.call_tool("acos_destroy", args.clone());
        assert!(!is_error(&reply), "acos_destroy {args} failed: {reply}");
    }
}
