//! JSON-RPC 2.0 message parsing and construction for the MCP stdio transport.
//!
//! MCP uses newline-delimited JSON-RPC 2.0. This module is deliberately small
//! and strict: it distinguishes requests from notifications by the *presence* of
//! an `id` member, and reports precise errors for malformed input.

use serde_json::{json, Value};

/// JSON-RPC parse error.
pub const PARSE_ERROR: i64 = -32700;
/// JSON-RPC invalid request.
pub const INVALID_REQUEST: i64 = -32600;
/// JSON-RPC method not found.
pub const METHOD_NOT_FOUND: i64 = -32601;
/// JSON-RPC invalid params.
pub const INVALID_PARAMS: i64 = -32602;
/// JSON-RPC internal error.
pub const INTERNAL_ERROR: i64 = -32603;
/// MCP server not initialized.
pub const SERVER_NOT_INITIALIZED: i64 = -32002;

/// A JSON-RPC error object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcError {
    /// Numeric error code.
    pub code: i64,
    /// Human-readable message.
    pub message: String,
    /// Optional structured data.
    pub data: Option<Value>,
}

impl RpcError {
    /// Build an error with no data.
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        RpcError {
            code,
            message: message.into(),
            data: None,
        }
    }

    /// Attach structured data.
    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }
}

/// A parsed incoming message.
#[derive(Debug, Clone, PartialEq)]
pub enum Parsed {
    /// A request expecting a response with this id.
    Request {
        /// The id to echo.
        id: Value,
        /// The method name.
        method: String,
        /// Params (an object, an array, or absent → `{}`).
        params: Value,
    },
    /// A notification (no response expected).
    Notification {
        /// The method name.
        method: String,
        /// Params (an object, an array, or absent → `{}`).
        params: Value,
    },
    /// Input that could not be interpreted; the server replies with `error`.
    Invalid {
        /// The id to echo, when it could be recovered.
        id: Option<Value>,
        /// The error to return.
        error: RpcError,
    },
}

/// Parse one JSON-RPC text frame.
pub fn parse(text: &str) -> Parsed {
    let value: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => {
            return Parsed::Invalid {
                id: None,
                error: RpcError::new(PARSE_ERROR, format!("invalid JSON: {e}")),
            }
        }
    };
    parse_value(value)
}

/// Parse an already-decoded JSON value.
pub fn parse_value(value: Value) -> Parsed {
    let Some(object) = value.as_object() else {
        let msg = if value.is_array() {
            "JSON-RPC batches are not supported by this server"
        } else {
            "JSON-RPC message must be an object"
        };
        return Parsed::Invalid {
            id: None,
            error: RpcError::new(INVALID_REQUEST, msg),
        };
    };

    let id_present = object.contains_key("id");
    let id = object.get("id").cloned().unwrap_or(Value::Null);

    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Parsed::Invalid {
            id: id_present.then(|| id.clone()),
            error: RpcError::new(INVALID_REQUEST, "missing or invalid `jsonrpc` member"),
        };
    }

    let method = match object.get("method") {
        Some(Value::String(m)) if !m.is_empty() => m.clone(),
        Some(_) => {
            return Parsed::Invalid {
                id: id_present.then(|| id.clone()),
                error: RpcError::new(INVALID_REQUEST, "`method` must be a non-empty string"),
            }
        }
        None => {
            return Parsed::Invalid {
                id: id_present.then(|| id.clone()),
                error: RpcError::new(INVALID_REQUEST, "missing `method` member"),
            }
        }
    };

    let params = match object.get("params") {
        None => json!({}),
        Some(Value::Object(_)) | Some(Value::Array(_)) => object.get("params").cloned().unwrap(),
        Some(_) => {
            return Parsed::Invalid {
                id: id_present.then(|| id.clone()),
                error: RpcError::new(INVALID_REQUEST, "`params` must be an object or array"),
            }
        }
    };

    if id_present {
        Parsed::Request { id, method, params }
    } else {
        Parsed::Notification { method, params }
    }
}

/// Build a successful response object.
pub fn result(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// Build an error response object.
pub fn error(id: &Value, error: &RpcError) -> Value {
    let mut err = json!({ "code": error.code, "message": error.message });
    if let Some(data) = &error.data {
        err["data"] = data.clone();
    }
    json!({ "jsonrpc": "2.0", "id": id, "error": err })
}

/// Build a notification object (no id).
pub fn notification(method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "method": method, "params": params })
}

/// A stable string key for a request id (used to match cancellations).
pub fn id_key(id: &Value) -> String {
    match id {
        Value::String(s) => format!("s:{s}"),
        Value::Number(n) => format!("n:{n}"),
        Value::Null => "null".to_string(),
        other => format!("v:{other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_and_notification_are_distinguished() {
        let req = parse(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#);
        assert!(matches!(req, Parsed::Request { .. }));
        let note = parse(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
        assert!(matches!(note, Parsed::Notification { .. }));
    }

    #[test]
    fn malformed_inputs_are_reported() {
        assert!(matches!(parse("{"), Parsed::Invalid { id: None, .. }));
        assert!(matches!(
            parse(r#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#),
            Parsed::Invalid { .. }
        ));
        assert!(matches!(
            parse(r#"{"jsonrpc":"2.0","id":1}"#),
            Parsed::Invalid { .. }
        ));
        assert!(matches!(
            parse(r#"{"jsonrpc":"2.0","id":1,"method":"ping","params":3}"#),
            Parsed::Invalid { .. }
        ));
        assert!(matches!(
            parse(r#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#),
            Parsed::Invalid { .. }
        ));
    }

    #[test]
    fn id_key_is_stable() {
        assert_eq!(id_key(&json!(1)), "n:1");
        assert_eq!(id_key(&json!("abc")), "s:abc");
    }
}
