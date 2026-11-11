//! `acos observe` / `tap` / `key` / `swipe` / `logs` / `session` — the
//! stateful agent commands over an in-process [`AgentSession`].
//!
//! Each CLI invocation owns its session for the duration of the command; there
//! is no daemon yet (the task explicitly defers `acos serve`). `--apk` seeds the
//! session with a real launch before the command runs, so
//! `acos observe --apk counter.apk` observes the state a real launch reached.
//! Without `--apk` the session is a fresh, un-launched VM-less session: `observe`
//! then returns `not_ready` (never a blank image) and `tap`/`key`/`swipe` return
//! the input dispatcher's structured `NoWindowAtPoint`/`NoFocusedWindow`.
//!
//! The MCP server holds the same [`AgentSession`] across tool calls; use it when
//! a session must persist between requests.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use acos_protocol::{
    Backend as ProtocolBackend, Envelope, ErrorCode, Fidelity, Metrics, ProtocolError,
};
use acos_runtime::session::{AgentSession, SessionError, SessionOptions};
use serde_json::{json, Value};

use crate::launch::session_error_envelope;
use crate::new_request_id;
use crate::serve;

use acos_mcp::daemon::{self, DaemonClient};

fn start_session(timeout: u64) -> AgentSession {
    AgentSession::start(SessionOptions {
        wall_secs: timeout,
        ..SessionOptions::default()
    })
}

fn fail(request_id: &str, code: ErrorCode, message: impl Into<String>, details: Value) -> Envelope {
    Envelope::failure(
        request_id,
        ProtocolBackend::AcosNative,
        Fidelity::NativeRuntime,
        ProtocolError::new(code, message).with_details(details),
        Metrics::wall_ms(0),
    )
}

fn emit(envelope: &Envelope, json: bool) -> ExitCode {
    if json {
        crate::print_json(envelope);
    } else {
        print!("{}", status_line(envelope));
        if let Some(error) = envelope.error() {
            eprintln!("  error: {}: {}", error.code, error.message);
        }
    }
    if envelope.status().is_pass() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn status_line(envelope: &Envelope) -> String {
    format!(
        "{}: ok={} status={}\n",
        envelope.request_id(),
        envelope.ok(),
        envelope.status()
    )
}

fn daemon_base(session: &str) -> Value {
    json!({"request_id": new_request_id(), "session_id": session})
}

fn emit_daemon_call(mut client: DaemonClient, tool: &str, args: &Value, json: bool) -> ExitCode {
    match client.call(tool, args) {
        Ok(envelope) => emit(&envelope, json),
        Err(e) => {
            let envelope = fail(
                &new_request_id(),
                ErrorCode::Internal,
                format!("session daemon: {e}"),
                json!({"tool": tool}),
            );
            emit(&envelope, json)
        }
    }
}

/// Use a daemon that is already running. Top-level commands never auto-start.
fn call_running_daemon(tool: &str, args: &Value, json: bool) -> Option<ExitCode> {
    let client = serve::try_daemon()?;
    Some(emit_daemon_call(client, tool, args, json))
}

/// Use the daemon, starting one detached if needed (`acos session …`).
fn call_ensure_daemon(tool: &str, args: &Value, json: bool) -> Option<ExitCode> {
    if !serve::daemon_enabled() {
        return None;
    }
    match serve::ensure_daemon() {
        Ok(client) => Some(emit_daemon_call(client, tool, args, json)),
        Err(e) => {
            let envelope = fail(&new_request_id(), ErrorCode::Internal, e, json!({}));
            Some(emit(&envelope, json))
        }
    }
}

/// Seed the session with a real launch when `--apk` was given.
fn maybe_launch(
    session: &AgentSession,
    apk: &Option<PathBuf>,
    request_id: &str,
) -> Option<Envelope> {
    let apk = apk.as_ref()?;
    match session.launch(apk) {
        Ok(_) => None,
        Err(err) => Some(session_error_envelope(request_id, &err)),
    }
}

// ---------------------------------------------------------------------------
// observe
// ---------------------------------------------------------------------------

/// Arguments for `acos observe`.
#[derive(Debug, clap::Args)]
pub struct ObserveArgs {
    /// Session id label (default `s_cli`); the CLI session is process-local.
    #[arg(long, default_value = "s_cli")]
    pub session: String,

    /// Launch this APK first, then observe the state it reached.
    #[arg(long)]
    pub apk: Option<PathBuf>,

    /// Write the completed frame as PNG to this path (requires a frame).
    #[arg(long)]
    pub screenshot: Option<PathBuf>,

    /// Request the structured UI tree.
    #[arg(long)]
    pub tree: bool,

    /// Request the flat text rendering.
    #[arg(long)]
    pub text: bool,

    /// Wall-clock ceiling for an `--apk` launch, in seconds.
    #[arg(long, default_value_t = 120)]
    pub timeout: u64,

    /// Emit the standard ACOS JSON envelope instead of a text summary.
    #[arg(long)]
    pub json: bool,
}

/// Run `acos observe`.
pub fn observe(args: ObserveArgs) -> ExitCode {
    if args.apk.is_none() {
        let tool = if args.tree || args.text {
            "acos_observe_tree"
        } else {
            "acos_observe"
        };
        // Route `--screenshot` through the daemon too: the live Flutter session
        // only exists there. The daemon writes the PNG to this absolute path.
        let screenshot = args
            .screenshot
            .as_ref()
            .map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()));
        let daemon_args = json!({
            "base": daemon_base(&args.session),
            "tree": args.tree,
            "text": args.text,
            "png": screenshot.is_some(),
            "screenshot": screenshot.as_ref().map(|p| p.display().to_string()),
        });
        if let Some(code) = call_running_daemon(tool, &daemon_args, args.json) {
            return code;
        }
    }
    let request_id = new_request_id();
    let session = start_session(args.timeout);
    if let Some(envelope) = maybe_launch(&session, &args.apk, &request_id) {
        return emit(&envelope, args.json);
    }

    let status = session.status().unwrap_or_else(|_| {
        // The worker should always answer status; a dead worker is reported
        // through the command's own error path below.
        empty_status()
    });

    // `--tree`/`--text` walk the live View hierarchy instead of the frame
    // store: a tree exists as soon as the app has built one, even before a
    // frame is published. An empty tree is never returned as a pass.
    if args.tree || args.text {
        let envelope = match session.observe_tree(2000) {
            Ok(tree) => {
                let mut result = json!({
                    "session_id": args.session,
                    "package": status.package.clone().unwrap_or_default(),
                    "stage_reached": status.stage_reached,
                    "windows": tree.windows,
                    "truncated": tree.truncated,
                    "tree": crate::observe_tree::tree_to_json(&tree),
                });
                if args.text {
                    result["text"] = json!(crate::observe_tree::tree_text(&tree));
                }
                Envelope::pass(
                    &request_id,
                    ProtocolBackend::AcosNative,
                    Fidelity::NativeRuntime,
                    Metrics::wall_ms(0),
                )
                .with_session_id(args.session.clone())
                .with_value_result(result)
                .with_capabilities(["ui.tree"])
            }
            Err(err) => session_error_envelope(&request_id, &err),
        };
        return emit(&envelope, args.json);
    }

    match session.observe(args.screenshot.clone()) {
        Ok(report) => {
            let frame = report.frame.as_ref().map(|frame| {
                json!({
                    "frame_seq": frame.frame_id,
                    "width": frame.width,
                    "height": frame.height,
                    "density": 0,
                    "content_hash": frame.content_hash,
                    "cause_id": frame.cause_id.to_string(),
                    "meaningful": !frame.pixels.is_empty(),
                })
            });
            let mut result = json!({
                "session_id": args.session,
                "package": status.package.clone().unwrap_or_default(),
                "stage_reached": report.stage_reached,
                "published_frames": report.published_frames,
                "frame": frame,
            });
            if let Some((path, bytes)) = &report.screenshot {
                result["screenshot"] = json!({"path": path, "bytes": bytes});
            }
            let envelope = Envelope::pass(
                &request_id,
                ProtocolBackend::AcosNative,
                Fidelity::NativeRuntime,
                Metrics::wall_ms(0),
            )
            .with_session_id(args.session.clone())
            .with_value_result(result)
            .with_capabilities(["ui.frame"]);
            emit(&envelope, args.json)
        }
        Err(err) => {
            let envelope = session_error_envelope(&request_id, &err);
            emit(&envelope, args.json)
        }
    }
}

fn empty_status() -> acos_runtime::session::SessionState {
    acos_runtime::session::SessionState {
        launched: false,
        package: None,
        apk: None,
        stage_reached: "none",
        published_frames: 0,
    }
}

// ---------------------------------------------------------------------------
// tap / key / swipe
// ---------------------------------------------------------------------------

/// Arguments for `acos tap`.
#[derive(Debug, clap::Args)]
pub struct TapArgs {
    /// Session id label (default `s_cli`).
    #[arg(long, default_value = "s_cli")]
    pub session: String,

    /// X coordinate.
    pub x: Option<i32>,

    /// Y coordinate.
    pub y: Option<i32>,

    /// Tap the node whose text equals this label (requires a UI tree).
    #[arg(long = "text")]
    pub label: Option<String>,

    /// Launch this APK first, then tap.
    #[arg(long)]
    pub apk: Option<PathBuf>,

    /// Wall-clock ceiling for an `--apk` launch, in seconds.
    #[arg(long, default_value_t = 120)]
    pub timeout: u64,

    /// Emit the standard ACOS JSON envelope instead of a text summary.
    #[arg(long)]
    pub json: bool,
}

/// Run `acos tap`.
pub fn tap(args: TapArgs) -> ExitCode {
    if args.apk.is_none() {
        let daemon_args = json!({
            "base": daemon_base(&args.session),
            "x": args.x,
            "y": args.y,
            "text": args.label,
        });
        if let Some(code) = call_running_daemon("acos_tap", &daemon_args, args.json) {
            return code;
        }
    }
    let request_id = new_request_id();
    let session = start_session(args.timeout);
    if let Some(envelope) = maybe_launch(&session, &args.apk, &request_id) {
        return emit(&envelope, args.json);
    }

    let result = if let Some(label) = &args.label {
        session.tap_text(label.clone())
    } else {
        match (args.x, args.y) {
            (Some(x), Some(y)) => session.tap(x as f32, y as f32),
            _ => {
                let envelope = fail(
                    &request_id,
                    ErrorCode::InvalidArgument,
                    "tap needs both `x` and `y`, or `--text <label>`",
                    json!({"field": "x/y"}),
                );
                return emit(&envelope, args.json);
            }
        }
    };
    finish_input(&session, &request_id, &args.session, args.json, result)
}

/// Arguments for `acos key`.
#[derive(Debug, clap::Args)]
pub struct KeyArgs {
    /// Session id label (default `s_cli`).
    #[arg(long, default_value = "s_cli")]
    pub session: String,

    /// Key code (`4` = BACK, `66` = ENTER) or a name (`back`, `home`, `enter`).
    pub code: String,

    /// Launch this APK first, then send the key.
    #[arg(long)]
    pub apk: Option<PathBuf>,

    /// Wall-clock ceiling for an `--apk` launch, in seconds.
    #[arg(long, default_value_t = 120)]
    pub timeout: u64,

    /// Emit the standard ACOS JSON envelope instead of a text summary.
    #[arg(long)]
    pub json: bool,
}

/// Run `acos key`.
pub fn key(args: KeyArgs) -> ExitCode {
    if args.apk.is_none() {
        let daemon_args = json!({
            "base": daemon_base(&args.session),
            "code": args.code,
        });
        if let Some(code) = call_running_daemon("acos_key", &daemon_args, args.json) {
            return code;
        }
    }
    let request_id = new_request_id();
    let session = start_session(args.timeout);
    if let Some(envelope) = maybe_launch(&session, &args.apk, &request_id) {
        return emit(&envelope, args.json);
    }
    let Some(code) = parse_key(&args.code) else {
        let envelope = fail(
            &request_id,
            ErrorCode::InvalidArgument,
            format!(
                "unknown key {:?}; use a key code or back/home/enter/tab/escape",
                args.code
            ),
            json!({"field": "code", "value": args.code}),
        );
        return emit(&envelope, args.json);
    };
    let result = session.key(code);
    finish_input(&session, &request_id, &args.session, args.json, result)
}

fn parse_key(value: &str) -> Option<i32> {
    if let Ok(code) = value.parse::<i32>() {
        return Some(code);
    }
    match value.to_ascii_lowercase().as_str() {
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

/// Arguments for `acos swipe`.
#[derive(Debug, clap::Args)]
pub struct SwipeArgs {
    /// Session id label (default `s_cli`).
    #[arg(long, default_value = "s_cli")]
    pub session: String,

    /// Start X.
    pub x1: i32,
    /// Start Y.
    pub y1: i32,
    /// End X.
    pub x2: i32,
    /// End Y.
    pub y2: i32,

    /// Gesture duration in milliseconds.
    #[arg(long, default_value_t = 300)]
    pub duration_ms: u64,

    /// Number of interpolated motion steps.
    #[arg(long, default_value_t = 8)]
    pub steps: usize,

    /// Launch this APK first, then swipe.
    #[arg(long)]
    pub apk: Option<PathBuf>,

    /// Wall-clock ceiling for an `--apk` launch, in seconds.
    #[arg(long, default_value_t = 120)]
    pub timeout: u64,

    /// Emit the standard ACOS JSON envelope instead of a text summary.
    #[arg(long)]
    pub json: bool,
}

/// Run `acos swipe`.
pub fn swipe(args: SwipeArgs) -> ExitCode {
    if args.apk.is_none() {
        let daemon_args = json!({
            "base": daemon_base(&args.session),
            "from": {"x": args.x1, "y": args.y1},
            "to": {"x": args.x2, "y": args.y2},
            "duration_ms": args.duration_ms,
        });
        if let Some(code) = call_running_daemon("acos_swipe", &daemon_args, args.json) {
            return code;
        }
    }
    let request_id = new_request_id();
    let session = start_session(args.timeout);
    if let Some(envelope) = maybe_launch(&session, &args.apk, &request_id) {
        return emit(&envelope, args.json);
    }
    let result = session.swipe(
        (args.x1 as f32, args.y1 as f32),
        (args.x2 as f32, args.y2 as f32),
        Duration::from_millis(args.duration_ms),
        args.steps.max(1),
    );
    finish_input(&session, &request_id, &args.session, args.json, result)
}

fn finish_input(
    _session: &AgentSession,
    request_id: &str,
    session_id: &str,
    json: bool,
    result: Result<acos_runtime::session::InputReport, SessionError>,
) -> ExitCode {
    let envelope = match result {
        Ok(report) => {
            let mut value = json!({
                "session_id": session_id,
                "action": report.kind,
                "accepted": report.accepted,
                "state_changed": report.state_changed,
                "input_ack_seq": report.ack_seqs.last().copied(),
                "text_path": report.path,
            });
            if report.accepted && !report.state_changed {
                value["note"] = json!(
                    "input was delivered but the live tree did not change; the event may not \
                     have landed. Re-observe the tree before trusting the action."
                );
            } else if report.path.as_deref() == Some("key-events") {
                value["note"] = json!(
                    "no editable InputConnection was found; text was sent as key events to the \
                     focused view, so it may not have been inserted."
                );
            }
            Envelope::pass(
                request_id,
                ProtocolBackend::AcosNative,
                Fidelity::NativeRuntime,
                Metrics::wall_ms(0),
            )
            .with_session_id(session_id)
            .with_value_result(value)
            .with_capabilities(["input"])
        }
        Err(err) => session_error_envelope(request_id, &err),
    };
    emit(&envelope, json)
}

// ---------------------------------------------------------------------------
// logs
// ---------------------------------------------------------------------------

/// Arguments for `acos logs`.
#[derive(Debug, clap::Args)]
pub struct LogsArgs {
    /// Session id label (default `s_cli`).
    #[arg(long, default_value = "s_cli")]
    pub session: String,

    /// Launch this APK first, then read its log buffer.
    #[arg(long)]
    pub apk: Option<PathBuf>,

    /// Wall-clock ceiling for an `--apk` launch, in seconds.
    #[arg(long, default_value_t = 120)]
    pub timeout: u64,

    /// Emit the standard ACOS JSON envelope instead of a text summary.
    #[arg(long)]
    pub json: bool,
}

/// Run `acos logs`.
pub fn logs(args: LogsArgs) -> ExitCode {
    if args.apk.is_none() {
        let daemon_args = json!({"base": daemon_base(&args.session)});
        if let Some(code) = call_running_daemon("acos_logs", &daemon_args, args.json) {
            return code;
        }
    }
    let request_id = new_request_id();
    let session = start_session(args.timeout);
    if let Some(envelope) = maybe_launch(&session, &args.apk, &request_id) {
        return emit(&envelope, args.json);
    }
    let envelope = match session.logs() {
        Ok(lines) => {
            let status = session.status().ok();
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
            // An empty buffer is a real pass, but it is misleading right after a
            // launch that stopped before `Activity.onCreate`; carry the launch
            // stage so the agent can tell the two apart.
            let stage_reached = status.as_ref().map(|s| s.stage_reached).unwrap_or("none");
            Envelope::pass(
                &request_id,
                ProtocolBackend::AcosNative,
                Fidelity::NativeRuntime,
                Metrics::wall_ms(0),
            )
            .with_session_id(args.session.clone())
            .with_value_result(json!({
                "session_id": args.session,
                "launched": status.as_ref().map(|s| s.launched).unwrap_or(false),
                "stage_reached": stage_reached,
                "count": records.len(),
                "logs": records,
                "note": if records.is_empty() {
                    "empty is valid only if the app reached onCreate; check stage_reached (and `acos launch`) before concluding anything"
                } else {
                    ""
                },
            }))
            .with_capabilities(["logs"])
        }
        Err(err) => session_error_envelope(&request_id, &err),
    };
    emit(&envelope, args.json)
}

// ---------------------------------------------------------------------------
// session status|reset|close
// ---------------------------------------------------------------------------

/// Arguments common to the `acos session` subcommands.
#[derive(Debug, clap::Args)]
pub struct SessionCmdArgs {
    /// Session id label (default `s_cli`).
    #[arg(long, default_value = "s_cli")]
    pub session: String,

    /// Launch this APK first.
    #[arg(long)]
    pub apk: Option<PathBuf>,

    /// Wall-clock ceiling for an `--apk` launch, in seconds.
    #[arg(long, default_value_t = 120)]
    pub timeout: u64,

    /// Emit the standard ACOS JSON envelope instead of a text summary.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `acos session list`.
#[derive(Debug, clap::Args)]
pub struct SessionListArgs {
    /// Emit the standard ACOS JSON envelope instead of a text summary.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `acos session observe`.
#[derive(Debug, clap::Args)]
pub struct SessionObserveArgs {
    /// Session id label (default `s_cli`).
    #[arg(long, default_value = "s_cli")]
    pub session: String,
    /// Request the structured UI tree.
    #[arg(long)]
    pub tree: bool,
    /// Request the flat text rendering.
    #[arg(long)]
    pub text: bool,
    /// Emit the standard ACOS JSON envelope instead of a text summary.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `acos session tap`.
#[derive(Debug, clap::Args)]
pub struct SessionTapArgs {
    /// Session id label (default `s_cli`).
    #[arg(long, default_value = "s_cli")]
    pub session: String,
    /// X coordinate.
    pub x: Option<i32>,
    /// Y coordinate.
    pub y: Option<i32>,
    /// Tap the node whose text equals this label.
    #[arg(long = "text")]
    pub label: Option<String>,
    /// Emit the standard ACOS JSON envelope instead of a text summary.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `acos session key`.
#[derive(Debug, clap::Args)]
pub struct SessionKeyArgs {
    /// Session id label (default `s_cli`).
    #[arg(long, default_value = "s_cli")]
    pub session: String,
    /// Key code (`4` = BACK, `66` = ENTER) or a name (`back`, `home`, `enter`).
    pub code: String,
    /// Emit the standard ACOS JSON envelope instead of a text summary.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `acos session text`.
#[derive(Debug, clap::Args)]
pub struct SessionTextArgs {
    /// Session id label (default `s_cli`).
    #[arg(long, default_value = "s_cli")]
    pub session: String,
    /// Text to type into the focused editor (positional form).
    pub text: Option<String>,
    /// Text to type into the focused editor (flag form, matching `acos tap --text`).
    #[arg(long = "text", value_name = "TEXT")]
    pub text_flag: Option<String>,
    /// Emit the standard ACOS JSON envelope instead of a text summary.
    #[arg(long)]
    pub json: bool,
}

impl SessionTextArgs {
    /// The text from either the positional or the `--text` flag.
    fn resolve_text(&self) -> Result<String, String> {
        self.text_flag
            .clone()
            .or_else(|| self.text.clone())
            .ok_or_else(|| "missing text (pass it positionally or with `--text`)".to_string())
    }
}

/// `acos session` subcommands. Every one talks to the `acos serve` daemon and
/// auto-starts it when it is not running (unless `ACOS_NO_DAEMON=1`).
#[derive(Debug, clap::Subcommand)]
pub enum SessionCommand {
    /// List the sessions the daemon is holding.
    List(SessionListArgs),
    /// Report the session's launched state and last reached stage.
    Status(SessionCmdArgs),
    /// Observe the session's latest frame or UI tree.
    Observe(SessionObserveArgs),
    /// Tap a point or a text label.
    Tap(SessionTapArgs),
    /// Press a key code.
    Key(SessionKeyArgs),
    /// Type text into the focused editor (real InputConnection.commitText path).
    Text(SessionTextArgs),
    /// Read the guest log buffer.
    Logs(SessionCmdArgs),
    /// Recreate the session's task state (drop the VM).
    Reset(SessionCmdArgs),
    /// Destroy the session (idempotent).
    Destroy(SessionCmdArgs),
    /// Alias of `destroy`.
    Close(SessionCmdArgs),
}

/// Run `acos session <subcommand>`.
pub fn session(command: SessionCommand) -> ExitCode {
    match command {
        SessionCommand::List(args) => session_list(args),
        SessionCommand::Status(args) => session_status(args),
        SessionCommand::Observe(args) => session_observe(args),
        SessionCommand::Tap(args) => session_tap(args),
        SessionCommand::Key(args) => session_key(args),
        SessionCommand::Text(args) => session_text(args),
        SessionCommand::Logs(args) => session_logs(args),
        SessionCommand::Reset(args) => session_reset(args),
        SessionCommand::Destroy(args) => session_destroy(args),
        SessionCommand::Close(args) => session_destroy(args),
    }
}

fn session_list(args: SessionListArgs) -> ExitCode {
    let daemon_args = json!({"base": daemon_base("s_cli")});
    if let Some(code) = call_ensure_daemon(daemon::TOOL_LIST, &daemon_args, args.json) {
        return code;
    }
    let envelope = fail(
        &new_request_id(),
        ErrorCode::UnsupportedBackend,
        "`acos session list` requires the session daemon; unset ACOS_NO_DAEMON to enable it",
        json!({"tool": daemon::TOOL_LIST}),
    );
    emit(&envelope, args.json)
}

fn session_status(args: SessionCmdArgs) -> ExitCode {
    let daemon_args = json!({"base": daemon_base(&args.session)});
    if let Some(code) = call_ensure_daemon("acos_status", &daemon_args, args.json) {
        return code;
    }
    run_status(args)
}

fn session_observe(args: SessionObserveArgs) -> ExitCode {
    let tool = if args.tree || args.text {
        "acos_observe_tree"
    } else {
        "acos_observe"
    };
    let daemon_args = json!({
        "base": daemon_base(&args.session),
        "tree": args.tree,
        "text": args.text,
    });
    if let Some(code) = call_ensure_daemon(tool, &daemon_args, args.json) {
        return code;
    }
    observe(ObserveArgs {
        session: args.session,
        apk: None,
        screenshot: None,
        tree: args.tree,
        text: args.text,
        timeout: 120,
        json: args.json,
    })
}

fn session_tap(args: SessionTapArgs) -> ExitCode {
    let daemon_args = json!({
        "base": daemon_base(&args.session),
        "x": args.x,
        "y": args.y,
        "text": args.label,
    });
    if let Some(code) = call_ensure_daemon("acos_tap", &daemon_args, args.json) {
        return code;
    }
    tap(TapArgs {
        session: args.session,
        x: args.x,
        y: args.y,
        label: args.label,
        apk: None,
        timeout: 120,
        json: args.json,
    })
}

fn session_key(args: SessionKeyArgs) -> ExitCode {
    let daemon_args = json!({
        "base": daemon_base(&args.session),
        "code": args.code,
    });
    if let Some(code) = call_ensure_daemon("acos_key", &daemon_args, args.json) {
        return code;
    }
    key(KeyArgs {
        session: args.session,
        code: args.code,
        apk: None,
        timeout: 120,
        json: args.json,
    })
}

fn session_text(args: SessionTextArgs) -> ExitCode {
    let text = match args.resolve_text() {
        Ok(text) => text,
        Err(reason) => {
            let envelope = fail(
                &new_request_id(),
                ErrorCode::InvalidArgument,
                reason,
                json!({"field": "text"}),
            );
            return emit(&envelope, args.json);
        }
    };
    let daemon_args = json!({
        "base": daemon_base(&args.session),
        "text": text.clone(),
    });
    if let Some(code) = call_ensure_daemon("acos_text", &daemon_args, args.json) {
        return code;
    }
    let request_id = new_request_id();
    let session = start_session(120);
    let result = session.text(text);
    finish_input(&session, &request_id, &args.session, args.json, result)
}

fn session_logs(args: SessionCmdArgs) -> ExitCode {
    let daemon_args = json!({"base": daemon_base(&args.session)});
    if let Some(code) = call_ensure_daemon("acos_logs", &daemon_args, args.json) {
        return code;
    }
    logs(LogsArgs {
        session: args.session,
        apk: None,
        timeout: args.timeout,
        json: args.json,
    })
}

fn session_reset(args: SessionCmdArgs) -> ExitCode {
    let daemon_args = json!({"base": daemon_base(&args.session)});
    if let Some(code) = call_ensure_daemon("acos_reset", &daemon_args, args.json) {
        return code;
    }
    run_reset(args)
}

fn session_destroy(args: SessionCmdArgs) -> ExitCode {
    let daemon_args = json!({"base": daemon_base(&args.session)});
    if let Some(code) = call_ensure_daemon("acos_destroy", &daemon_args, args.json) {
        return code;
    }
    run_close(args)
}

fn run_status(args: SessionCmdArgs) -> ExitCode {
    let request_id = new_request_id();
    let session = start_session(args.timeout);
    if let Some(envelope) = maybe_launch(&session, &args.apk, &request_id) {
        return emit(&envelope, args.json);
    }
    let envelope = match session.status() {
        Ok(state) => Envelope::pass(
            &request_id,
            ProtocolBackend::AcosNative,
            Fidelity::NativeRuntime,
            Metrics::wall_ms(0),
        )
        .with_session_id(args.session.clone())
        .with_value_result(json!({
            "session_id": args.session,
            "launched": state.launched,
            "package": state.package,
            "apk": state.apk,
            "stage_reached": state.stage_reached,
            "published_frames": state.published_frames,
            "live": true,
        })),
        Err(err) => session_error_envelope(&request_id, &err),
    };
    emit(&envelope, args.json)
}

fn run_reset(args: SessionCmdArgs) -> ExitCode {
    let request_id = new_request_id();
    let session = start_session(args.timeout);
    if let Some(envelope) = maybe_launch(&session, &args.apk, &request_id) {
        return emit(&envelope, args.json);
    }
    let envelope = match session.reset() {
        Ok(state) => Envelope::pass(
            &request_id,
            ProtocolBackend::AcosNative,
            Fidelity::NativeRuntime,
            Metrics::wall_ms(0),
        )
        .with_session_id(args.session.clone())
        .with_value_result(json!({
            "session_id": args.session,
            "scope": "all",
            "state_reset": true,
            "data_cleared": false,
            "note": "in-process session: the guest VFS and log sink are process-global, so app data is not cleared",
            "stage_reached": state.stage_reached,
        })),
        Err(err) => session_error_envelope(&request_id, &err),
    };
    emit(&envelope, args.json)
}

fn run_close(args: SessionCmdArgs) -> ExitCode {
    let request_id = new_request_id();
    // `close` never launches; it just tears the (process-local) session down.
    start_session(args.timeout).close();
    let envelope = Envelope::pass(
        &request_id,
        ProtocolBackend::AcosNative,
        Fidelity::NativeRuntime,
        Metrics::wall_ms(0),
    )
    .with_session_id(args.session.clone())
    .with_value_result(json!({
        "session_id": args.session,
        "destroyed": true,
        "already_absent": false,
    }));
    emit(&envelope, args.json)
}
