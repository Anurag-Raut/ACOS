//! In-process MCP session round trip.
//!
//! Drives the real dispatcher with the [`SessionBackend`] (the backend the
//! `acos-mcp` binary installs). The session commands do not need the boot
//! classpath: `observe`/`tap`/`key`/`swipe`/`logs`/`status` report the honest
//! un-launched state, and `launch` of a non-APK is a fast structured error.

mod common;

use acos_mcp::{BackendConfig, CliBackend, SessionBackend};
use acos_runtime::session::SessionOptions;
use common::{envelope_of, is_error, repo_root, tool_names, McpClient};
use serde_json::json;

fn session_server() -> McpClient {
    // The session tools never call the CLI, so a missing binary is fine; the
    // CLI path is only used by the delegated one-shot commands.
    let cli = CliBackend::new(BackendConfig::for_binary("/nonexistent/acos-w34b"));
    McpClient::start(
        SessionBackend::with_cli(cli, SessionOptions::default()),
        common::test_config(),
    )
}

fn tool_call(client: &mut McpClient, name: &str, args: serde_json::Value) -> serde_json::Value {
    let response = client.call_tool(name, args);
    assert!(
        response.get("error").is_none(),
        "tool `{name}` returned a JSON-RPC error: {response}"
    );
    envelope_of(&response)
}

#[test]
fn session_tools_round_trip_in_process() {
    let mut client = session_server();
    client.initialize("2025-06-18");

    // The in-process session tools are advertised.
    let list = client.request("tools/list", json!({}));
    let names = tool_names(&list);
    for expected in [
        "acos_launch",
        "acos_observe",
        "acos_action",
        "acos_run",
        "acos_reset",
        "acos_destroy",
        "acos_tap",
        "acos_key",
        "acos_swipe",
        "acos_logs",
        "acos_status",
    ] {
        assert!(
            names.iter().any(|n| n == expected),
            "missing tool {expected}"
        );
    }

    // observe before any frame: structured `not_ready`, never a blank frame.
    let observe = tool_call(
        &mut client,
        "acos_observe",
        json!({"base": {"request_id": "req_o", "session_id": "s_mcp"}, "tree": true}),
    );
    assert_eq!(observe["status"], "unsupported");
    assert_eq!(observe["error"]["code"], "UNSUPPORTED_BACKEND");
    assert_eq!(observe["error"]["details"]["reason"], "not_ready");
    assert!(observe["result"].is_null());

    // The session persists across calls: status sees the same id.
    let status = tool_call(
        &mut client,
        "acos_status",
        json!({"base": {"request_id": "req_s", "session_id": "s_mcp"}}),
    );
    assert_eq!(status["ok"], true);
    assert_eq!(status["result"]["session_id"], "s_mcp");
    assert_eq!(status["result"]["launched"], false);

    // Input without a window / focus is a structured error.
    let tap = tool_call(
        &mut client,
        "acos_tap",
        json!({"x": 10, "y": 20, "base": {"request_id": "req_t", "session_id": "s_mcp"}}),
    );
    assert_eq!(tap["error"]["code"], "ELEMENT_NOT_FOUND");
    assert_eq!(tap["error"]["details"]["kind"], "NoWindowAtPoint");

    let key = tool_call(
        &mut client,
        "acos_key",
        json!({"code": "back", "base": {"request_id": "req_k", "session_id": "s_mcp"}}),
    );
    assert_eq!(key["error"]["details"]["kind"], "NoFocusedWindow");

    let swipe = tool_call(
        &mut client,
        "acos_swipe",
        json!({
            "from": {"x": 10, "y": 20},
            "to": {"x": 30, "y": 40},
            "base": {"request_id": "req_w", "session_id": "s_mcp"}
        }),
    );
    assert_eq!(swipe["error"]["details"]["kind"], "NoWindowAtPoint");

    // logs and reset are passes over the (empty) session.
    let logs = tool_call(
        &mut client,
        "acos_logs",
        json!({"base": {"request_id": "req_l", "session_id": "s_mcp"}}),
    );
    assert_eq!(logs["ok"], true);
    assert!(logs["result"]["logs"].is_array());

    let reset = tool_call(
        &mut client,
        "acos_reset",
        json!({"base": {"request_id": "req_r", "session_id": "s_mcp"}, "scope": "all"}),
    );
    assert_eq!(reset["ok"], true);
    assert_eq!(reset["result"]["state_reset"], true);
    assert_eq!(reset["result"]["data_cleared"], false);

    // destroy is idempotent.
    let destroy = tool_call(
        &mut client,
        "acos_destroy",
        json!({"base": {"request_id": "req_d", "session_id": "s_mcp"}}),
    );
    assert_eq!(destroy["result"]["destroyed"], true);
    let again = tool_call(
        &mut client,
        "acos_destroy",
        json!({"base": {"request_id": "req_d2", "session_id": "s_mcp"}}),
    );
    assert_eq!(again["result"]["already_absent"], true);
}

#[test]
fn launch_non_apk_is_an_error_not_a_pass() {
    let mut client = session_server();
    client.initialize("2025-06-18");

    let not_apk = repo_root().join("Cargo.toml");
    let response = client.call_tool(
        "acos_launch",
        json!({"apk": not_apk, "base": {"request_id": "req_bad", "session_id": "s_bad"}}),
    );
    let envelope = envelope_of(&response);
    assert!(is_error(&response), "a non-APK launch must be an error");
    assert_eq!(envelope["status"], "error");
    assert_eq!(envelope["error"]["code"], "APK_NOT_ZIP");
    assert!(envelope["result"].is_null());
}

#[test]
fn launch_without_apk_is_invalid_argument() {
    let mut client = session_server();
    client.initialize("2025-06-18");
    let envelope = tool_call(
        &mut client,
        "acos_launch",
        json!({"base": {"request_id": "req_none"}}),
    );
    assert_eq!(envelope["error"]["code"], "INVALID_ARGUMENT");
}
