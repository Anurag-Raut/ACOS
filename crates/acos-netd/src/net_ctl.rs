//! The `acos_net` control verb: **one** definition of its arguments, shared by
//! the two surfaces that expose it.
//!
//! NET4: the verb was implemented in the `acos serve` session API only, so an
//! MCP client could not call it (`tools/call` answered ``unknown tool
//! `acos_net` ``) and an agent had no way to learn its arguments. The schema and
//! the parser therefore live here, next to the wire protocol, and both surfaces
//! read them:
//!
//! * the session API (`acos serve`, and the CLI behind it) parses with
//!   [`NetAction::from_args`];
//! * `acos-mcp` advertises [`net_ctl_schema`] as the tool's `inputSchema`.
//!
//! One definition, two readers: `crates/acos-mcp/tests/net_ctl_parity.rs`
//! asserts the advertised schema *is* this value, and that the parser accepts
//! exactly the keys the schema allows.
//!
//! The schema is **closed** (`additionalProperties: false`): a typo such as
//! `offline: true` must be an error, never a silent success — the same rule the
//! rest of the module keeps ("never a fake success").

use serde_json::{json, Value};

use crate::proto::{ErrorCode, ProtoError};

/// MCP tool name and session-API verb name of the network control.
pub const NET_CTL_TOOL: &str = "acos_net";

/// Keys that identify the *request*, not the operation: the transport adds them
/// (the session API's `base` envelope and the MCP request id), so they are part
/// of the schema but never change what the verb does.
pub const IDENTITY_KEYS: &[&str] = &["request_id", "session_id", "base"];

/// The verb's own keys.
pub const ARG_KEYS: &[&str] = &["action", "ms", "jitter_ms", "loss_pct", "port", "on"];

/// The `acos_net` arguments, as JSON Schema. Advertised verbatim by `acos-mcp`
/// and enforced by [`NetAction::from_args`].
#[must_use]
pub fn net_ctl_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "request_id": {"type": "string"},
            "session_id": {"type": "string"},
            "base": {
                "type": "object",
                "description": "Session identity envelope (transport plumbing).",
                "properties": {
                    "request_id": {"type": "string"},
                    "session_id": {"type": "string"}
                }
            },
            "action": {
                "type": "string",
                "enum": ["status", "offline", "online", "delay", "hostport"],
                "default": "status",
                "description": "`status` (default) reports the relay and this session's row; \
                                `offline`/`online` drop/restore this session's traffic (its own \
                                veth only); `delay` shapes it (`ms`, `jitter_ms`, `loss_pct`, \
                                `0/0/0` clears); `hostport` is the explicit host-loopback opt-in \
                                (Metro's `localhost`), which needs `port`."
            },
            "ms": {
                "type": "integer", "minimum": 0, "maximum": 60000,
                "description": "`delay`: one-way latency on this session's veth, milliseconds."
            },
            "jitter_ms": {
                "type": "integer", "minimum": 0, "maximum": 60000,
                "description": "`delay`: jitter of that latency, milliseconds."
            },
            "loss_pct": {
                "type": "integer", "minimum": 0, "maximum": 100,
                "description": "`delay`: packet loss percentage."
            },
            "port": {
                "type": "integer", "minimum": 1, "maximum": 65535,
                "description": "`hostport`: the host port to open for this session. Only an \
                                explicitly named port is reachable, and only from sessions that \
                                opted in."
            },
            "on": {
                "type": "boolean", "default": true,
                "description": "`hostport`: `true` (default) opens the port, `false` revokes it."
            }
        },
        "additionalProperties": false
    })
}

/// The keys a given action accepts besides `action` (the transport's identity
/// keys are always accepted). The flat schema above cannot express this; the
/// parser is stricter than the schema on purpose.
#[must_use]
pub fn action_keys(action: &str) -> &'static [&'static str] {
    match action {
        "delay" => &["ms", "jitter_ms", "loss_pct"],
        "hostport" => &["port", "on"],
        _ => &[],
    }
}

/// One parsed `acos_net` call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NetAction {
    /// The relay state and this session's row.
    Status,
    /// Drop (`true`) or restore (`false`) this session's traffic.
    Offline(bool),
    /// Shape this session's veth; `0/0/0` clears it.
    Delay { ms: u32, jitter_ms: u32, loss_pct: u32 },
    /// The explicit host-port opt-in.
    HostPort { port: u16, on: bool },
}

impl NetAction {
    /// Parse one argument object against [`net_ctl_schema`].
    ///
    /// Unknown keys, wrong types and out-of-range values are [`ErrorCode::Invalid`]
    /// — a closed schema, so a misspelled argument cannot look like a pass.
    pub fn from_args(args: &Value) -> Result<Self, ProtoError> {
        let Some(object) = args.as_object() else {
            return Err(invalid("`acos_net` arguments must be a JSON object"));
        };
        for key in object.keys() {
            if !ARG_KEYS.contains(&key.as_str()) && !IDENTITY_KEYS.contains(&key.as_str()) {
                return Err(invalid(format!(
                    "`acos_net`: unknown argument `{key}` (expected one of: {})",
                    ARG_KEYS.join(", ")
                )));
            }
        }
        let action = match args.get("action") {
            None | Some(Value::Null) => "status",
            Some(Value::String(text)) => text.as_str(),
            Some(_) => return Err(invalid("`acos_net`: `action` must be a string")),
        };
        let number = |key: &str, max: u64| -> Result<u64, ProtoError> {
            match args.get(key) {
                None | Some(Value::Null) => Ok(0),
                Some(Value::Number(n)) => match n.as_u64() {
                    Some(v) if v <= max => Ok(v),
                    _ => Err(invalid(format!(
                        "`acos_net`: `{key}` must be an integer in 0..={max}"
                    ))),
                },
                Some(_) => Err(invalid(format!("`acos_net`: `{key}` must be an integer"))),
            }
        };
        // Per action, the keys that can mean anything. A flat "any key of the
        // verb" rule would let `{"action":"offline","port":8081}` through and the
        // caller would believe it had opened a host port.
        let allowed: &[&str] = action_keys(action);
        for key in object.keys() {
            if IDENTITY_KEYS.contains(&key.as_str()) || key == "action" {
                continue;
            }
            if !allowed.contains(&key.as_str()) {
                let takes = if allowed.is_empty() {
                    String::from("it takes no arguments")
                } else {
                    format!("allowed: {}", allowed.join(", "))
                };
                return Err(invalid(format!(
                    "`acos_net`: `{key}` means nothing for action `{action}` ({takes})"
                )));
            }
        }
        match action {
            "status" => Ok(Self::Status),
            "offline" | "online" => Ok(Self::Offline(action == "offline")),
            "delay" => Ok(Self::Delay {
                ms: number("ms", 60_000)? as u32,
                jitter_ms: number("jitter_ms", 60_000)? as u32,
                loss_pct: number("loss_pct", 100)? as u32,
            }),
            "hostport" => {
                let port = number("port", u64::from(u16::MAX))?;
                if port == 0 {
                    return Err(invalid("`hostport` needs `port` (1..=65535)"));
                }
                let on = match args.get("on") {
                    None | Some(Value::Null) => true,
                    Some(Value::Bool(on)) => *on,
                    Some(_) => return Err(invalid("`acos_net`: `on` must be a boolean")),
                };
                Ok(Self::HostPort { port: port as u16, on })
            }
            other => Err(invalid(format!(
                "acos_net: unknown action `{other}` (status|offline|online|delay|hostport)"
            ))),
        }
    }

    /// The action name, as the reply echoes it.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Offline(false) => "online",
            Self::Offline(true) => "offline",
            Self::Delay { .. } => "delay",
            Self::HostPort { .. } => "hostport",
        }
    }
}

fn invalid(message: impl Into<String>) -> ProtoError {
    ProtoError { code: ErrorCode::Invalid, message: message.into() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_schema_is_closed_and_lists_every_key_the_parser_takes() {
        let schema = net_ctl_schema();
        assert_eq!(schema["additionalProperties"], json!(false));
        let properties = schema["properties"].as_object().expect("properties");
        for key in ARG_KEYS.iter().chain(IDENTITY_KEYS) {
            assert!(properties.contains_key(*key), "schema lacks `{key}`");
        }
        // Every key the schema allows is accepted for the action that owns it
        // (and `action` itself everywhere): the schema is a lower bound, the
        // parser stricter only about which action a key means something for.
        for (action, keys) in [
            ("status", &[][..]),
            ("offline", &[][..]),
            ("online", &[][..]),
            ("delay", action_keys("delay")),
            ("hostport", action_keys("hostport")),
        ] {
            for key in keys {
                // `port` is the one required argument: give it a legal value,
                // and keep it present while probing `on`.
                let mut args = json!({"action": action});
                if keys.contains(&"port") {
                    args["port"] = json!(8081);
                }
                args[key.to_string()] = if *key == "port" { json!(8081) } else { json!(null) };
                assert!(
                    NetAction::from_args(&args).is_ok(),
                    "schema allows `{key}` for `{action}` but the parser rejects it"
                );
            }
        }
        for key in IDENTITY_KEYS {
            let args = json!({ key.to_string(): null, "action": "status" });
            assert!(NetAction::from_args(&args).is_ok(), "identity key `{key}`");
        }
        // And every key of the verb is owned by at least one action.
        let owned: Vec<&str> =
            action_keys("delay").iter().chain(action_keys("hostport")).copied().collect();
        for key in ARG_KEYS.iter().filter(|k| **k != "action") {
            assert!(owned.contains(key), "`{key}` belongs to no action");
        }
        let err = NetAction::from_args(&json!({"offline": true})).expect_err("closed schema");
        assert_eq!(err.code, ErrorCode::Invalid);
        assert!(err.message.contains("unknown argument `offline`"), "{}", err.message);
    }

    #[test]
    fn actions_parse_and_ranges_are_enforced() {
        assert_eq!(NetAction::from_args(&json!({})).unwrap(), NetAction::Status);
        assert_eq!(
            NetAction::from_args(&json!({"action": "offline"})).unwrap(),
            NetAction::Offline(true)
        );
        assert_eq!(
            NetAction::from_args(&json!({"action": "online"})).unwrap(),
            NetAction::Offline(false)
        );
        assert_eq!(
            NetAction::from_args(&json!({"action": "delay", "ms": 400, "jitter_ms": 20, "loss_pct": 5}))
                .unwrap(),
            NetAction::Delay { ms: 400, jitter_ms: 20, loss_pct: 5 }
        );
        assert_eq!(
            NetAction::from_args(&json!({"action": "delay"})).unwrap(),
            NetAction::Delay { ms: 0, jitter_ms: 0, loss_pct: 0 },
            "delay with no numbers clears the shaping"
        );
        assert_eq!(
            NetAction::from_args(&json!({"action": "hostport", "port": 8081})).unwrap(),
            NetAction::HostPort { port: 8081, on: true }
        );
        assert_eq!(
            NetAction::from_args(&json!({"action": "hostport", "port": 8081, "on": false})).unwrap(),
            NetAction::HostPort { port: 8081, on: false }
        );
        // Failures are structured, never a silent default.
        for (args, needle) in [
            (json!({"action": "hostport"}), "needs `port`"),
            (json!({"action": "hostport", "port": 0}), "needs `port`"),
            (json!({"action": "hostport", "port": 70000}), "0..=65535"),
            (json!({"action": "delay", "loss_pct": 101}), "0..=100"),
            (json!({"action": "nope"}), "unknown action `nope`"),
            (json!({"action": "hostport", "port": 1, "on": "yes"}), "`on` must be a boolean"),
            // A key that means nothing for this action is refused, not ignored.
            (json!({"action": "offline", "port": 8081}), "means nothing for action `offline`"),
            (json!({"action": "status", "ms": 10}), "means nothing for action `status`"),
            (json!({"action": "delay", "port": 9}), "means nothing for action `delay`"),
        ] {
            let err = NetAction::from_args(&args).expect_err(&format!("must fail: {args}"));
            assert_eq!(err.code, ErrorCode::Invalid, "{args}: {}", err.message);
            assert!(err.message.contains(needle), "{args}: {}", err.message);
        }
        // The transport's identity keys are accepted and ignored.
        assert_eq!(
            NetAction::from_args(&json!({
                "base": {"request_id": "r", "session_id": "s"},
                "request_id": "r",
                "session_id": "s",
                "action": "status"
            }))
            .unwrap(),
            NetAction::Status
        );
    }

    #[test]
    fn names_round_trip() {
        assert_eq!(NetAction::Status.name(), "status");
        assert_eq!(NetAction::Offline(true).name(), "offline");
        assert_eq!(NetAction::Offline(false).name(), "online");
        assert_eq!(NetAction::Delay { ms: 1, jitter_ms: 0, loss_pct: 0 }.name(), "delay");
        assert_eq!(NetAction::HostPort { port: 1, on: false }.name(), "hostport");
    }
}
