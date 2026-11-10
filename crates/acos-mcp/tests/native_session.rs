//! Native sessions through the MCP `native` option.
//!
//! The MCP server is multi-threaded and cannot enter the ADR-0022 seccomp
//! confinement itself, so [`SessionBackend`] re-execs a confined
//! `acos-mcp --confined-child` process for `acos_launch { native: true }` and
//! proxies the session commands to it. This test drives the real in-process MCP
//! dispatcher and asserts:
//!
//! * the launch runs in the confined child (never `CONFINEMENT_UNAVAILABLE`);
//! * the native evidence reaches the ladder (APK asset bytes + FrameStore
//!   frame);
//! * `acos_observe` sees the frame published through `ANativeWindow`.

mod common;

use std::path::PathBuf;

use acos_mcp::{BackendConfig, CliBackend, SessionBackend};
use acos_runtime::session::SessionOptions;
use common::{envelope_of, repo_root, McpClient};
use serde_json::json;

fn boot_available() -> bool {
    repo_root().join(".cache/boot/android-34/BOOTCLASSPATH").is_file()
}

fn native_fixture() -> Option<PathBuf> {
    let apk = repo_root().join("fixtures/build/apps/native-hello.apk");
    if apk.is_file() {
        Some(apk)
    } else {
        eprintln!("SKIP native_session: missing {}", apk.display());
        None
    }
}

fn backend() -> SessionBackend {
    let cli = CliBackend::new(BackendConfig::for_binary("/nonexistent/acos-w80a"));
    SessionBackend::with_cli(cli, SessionOptions::default()).with_native_child_binary(PathBuf::from(
        env!("CARGO_BIN_EXE_acos-mcp"),
    ))
}

#[test]
fn native_launch_runs_in_a_confined_child_session() {
    if !boot_available() {
        eprintln!("SKIP native_launch_runs_in_a_confined_child_session: no boot cache");
        return;
    }
    let Some(apk) = native_fixture() else {
        return;
    };
    let mut client = McpClient::start(backend(), common::test_config());
    client.initialize("2025-06-18");

    let response = client.call_tool(
        "acos_launch",
        json!({
            "apk": apk,
            "native": true,
            "base": {"request_id": "req_native", "session_id": "s_native"},
        }),
    );
    let envelope = envelope_of(&response);
    if let Some(error) = envelope.get("error") {
        assert_ne!(
            error["code"], "CONFINEMENT_UNAVAILABLE",
            "native launch must run in a confined child, not fail closed: {envelope:#?}"
        );
    }
    let events: Vec<&str> = envelope["result"]["events"]
        .as_array()
        .map(|a| a.iter().filter_map(serde_json::Value::as_str).collect())
        .unwrap_or_default();
    assert!(
        events.contains(&"asset=hello-acos!!"),
        "the confined child must read the APK asset; events: {events:#?}\nenvelope: {envelope:#?}"
    );
    assert!(
        events.contains(&"frame=1"),
        "the native ANativeWindow post must publish a frame; events: {events:#?}"
    );
    let stages = envelope["result"]["stages"].as_array().cloned().unwrap_or_default();
    let first_frame = stages
        .iter()
        .find(|s| s["stage"] == "first_frame")
        .map(|s| s["status"].clone())
        .unwrap_or_default();
    assert_eq!(
        first_frame, "ok",
        "the child's FrameStore must hold the posted frame; envelope: {envelope:#?}"
    );

    // The child session survives the launch: observe reads the same FrameStore.
    let observe = client.call_tool(
        "acos_observe",
        json!({
            "tree": false,
            "base": {"request_id": "req_observe", "session_id": "s_native"},
        }),
    );
    let observed = envelope_of(&observe);
    assert_eq!(
        observed["status"], "pass",
        "observe must read the confined child's frame: {observed:#?}"
    );
    assert_eq!(observed["result"]["published_frames"], 1);
    assert!(
        observed["result"]["frame"]["width"].as_u64().unwrap_or(0) > 0
            && observed["result"]["frame"]["height"].as_u64().unwrap_or(0) > 0,
        "the observed frame must carry non-zero geometry: {observed:#?}"
    );
    assert_eq!(observed["result"]["frame"]["meaningful"], true);

    // Destroy tears the child down.
    let destroy = client.call_tool(
        "acos_destroy",
        json!({"base": {"request_id": "req_destroy", "session_id": "s_native"}}),
    );
    assert_eq!(envelope_of(&destroy)["result"]["destroyed"], true);
}
