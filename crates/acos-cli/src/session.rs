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
    emit_with_text(envelope, json, None)
}

/// A tool's result rendered as the human lines after the status line.
///
/// Without one, text mode answers only with the envelope line (`req_…:
/// ok=true status=pass`), which is unreadable for a tool whose result *is* the
/// answer (`session list`: "how many sessions are live?" was only answerable
/// with `--json`).
type ResultText = fn(&Envelope) -> String;

fn emit_with_text(envelope: &Envelope, json: bool, text: Option<ResultText>) -> ExitCode {
    if json {
        crate::print_json(envelope);
    } else {
        // The request id is labelled as a *request*: it looked like a session
        // handle, so an agent tried `acos session destroy req_…`.
        print!("{}", status_line(envelope));
        if envelope.status().is_pass() {
            if let (Some(render), Some(_)) = (text, envelope.result()) {
                print!("{}", render(envelope));
            }
        } else if let Some(error) = envelope.error() {
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
        "request {}: ok={} status={}\n",
        envelope.request_id(),
        envelope.ok(),
        envelope.status()
    )
}

fn daemon_base(session: &str) -> Value {
    json!({"request_id": new_request_id(), "session_id": session})
}

fn emit_daemon_call(client: DaemonClient, tool: &str, args: &Value, json: bool) -> ExitCode {
    emit_daemon_call_with(client, tool, args, json, None)
}

fn emit_daemon_call_with(
    mut client: DaemonClient,
    tool: &str,
    args: &Value,
    json: bool,
    text: Option<ResultText>,
) -> ExitCode {
    match client.call(tool, args) {
        Ok(envelope) => emit_with_text(&envelope, json, text),
        Err(e) => {
            let envelope = fail(
                &new_request_id(),
                ErrorCode::Internal,
                format!("session daemon: {e}"),
                json!({"tool": tool}),
            );
            emit_with_text(&envelope, json, text)
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
    call_ensure_daemon_with(tool, args, json, None)
}

fn call_ensure_daemon_with(
    tool: &str,
    args: &Value,
    json: bool,
    text: Option<ResultText>,
) -> Option<ExitCode> {
    if !serve::daemon_enabled() {
        return None;
    }
    match serve::ensure_daemon() {
        Ok(client) => Some(emit_daemon_call_with(client, tool, args, json, text)),
        Err(e) => {
            let envelope = fail(&new_request_id(), ErrorCode::Internal, e, json!({}));
            Some(emit_with_text(&envelope, json, text))
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

    /// Request the compact visible-text rendering.
    #[arg(long)]
    pub text: bool,

    /// Return the full tree inline instead of the compact screen (the full
    /// tree is always written to `tree_path`).
    #[arg(long)]
    pub full: bool,

    /// Compact-screen page cursor (`tree_next_cursor` from a previous reply).
    #[arg(long, default_value_t = 0)]
    pub cursor: usize,

    /// Explicitly request the frame (the default `observe` result). Mutually
    /// exclusive with `--tree`/`--text`; accepted so `observe --frame` works.
    #[arg(long)]
    pub frame: bool,

    /// Wall-clock ceiling for an `--apk` launch, in seconds.
    #[arg(long, default_value_t = 120)]
    pub timeout: u64,

    /// Emit the standard ACOS JSON envelope instead of a text summary.
    #[arg(long)]
    pub json: bool,
}

/// Run `acos observe`.
pub fn observe(args: ObserveArgs) -> ExitCode {
    if args.frame && (args.tree || args.text) {
        let envelope = fail(
            &new_request_id(),
            ErrorCode::InvalidArgument,
            "`--frame` cannot be combined with `--tree`/`--text`",
            json!({"field": "frame"}),
        );
        return emit(&envelope, args.json);
    }
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
            "full": args.full,
            "cursor": args.cursor,
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
                    "full_tree": args.full,
                    "cursor": args.cursor,
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

    /// Tap the node whose `resource_id` equals this value (or its bare local
    /// name after `/`), the same value `observe --tree` exposes.
    #[arg(long = "id")]
    pub id: Option<String>,
    /// Tap the node whose `content_desc` equals this value — the accessibility
    /// label a React Native pressable carries its whole name in (for example
    /// `--content-desc "Search Japanese dictionary"`). Exactly one visible
    /// match; several are `AMBIGUOUS_SELECTOR` with the candidates listed.
    #[arg(long = "content-desc")]
    pub content_desc: Option<String>,
    /// Tap the React Native node with this `testID` (resolves to the nearest
    /// clickable ancestor; requires an RN session).
    #[arg(long = "testID")]
    pub test_id: Option<String>,

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
            "id": args.id,
            "content_desc": args.content_desc,
            "testID": args.test_id,
        });
        if let Some(code) = call_running_daemon("acos_tap", &daemon_args, args.json) {
            return code;
        }
    }
    let request_id = new_request_id();
    if let Some(desc) = &args.content_desc {
        if args.apk.is_none() {
            // A content description can only be resolved against a live tree,
            // which the daemon owns. Say so instead of falling back to a
            // tree-less session that cannot honour the selector.
            let envelope = fail(
                &request_id,
                ErrorCode::UnsupportedBackend,
                format!(
                    "`--content-desc {desc}` needs a live session in the `acos serve` daemon \
                     (start one, or launch with `acos launch --keep`); the stateless path has no \
                     UI tree to resolve it against"
                ),
                json!({"field": "content_desc", "engine": "daemon"}),
            );
            return emit(&envelope, args.json);
        }
    }
    let session = start_session(args.timeout);
    if let Some(envelope) = maybe_launch(&session, &args.apk, &request_id) {
        return emit(&envelope, args.json);
    }

    let result = if let Some(id) = &args.id {
        session.tap_id(id.clone())
    } else if let Some(test_id) = &args.test_id {
        // A testID selector only exists on a React Native tree, which is owned
        // by the session daemon's confined child; the stateless CLI cannot
        // resolve it.
        let envelope = fail(
            &request_id,
            ErrorCode::InvalidArgument,
            format!(
                "`--testID {test_id}` needs a live React Native session in the `acos serve` daemon"
            ),
            json!({"field": "testID"}),
        );
        return emit(&envelope, args.json);
    } else if let Some(label) = &args.label {
        session.tap_text(label.clone())
    } else {
        match (args.x, args.y) {
            (Some(x), Some(y)) => session.tap(x as f32, y as f32),
            _ => {
                let envelope = fail(
                    &request_id,
                    ErrorCode::InvalidArgument,
                    "tap needs both `x` and `y`, `--text <label>`, `--id <resource_id>`, or `--testID <id>`",
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

/// Arguments for `acos scroll`.
///
/// A scroll is a vertical MotionEvent gesture: DOWN at `(x, y1)`, interpolated
/// MOVEs, UP at `(x, y2)`. It is the tool an agent uses to scroll a list that
/// has no scroll button; `acos swipe` delivers the same sequence but `scroll`
/// names the intent and takes a single axis.
#[derive(Debug, clap::Args)]
pub struct ScrollArgs {
    /// Session id label (default `s_cli`).
    #[arg(long, default_value = "s_cli")]
    pub session: String,

    /// X coordinate of the gesture (finger stays on this column).
    pub x: i32,

    /// Start Y (finger down).
    pub y1: i32,

    /// End Y (finger up). A smaller `y2` scrolls content down (finger up).
    pub y2: i32,

    /// Gesture duration in milliseconds.
    #[arg(long, default_value_t = 300)]
    pub duration_ms: u64,

    /// Number of interpolated motion steps.
    #[arg(long, default_value_t = 8)]
    pub steps: usize,

    /// Launch this APK first, then scroll.
    #[arg(long)]
    pub apk: Option<PathBuf>,

    /// Wall-clock ceiling for an `--apk` launch, in seconds.
    #[arg(long, default_value_t = 120)]
    pub timeout: u64,

    /// Emit the standard ACOS JSON envelope instead of a text summary.
    #[arg(long)]
    pub json: bool,
}

/// Run `acos scroll`.
pub fn scroll(args: ScrollArgs) -> ExitCode {
    if args.apk.is_none() {
        let daemon_args = json!({
            "base": daemon_base(&args.session),
            "from": {"x": args.x, "y": args.y1},
            "to": {"x": args.x, "y": args.y2},
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
        (args.x as f32, args.y1 as f32),
        (args.x as f32, args.y2 as f32),
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
            "state_changed": report.state_changed,
            "input_ack_seq": report.ack_seqs.last().copied(),
            "text_path": report.path,
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

    /// Only keep records whose tag contains this substring (case-insensitive).
    #[arg(long)]
    pub tag: Option<String>,

    /// Only keep records at or above this logcat priority (`V`/`D`/`I`/`W`/`E`/`F`
    /// or `2`..=`7`). Defaults to `W`; pass `V` for the whole buffer.
    #[arg(long = "min-priority")]
    pub min_priority: Option<String>,

    /// Emit the standard ACOS JSON envelope instead of a text summary.
    #[arg(long)]
    pub json: bool,
}

/// Run `acos logs`.
pub fn logs(args: LogsArgs) -> ExitCode {
    if args.apk.is_none() {
        let daemon_args = json!({
            "base": daemon_base(&args.session),
            "tag": args.tag,
            "min_priority": args.min_priority,
        });
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
            // Default view: `W` and above (an agent reading logs wants the app's
            // problems, not 235 lines of SoLoader noise); `--min-priority V`
            // still returns the whole buffer, which is also what stays in
            // `logs/logcat.txt` on an artifacts run.
            let min_priority = match args.min_priority.as_deref() {
                Some(text) => acos_runtime::session::parse_log_priority(text),
                None => Some(5),
            };
            let records: Vec<Value> =
                acos_runtime::session::summarize_logs(&lines, args.tag.as_deref(), min_priority)
                    .into_iter()
                    .map(|record| {
                        let repeat = record.repeat_count;
                        let line = if repeat > 1 {
                            format!("{} (repeated {repeat} times)", record.line)
                        } else {
                            record.line.clone()
                        };
                        json!({
                            "priority": record.priority,
                            "tag": record.tag,
                            "message": record.message,
                            "line": line,
                            "repeat_count": repeat,
                        })
                    })
                    .collect();
            let kept = records
                .iter()
                .map(|r| r["repeat_count"].as_u64().unwrap_or(1))
                .sum::<u64>();
            let collapsed = kept - records.len() as u64;
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
                "total": lines.len(),
                "count": records.len(),
                "collapsed": collapsed,
                "filter": {
                    "min_priority": args.min_priority.clone().unwrap_or_else(|| "W".to_string()),
                    "default_view": args.min_priority.is_none(),
                },
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
    /// Session id or label (positional form, e.g. `acos session destroy s_cli`).
    #[arg(value_name = "SESSION")]
    pub session_id: Option<String>,

    /// Session id label (default `s_cli`).
    #[arg(long, default_value = "s_cli")]
    pub session: String,

    /// Launch this APK first.
    #[arg(long)]
    pub apk: Option<PathBuf>,

    /// Wall-clock ceiling for an `--apk` launch, in seconds.
    #[arg(long, default_value_t = 120)]
    pub timeout: u64,

    /// Only keep log records whose tag contains this substring (`session logs`).
    #[arg(long)]
    pub tag: Option<String>,

    /// Only keep log records at or above this priority (`session logs`).
    /// Defaults to `W`; pass `V` for the whole buffer.
    #[arg(long = "min-priority")]
    pub min_priority: Option<String>,

    /// Emit the standard ACOS JSON envelope instead of a text summary.
    #[arg(long)]
    pub json: bool,
}

impl SessionCmdArgs {
    /// The addressed session: the positional id wins, `--session` stays as the
    /// default-bearing flag. Before this, `acos session destroy s_cli` was a
    /// clap error ("unexpected argument") and only `--session` worked, while
    /// the printed request id looked like the handle an agent tried there.
    pub fn resolve_session(&self) -> String {
        self.session_id
            .clone()
            .unwrap_or_else(|| self.session.clone())
    }
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
    /// Request the compact visible-text rendering.
    #[arg(long)]
    pub text: bool,
    /// Return the full tree inline instead of the compact screen.
    #[arg(long)]
    pub full: bool,
    /// Compact-screen page cursor.
    #[arg(long, default_value_t = 0)]
    pub cursor: usize,
    /// Explicitly request the frame (the default result).
    #[arg(long)]
    pub frame: bool,
    /// Write the completed frame as PNG to this path.
    #[arg(long)]
    pub screenshot: Option<PathBuf>,
    /// P12b: strict/fast settle deadline in milliseconds (default: engine).
    #[arg(long)]
    pub settle_timeout_ms: Option<u64>,
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
    /// Tap the node whose `resource_id` equals this value.
    #[arg(long = "id")]
    pub id: Option<String>,
    /// Tap the node whose `content_desc` equals this value (the accessibility
    /// label RN pressables carry their name in); exactly one visible match.
    #[arg(long = "content-desc")]
    pub content_desc: Option<String>,
    /// Tap the React Native node with this `testID`.
    #[arg(long = "testID")]
    pub test_id: Option<String>,
    /// P12b: strict/fast settle deadline in milliseconds (default: engine).
    #[arg(long)]
    pub settle_timeout_ms: Option<u64>,
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
    /// P12b: strict/fast settle deadline in milliseconds (default: engine).
    #[arg(long)]
    pub settle_timeout_ms: Option<u64>,
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
    /// P12b: strict/fast settle deadline in milliseconds (default: engine).
    #[arg(long)]
    pub settle_timeout_ms: Option<u64>,
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
    if let Some(code) = call_ensure_daemon_with(
        daemon::TOOL_LIST,
        &daemon_args,
        args.json,
        Some(session_list_text),
    ) {
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

/// Text mode for `acos session list`: the count and one line per session.
///
/// `0 sessions (art)` answers "is anything live?" without `--json`; each row
/// carries the id, the agent's launch label, the age and the guest's RSS (the
/// same hygiene fields `acos_status` reports). Missing fields print `-` rather
/// than disappearing: an older daemon must still produce readable rows.
fn session_list_text(envelope: &Envelope) -> String {
    let Some(result) = envelope.result() else {
        return String::new();
    };
    let engine = result
        .get("engine")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let sessions = result
        .get("sessions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut out = format!(
        "{} session{} ({engine})\n",
        sessions.len(),
        if sessions.len() == 1 { "" } else { "s" }
    );
    for session in &sessions {
        let text = |key: &str| {
            session
                .get(key)
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .unwrap_or("-")
        };
        let age = session
            .get("age_s")
            .and_then(Value::as_u64)
            .map_or_else(|| "-".to_string(), |secs| format!("{secs}s"));
        let rss = session
            .get("guest_rss_kb")
            .and_then(Value::as_u64)
            .map_or_else(|| "-".to_string(), |kb| format!("{kb} kB"));
        out.push_str(&format!(
            "  {}  label={}  age={age}  rss={rss}\n",
            text("session_id"),
            text("label"),
        ));
    }
    out
}

fn session_status(args: SessionCmdArgs) -> ExitCode {
    let daemon_args = json!({"base": daemon_base(&args.resolve_session())});
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
        "full": args.full,
        "cursor": args.cursor,
        "png": args.screenshot.is_some(),
        "screenshot": args.screenshot.as_ref().map(|p| p.display().to_string()),
        "settle_timeout_ms": args.settle_timeout_ms,
    });
    if let Some(code) = call_ensure_daemon(tool, &daemon_args, args.json) {
        return code;
    }
    observe(ObserveArgs {
        session: args.session,
        apk: None,
        screenshot: args.screenshot,
        tree: args.tree,
        text: args.text,
        full: args.full,
        cursor: args.cursor,
        frame: args.frame,
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
        "id": args.id,
        "content_desc": args.content_desc,
        "testID": args.test_id,
        "settle_timeout_ms": args.settle_timeout_ms,
    });
    if let Some(code) = call_ensure_daemon("acos_tap", &daemon_args, args.json) {
        return code;
    }
    tap(TapArgs {
        session: args.session,
        x: args.x,
        y: args.y,
        label: args.label,
        id: args.id,
        content_desc: args.content_desc,
        test_id: args.test_id,
        apk: None,
        timeout: 120,
        json: args.json,
    })
}

fn session_key(args: SessionKeyArgs) -> ExitCode {
    let daemon_args = json!({
        "base": daemon_base(&args.session),
        "code": args.code,
        "settle_timeout_ms": args.settle_timeout_ms,
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
        "settle_timeout_ms": args.settle_timeout_ms,
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
    let session = args.resolve_session();
    let daemon_args = json!({
        "base": daemon_base(&session),
        "tag": args.tag,
        "min_priority": args.min_priority,
    });
    if let Some(code) = call_ensure_daemon("acos_logs", &daemon_args, args.json) {
        return code;
    }
    logs(LogsArgs {
        session,
        apk: None,
        timeout: args.timeout,
        tag: args.tag.clone(),
        min_priority: args.min_priority.clone(),
        json: args.json,
    })
}

fn session_reset(args: SessionCmdArgs) -> ExitCode {
    let daemon_args = json!({"base": daemon_base(&args.resolve_session())});
    if let Some(code) = call_ensure_daemon("acos_reset", &daemon_args, args.json) {
        return code;
    }
    run_reset(args)
}

fn session_destroy(args: SessionCmdArgs) -> ExitCode {
    let daemon_args = json!({"base": daemon_base(&args.resolve_session())});
    if let Some(code) = call_ensure_daemon("acos_destroy", &daemon_args, args.json) {
        return code;
    }
    run_close(args)
}

fn run_status(args: SessionCmdArgs) -> ExitCode {
    let request_id = new_request_id();
    let session_id = args.resolve_session();
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
        .with_session_id(session_id.clone())
        .with_value_result(json!({
            "session_id": session_id,
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
    let session_id = args.resolve_session();
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
        .with_session_id(session_id.clone())
        .with_value_result(json!({
            "session_id": session_id,
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
    let session_id = args.resolve_session();
    // `close` never launches; it just tears the (process-local) session down.
    start_session(args.timeout).close();
    let envelope = Envelope::pass(
        &request_id,
        ProtocolBackend::AcosNative,
        Fidelity::NativeRuntime,
        Metrics::wall_ms(0),
    )
    .with_session_id(session_id.clone())
    .with_value_result(json!({
        "session_id": session_id,
        "destroyed": true,
        "already_absent": false,
    }));
    emit(&envelope, args.json)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pass_with(result: Value) -> Envelope {
        Envelope::pass(
            "req_test",
            ProtocolBackend::AcosNative,
            Fidelity::NativeRuntime,
            Metrics::wall_ms(0),
        )
        .with_value_result(result)
    }

    #[test]
    fn session_list_text_answers_the_count_without_json() {
        let envelope = pass_with(json!({"sessions": [], "count": 0, "engine": "art"}));
        assert_eq!(session_list_text(&envelope), "0 sessions (art)\n");
    }

    #[test]
    fn session_list_text_prints_id_label_age_and_rss_per_session() {
        let envelope = pass_with(json!({
            "sessions": [
                {"session_id": "s_cli", "label": "counter", "age_s": 12, "guest_rss_kb": 204800},
                {"session_id": "mcp-1", "label": "", "age_s": 1, "guest_rss_kb": 1024},
            ],
            "count": 2,
            "engine": "art",
        }));
        let text = session_list_text(&envelope);
        let mut lines = text.lines();
        assert_eq!(lines.next(), Some("2 sessions (art)"));
        assert_eq!(
            lines.next(),
            Some("  s_cli  label=counter  age=12s  rss=204800 kB")
        );
        assert_eq!(lines.next(), Some("  mcp-1  label=-  age=1s  rss=1024 kB"));
        assert_eq!(lines.next(), None);
    }

    #[test]
    fn session_list_text_keeps_rows_readable_when_fields_are_absent() {
        // An older daemon reports only the id: the row still prints, with `-`
        // for every hygiene field (never a silently missing row).
        let envelope = pass_with(json!({"sessions": [{"session_id": "s_cli"}]}));
        let text = session_list_text(&envelope);
        assert!(text.starts_with("1 session (unknown)\n"), "{text}");
        assert!(text.contains("  s_cli  label=-  age=-  rss=-"), "{text}");
    }

    #[test]
    fn text_status_line_labels_the_request_id() {
        let envelope = pass_with(json!({}));
        let line = status_line(&envelope);
        assert!(line.starts_with("request req_test: "), "{line}");
        assert!(line.contains("ok=true status=pass"), "{line}");
    }
}
