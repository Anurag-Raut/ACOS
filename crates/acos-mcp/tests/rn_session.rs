//! Fixture-gated React Native product e2e (W162-A).
//!
//! Drives the real MCP `SessionBackend` (parent) → confined `acos-mcp` child →
//! [`acos_mcp::rn_backend::RnSessions`] → [`acos_runtime::RnSession`] path end
//! to end:
//!
//! 1. `acos_launch { apk }` structurally detects the RN APK, implies native, and
//!    mounts the legacy (Paper) bridge in the confined child;
//! 2. `acos_observe_tree` returns the UIManager op tree (`Hello ACOS`);
//! 3. `acos_tap { testID: "hello-button" }` re-enters the live looper and the
//!    refreshed tree says `count: 1`;
//! 4. `acos_tap { text: "TAP ME" }` resolves the RCTText to the same touchable;
//! 5. `acos_observe` returns the structured `UNSUPPORTED_RN_NO_RENDERER`;
//! 6. status/logs/reset/destroy flow through the same session table.
//!
//! Evidence: `results/rn/w162a-rn-session.json`.

mod common;

use std::path::PathBuf;

use acos_mcp::{BackendConfig, CliBackend, SessionBackend};
use acos_runtime::session::SessionOptions;
use common::{envelope_of, is_error, repo_root, McpClient};
use serde_json::{json, Value};

fn have(rel: &str) -> bool {
    acos_test_support::require_prereq(
        acos_test_support::have_fixture(rel),
        "rn_session",
        &format!("missing {rel} — build it with tools/build/build-apps.sh"),
    )
    .is_some()
}

fn prerequisites() -> Option<PathBuf> {
    if !have("fixtures/build/apps/rn-hello.apk")
        || !have("crates/acos-jni/tests/data/rn-bridge-probe.dex")
        || !have(".cache/boot/android-34/BOOTCLASSPATH")
    {
        return None;
    }
    Some(repo_root().join("fixtures/build/apps/rn-hello.apk"))
}

fn backend() -> SessionBackend {
    let cli = CliBackend::new(BackendConfig::for_binary("/nonexistent/acos-w162a"));
    SessionBackend::with_cli(cli, SessionOptions::default())
        .with_native_child_binary(PathBuf::from(env!("CARGO_BIN_EXE_acos-mcp")))
}

/// Every node text in a nested RN tree (roots → children).
fn collect_texts(nodes: &[Value], out: &mut Vec<String>) {
    for node in nodes {
        if let Some(text) = node.get("text").and_then(Value::as_str) {
            out.push(text.to_string());
        }
        if let Some(children) = node.get("children").and_then(Value::as_array) {
            collect_texts(children, out);
        }
    }
}

fn tree_texts(envelope: &Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(nodes) = envelope.pointer("/result/tree").and_then(Value::as_array) {
        collect_texts(nodes, &mut out);
    }
    out
}

fn nodes_of(envelope: &Value) -> usize {
    envelope
        .pointer("/result/nodes")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize
}

#[test]
fn rn_launch_observe_tree_tap_round_trip() {
    let Some(apk) = prerequisites() else {
        return;
    };
    let mut client = McpClient::start(backend(), common::test_config());
    client.initialize("2025-06-18");

    let launch = client.call_tool(
        "acos_launch",
        json!({
            "apk": apk,
            "timeout_ms": 180_000,
            "base": {"request_id": "req_rn_launch", "session_id": "s_rn"},
        }),
    );
    let launched = envelope_of(&launch);
    assert!(
        !is_error(&launch),
        "rn launch must not be a protocol error: {launched:#?}"
    );
    assert_eq!(
        launched["status"], "pass",
        "rn launch must reach the UIManager tree: {launched:#?}"
    );
    assert_eq!(
        launched["result"]["reached_stage"], "ui_tree",
        "rn launch ladder: {launched:#?}"
    );
    assert_eq!(
        launched["result"]["renderer"], false,
        "the RN bridge has no native view managers: {launched:#?}"
    );
    eprintln!(
        "E2E launch wall_ms={} events={}",
        launched["result"]["wall_ms"], launched["result"]["events"]
    );

    let tree = client.call_tool(
        "acos_observe_tree",
        json!({
            "base": {"request_id": "req_rn_tree", "session_id": "s_rn"},
        }),
    );
    let tree_env = envelope_of(&tree);
    assert_eq!(
        tree_env["status"], "pass",
        "rn observe --tree must pass: {tree_env:#?}"
    );
    assert_eq!(
        tree_env["result"]["react_native"], true,
        "the tree must be identified as React Native: {tree_env:#?}"
    );
    let texts = tree_texts(&tree_env);
    assert!(
        texts.iter().any(|t| t == "Hello ACOS"),
        "tree must contain 'Hello ACOS'; got {texts:?}"
    );
    assert!(
        texts.iter().any(|t| t == "TAP ME"),
        "tree must contain 'TAP ME'; got {texts:?}"
    );
    let first_nodes = nodes_of(&tree_env);
    assert!(first_nodes > 0, "tree must have nodes: {tree_env:#?}");

    // Real tap through the RN event dispatcher, addressed by testID.
    let tap_id = client.call_tool(
        "acos_tap",
        json!({
            "testID": "hello-button",
            "base": {"request_id": "req_rn_tap_id", "session_id": "s_rn"},
        }),
    );
    let tapped_id = envelope_of(&tap_id);
    assert_eq!(
        tapped_id["status"], "pass",
        "rn tap --testID must dispatch: {tapped_id:#?}"
    );
    assert_eq!(tapped_id["result"]["action"], "tap_test_id");
    assert!(
        tapped_id["result"]["tapped_tag"].as_i64().unwrap_or(-1) > 0,
        "hello-button must resolve to a tag: {tapped_id:#?}"
    );

    let after_id = client.call_tool(
        "acos_observe_tree",
        json!({
            "base": {"request_id": "req_rn_tree_after_id", "session_id": "s_rn"},
        }),
    );
    let after_id_env = envelope_of(&after_id);
    let texts = tree_texts(&after_id_env);
    assert!(
        texts.iter().any(|t| t == "count: 1"),
        "tap --testID must move the tree to 'count: 1'; got {texts:?}"
    );

    // The same touchable is reachable by text; the counter advances again.
    let tap_text = client.call_tool(
        "acos_tap",
        json!({
            "text": "TAP ME",
            "base": {"request_id": "req_rn_tap_text", "session_id": "s_rn"},
        }),
    );
    let tapped_text = envelope_of(&tap_text);
    assert_eq!(
        tapped_text["status"], "pass",
        "rn tap --text must dispatch: {tapped_text:#?}"
    );
    assert_eq!(tapped_text["result"]["action"], "tap_text");
    let after_text = client.call_tool(
        "acos_observe_tree",
        json!({
            "base": {"request_id": "req_rn_tree_after_text", "session_id": "s_rn"},
        }),
    );
    let after_text_env = envelope_of(&after_text);
    let texts = tree_texts(&after_text_env);
    assert!(
        texts.iter().any(|t| t == "count: 2"),
        "tap --text must move the tree to 'count: 2'; got {texts:?}"
    );

    // `observe --frame` is honestly unsupported, with the structured code.
    let observe = client.call_tool(
        "acos_observe",
        json!({
            "base": {"request_id": "req_rn_observe", "session_id": "s_rn"},
        }),
    );
    let observed = envelope_of(&observe);
    assert_eq!(
        observed["error"]["code"], "UNSUPPORTED_RN_NO_RENDERER",
        "observe --frame must be honestly unsupported: {observed:#?}"
    );

    // status / logs / reset / destroy flow through the same RN table.
    let status = client.call_tool(
        "acos_status",
        json!({"base": {"request_id": "req_rn_status", "session_id": "s_rn"}}),
    );
    let status_env = envelope_of(&status);
    assert_eq!(status_env["status"], "pass", "{status_env:#?}");
    assert_eq!(status_env["result"]["react_native"], true);
    assert_eq!(status_env["result"]["renderer"], false);
    assert_eq!(status_env["result"]["published_frames"], 0);

    let logs = client.call_tool(
        "acos_logs",
        json!({"base": {"request_id": "req_rn_logs", "session_id": "s_rn"}}),
    );
    let logs_env = envelope_of(&logs);
    assert_eq!(logs_env["status"], "pass", "{logs_env:#?}");
    assert!(
        logs_env["result"]["count"].as_u64().unwrap_or(0) > 0,
        "{logs_env:#?}"
    );

    let summary = json!({
        "nodes_initial": first_nodes,
        "nodes_after": nodes_of(&after_text_env),
        "tap_test_id_tag": tapped_id["result"]["tapped_tag"],
        "tap_text_tag": tapped_text["result"]["tapped_tag"],
        "launch_ms": launched["result"]["wall_ms"],
        "natives": launched["result"]["natives"],
        "real_boot": launched["result"]["real_boot"],
        "log_count": logs_env["result"]["count"],
        "frame_code": observed["error"]["code"],
    });
    let out_dir = acos_test_support::test_artifacts_dir("rn");
    let _ = std::fs::create_dir_all(&out_dir);
    let _ = std::fs::write(
        out_dir.join("w162a-rn-session.json"),
        serde_json::to_string_pretty(&json!({
            "summary": summary,
            "launch": launched["result"],
            "tree": tree_env["result"]["tree"],
            "after_test_id": after_id_env["result"]["tree"],
        }))
        .unwrap_or_default(),
    );

    let reset = client.call_tool(
        "acos_reset",
        json!({"base": {"request_id": "req_rn_reset", "session_id": "s_rn"}}),
    );
    let reset_env = envelope_of(&reset);
    assert_eq!(reset_env["status"], "pass", "{reset_env:#?}");
    assert_eq!(reset_env["result"]["state_reset"], true);

    let destroy = client.call_tool(
        "acos_destroy",
        json!({"base": {"request_id": "req_rn_destroy", "session_id": "s_rn"}}),
    );
    let destroyed = envelope_of(&destroy);
    assert_eq!(destroyed["status"], "pass", "{destroyed:#?}");
    assert!(
        destroyed["result"]["destroyed"] == true || destroyed["result"]["already_absent"] == true,
        "destroy must be idempotent and truthful: {destroyed:#?}"
    );
}
