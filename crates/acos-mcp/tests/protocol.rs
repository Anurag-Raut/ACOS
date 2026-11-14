//! Protocol-level tests driving the server in-process over pipes.

mod common;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use acos_protocol::{mcp_tools, CommandName};
use common::*;
use serde_json::{json, Value};

#[test]
fn initialize_handshake_negotiates_and_describes_server() {
    let mut client = McpClient::with_real_acos();
    let response = client.initialize("2025-03-26");
    let result = &response["result"];
    assert_eq!(result["protocolVersion"], "2025-03-26");
    assert_eq!(result["serverInfo"]["name"], "acos-mcp");
    assert_eq!(result["capabilities"]["tools"]["listChanged"], false);
    assert!(result["instructions"].as_str().unwrap().contains("inspect"));
}

#[test]
fn unknown_protocol_version_falls_back_to_latest() {
    let mut client = McpClient::with_real_acos();
    let response = client.initialize("1999-01-01");
    assert_eq!(response["result"]["protocolVersion"], "2025-03-26");
}

/// F12: `tools/list` advertises only what this build can run. A stub command
/// (`acos_capture`, `acos_assert`, `acos_events`, `acos_profile`,
/// `acos_capability_report`, and the protocol `acos_run`/`acos_reset`/
/// `acos_destroy` names the session tools cover) is not listed — an agent that
/// sees it tries it, pays a round trip and gets `unsupported`. Its **name stays
/// reserved**: calling it still returns the structured `unsupported` envelope,
/// and the protocol descriptors are unchanged (so a future build that
/// implements one only has to stop hiding it).
#[test]
fn tools_list_advertises_only_what_this_build_can_run() {
    let mut client = McpClient::with_real_acos();
    client.initialize("2025-03-26");
    let list = client.request("tools/list", json!({}));
    let names = tool_names(&list);

    // What the current `acos` binary really implements, plus the server tools.
    for expected in [
        "acos_inspect",
        "acos_run_dex",
        "acos_observe",
        "acos_verify",
        "acos_capabilities",
        "acos_help",
    ] {
        assert!(names.contains(&expected.to_string()), "missing {expected}");
    }
    for session in acos_mcp::descriptors::SESSION_TOOL_NAMES {
        assert!(names.contains(&session.to_string()), "missing {session}");
    }

    // Nothing advertised is a stub: every listed protocol tool is available.
    for tool in list
        .pointer("/result/tools")
        .and_then(Value::as_array)
        .expect("tools array")
    {
        assert_eq!(
            tool.pointer("/annotations/available"),
            Some(&json!(true)),
            "an unavailable tool was advertised: {tool}"
        );
        assert!(tool["description"]
            .as_str()
            .unwrap()
            .contains("available=true"));
    }

    // The stubs are hidden from the list ...
    for command in [
        CommandName::Run,
        CommandName::Capture,
        CommandName::Action,
        CommandName::Assert,
        CommandName::Events,
        CommandName::Reset,
        CommandName::Profile,
        CommandName::Destroy,
        CommandName::CapabilityReport,
    ] {
        assert!(
            !names.contains(&command.tool_name().to_string()),
            "{} is a stub and must not be advertised",
            command.as_str()
        );
    }

    // ... but every protocol name is still reserved and answers honestly.
    for descriptor in mcp_tools().tools {
        assert!(
            acos_mcp::descriptors::command_from_tool(&descriptor.name).is_some(),
            "the protocol descriptor {} must stay reserved",
            descriptor.name
        );
    }
    let response = client.call_tool(
        "acos_assert",
        json!({"base": {"request_id": "t_stub"}, "assertion": {"kind": "exists"}}),
    );
    assert!(is_error(&response));
    let envelope = envelope_of(&response);
    assert_eq!(envelope["status"], "unsupported");
    assert_eq!(envelope["error"]["code"], "UNSUPPORTED_BACKEND");
    assert_eq!(envelope["error"]["details"]["available"], false);

    // An available protocol tool keeps its live annotations.
    let inspect = tool(&list, "acos_inspect").unwrap();
    assert_eq!(inspect["annotations"]["available"], true);
    assert_eq!(inspect["annotations"]["fidelity"], "static-analysis");
    assert_eq!(inspect["_meta"]["acos"]["command"], "inspect");
    let verify = tool(&list, "acos_verify").unwrap();
    assert_eq!(verify["annotations"]["fidelity"], "native-runtime");
}

#[test]
fn inspect_fixture_returns_pass_with_static_fidelity() {
    let mut client = McpClient::with_real_acos();
    client.initialize("2025-03-26");
    let response = client.call_tool(
        "acos_inspect",
        json!({
            "base": {"request_id": "t_inspect"},
            "apk": fixture_path("fixtures/build/apps/hello-views.apk"),
        }),
    );
    assert!(!is_error(&response), "unexpected error: {response}");
    let envelope = envelope_of(&response);
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["status"], "pass");
    assert_eq!(envelope["fidelity"], "static-analysis");
    assert_eq!(envelope["backend"], "acos-native");
    assert_eq!(
        envelope["result"]["package"],
        "dev.acos.fixtures.helloviews"
    );
}

#[test]
fn run_dex_fixture_matches_expected_stdout() {
    let mut client = McpClient::with_real_acos();
    client.initialize("2025-03-26");
    let response = client.call_tool(
        "acos_run_dex",
        json!({
            "base": {"request_id": "t_rundex"},
            "path": fixture_path("fixtures/build/dexprogs/01_hello/classes.dex"),
            "main": "Main",
        }),
    );
    assert!(!is_error(&response), "unexpected error: {response}");
    let envelope = envelope_of(&response);
    assert_eq!(envelope["status"], "pass");
    assert_eq!(envelope["fidelity"], "native-runtime");
    assert_eq!(envelope["result"]["exit_code"], 0);
    let expected = fixture("fixtures/expected/dexprogs/01_hello.stdout");
    assert_eq!(envelope["result"]["stdout"].as_str().unwrap(), expected);
}

#[test]
fn unimplemented_command_returns_unsupported_never_a_pass() {
    let mut client = McpClient::with_real_acos();
    client.initialize("2025-03-26");
    let response = client.call_tool(
        "acos_run",
        json!({
            "base": {"request_id": "t_run"},
            "apk": fixture_path("fixtures/build/apps/hello-views.apk"),
        }),
    );
    assert!(is_error(&response));
    let envelope = envelope_of(&response);
    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["status"], "unsupported");
    assert_eq!(envelope["error"]["code"], "UNSUPPORTED_BACKEND");
    assert!(envelope["error"]["hint"].as_str().unwrap().contains("unsupported"));
    assert_eq!(envelope["error"]["details"]["available"], false);
}

#[test]
fn verify_returns_the_same_truthful_envelope_as_the_cli() {
    let mut client = McpClient::with_real_acos();
    client.initialize("2025-03-26");
    let artifacts = repo_root().join("target/acos-mcp-artifacts/verify-wrong-text");
    let _ = std::fs::remove_dir_all(&artifacts);
    let response = client.call_tool(
        "acos_verify",
        json!({
            "base": {"request_id": "t_verify"},
            "scenario": fixture_path("scenarios/negative/wrong-text.yaml"),
            "apk": fixture_path("fixtures/build/apps/hello-views.apk"),
            "reference": fixture_path("fixtures/reference/hello-views/art-api34-x86_64"),
            "artifacts": artifacts.display().to_string(),
        }),
    );
    assert!(is_error(&response), "unexpected: {response}");
    let envelope = envelope_of(&response);
    assert_eq!(envelope["status"], "fail");
    assert_eq!(envelope["error"]["code"], "ASSERTION_FAILED");
    assert_eq!(envelope["result"]["verdict"]["verdict"], "fail");
    let _ = std::fs::remove_dir_all(&artifacts);
}

#[test]
fn malformed_json_reports_parse_error() {
    let mut client = McpClient::with_real_acos();
    client.send_raw("{ this is not json");
    let response = client.recv();
    assert_eq!(response["error"]["code"], -32700);
    assert!(response["id"].is_null());
}

#[test]
fn oversized_request_is_rejected_without_crashing() {
    let mut config = test_config();
    config.max_request_bytes = 512;
    let mut client = McpClient::start(backend_for(&acos_bin(), Vec::new()), config);
    let long = format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\",\"params\":{{\"pad\":\"{}\"}}}}",
        "x".repeat(2048)
    );
    client.send_raw(&long);
    let response = client.recv();
    assert_eq!(response["error"]["code"], -32700);
    // The connection is still usable.
    client.initialize("2025-03-26");
    let pong = client.request("ping", json!({}));
    assert!(pong["result"].is_object());
}

#[test]
fn unknown_method_reports_method_not_found() {
    let mut client = McpClient::with_real_acos();
    client.initialize("2025-03-26");
    let response = client.request("does/not/exist", json!({}));
    assert_eq!(response["error"]["code"], -32601);
}

#[test]
fn invalid_params_are_rejected() {
    let mut client = McpClient::with_real_acos();
    client.initialize("2025-03-26");

    let missing_name = client.request("tools/call", json!({"arguments": {}}));
    assert_eq!(missing_name["error"]["code"], -32602);

    let unknown_tool = client.request(
        "tools/call",
        json!({"name": "acos_nope", "arguments": {}}),
    );
    assert_eq!(unknown_tool["error"]["code"], -32602);

    let bad_arguments = client.request(
        "tools/call",
        json!({"name": "acos_inspect", "arguments": [1, 2, 3]}),
    );
    assert_eq!(bad_arguments["error"]["code"], -32602);
}

#[test]
fn path_escape_attempts_are_rejected() {
    let mut client = McpClient::with_real_acos();
    client.initialize("2025-03-26");

    // An absolute path outside the allowed roots (and one that climbs with `..`)
    // is still `UNSAFE_PATH`: the roots check is unchanged.
    for path in ["/etc/passwd", "/etc/../etc/passwd"] {
        let response = client.call_tool(
            "acos_inspect",
            json!({"base": {"request_id": "t_escape"}, "apk": path}),
        );
        assert!(is_error(&response), "path {path} was not rejected");
        let envelope = envelope_of(&response);
        assert_eq!(envelope["status"], "error");
        assert_eq!(envelope["error"]["code"], "UNSAFE_PATH");
    }
    // FB5: a **relative** path (with or without `..`) is refused before any
    // resolution: the server's cwd is not the client's, and the reply carries
    // the absolute path it would have used. It is still an error, and it still
    // cannot reach /etc.
    for path in ["../../etc/passwd", "fixtures/../../etc/passwd"] {
        let response = client.call_tool(
            "acos_inspect",
            json!({"base": {"request_id": "t_escape"}, "apk": path}),
        );
        assert!(is_error(&response), "path {path} was not rejected");
        let envelope = envelope_of(&response);
        assert_eq!(envelope["status"], "error");
        assert_eq!(envelope["error"]["code"], "INVALID_ARGUMENT");
        let resolved = envelope["error"]["details"]["resolved_path"]
            .as_str()
            .unwrap_or_default();
        assert!(
            resolved.starts_with('/'),
            "the reply names the absolute path it refused: {envelope}"
        );
    }
}

#[test]
fn missing_required_argument_is_invalid_argument() {
    let mut client = McpClient::with_real_acos();
    client.initialize("2025-03-26");
    let response = client.call_tool(
        "acos_inspect",
        json!({"base": {"request_id": "t_missing"}}),
    );
    assert!(is_error(&response));
    let envelope = envelope_of(&response);
    assert_eq!(envelope["error"]["code"], "INVALID_ARGUMENT");
}

#[test]
fn cancellation_kills_a_long_running_child() {
    let script = repo_root().join("crates/acos-mcp/tests/fixtures/sleep.sh");
    ensure_executable(&script);
    let backend = backend_for(
        &script,
        vec![("ACOS_TEST_SLEEP_SECS".to_string(), "30".to_string())],
    );
    let mut client = McpClient::start(backend, test_config());
    client.initialize("2025-03-26");

    let id = 4242;
    client.send_request(
        id,
        "tools/call",
        json!({
            "name": "acos_inspect",
            "arguments": {
                "base": {"request_id": "t_cancel"},
                "apk": fixture_path("Cargo.toml"),
            },
        }),
    );
    std::thread::sleep(Duration::from_millis(150));
    let started = Instant::now();
    client.send_notification("notifications/cancelled", json!({"requestId": id}));
    let response = client.recv_for(id);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "cancellation did not interrupt the child"
    );
    assert!(is_error(&response));
    let envelope = envelope_of(&response);
    assert_eq!(envelope["status"], "error");
    assert_eq!(envelope["error"]["code"], "INTERNAL");
    assert_eq!(envelope["error"]["details"]["cancelled"], true);
}

#[test]
fn wall_clock_budget_produces_timeout() {
    let script = repo_root().join("crates/acos-mcp/tests/fixtures/sleep.sh");
    ensure_executable(&script);
    let backend = backend_for(
        &script,
        vec![("ACOS_TEST_SLEEP_SECS".to_string(), "30".to_string())],
    );
    let mut client = McpClient::start(backend, test_config());
    client.initialize("2025-03-26");

    let started = Instant::now();
    let response = client.call_tool(
        "acos_inspect",
        json!({
            "base": {"request_id": "t_timeout", "budget": {"wall_ms": 200}},
            "apk": fixture_path("Cargo.toml"),
        }),
    );
    assert!(is_error(&response));
    let envelope = envelope_of(&response);
    assert_eq!(envelope["status"], "timeout");
    assert_eq!(envelope["error"]["code"], "TIMEOUT");
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn concurrency_limit_serialises_calls() {
    let script = repo_root().join("crates/acos-mcp/tests/fixtures/lockcheck.sh");
    ensure_executable(&script);
    let marker = std::env::temp_dir().join(format!("acos-mcp-lock-{}", std::process::id()));
    let _ = std::fs::remove_file(&marker);
    let backend = backend_for(
        &script,
        vec![
            (
                "ACOS_TEST_MARKER".to_string(),
                marker.display().to_string(),
            ),
            ("ACOS_TEST_SLEEP_SECS".to_string(), "0.4".to_string()),
        ],
    );
    let mut config = test_config();
    config.max_concurrency = 1;
    let mut client = McpClient::start(backend, config);
    client.initialize("2025-03-26");

    for id in [1_i64, 2] {
        client.send_request(
            id,
            "tools/call",
            json!({
                "name": "acos_inspect",
                "arguments": {
                    "base": {"request_id": format!("c{id}")},
                    "apk": fixture_path("Cargo.toml"),
                },
            }),
        );
    }
    let mut responses: HashMap<i64, Value> = HashMap::new();
    for _ in 0..2 {
        let value = client.recv();
        responses.insert(value["id"].as_i64().unwrap(), value);
    }
    assert_eq!(responses.len(), 2);
    for value in responses.values() {
        assert!(!is_error(value), "unexpected error: {value}");
    }

    let contents = std::fs::read_to_string(&marker).expect("marker file");
    assert!(
        !contents.contains("overlap"),
        "the concurrency gate allowed overlap:\n{contents}"
    );
    assert_eq!(
        contents.lines().filter(|line| *line == "start").count(),
        2,
        "expected two serialised starts, got:\n{contents}"
    );
    let _ = std::fs::remove_file(&marker);
}

#[test]
fn capabilities_tool_reports_available_commands() {
    let mut client = McpClient::with_real_acos();
    client.initialize("2025-03-26");
    let response = client.call_tool("acos_capabilities", json!({}));
    assert!(!is_error(&response));
    let envelope = envelope_of(&response);
    assert_eq!(envelope["status"], "pass");
    let commands = envelope["result"]["commands"].as_array().unwrap();
    let inspect = commands
        .iter()
        .find(|c| c["command"] == "inspect")
        .unwrap();
    assert_eq!(inspect["available"], true);
    assert_eq!(inspect["fidelity"], "static-analysis");
    let run = commands.iter().find(|c| c["command"] == "run").unwrap();
    assert_eq!(run["available"], false);
    assert!(envelope["result"]["binary"].as_str().unwrap().contains("acos"));
}

#[test]
fn help_tool_returns_the_agent_workflow() {
    let mut client = McpClient::with_real_acos();
    client.initialize("2025-03-26");
    let response = client.call_tool("acos_help", json!({}));
    assert!(!is_error(&response));
    let envelope = envelope_of(&response);
    let result = &envelope["result"];
    assert!(result["loop"].as_str().unwrap().contains("inspect"));
    assert!(result["workflow"].as_array().unwrap().len() >= 5);
    assert!(result["error_playbook"].as_array().unwrap().len() >= 5);
    assert!(result["markdown"].as_str().unwrap().contains("acos_inspect"));
    let supported = result["supported"].as_array().unwrap();
    assert!(supported.iter().any(|v| v == "acos_inspect"));
}

#[test]
fn doctor_tool_returns_the_preflight_report() {
    let mut client = McpClient::with_real_acos();
    client.initialize("2025-03-26");
    let response = client.call_tool(
        "acos_doctor",
        json!({"base": {"request_id": "t_doctor"}}),
    );
    let envelope = envelope_of(&response);
    let report = &envelope["result"];
    assert!(
        report["checks"].is_array(),
        "doctor report missing checks: {envelope}"
    );
    assert!(
        report["ok"].is_boolean(),
        "doctor report missing ok: {envelope}"
    );
    // The verdict must be honest: healthy => pass; unhealthy => fail carrying
    // the failing check names so an agent can act on the `fix` strings.
    if report["ok"] == true {
        assert_eq!(envelope["status"], "pass");
    } else {
        assert_eq!(envelope["status"], "fail");
        assert_eq!(envelope["error"]["code"], "ASSERTION_FAILED");
        assert!(envelope["error"]["details"]["failures"].is_array());
    }
}

#[test]
fn structured_content_is_only_sent_for_2025_06_18() {
    let mut modern = McpClient::with_real_acos();
    modern.initialize("2025-06-18");
    let response = modern.call_tool("acos_capabilities", json!({}));
    assert!(response.pointer("/result/structuredContent").is_some());
    assert_eq!(response["result"]["structuredContent"]["status"], "pass");

    let mut legacy = McpClient::with_real_acos();
    legacy.initialize("2025-03-26");
    let response = legacy.call_tool("acos_capabilities", json!({}));
    assert!(response.pointer("/result/structuredContent").is_none());
}
