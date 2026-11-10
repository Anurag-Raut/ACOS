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
        Ok(report) => Envelope::pass(
            request_id,
            ProtocolBackend::AcosNative,
            Fidelity::NativeRuntime,
            Metrics::wall_ms(0),
        )
        .with_session_id(session_id)
        .with_value_result(json!({
            "session_id": session_id,
            "action": report.kind,
            "accepted": report.accepted,
            "state_changed": false,
            "input_ack_seq": report.ack_seqs.last().copied(),
        }))
        .with_capabilities(["input"]),
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
    let request_id = new_request_id();
    let session = start_session(args.timeout);
    if let Some(envelope) = maybe_launch(&session, &args.apk, &request_id) {
        return emit(&envelope, args.json);
    }
    let envelope = match session.logs() {
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
                &request_id,
                ProtocolBackend::AcosNative,
                Fidelity::NativeRuntime,
                Metrics::wall_ms(0),
            )
            .with_session_id(args.session.clone())
            .with_value_result(json!({
                "session_id": args.session,
                "count": records.len(),
                "logs": records,
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

/// `acos session` subcommands.
#[derive(Debug, clap::Subcommand)]
pub enum SessionCommand {
    /// Report the session's launched state and last reached stage.
    Status(SessionCmdArgs),
    /// Recreate the session's task state (drop the VM).
    Reset(SessionCmdArgs),
    /// Close the session (idempotent).
    Close(SessionCmdArgs),
}

/// Run `acos session <subcommand>`.
pub fn session(command: SessionCommand) -> ExitCode {
    match command {
        SessionCommand::Status(args) => run_status(args),
        SessionCommand::Reset(args) => run_reset(args),
        SessionCommand::Close(args) => run_close(args),
    }
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
