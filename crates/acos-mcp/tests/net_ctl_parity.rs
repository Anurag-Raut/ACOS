//! NET4: `acos_net` is advertised over MCP, and the CLI and MCP surfaces agree.
//!
//! NET3 proved the verb through a live `acos serve` daemon and then found that
//! MCP answered ``unknown tool `acos_net` ``: the module implemented the control
//! but never advertised it, so an agent could not discover it, and a hand-written
//! second copy of the arguments would have drifted from the dispatcher's.
//!
//! The parity is structural, not textual:
//!
//! * the session API's dispatch arm and this server's tool name are the same
//!   constant, [`acos_netd::net_ctl::NET_CTL_TOOL`];
//! * the schema this server advertises *is*
//!   [`acos_netd::net_ctl::net_ctl_schema`] — the value the session API parses
//!   its arguments with ([`acos_netd::net_ctl::NetAction::from_args`]).
//!
//! So these tests assert the two surfaces cannot disagree: same name, same
//! closed argument set, same ranges.

use serde_json::json;

use acos_mcp::descriptors::{session_tools, SESSION_TOOL_NAMES};
use acos_netd::net_ctl::{net_ctl_schema, NetAction, ARG_KEYS, IDENTITY_KEYS, NET_CTL_TOOL};

fn advertised() -> acos_mcp::descriptors::ToolInfo {
    session_tools()
        .into_iter()
        .find(|tool| tool.tool_name == NET_CTL_TOOL)
        .unwrap_or_else(|| panic!("`{NET_CTL_TOOL}` is not advertised by tools/list"))
}

/// The tool exists, is listed, is available, and is the module's verb — not an
/// alias or a second spelling.
#[test]
fn mcp_advertises_the_modules_verb_under_its_own_name() {
    assert!(SESSION_TOOL_NAMES.contains(&NET_CTL_TOOL));
    let tool = advertised();
    assert_eq!(tool.tool_name, "acos_net", "the CLI verb's name, verbatim");
    assert_eq!(tool.command, None, "served by the session backend, not a CLI subprocess");
    assert!(tool.available, "the default acos-mcp binary holds sessions");
    assert!(tool.reason.is_none());
    assert!(
        tool.description.contains("acos_net"),
        "the description names the verb it mirrors: {}",
        tool.description
    );
}

/// The advertised schema **is** the dispatcher's schema: one definition, two
/// readers. A copy here would pass a hand-written test and still drift.
#[test]
fn the_advertised_schema_is_the_session_apis_schema() {
    let tool = advertised();
    assert_eq!(
        tool.input_schema,
        net_ctl_schema(),
        "the MCP schema must be the session API's, value for value"
    );
    assert_eq!(tool.input_schema["type"], json!("object"));
    assert_eq!(
        tool.input_schema["additionalProperties"],
        json!(false),
        "closed: an unknown argument is an error, never a silent success"
    );
    let properties = tool.input_schema["properties"]
        .as_object()
        .expect("properties")
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    for key in ARG_KEYS.iter().chain(IDENTITY_KEYS) {
        assert!(properties.contains(&(*key).to_string()), "schema lacks `{key}`");
    }
    assert_eq!(
        properties.len(),
        ARG_KEYS.len() + IDENTITY_KEYS.len(),
        "no argument outside the documented sets: {properties:?}"
    );
}

/// Everything the schema advertises is callable, and what it does not advertise
/// is refused — the CLI parser and the MCP schema are the same contract.
#[test]
fn every_advertised_argument_parses_and_nothing_else_does() {
    for args in [
        json!({}),                                             // status (default)
        json!({"action": "status"}),
        json!({"action": "offline"}),
        json!({"action": "online"}),
        json!({"action": "delay", "ms": 400, "jitter_ms": 25, "loss_pct": 10}),
        json!({"action": "hostport", "port": 8081}),
        json!({"action": "hostport", "port": 8081, "on": false}),
        // The transport's identity envelope travels with the call.
        json!({"base": {"request_id": "r", "session_id": "s"}, "action": "status"}),
    ] {
        let action = NetAction::from_args(&args).expect("the schema's own arguments parse");
        assert!(!action.name().is_empty());
    }
    for args in [
        json!({"action": "offline", "port": 8081}), // key means nothing for this action
        json!({"offline": true}),                 // spelling of the *result*, not an arg
        json!({"action": "hostport"}),            // port is required
        json!({"action": "hostport", "port": 0}),
        json!({"action": "delay", "loss_pct": 101}),
        json!({"action": "restart"}),
    ] {
        let err = NetAction::from_args(&args).expect_err("not in the schema");
        assert_eq!(err.code, acos_netd::proto::ErrorCode::Invalid, "{args}");
    }
}

/// `tools/list` carries the schema to the client (not just `session_tools()`),
/// so an agent sees `acos_net` argument-for-argument.
#[test]
fn the_tool_list_renders_the_same_schema() {
    let caps = acos_mcp::backend::Capabilities::unprobed();
    let listed = acos_mcp::descriptors::tool_infos(&caps)
        .into_iter()
        .find(|tool| tool.tool_name == NET_CTL_TOOL)
        .expect("tool_infos lists the session tools");
    let rendered = listed.to_json(acos_mcp::descriptors::LATEST_PROTOCOL_VERSION);
    assert_eq!(rendered["name"], json!(NET_CTL_TOOL));
    assert_eq!(rendered["inputSchema"], net_ctl_schema());
}
