//! In-process session backend for the MCP server.
//!
//! [`CliBackend`] shells out to the `acos` binary for every call, so it cannot
//! hold a session across calls. This backend adds a small map of
//! [`AgentSession`]s (one worker thread each, owning the `Vm`) and serves the
//! session commands **in-process**: `acos_run`/`acos_launch`,
//! `acos_observe`, `acos_action`/`acos_tap`/`acos_key`/`acos_swipe`,
//! `acos_logs`, `acos_status`, `acos_reset` and `acos_destroy`.
//!
//! Everything else (`inspect`, `run-dex`, `verify`, `capability-report`)
//! delegates to the wrapped [`CliBackend`], so the subprocess path is unchanged.
//!
//! Sessions are created lazily: an `observe`/`status`/`logs`/`action` on an
//! unknown session id creates a fresh, un-launched session, which is why
//! `observe` before any frame returns a structured `not_ready` rather than
//! `SESSION_NOT_FOUND`. `destroy` removes the session; the next read creates a
//! new one. This mirrors the CLI's process-local session.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use acos_protocol::{
    Backend as ProtocolBackend, CommandName, Envelope, ErrorCode, Fidelity, Metrics, ProtocolError,
};
use acos_runtime::session::{
    AgentSession, LaunchReport, SessionError, SessionOptions, StageStatus,
};
use acos_runtime::view_tree::{ViewNode, ViewTree};
use serde_json::{json, Value};

use crate::backend::{Backend, BackendConfig, BackendInfo, CallContext, Capabilities, CliBackend};

/// Commands the session backend serves itself (bypassing the CLI capability
/// probe, which cannot see them).
const SESSION_COMMANDS: &[CommandName] = &[
    CommandName::Run,
    CommandName::Observe,
    CommandName::Action,
    CommandName::Reset,
    CommandName::Destroy,
];

/// The default session id when a request omits `base.session_id`.
pub const DEFAULT_SESSION: &str = "s_mcp";

/// A [`Backend`] that holds in-process sessions and delegates the rest to a
/// [`CliBackend`].
pub struct SessionBackend {
    cli: CliBackend,
    sessions: Mutex<HashMap<String, AgentSession>>,
    limits: SessionOptions,
}

impl std::fmt::Debug for SessionBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionBackend")
            .field(
                "sessions",
                &self.sessions.lock().map(|s| s.len()).unwrap_or(0),
            )
            .finish_non_exhaustive()
    }
}

impl SessionBackend {
    /// Wrap a CLI backend and start with no sessions.
    pub fn new(config: BackendConfig, limits: SessionOptions) -> Self {
        SessionBackend {
            cli: CliBackend::new(config),
            sessions: Mutex::new(HashMap::new()),
            limits,
        }
    }

    /// Wrap an already-probed CLI backend.
    pub fn with_cli(cli: CliBackend, limits: SessionOptions) -> Self {
        SessionBackend {
            cli,
            sessions: Mutex::new(HashMap::new()),
            limits,
        }
    }

    /// The CLI backend used for non-session commands.
    pub fn cli(&self) -> &CliBackend {
        &self.cli
    }

    fn with_session<T>(&self, id: &str, f: impl FnOnce(&AgentSession) -> T) -> T {
        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        let session = sessions
            .entry(id.to_string())
            .or_insert_with(|| AgentSession::start(self.limits));
        f(session)
    }

    fn remove_session(&self, id: &str) -> bool {
        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        sessions.remove(id).is_some()
    }

    fn call_run(&self, request_id: &str, args: &Value, ctx: &CallContext) -> Envelope {
        let Some(apk) = args.get("apk").and_then(Value::as_str) else {
            return invalid_argument(request_id, "apk", "missing required string `apk`");
        };
        let apk = match crate::safety::validate_path(apk, &ctx.allowed_roots) {
            Ok(path) => path,
            Err(reason) => return unsafe_path(request_id, apk, &reason),
        };
        let id = session_id(args);
        let mut limits = self.limits;
        if let Some(ms) = args
            .get("timeout_ms")
            .and_then(Value::as_u64)
            .or(ctx.budget.wall_ms)
        {
            limits.wall_secs = (ms / 1000).max(1);
        }
        // Opt-in native code (`acos_launch { native: true }`). The JNI loader
        // requires an active process confinement and fails closed with
        // `CONFINEMENT_UNAVAILABLE` otherwise; this server never enters a
        // sandbox mid-flight.
        limits.native = args.get("native").and_then(Value::as_bool).unwrap_or(false);
        // Replace any prior session for this id so a second launch is a clean
        // task (the process-global transport/surface seams tolerate only one
        // live runtime).
        let _ = self.remove_session(&id);
        let session = AgentSession::start(limits);
        let result = session.launch(&apk);
        let envelope = match result {
            Ok(report) => {
                // Keep the session in the map so observe/action see its state.
                {
                    let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
                    sessions.insert(id.clone(), session);
                }
                launch_envelope(request_id, &id, &report)
            }
            Err(err) => {
                session.close();
                session_error_envelope(request_id, Some(&id), &err)
            }
        };
        envelope
    }

    fn call_observe(&self, request_id: &str, args: &Value, _ctx: &CallContext) -> Envelope {
        let id = session_id(args);
        let tree = args.get("tree").and_then(Value::as_bool).unwrap_or(true);
        let text = args.get("text").and_then(Value::as_bool).unwrap_or(false);
        self.with_session(&id, |session| {
            let status = session.status().ok();
            match session.observe(None) {
                Ok(report) => {
                    if tree || text {
                        return unsupported(
                            request_id,
                            "ui.tree",
                            "UI tree extraction is not implemented in this build",
                        )
                        .with_session_id(id.clone());
                    }
                    let frame = report.frame.as_ref().map(|frame| {
                        json!({
                            "frame_seq": frame.frame_id,
                            "width": frame.width,
                            "height": frame.height,
                            "content_hash": frame.content_hash,
                            "cause_id": frame.cause_id.to_string(),
                            "meaningful": !frame.pixels.is_empty(),
                        })
                    });
                    Envelope::pass(
                        request_id,
                        ProtocolBackend::AcosNative,
                        Fidelity::NativeRuntime,
                        Metrics::wall_ms(0),
                    )
                    .with_session_id(id.clone())
                    .with_value_result(json!({
                        "session_id": id,
                        "package": status.and_then(|s| s.package).unwrap_or_default(),
                        "stage_reached": report.stage_reached,
                        "published_frames": report.published_frames,
                        "frame": frame,
                    }))
                    .with_capabilities(["ui.frame"])
                }
                Err(err) => session_error_envelope(request_id, Some(&id), &err),
            }
        })
    }

    fn call_observe_tree(
        &self,
        request_id: &str,
        args: &Value,
        _ctx: &CallContext,
    ) -> Envelope {
        let id = session_id(args);
        let max_nodes = args
            .get("max_nodes")
            .and_then(Value::as_u64)
            .unwrap_or(2000) as usize;
        self.with_session(&id, |session| {
            let status = session.status().ok();
            match session.observe_tree(max_nodes) {
                Ok(tree) => {
                    let package = status
                        .as_ref()
                        .and_then(|s| s.package.clone())
                        .unwrap_or_default();
                    let stage = status
                        .as_ref()
                        .map(|s| s.stage_reached)
                        .unwrap_or("none");
                    Envelope::pass(
                        request_id,
                        ProtocolBackend::AcosNative,
                        Fidelity::NativeRuntime,
                        Metrics::wall_ms(0),
                    )
                    .with_session_id(id.clone())
                    .with_value_result(json!({
                        "session_id": id,
                        "package": package,
                        "stage_reached": stage,
                        "windows": tree.windows,
                        "truncated": tree.truncated,
                        "tree": view_tree_json(&tree),
                    }))
                    .with_capabilities(["ui.tree"])
                }
                Err(err) => session_error_envelope(request_id, Some(&id), &err),
            }
        })
    }

    fn call_action(&self, request_id: &str, args: &Value, _ctx: &CallContext) -> Envelope {
        let id = session_id(args);
        let action = args.get("action").cloned().unwrap_or(Value::Null);
        let kind = action.get("kind").and_then(Value::as_str).unwrap_or("tap");
        self.with_session(&id, |session| match kind {
            "tap" => {
                if let Some(label) = action.get("text").and_then(Value::as_str) {
                    return input_envelope(request_id, &id, session.tap_text(label), "tap");
                }
                match (
                    action.get("x").and_then(Value::as_f64),
                    action.get("y").and_then(Value::as_f64),
                ) {
                    (Some(x), Some(y)) => {
                        input_envelope(request_id, &id, session.tap(x as f32, y as f32), "tap")
                    }
                    _ => invalid_argument(request_id, "action", "tap needs `x` and `y`, or `text`"),
                }
            }
            "key" => {
                let Some(code) = action_key_code(&action) else {
                    return invalid_argument(request_id, "action.key", "unknown key code");
                };
                input_envelope(request_id, &id, session.key(code), "key")
            }
            "back" => input_envelope(request_id, &id, session.key(4), "key"),
            "swipe" => {
                let from = action.get("from");
                let to = action.get("to");
                let (Some(from), Some(to)) = (from, to) else {
                    return invalid_argument(request_id, "action", "swipe needs `from` and `to`");
                };
                let point = |value: &Value| -> Option<(f32, f32)> {
                    Some((
                        value.get("x")?.as_f64()? as f32,
                        value.get("y")?.as_f64()? as f32,
                    ))
                };
                let (Some(from), Some(to)) = (point(from), point(to)) else {
                    return invalid_argument(request_id, "action", "swipe points need `x` and `y`");
                };
                let duration = action
                    .get("duration_ms")
                    .and_then(Value::as_u64)
                    .unwrap_or(300);
                input_envelope(
                    request_id,
                    &id,
                    session.swipe(from, to, Duration::from_millis(duration), 8),
                    "swipe",
                )
            }
            other => invalid_argument(request_id, "action.kind", &format!("unsupported {other:?}")),
        })
    }

    fn call_reset(&self, request_id: &str, args: &Value, _ctx: &CallContext) -> Envelope {
        let id = session_id(args);
        self.with_session(&id, |session| match session.reset() {
            Ok(state) => Envelope::pass(
                request_id,
                ProtocolBackend::AcosNative,
                Fidelity::NativeRuntime,
                Metrics::wall_ms(0),
            )
            .with_session_id(id.clone())
            .with_value_result(json!({
                "session_id": id,
                "scope": "all",
                "state_reset": true,
                "data_cleared": false,
                "isolation": "shared-task",
                "note": "in-process session: the guest VFS and log sink are process-global, so app data is not cleared",
                "stage_reached": state.stage_reached,
            })),
            Err(err) => session_error_envelope(request_id, Some(&id), &err),
        })
    }

    fn call_destroy(&self, request_id: &str, args: &Value, _ctx: &CallContext) -> Envelope {
        let id = session_id(args);
        let destroyed = self.remove_session(&id);
        Envelope::pass(
            request_id,
            ProtocolBackend::AcosNative,
            Fidelity::NativeRuntime,
            Metrics::wall_ms(0),
        )
        .with_session_id(id)
        .with_value_result(json!({
            "destroyed": destroyed,
            "already_absent": !destroyed,
        }))
    }

    /// Server-local session tools (`acos_launch`, `acos_tap`, ...).
    pub fn session_call(&self, name: &str, args: &Value, ctx: &CallContext) -> Option<Envelope> {
        let request_id = request_id(args);
        match name {
            "acos_launch" => Some(self.call_run(&request_id, args, ctx)),
            "acos_observe_tree" => Some(self.call_observe_tree(&request_id, args, ctx)),
            "acos_observe" => Some(self.call_observe(&request_id, args, ctx)),
            "acos_tap" => Some(self.call_action(
                &request_id,
                &json!({"session_id": session_id(args), "action": {"kind": "tap", "x": args.get("x"), "y": args.get("y"), "text": args.get("text")}}),
                ctx,
            )),
            "acos_key" => Some(self.call_action(
                &request_id,
                &json!({"session_id": session_id(args), "action": {"kind": "key", "code": args.get("code")}}),
                ctx,
            )),
            "acos_swipe" => Some(self.call_action(
                &request_id,
                &json!({"session_id": session_id(args), "action": {"kind": "swipe", "from": args.get("from"), "to": args.get("to"), "duration_ms": args.get("duration_ms")}}),
                ctx,
            )),
            "acos_logs" => Some(self.call_logs(&request_id, args)),
            "acos_status" => Some(self.call_status(&request_id, args)),
            _ => None,
        }
    }

    fn call_logs(&self, request_id: &str, args: &Value) -> Envelope {
        let id = session_id(args);
        self.with_session(&id, |session| match session.logs() {
            Ok(lines) => {
                let records: Vec<Value> = lines
                    .iter()
                    .map(|line| {
                        json!({
                            "priority": line.priority,
                            "tag": line.tag,
                            "message": line.message,
                            "line": line.line,
                        })
                    })
                    .collect();
                Envelope::pass(
                    request_id,
                    ProtocolBackend::AcosNative,
                    Fidelity::NativeRuntime,
                    Metrics::wall_ms(0),
                )
                .with_session_id(id.clone())
                .with_value_result(json!({
                    "session_id": id,
                    "count": records.len(),
                    "logs": records,
                }))
                .with_capabilities(["logs"])
            }
            Err(err) => session_error_envelope(request_id, Some(&id), &err),
        })
    }

    fn call_status(&self, request_id: &str, args: &Value) -> Envelope {
        let id = session_id(args);
        self.with_session(&id, |session| match session.status() {
            Ok(state) => Envelope::pass(
                request_id,
                ProtocolBackend::AcosNative,
                Fidelity::NativeRuntime,
                Metrics::wall_ms(0),
            )
            .with_session_id(id.clone())
            .with_value_result(json!({
                "session_id": id,
                "launched": state.launched,
                "package": state.package,
                "apk": state.apk,
                "stage_reached": state.stage_reached,
                "published_frames": state.published_frames,
                "live": true,
            })),
            Err(err) => session_error_envelope(request_id, Some(&id), &err),
        })
    }
}

impl Backend for SessionBackend {
    fn call(&self, command: CommandName, args: &Value, ctx: &CallContext) -> Envelope {
        let request_id = request_id(args);
        match command {
            CommandName::Run => self.call_run(&request_id, args, ctx),
            CommandName::Observe => self.call_observe(&request_id, args, ctx),
            CommandName::Action => self.call_action(&request_id, args, ctx),
            CommandName::Reset => self.call_reset(&request_id, args, ctx),
            CommandName::Destroy => self.call_destroy(&request_id, args, ctx),
            other => self.cli.call(other, args, ctx),
        }
    }

    fn capabilities(&self) -> Capabilities {
        let mut caps = self.cli.capabilities();
        for command in SESSION_COMMANDS {
            caps.supported.insert(*command);
        }
        caps
    }

    fn refresh_capabilities(&self) -> Capabilities {
        let _ = self.cli.refresh_capabilities();
        self.capabilities()
    }

    fn info(&self) -> BackendInfo {
        self.cli.info()
    }

    fn session_call(&self, name: &str, args: &Value, ctx: &CallContext) -> Option<Envelope> {
        SessionBackend::session_call(self, name, args, ctx)
    }
}

fn session_id(args: &Value) -> String {
    args.pointer("/base/session_id")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_SESSION)
        .to_string()
}

fn request_id(args: &Value) -> String {
    args.pointer("/base/request_id")
        .and_then(Value::as_str)
        .unwrap_or("req_mcp")
        .to_string()
}

fn action_key_code(action: &Value) -> Option<i32> {
    let code = action.get("code")?;
    if let Some(value) = code.as_i64() {
        return Some(value as i32);
    }
    match code.as_str()?.to_ascii_lowercase().as_str() {
        "home" => Some(3),
        "back" => Some(4),
        "menu" => Some(82),
        "enter" => Some(66),
        "tab" => Some(61),
        "escape" => Some(111),
        "delete" => Some(67),
        "volume_up" => Some(24),
        "volume_down" => Some(25),
        "power" => Some(26),
        _ => None,
    }
}

fn input_envelope(
    request_id: &str,
    session_id: &str,
    result: Result<acos_runtime::session::InputReport, SessionError>,
    kind: &str,
) -> Envelope {
    match result {
        Ok(report) => Envelope::pass(
            request_id,
            ProtocolBackend::AcosNative,
            Fidelity::NativeRuntime,
            Metrics::wall_ms(0),
        )
        .with_session_id(session_id.to_string())
        .with_value_result(json!({
            "session_id": session_id,
            "action": kind,
            "accepted": report.accepted,
            "state_changed": false,
            "input_ack_seq": report.ack_seqs.last().copied(),
        }))
        .with_capabilities(["input"]),
        Err(err) => session_error_envelope(request_id, Some(session_id), &err),
    }
}

fn launch_envelope(request_id: &str, session_id: &str, report: &LaunchReport) -> Envelope {
    let result = ladder_json(report);
    let mut metrics = Metrics::wall_ms(report.wall_ms);
    if report.peak_rss_bytes > 0 {
        metrics = metrics.with_peak_rss_bytes(report.peak_rss_bytes);
    }
    metrics.classes_loaded = Some(report.classes_loaded as u64);
    let envelope = match &report.root_blocker {
        None => Envelope::pass(
            request_id,
            ProtocolBackend::AcosNative,
            Fidelity::NativeRuntime,
            metrics,
        ),
        Some(blocker) => {
            let code = ErrorCode::from_str_opt(&blocker.code).unwrap_or(ErrorCode::Internal);
            Envelope::failure(
                request_id,
                ProtocolBackend::AcosNative,
                Fidelity::NativeRuntime,
                ProtocolError::new(code, blocker.message.clone())
                    .with_details(details_json(&blocker.details)),
                metrics,
            )
        }
    };
    envelope
        .with_session_id(session_id.to_string())
        .with_value_result(result)
        .with_capabilities(["apk.inspect", "lifecycle"])
}

fn ladder_json(report: &LaunchReport) -> Value {
    let stages: Vec<Value> = report
        .stages
        .iter()
        .map(|stage| {
            let status = match stage.status {
                StageStatus::Ok => "ok",
                StageStatus::Blocked => "blocked",
                StageStatus::NotReached => "not_reached",
            };
            json!({
                "stage": stage.name,
                "ok": stage.status == StageStatus::Ok,
                "status": status,
                "blocker": stage.blocker.as_ref().map(|b| json!({
                    "code": b.code,
                    "message": b.message,
                    "details": details_json(&b.details),
                })),
            })
        })
        .collect();
    json!({
        "stages": stages,
        "reached_stage": report.reached_stage,
        "package": report.package,
        "activity": report.activity,
        "classes_loaded": report.classes_loaded,
        "events": report.events,
        "wall_ms": report.wall_ms,
        "peak_rss_bytes": report.peak_rss_bytes,
    })
}

fn details_json(details: &[(String, String)]) -> Value {
    let mut map = serde_json::Map::new();
    for (key, value) in details {
        map.insert(key.clone(), Value::String(value.clone()));
    }
    Value::Object(map)
}

fn session_error_envelope(
    request_id: &str,
    session_id: Option<&str>,
    err: &SessionError,
) -> Envelope {
    let code = ErrorCode::from_str_opt(err.code).unwrap_or(ErrorCode::Internal);
    let mut error = ProtocolError::new(code, err.message.clone());
    if !err.details.is_empty() {
        error = error.with_details(details_json(&err.details));
    }
    let mut envelope = Envelope::failure(
        request_id,
        ProtocolBackend::AcosNative,
        Fidelity::NativeRuntime,
        error,
        Metrics::wall_ms(0),
    );
    if let Some(id) = session_id {
        envelope = envelope.with_session_id(id.to_string());
    }
    envelope
}

/// Serialize a live `ViewTree` into the `observe_tree` result object.
fn view_tree_json(tree: &ViewTree) -> Value {
    fn walk(node: &ViewNode) -> Value {
        json!({
            "id": node.id,
            "class": node.class,
            "resource_id": node.resource_id,
            "text": node.text,
            "content_desc": node.content_desc,
            "bounds": node.bounds,
            "visible": node.visible,
            "enabled": node.enabled,
            "focused": node.focused,
            "clickable": node.clickable,
            "scrollable": node.scrollable,
            "checked": node.checked,
            "package": node.package,
            "children": node.children.iter().map(walk).collect::<Vec<_>>(),
        })
    }
    json!({
        "windows": tree.windows,
        "truncated": tree.truncated,
        "max_nodes": tree.max_nodes,
        "nodes": tree.nodes.iter().map(walk).collect::<Vec<_>>(),
    })
}

fn invalid_argument(request_id: &str, field: &str, reason: &str) -> Envelope {
    Envelope::failure(
        request_id,
        ProtocolBackend::AcosNative,
        Fidelity::NativeRuntime,
        ProtocolError::new(
            ErrorCode::InvalidArgument,
            format!("invalid argument `{field}`: {reason}"),
        )
        .with_details(json!({"field": field, "reason": reason})),
        Metrics::wall_ms(0),
    )
}

fn unsafe_path(request_id: &str, path: &str, reason: &str) -> Envelope {
    Envelope::failure(
        request_id,
        ProtocolBackend::AcosNative,
        Fidelity::NativeRuntime,
        ProtocolError::new(ErrorCode::UnsafePath, reason.to_string())
            .with_details(json!({"path": path})),
        Metrics::wall_ms(0),
    )
}

fn unsupported(request_id: &str, capability: &str, message: &str) -> Envelope {
    Envelope::failure(
        request_id,
        ProtocolBackend::AcosNative,
        Fidelity::NativeRuntime,
        ProtocolError::new(ErrorCode::UnsupportedBackend, message)
            .with_details(json!({"capability": capability})),
        Metrics::wall_ms(0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observe_without_a_frame_is_not_ready() {
        let backend = SessionBackend::with_cli(
            CliBackend::new(BackendConfig::for_binary("/nonexistent/acos")),
            SessionOptions::default(),
        );
        let ctx = CallContext {
            cancel: crate::CancelToken::new(),
            deadline: std::time::Instant::now() + Duration::from_secs(5),
            budget: Default::default(),
            allowed_roots: vec![std::env::current_dir().unwrap()],
            artifacts_dir: std::env::current_dir().unwrap(),
            max_arg_chars: 4096,
            max_output_bytes: 4096,
        };
        let envelope = backend.call(
            CommandName::Observe,
            &json!({"base": {"request_id": "req_1", "session_id": "s_test"}}),
            &ctx,
        );
        assert_eq!(envelope.status(), acos_protocol::Status::Unsupported);
        assert_eq!(
            envelope.error().unwrap().details.as_ref().unwrap()["reason"],
            "not_ready"
        );
    }
}
