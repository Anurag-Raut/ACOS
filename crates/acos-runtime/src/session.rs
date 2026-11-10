//! **In-process agent session: one worker thread owns the [`Runtime`] behind a
//! channel command loop.**
//!
//! This is the stateful half of the agent API (`acos launch`/`observe`/`tap`/…).
//! It exists because [`Runtime`] owns an `Rc<RefCell<ServiceHost>>` and a `Vm`
//! that are not `Send`: no `&mut Runtime` may cross a thread. Mirrors
//! [`crate::session_pool`]'s worker design — the caller holds a `Sender<Cmd>`,
//! every command carries its own reply channel, and the worker creates the
//! `Runtime` **on its own thread**.
//!
//! # Honesty
//!
//! Nothing here fabricates an application state. Until the framework actually
//! reaches a lifecycle callback or publishes a completed frame, the matching
//! stage is reported as `blocked`/`not_reached`, `observe` returns a
//! `not_ready` `unsupported` error (never a blank image or an empty tree), and
//! input that has no window returns the dispatcher's structured
//! `NoWindowAtPoint`/`NoFocusedWindow` error.
//!
//! # Scope
//!
//! One session per process is the supported configuration for now: the Binder
//! transport and the surface backend are process-global seams (installed by
//! [`Runtime::with_host`] / [`install_frame_handoff`]). A second concurrent
//! `launch` in the same process would replace them, so the MCP server exposes a
//! single named session. This is the same constraint `docs/AGENT_API.md` §6
//! records as `shared-task` isolation.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use acos_apk::Apk;
use acos_vm::limits::HostGuard;
use acos_vm::natives::os_log::{self, LogRecord};
use acos_vm::vm::{CoreLib, VmOptions};

use crate::frame_handoff::{install_frame_handoff, FrameStore};
use crate::input_java;
use crate::lifecycle::LifecycleReport;
use crate::view_tree::ViewTree;
use crate::Runtime;

/// The launch stage ladder, in execution order.
///
/// `apk_parsed` is static; every later stage is only `ok` when the real
/// framework actually got there.
pub const STAGES: &[&str] = &[
    "apk_parsed",
    "process_booted",
    "bind_application",
    "application_on_create",
    "client_transaction",
    "activity_on_create",
    "on_resume",
    "first_frame",
];

/// Per-stage outcome on the launch ladder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageStatus {
    /// The stage completed.
    Ok,
    /// This is the first stage that could not complete.
    Blocked,
    /// An earlier stage blocked, so this one never ran.
    NotReached,
}

/// The structured blocker attached to a blocked stage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageBlocker {
    /// ACOS error code string (maps through `ErrorCode::from_str_opt`).
    pub code: String,
    /// Human-readable detail (may include the exception stack top / native tail).
    pub message: String,
    /// Machine-readable details (`reason`, `stage`, `classes`, ...).
    pub details: Vec<(String, String)>,
}

/// One rung of the launch ladder.
#[derive(Debug, Clone)]
pub struct StageEntry {
    /// Stable stage name.
    pub name: &'static str,
    /// Outcome.
    pub status: StageStatus,
    /// The blocker, on the first blocked stage.
    pub blocker: Option<StageBlocker>,
}

/// The `launch` result: the ladder plus resource accounting.
#[derive(Debug, Clone)]
pub struct LaunchReport {
    /// Every stage, in order.
    pub stages: Vec<StageEntry>,
    /// The first blocked stage's blocker, if any.
    pub root_blocker: Option<StageBlocker>,
    /// The last stage that completed.
    pub reached_stage: &'static str,
    /// Package name, when the APK manifest parsed.
    pub package: Option<String>,
    /// Launcher activity, when known.
    pub activity: Option<String>,
    /// Classes loaded when the run stopped.
    pub classes_loaded: usize,
    /// Guest log messages observed during the run (`onCreate`, `onResume`, ...).
    pub events: Vec<String>,
    /// Wall clock for the launch.
    pub wall_ms: u64,
    /// Peak host RSS after the run (process-wide, `/proc/self/status`).
    pub peak_rss_bytes: u64,
}

/// Frame metadata returned by `observe`.
#[derive(Debug, Clone)]
pub struct FrameInfo {
    /// Compositor frame sequence.
    pub frame_id: u64,
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Stable content hash (hex).
    pub content_hash: String,
    /// Action/request id that caused the frame.
    pub cause_id: i64,
    /// Premultiplied RGBA8888 pixels.
    pub pixels: Vec<u8>,
}

/// The `observe` result.
#[derive(Debug, Clone)]
pub struct ObserveReport {
    /// The latest completed frame, if one exists.
    pub frame: Option<FrameInfo>,
    /// Number of frames published so far.
    pub published_frames: u64,
    /// Where the run had reached when observe was called.
    pub stage_reached: &'static str,
    /// Output path + byte count when a PNG was written.
    pub screenshot: Option<(PathBuf, u64)>,
}

/// The `action` result for tap/key/swipe.
#[derive(Debug, Clone)]
pub struct InputReport {
    /// Action kind (`tap`, `key`, `swipe`).
    pub kind: String,
    /// Whether the dispatcher accepted and routed the event.
    pub accepted: bool,
    /// Acknowledgement sequence numbers.
    pub ack_seqs: Vec<u32>,
    /// For `text`, which path edited the field (`input-connection` or
    /// `key-events`); `None` for every other action.
    pub path: Option<String>,
}

/// One guest log line.
#[derive(Debug, Clone)]
pub struct LogLine {
    /// Priority (`2`..=`7`).
    pub priority: i32,
    /// Log tag.
    pub tag: String,
    /// Message.
    pub message: String,
    /// `logcat -v threadtime` rendering.
    pub line: String,
}

/// Session status snapshot.
#[derive(Debug, Clone)]
pub struct SessionState {
    /// Whether `launch` has run.
    pub launched: bool,
    /// Package, once launched.
    pub package: Option<String>,
    /// The APK path, once launched.
    pub apk: Option<String>,
    /// The last stage reached.
    pub stage_reached: &'static str,
    /// Frames published.
    pub published_frames: u64,
}

/// A structured session failure carrying an ACOS error-code string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionError {
    /// ACOS error code (for example `UNSUPPORTED_BACKEND`, `ELEMENT_NOT_FOUND`).
    pub code: &'static str,
    /// Human-readable detail.
    pub message: String,
    /// Machine-readable details.
    pub details: Vec<(String, String)>,
}

/// Situation-specific hint for a session error, overriding the code's generic
/// [`acos_protocol::ErrorCode::hint`].
///
/// The generic hints are written for a backend-selection problem (`Choose a
/// backend...`); an agent that hits a `not_ready` tree/frame or taps before the
/// app has a window needs a different next action. Kept here (rather than in
/// each front-end) so the CLI and MCP server answer identically.
#[must_use]
pub fn session_error_hint(code: &str, details: &[(String, String)]) -> Option<&'static str> {
    let get = |key: &str| details.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());
    match code {
        "UNSUPPORTED_BACKEND" => match get("capability") {
            Some("ui.tree") => Some(
                "The app has not added a window yet, so there is no View hierarchy to walk. \
                 Run `acos launch` and read its ladder: a tree only exists after \
                 `activity_on_create` completes. Do not treat this as a pass.",
            ),
            Some("ui.frame") => Some(
                "No frame has been published yet. Run `acos launch` and read its ladder: a frame \
                 only exists after the render/handoff path completes. Do not treat this as a pass.",
            ),
            Some("input.text") => Some(
                "Typed text edits the focused editor, so a field must have input focus first. \
                 Observe the tree, tap the text field (or otherwise focus it), then type. \
                 For characters the key-event fallback cannot map, the focused field must expose \
                 an InputConnection (a TextView/EditText does).",
            ),
            _ => None,
        },
        "ELEMENT_NOT_FOUND" => match get("kind") {
            Some("NoUiTree") => Some(
                "The session has no UI tree yet, so a text label cannot be resolved. Launch the \
                 app first (`acos_launch` / `acos launch`) and confirm the launch ladder, then \
                 `acos_observe_tree` before tapping by text.",
            ),
            Some("NoWindowAtPoint") => Some(
                "No window covers that point because the app has not published a frame. Launch and \
                 observe first; then tap inside the observed node bounds.",
            ),
            Some("NoFocusedWindow") => Some(
                "No window holds input focus yet. Launch and observe first; the app must add and \
                 focus a window before key input is delivered.",
            ),
            Some("NoFocusedEditor") => Some(
                "No view currently holds editor focus. Tap the text field first (the IME edits \
                 the focused view), then retry `acos session text`.",
            ),
            _ => None,
        },
        _ => None,
    }
}

impl SessionError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        SessionError {
            code,
            message: message.into(),
            details: Vec::new(),
        }
    }

    fn with_details(mut self, details: Vec<(&str, String)>) -> Self {
        self.details = details
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        self
    }
}

/// Session options.
#[derive(Debug, Clone, Copy)]
pub struct SessionOptions {
    /// Host RSS ceiling sampled by the VM watchdog (`0` = disabled).
    pub rss_max_bytes: usize,
    /// Guest heap ceiling (`0` = the VM default).
    pub heap_max_bytes: usize,
    /// Wall-clock ceiling for `launch` (`0` = no VM-level watchdog).
    pub wall_secs: u64,
    /// Record native invocations (needed for the native tail on a blocker).
    pub trace_natives: bool,
    /// Run the APK's native code under confinement (`--native` / `ACOS_NATIVE=1`).
    ///
    /// The confinement must already be active in this process (the launch gate
    /// enters it before the session worker starts); the JNI runtime's loader
    /// fails closed with `CONFINEMENT_UNAVAILABLE` otherwise.
    pub native: bool,
}

impl Default for SessionOptions {
    fn default() -> Self {
        SessionOptions {
            rss_max_bytes: 0,
            heap_max_bytes: 0,
            wall_secs: 120,
            trace_natives: true,
            native: false,
        }
    }
}

enum Cmd {
    Launch {
        apk: PathBuf,
        reply: Sender<Result<LaunchReport, SessionError>>,
    },
    Observe {
        screenshot: Option<PathBuf>,
        reply: Sender<Result<ObserveReport, SessionError>>,
    },
    ObserveTree {
        max_nodes: usize,
        reply: Sender<Result<ViewTree, SessionError>>,
    },
    Tap {
        x: f32,
        y: f32,
        reply: Sender<Result<InputReport, SessionError>>,
    },
    TapText {
        label: String,
        reply: Sender<Result<InputReport, SessionError>>,
    },
    Key {
        code: i32,
        reply: Sender<Result<InputReport, SessionError>>,
    },
    Text {
        text: String,
        reply: Sender<Result<InputReport, SessionError>>,
    },
    Swipe {
        from: (f32, f32),
        to: (f32, f32),
        duration_nanos: i64,
        steps: usize,
        reply: Sender<Result<InputReport, SessionError>>,
    },
    Logs {
        reply: Sender<Result<Vec<LogLine>, SessionError>>,
    },
    Status {
        reply: Sender<Result<SessionState, SessionError>>,
    },
    Reset {
        reply: Sender<Result<SessionState, SessionError>>,
    },
    Close {
        reply: Sender<()>,
    },
}

struct WorkerState {
    limits: SessionOptions,
    runtime: Option<Runtime>,
    store: Arc<FrameStore>,
    launched: bool,
    package: Option<String>,
    activity: Option<String>,
    apk: Option<PathBuf>,
    stage_reached: &'static str,
    launch_events: Vec<String>,
}

impl WorkerState {
    fn new(limits: SessionOptions) -> WorkerState {
        let store = match install_frame_handoff() {
            Ok(store) => store,
            Err(e) => {
                eprintln!("acos-session: frame handoff install failed: {e}");
                Arc::new(FrameStore::new())
            }
        };
        WorkerState {
            limits,
            runtime: None,
            store,
            launched: false,
            package: None,
            activity: None,
            apk: None,
            stage_reached: "none",
            launch_events: Vec::new(),
        }
    }

    fn status(&self) -> SessionState {
        SessionState {
            launched: self.launched,
            package: self.package.clone(),
            apk: self.apk.as_ref().map(|p| p.display().to_string()),
            stage_reached: self.stage_reached,
            published_frames: self.store.published_count(),
        }
    }

    fn reset(&mut self) {
        // Drop the VM/service state; the next launch reconstructs it. The
        // process-global VFS and log sink are shared, so `data_cleared` is
        // reported as false rather than pretending per-session isolation.
        self.runtime = None;
        self.launched = false;
        self.package = None;
        self.activity = None;
        self.apk = None;
        self.stage_reached = "none";
        self.launch_events.clear();
    }
}

/// A live agent session. Dropping it shuts the worker down.
pub struct AgentSession {
    tx: Sender<Cmd>,
    join: Option<JoinHandle<()>>,
    request_id: String,
}

impl std::fmt::Debug for AgentSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentSession")
            .field("request_id", &self.request_id)
            .finish_non_exhaustive()
    }
}

impl AgentSession {
    /// Start the worker thread. The VM is created lazily on the first `launch`.
    pub fn start(limits: SessionOptions) -> AgentSession {
        let (tx, rx) = channel::<Cmd>();
        let join = thread::Builder::new()
            .name("acos-agent-session".to_string())
            .spawn(move || worker_loop(rx, limits))
            .expect("spawn acos agent session worker");
        AgentSession {
            tx,
            join: Some(join),
            request_id: "s_default".to_string(),
        }
    }

    /// The stable session id.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.request_id
    }

    fn round_trip<T>(
        &self,
        make: impl FnOnce(Sender<Result<T, SessionError>>) -> Cmd,
    ) -> Result<T, SessionError> {
        let (reply_tx, reply_rx) = channel();
        self.tx
            .send(make(reply_tx))
            .map_err(|_| SessionError::new("SESSION_NOT_FOUND", "session worker is gone"))?;
        reply_rx
            .recv()
            .map_err(|_| SessionError::new("SESSION_NOT_FOUND", "session worker is gone"))?
    }

    /// Boot the app and return the launch stage ladder.
    pub fn launch(&self, apk: impl Into<PathBuf>) -> Result<LaunchReport, SessionError> {
        self.round_trip(|reply| Cmd::Launch {
            apk: apk.into(),
            reply,
        })
    }

    /// Read the latest completed frame. Returns `UNSUPPORTED_BACKEND` with a
    /// `not_ready` reason when no frame has been published; never a blank frame.
    pub fn observe(&self, screenshot: Option<PathBuf>) -> Result<ObserveReport, SessionError> {
        self.round_trip(|reply| Cmd::Observe { screenshot, reply })
    }

    /// Walk the live framework `View` hierarchy of the session's VM.
    ///
    /// Returns `UNSUPPORTED_BACKEND` with `details.reason = "not_ready"` when
    /// the app has not added a window, so an empty tree is never a pass.
    pub fn observe_tree(&self, max_nodes: usize) -> Result<ViewTree, SessionError> {
        self.round_trip(|reply| Cmd::ObserveTree { max_nodes, reply })
    }

    /// Tap at display coordinates.
    pub fn tap(&self, x: f32, y: f32) -> Result<InputReport, SessionError> {
        self.round_trip(|reply| Cmd::Tap { x, y, reply })
    }

    /// Tap a node found by text. Requires a UI tree, which this build does not
    /// extract yet, so it returns a structured error until tree extraction lands.
    pub fn tap_text(&self, label: impl Into<String>) -> Result<InputReport, SessionError> {
        self.round_trip(|reply| Cmd::TapText {
            label: label.into(),
            reply,
        })
    }

    /// Press a key code (for example `4` = BACK, `66` = ENTER).
    pub fn key(&self, code: i32) -> Result<InputReport, SessionError> {
        self.round_trip(|reply| Cmd::Key { code, reply })
    }

    /// Type `text` into the focused editor.
    ///
    /// Uses the real framework `InputConnection.commitText` path when the
    /// focused view is editable, and the dispatcher's key-event fallback
    /// otherwise.
    pub fn text(&self, text: impl Into<String>) -> Result<InputReport, SessionError> {
        self.round_trip(|reply| Cmd::Text {
            text: text.into(),
            reply,
        })
    }

    /// Swipe between two points.
    pub fn swipe(
        &self,
        from: (f32, f32),
        to: (f32, f32),
        duration: Duration,
        steps: usize,
    ) -> Result<InputReport, SessionError> {
        self.round_trip(|reply| Cmd::Swipe {
            from,
            to,
            duration_nanos: duration.as_nanos().min(i64::MAX as u128) as i64,
            steps,
            reply,
        })
    }

    /// The current guest log buffer (does not drain it).
    pub fn logs(&self) -> Result<Vec<LogLine>, SessionError> {
        self.round_trip(|reply| Cmd::Logs { reply })
    }

    /// Session status.
    pub fn status(&self) -> Result<SessionState, SessionError> {
        self.round_trip(|reply| Cmd::Status { reply })
    }

    /// Recreate the session's task state (the next `launch` boots a fresh VM).
    pub fn reset(&self) -> Result<SessionState, SessionError> {
        self.round_trip(|reply| Cmd::Reset { reply })
    }

    /// Shut the worker down and join it.
    pub fn close(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        if let Some(join) = self.join.take() {
            let (reply_tx, reply_rx) = channel();
            let _ = self.tx.send(Cmd::Close { reply: reply_tx });
            let _ = reply_rx.recv_timeout(Duration::from_secs(30));
            let _ = join.join();
        }
    }
}

impl Drop for AgentSession {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn worker_loop(rx: Receiver<Cmd>, limits: SessionOptions) {
    let mut state = WorkerState::new(limits);
    while let Ok(cmd) = rx.recv() {
        match cmd {
            Cmd::Launch { apk, reply } => {
                let _ = reply.send(do_launch(&mut state, &apk));
            }
            Cmd::Observe { screenshot, reply } => {
                let _ = reply.send(do_observe(&state, screenshot.as_deref()));
            }
            Cmd::ObserveTree { max_nodes, reply } => {
                let _ = reply.send(do_observe_tree(&mut state, max_nodes));
            }
            Cmd::Tap { x, y, reply } => {
                let _ = reply.send(finish_input(
                    "tap",
                    input_java::inject_tap(x, y).map_err(input_error),
                ));
            }
            Cmd::TapText { label, reply } => {
                let _ = reply.send(Err(SessionError::new(
                    "ELEMENT_NOT_FOUND",
                    format!("cannot resolve label {label:?}: no UI tree has been extracted"),
                )
                .with_details(vec![("kind", "NoUiTree".to_string()), ("label", label)])));
            }
            Cmd::Key { code, reply } => {
                let _ = reply.send(finish_input(
                    "key",
                    input_java::inject_key(code).map_err(input_error),
                ));
            }
            Cmd::Text { text, reply } => {
                let _ = reply.send(do_text(&mut state, &text));
            }
            Cmd::Swipe {
                from,
                to,
                duration_nanos,
                steps,
                reply,
            } => {
                let _ = reply.send(finish_input(
                    "swipe",
                    input_java::inject_swipe(from, to, duration_nanos, steps).map_err(input_error),
                ));
            }
            Cmd::Logs { reply } => {
                let mut lines = current_logs();
                // `boot_and_run_lifecycle` drains the fixture-tagged records
                // into `report.events`; put them back into the log view so
                // `acos logs` shows the lifecycle the run actually produced.
                for msg in &state.launch_events {
                    lines.push(LogLine {
                        priority: 4,
                        tag: "AcosFixture".to_string(),
                        message: msg.clone(),
                        line: format!("I AcosFixture: {msg}"),
                    });
                }
                let _ = reply.send(Ok(lines));
            }
            Cmd::Status { reply } => {
                let _ = reply.send(Ok(state.status()));
            }
            Cmd::Reset { reply } => {
                state.reset();
                let _ = reply.send(Ok(state.status()));
            }
            Cmd::Close { reply } => {
                let _ = reply.send(());
                break;
            }
        }
    }
}

fn input_error(e: acos_services::input::InputError) -> SessionError {
    use acos_services::input::InputError;
    match &e {
        InputError::NoWindowAtPoint { x, y } => SessionError::new(
            "ELEMENT_NOT_FOUND",
            format!(
                "no touchable window contains ({x},{y}); the app has not published a frame yet"
            ),
        )
        .with_details(vec![
            ("kind", "NoWindowAtPoint".to_string()),
            ("x", x.to_string()),
            ("y", y.to_string()),
        ]),
        InputError::NoFocusedWindow => {
            SessionError::new("ELEMENT_NOT_FOUND", "no window currently holds input focus")
                .with_details(vec![("kind", "NoFocusedWindow".to_string())])
        }
        InputError::NoChannel(window) => SessionError::new(
            "ELEMENT_NOT_FOUND",
            format!("window {window} has no input channel"),
        )
        .with_details(vec![("kind", "NoChannel".to_string())]),
        other => SessionError::new("INTERNAL", other.to_string())
            .with_details(vec![("kind", "InputDispatch".to_string())]),
    }
}

fn finish_input(
    kind: &str,
    result: Result<Vec<u32>, SessionError>,
) -> Result<InputReport, SessionError> {
    match result {
        Ok(ack_seqs) => Ok(InputReport {
            kind: kind.to_string(),
            accepted: true,
            ack_seqs,
            path: None,
        }),
        Err(e) => Err(e),
    }
}

/// Type `text` into the focused editor through the real `InputConnection` path.
fn do_text(state: &mut WorkerState, text: &str) -> Result<InputReport, SessionError> {
    let stage = state.stage_reached;
    let Some(runtime) = state.runtime.as_mut() else {
        return Err(SessionError::new(
            "UNSUPPORTED_BACKEND",
            format!(
                "cannot type text: the runtime is not booted (stage reached: {stage})"
            ),
        )
        .with_details(vec![
            ("reason", "not_ready".to_string()),
            ("capability", "input.text".to_string()),
            ("stage_reached", stage.to_string()),
        ]));
    };
    match crate::text_java::inject_text(runtime.vm_mut(), text) {
        Ok(report) => Ok(InputReport {
            kind: "text".to_string(),
            accepted: true,
            ack_seqs: report.ack_seqs,
            path: Some(report.path.as_str().to_string()),
        }),
        Err(e) => Err(text_error(&e, stage)),
    }
}

fn text_error(e: &crate::text_java::TextError, stage: &str) -> SessionError {
    let mut details = vec![
        ("kind", e.kind().to_string()),
        ("stage_reached", stage.to_string()),
    ];
    match e {
        crate::text_java::TextError::NoFocusedEditor => {
            details.push(("capability", "input.text".to_string()));
        }
        crate::text_java::TextError::NoFocusedWindow => {}
        crate::text_java::TextError::UnsupportedCharacter(ch) => {
            details.push(("character", ch.to_string()));
            details.push(("capability", "input.text".to_string()));
        }
        crate::text_java::TextError::NotReady(_) => {
            details.push(("reason", "not_ready".to_string()));
            details.push(("capability", "input.text".to_string()));
        }
        crate::text_java::TextError::Framework(_) => {}
    }
    SessionError::new(e.code(), e.message()).with_details(details)
}

fn do_observe(
    state: &WorkerState,
    screenshot: Option<&Path>,
) -> Result<ObserveReport, SessionError> {    let published = state.store.published_count();
    let Some(frame) = state.store.latest() else {
        return Err(SessionError::new(
            "UNSUPPORTED_BACKEND",
            format!(
                "no completed frame has been published yet (stage reached: {}); \
                 refusing to return a blank image or an empty tree",
                state.stage_reached
            ),
        )
        .with_details(vec![
            ("reason", "not_ready".to_string()),
            ("capability", "ui.frame".to_string()),
            ("stage_reached", state.stage_reached.to_string()),
            ("published_frames", published.to_string()),
        ]));
    };

    let info = FrameInfo {
        frame_id: frame.frame_id,
        width: frame.width,
        height: frame.height,
        content_hash: format!("{:032x}", frame.content_hash),
        cause_id: frame.cause_id,
        pixels: frame.pixels.clone(),
    };

    let mut written = None;
    if let Some(path) = screenshot {
        let bytes = encode_png(&info.pixels, info.width, info.height).map_err(|e| {
            SessionError::new("RENDER_IO", format!("screenshot encode failed: {e}"))
        })?;
        std::fs::write(path, &bytes).map_err(|e| {
            SessionError::new(
                "RENDER_IO",
                format!("screenshot write {path:?} failed: {e}"),
            )
        })?;
        written = Some((path.to_path_buf(), bytes.len() as u64));
    }

    Ok(ObserveReport {
        frame: Some(info),
        published_frames: published,
        stage_reached: state.stage_reached,
        screenshot: written,
    })
}

/// Minimal PNG writer for premultiplied RGBA8888 pixels.
///
/// Implemented here (instead of dragging `acos-render`'s `tiny_skia::Pixmap`
/// across the public API) so `observe --screenshot` is honest: the bytes are the
/// pixels of the frame that was actually published.
/// Walk the live `View` hierarchy of the session VM, or report why not.
fn do_observe_tree(
    state: &mut WorkerState,
    max_nodes: usize,
) -> Result<ViewTree, SessionError> {
    let stage = state.stage_reached;
    let Some(runtime) = state.runtime.as_mut() else {
        return Err(SessionError::new(
            "UNSUPPORTED_BACKEND",
            format!(
                "no View hierarchy to walk: the runtime is not booted (stage reached: {stage})"
            ),
        )
        .with_details(vec![
            ("reason", "not_ready".to_string()),
            ("capability", "ui.tree".to_string()),
            ("stage_reached", stage.to_string()),
        ]));
    };
    match crate::view_tree::walk(runtime.vm_mut(), max_nodes) {
        Ok(tree) => Ok(tree),
        Err(e) => Err(SessionError::new(e.code(), e.message().to_string()).with_details(vec![
            ("reason", e.reason().to_string()),
            ("capability", "ui.tree".to_string()),
            ("stage_reached", stage.to_string()),
        ])),
    }
}

fn encode_png(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    if width == 0 || height == 0 {
        return Err("zero-sized frame".to_string());
    }
    let expected = width as usize * height as usize * 4;
    if rgba.len() < expected {
        return Err(format!(
            "frame has {} bytes, expected {expected} for {width}x{height}",
            rgba.len()
        ));
    }
    // Filter type 0 (None) per scanline; zlib stream with stored deflate blocks.
    let mut raw = Vec::with_capacity(expected + height as usize);
    for y in 0..height as usize {
        raw.push(0u8);
        let start = y * width as usize * 4;
        raw.extend_from_slice(&rgba[start..start + width as usize * 4]);
    }
    let idat = zlib_stored(&raw);
    let mut out = Vec::with_capacity(idat.len() + 128);
    out.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA, no interlace
    push_chunk(&mut out, b"IHDR", &ihdr);
    push_chunk(&mut out, b"IDAT", &idat);
    push_chunk(&mut out, b"IEND", &[]);
    Ok(out)
}

fn push_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc = Crc32::new();
    crc.update(kind);
    crc.update(data);
    out.extend_from_slice(&crc.finish().to_be_bytes());
}

/// zlib wrapper around stored (uncompressed) deflate blocks.
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + data.len() / 65535 * 5 + 16);
    out.extend_from_slice(&[0x78, 0x01]);
    let mut offset = 0usize;
    while offset < data.len() {
        let len = (data.len() - offset).min(65535);
        let final_block = offset + len == data.len();
        out.push(if final_block { 1 } else { 0 });
        out.extend_from_slice(&(len as u16).to_le_bytes());
        out.extend_from_slice(&(!(len as u16)).to_le_bytes());
        out.extend_from_slice(&data[offset..offset + len]);
        offset += len;
    }
    if data.is_empty() {
        out.push(1);
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&(!0u16).to_le_bytes());
    }
    let mut adler = Adler32::new();
    adler.update(data);
    out.extend_from_slice(&adler.finish().to_be_bytes());
    out
}

struct Crc32 {
    value: u32,
}

impl Crc32 {
    fn new() -> Self {
        Crc32 { value: 0xffff_ffff }
    }
    fn update(&mut self, data: &[u8]) {
        for &b in data {
            self.value ^= u32::from(b);
            for _ in 0..8 {
                let mask = (self.value & 1).wrapping_neg();
                self.value = (self.value >> 1) ^ (0xedb8_8320 & mask);
            }
        }
    }
    fn finish(&self) -> u32 {
        self.value ^ 0xffff_ffff
    }
}

struct Adler32 {
    a: u32,
    b: u32,
}

impl Adler32 {
    fn new() -> Self {
        Adler32 { a: 1, b: 0 }
    }
    fn update(&mut self, data: &[u8]) {
        const MOD: u32 = 65521;
        for &byte in data {
            self.a = (self.a + u32::from(byte)) % MOD;
            self.b = (self.b + self.a) % MOD;
        }
    }
    fn finish(&self) -> u32 {
        (self.b << 16) | self.a
    }
}

fn current_logs() -> Vec<LogLine> {
    os_log::records().into_iter().map(log_line).collect()
}

fn log_line(record: LogRecord) -> LogLine {
    LogLine {
        priority: record.priority,
        tag: record.tag.clone(),
        message: record.msg.clone(),
        line: os_log::threadtime_line(&record),
    }
}

fn do_launch(state: &mut WorkerState, apk: &Path) -> Result<LaunchReport, SessionError> {
    let started = Instant::now();

    // --- apk_parsed ---------------------------------------------------------
    let apk_file = match Apk::open(apk) {
        Ok(apk) => apk,
        Err(e) => {
            return Err(
                SessionError::new(e.code(), format!("{}: {}", e.code(), e.message())).with_details(
                    vec![
                        ("stage", "apk_parsed".to_string()),
                        ("apk", apk.display().to_string()),
                    ],
                ),
            );
        }
    };
    let (package, activity) = match apk_file.manifest() {
        Ok(manifest) => (
            Some(manifest.package.clone()),
            manifest.launcher_activity().map(|c| c.name.clone()),
        ),
        Err(_) => (None, None),
    };

    // A fresh launch replaces any prior VM state (the transport/surface seams
    // are process-global). This is the documented `shared-task` behaviour.
    state.runtime = None;
    state.launched = false;
    state.launch_events.clear();
    let _ = os_log::drain();

    let options = VmOptions {
        corelib: CoreLib::Real,
        heap_max_bytes: if state.limits.heap_max_bytes == 0 {
            256 * 1024 * 1024
        } else {
            state.limits.heap_max_bytes
        },
        trace_natives: state.limits.trace_natives,
        ..VmOptions::default()
    };
    let (mut runtime, boot_thread) = match build_launch_runtime(options) {
        Ok(pair) => pair,
        Err(e) => {
            state.stage_reached = "apk_parsed";
            return Err(
                SessionError::new(e.code(), format!("{}: {e}", e.code())).with_details(vec![
                    ("stage", "process_booted".to_string()),
                    ("apk", apk.display().to_string()),
                ]),
            );
        }
    };
    runtime.vm_mut().host = HostGuard::new(state.limits.rss_max_bytes, state.limits.wall_secs);

    // Route the APK's DEX `native` methods to the JNI dispatcher. This is
    // unconditional: without a JNI host (the default) a native call reports
    // `UNSUPPORTED_JAVA_API` with the `--native` hint.
    if let Err(e) = runtime.register_app_natives(apk) {
        state.stage_reached = "apk_parsed";
        return Err(e);
    }
    // Opt-in native code (ADR-0018 update). Fail closed: without an active
    // process confinement the first `System.loadLibrary` reports
    // `CONFINEMENT_UNAVAILABLE` and the session still returns a report.
    if state.limits.native {
        if let Err(e) = runtime.enable_native(apk) {
            state.stage_reached = "apk_parsed";
            return Err(e);
        }
    }

    // A cache-restored framework is already past `prepareMainLooper` +
    // `ActivityThread` + `attach`, so it skips `boot_app_process` and runs the
    // same real lifecycle from the restored heap (T-43/W102-A).
    let report: LifecycleReport = match boot_thread {
        Some(thread) => runtime.boot_and_run_lifecycle_from_boot(apk, thread),
        None => runtime.boot_and_run_lifecycle(apk),
    };
    let log_records = os_log::records();
    let mut lifecycle_msgs: Vec<String> = report.events.clone();
    lifecycle_msgs.extend(log_records.iter().map(|r| r.msg.clone()));

    let boot_ok = report.bind_delivered;
    let bind_ok = report.bind_delivered;
    let app_ok = report.app_created;
    let tx_scheduled = report.transaction_scheduled;
    let activity_ok = report.transaction_delivered;
    let resume_ok = lifecycle_msgs.iter().any(|m| m.contains("onResume"));
    let frame = state.store.latest();
    let frame_ok = frame.is_some();

    let flags = [
        true, // apk_parsed
        boot_ok,
        bind_ok,
        app_ok,
        tx_scheduled,
        activity_ok,
        resume_ok,
        frame_ok,
    ];

    let first_fail = flags.iter().position(|ok| !ok);
    let root_blocker = first_fail.map(|idx| blocker_for(idx, &report, &lifecycle_msgs));

    let mut stages = Vec::with_capacity(STAGES.len());
    for (idx, name) in STAGES.iter().enumerate() {
        let status = if flags[idx] {
            StageStatus::Ok
        } else if Some(idx) == first_fail {
            StageStatus::Blocked
        } else {
            StageStatus::NotReached
        };
        // Every non-ok rung carries the same root blocker: it is the reason
        // this rung did not run. The first non-ok rung is `blocked`, later ones
        // are `not_reached`.
        let blocker = if status == StageStatus::Ok {
            None
        } else {
            root_blocker.clone()
        };
        stages.push(StageEntry {
            name,
            status,
            blocker,
        });
    }

    let reached_stage = first_fail
        .and_then(|idx| idx.checked_sub(1))
        .map(|idx| STAGES[idx])
        .unwrap_or(STAGES[0]);

    state.runtime = Some(runtime);
    state.launched = true;
    state.package = package.clone();
    state.activity = activity.clone();
    state.apk = Some(apk.to_path_buf());
    state.stage_reached = reached_stage;
    state.launch_events = lifecycle_msgs.clone();

    Ok(LaunchReport {
        stages,
        root_blocker,
        reached_stage,
        package,
        activity,
        classes_loaded: report.classes_loaded,
        events: lifecycle_msgs,
        wall_ms: started.elapsed().as_millis().min(u64::MAX as u128) as u64,
        peak_rss_bytes: peak_rss_bytes(),
    })
}

/// Build the `Runtime` a launch runs on, preferring the on-disk boot-image
/// cache (T-43/W102-A).
///
/// With the cache enabled a warm entry restores the post-attach framework
/// snapshot in milliseconds and returns the preserved `ActivityThread` handle;
/// a miss boots the framework once (populating the cache) and then restores it,
/// exactly as [`crate::session_pool::RuntimeTemplate`] does. With the cache
/// disabled (`ACOS_NO_BOOT_CACHE=1` / `ACOS_BOOT_CACHE=0`) or on any template
/// failure this is the plain `Runtime::standard_with` boot and the caller runs
/// the normal `boot_app_process` lifecycle.
fn build_launch_runtime(
    options: VmOptions,
) -> Result<(Runtime, Option<u32>), acos_vm::VmError> {
    if let Some(cache) = crate::boot_image_cache::from_env() {
        if let Ok(template) =
            crate::session_pool::RuntimeTemplate::boot_with_options(options.clone(), Some(&cache))
        {
            let thread = template.activity_thread();
            if let Ok(rt) = template.restore_runtime() {
                return Ok((rt, Some(thread)));
            }
        }
    }
    Ok((Runtime::standard_with(options)?, None))
}

fn blocker_for(idx: usize, report: &LifecycleReport, msgs: &[String]) -> StageBlocker {
    let stage = STAGES[idx];
    if stage == "first_frame" {
        return StageBlocker {
            code: "UNSUPPORTED_BACKEND".to_string(),
            message: format!(
                "the app process reached `{stage}` but no frame has been published; \
                 the render/handoff path has not produced a completed frame"
            ),
            details: vec![
                ("reason".to_string(), "not_ready".to_string()),
                ("stage".to_string(), "first_frame".to_string()),
                ("capability".to_string(), "ui.frame".to_string()),
            ],
        };
    }
    let raw = report.error.clone().unwrap_or_else(|| {
        format!("stage `{stage}` did not complete and the runtime reported no error")
    });
    let code = classify_error(&raw, stage);
    let mut details = vec![
        ("stage".to_string(), stage.to_string()),
        ("classes".to_string(), report.classes_loaded.to_string()),
    ];
    if stage == "application_on_create" {
        details.push((
            "hint".to_string(),
            "handleBindApplication threw before Application.onCreate".to_string(),
        ));
    }
    if stage == "activity_on_create" {
        details.push((
            "hint".to_string(),
            "performLaunchActivity threw before Activity.onCreate".to_string(),
        ));
    }
    if stage == "on_resume" {
        details.push(("last_lifecycle_events".to_string(), msgs.join("; ")));
    }
    // The protocol hints for `UNSUPPORTED_JAVA_API`/`UNSUPPORTED_ANDROID_API`
    // tell the agent to read `details.api`/`details.at`; make that true by
    // pulling the first `Lclass;->name(sig)` and the `(at ... pc=...)` frame out
    // of the runtime message.
    if code.starts_with("UNSUPPORTED") {
        if let Some(api) = extract_api(&raw) {
            details.push(("api".to_string(), api));
        }
        if let Some(at) = extract_at(&raw) {
            details.push(("at".to_string(), at));
        }
    }
    StageBlocker {
        code: code.to_string(),
        message: raw,
        details,
    }
}

/// First `Lclass;->name(sig)` token in a runtime error message.
fn extract_api(raw: &str) -> Option<String> {
    let head = raw.lines().next().unwrap_or(raw);
    let start = head.find('L')?;
    let tail = &head[start..];
    let end = tail
        .find(" is not implemented")
        .unwrap_or_else(|| tail.find(' ').unwrap_or(tail.len()));
    let api = tail[..end].trim();
    if api.contains("->") {
        Some(api.to_string())
    } else {
        None
    }
}

/// The `Lclass;.name` inside a `(at ... pc=...)` runtime frame, if present.
fn extract_at(raw: &str) -> Option<String> {
    let head = raw.lines().next().unwrap_or(raw);
    let start = head.find("(at ")? + 4;
    let tail = &head[start..];
    let end = tail.find(" pc=").unwrap_or(tail.len());
    let at = tail[..end].trim();
    if at.is_empty() {
        None
    } else {
        Some(at.to_string())
    }
}

/// Map a [`LifecycleReport`] error string onto a stable ACOS error code.
fn classify_error(raw: &str, stage: &str) -> &'static str {
    let msg = raw.to_ascii_lowercase();
    if msg.contains("unsupported") || msg.contains("is not implemented") {
        "UNSUPPORTED_JAVA_API"
    } else if msg.contains("java exception") {
        "JAVA_EXCEPTION"
    } else if msg.contains("timeout") || msg.contains("not responding") {
        "TIMEOUT"
    } else if msg.contains("dex:") || msg.contains("dex ") {
        "DEX_LOAD_ERROR"
    } else if msg.contains("apk") {
        "APK_IO"
    } else if stage == "first_frame" {
        "UNSUPPORTED_BACKEND"
    } else {
        "VM_INTERNAL"
    }
}

fn peak_rss_bytes() -> u64 {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return 0;
    };
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            if let Some(kb) = rest.split_whitespace().next() {
                return kb.parse::<u64>().unwrap_or(0) * 1024;
            }
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_encoder_is_a_valid_png() {
        // 2x1 red + transparent green, premultiplied RGBA.
        let rgba = [255, 0, 0, 255, 0, 255, 0, 0];
        let png = encode_png(&rgba, 2, 1).expect("encode");
        assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
        assert!(png.windows(4).any(|w| w == b"IHDR"));
        assert!(png.windows(4).any(|w| w == b"IDAT"));
        assert!(png.windows(4).any(|w| w == b"IEND"));
    }

    #[test]
    fn unsupported_message_yields_api_and_at_details() {
        let raw = "vm: Landroid/graphics/drawable/VectorDrawable;->nCreateGroup()J is not implemented \
                   (at Landroid/graphics/drawable/VectorDrawable;.nCreateGroup pc=0)\n  java-tail: \n    Lx;->y()V";
        assert_eq!(
            extract_api(raw).as_deref(),
            Some("Landroid/graphics/drawable/VectorDrawable;->nCreateGroup()J")
        );
        assert_eq!(
            extract_at(raw).as_deref(),
            Some("Landroid/graphics/drawable/VectorDrawable;.nCreateGroup")
        );
        assert_eq!(extract_api("plain failure with no api token"), None);
    }

    #[test]
    fn not_ready_hints_override_the_generic_backend_hint() {
        let details = vec![
            ("reason".to_string(), "not_ready".to_string()),
            ("capability".to_string(), "ui.tree".to_string()),
        ];
        let hint = session_error_hint("UNSUPPORTED_BACKEND", &details).expect("hint");
        assert!(hint.contains("acos launch"), "{hint}");
        assert!(hint.contains("not treat this as a pass"), "{hint}");

        let details = vec![("kind".to_string(), "NoUiTree".to_string())];
        let hint = session_error_hint("ELEMENT_NOT_FOUND", &details).expect("hint");
        assert!(hint.contains("acos_observe_tree"), "{hint}");

        assert_eq!(session_error_hint("TIMEOUT", &[]), None);
    }

    #[test]
    fn observe_without_a_frame_is_not_ready() {
        let state = WorkerState {
            limits: SessionOptions::default(),
            runtime: None,
            store: Arc::new(FrameStore::new()),
            launched: false,
            package: None,
            activity: None,
            apk: None,
            stage_reached: "none",
            launch_events: Vec::new(),
        };
        let err = do_observe(&state, None).expect_err("no frame");
        assert_eq!(err.code, "UNSUPPORTED_BACKEND");
        assert!(err
            .details
            .iter()
            .any(|(k, v)| k == "reason" && v == "not_ready"));
    }
}
