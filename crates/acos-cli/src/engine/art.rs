//! The real-ART engine: one `acos-art launch --interactive` child (the real
//! framework + real BLAST/C1 guest, W10), driven by a line-JSON protocol.
//!
//! Protocol (served by `crates/acos-art/src/launch.rs` in interactive mode):
//!
//! ```text
//! caller  ->  {"cmd":"tree"}                  reply: tree + frame_seq + png + md5
//! caller  ->  {"cmd":"screenshot"}            reply: png + md5 + frame_seq
//! caller  ->  {"cmd":"tap","x":..,"y":..}     reply: tree + frame_seq + png + md5
//! caller  ->  {"cmd":"key","code":..}         reply: tree + frame_seq + png + md5
//! caller  ->  {"cmd":"type_text","text":".."} reply: tree + frame_seq + png + md5
//! caller  ->  {"cmd":"logs"}                reply: the guest logd snapshot
//! caller  ->  {"cmd":"close"}                 reply: ok, then exit
//!
//! `frame_seq` is the session's capture sequence (one per capture), and the
//! reply also carries C1's `buffer_generation` and the producer's
//! `frame_number`, plus `capture_source: "acos-c1-buffer"`. A `tap` reply is
//! only sent after C1 stored a **new** buffer and the guest probe produced a
//! **new** tree sample; otherwise the reply is a structured `NO_FRESH_FRAME`.
//! ```
//!
//! The first stdout line is the session's ready reply. Every line is bounded;
//! reads have the session timeout; the child is spawned in its own process group
//! and reaped on drop, so a wedged ART cannot leave orphans behind.
//!
//! `key`/`type_text` publish real `InputMessage` key events on the live
//! `InputChannel` (W13): the guest's focused view receives them as `KeyEvent`s,
//! exactly like a physical keyboard. `type_text` is ASCII-only (the US/QWERTY
//! virtual-keyboard map) and a character with no key is a structured error.
//!
//! The protocol has no `rotate`. `reset` is implemented **host-side**: the
//! session stops the guest and starts the same APK again in a fresh process
//! with a fresh run directory (empty app-data), which is the honest meaning of
//! "clear app state, back to the initial UI" on this channel. The guest's own
//! reset command is not used and the app's Reset button is never tapped.
//! [`SessionCapabilities`] names anything still missing, and a scenario step
//! that needs it fails with `UNSUPPORTED_BACKEND` naming it. Nothing here renders
//! offline: this engine is [`Mode::Live`]; if the binary, the sysroot or a command
//! is unavailable the caller gets a structured error, never a preview pass.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use acos_protocol::ui::UiTree;
use acos_protocol::{ErrorCode, FrameMeta, LogLevel, Selector};
use acos_verify::backend::{CrashRecord, InputAck, InputDelivery, LogRecord};
use acos_verify::{BackendError, LaunchOutcome};

use super::{
    key_code, live_frame_unavailable, parse_bounds, stale_frame, ui_tree_from_json, Engine,
    FrameEvidence, Mode, ObservedFrame, SessionBackend, SessionCapabilities, TeardownReport,
    LIVE_FRAME_UNAVAILABLE,
};

/// Largest single protocol line accepted (a full View tree fits easily).
const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;
/// Largest PNG accepted from one reply.
const MAX_PNG_BYTES: u64 = 64 * 1024 * 1024;

/// Guest-side input settle window (`ACOSLAUNCH_FRAME_WAIT_SECS`), in seconds.
///
/// The guest's own settle test needs a new C1 buffer generation **and** a new
/// probe sample **and** changed pixels. A focus-only tap changes no pixels, so
/// the guest can only answer `NO_FRESH_FRAME` — and it answers after this whole
/// window. Measured (W34, baseline): every such tap cost 16 s (15 s guest +
/// 1 s recovery capture) while the same tap costs nothing the app can observe.
/// The engine therefore shortens the guest window and finishes the wait itself
/// in [`ArtSession::settle_after_input`], on bound frames it can prove. 3 s
/// keeps every tap that the guest *can* settle (measured max 2.01 s) on the
/// guest's stronger path, so no currently-fast tap regresses.
const DEFAULT_FRAME_WAIT_SECS: u64 = 3;
/// How long [`ArtSession::settle_after_input`] may keep polling after the guest
/// refused the frame, measured from the refusal. This is the engine's coverage
/// for a slow UI update; matches the guest window it replaces (15 s).
const DEFAULT_SETTLE_BUDGET_MS: u64 = 15_000;
/// Consecutive identical captures that prove "delivered, no visual change".
/// Each poll is a full traversal whose own settle window is 1.5 s, so two polls
/// already cover ~3 s of no change.
const DEFAULT_SETTLE_NOCHANGE_POLLS: u32 = 2;
/// Pause between settle polls (a poll already costs ~1 s of guest work).
const SETTLE_POLL_GAP_MS: u64 = 250;

/// One ART session.
pub struct ArtSession {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    lines: Receiver<Result<String, String>>,
    stderr_tail: Arc<Mutex<VecDeque<u8>>>,
    timeout: Duration,
    /// The APK this session was started for (a session serves one app).
    apk: PathBuf,
    /// Where captured frames are mirrored (a `reset` relaunch reuses it).
    artifacts: PathBuf,
    package: String,
    activity: Option<String>,
    width: u32,
    height: u32,
    density: u32,
    /// The newest *bound* frame seen (tree and PNG of one traversal).
    cached: Option<CachedFrame>,
    /// The highest frame sequence any reply has reported.
    seen_seq: u64,
    /// Added to every reply's `frame_seq`. A `reset` starts a new guest whose
    /// sequence restarts at 1; the engine's own sequence must stay monotonic so
    /// a frame captured after the relaunch is still provably newer than the one
    /// before it (the stale-frame rule).
    seq_offset: u64,
    /// Added to every log entry's absolute ordinal. A relaunched guest's logd
    /// sink starts counting at 0 again; without this, records written after a
    /// reset would collide with records already consumed before it.
    log_offset: u64,
    /// The raw entry count the current guest's logd sink has reported.
    logs_total_raw: u64,
    /// The first fatal log entry the guest's logd sink carried (a real crash
    /// signature), if any. Guest-asserted: an untrusted run never sees it.
    crash: Option<CrashRecord>,
    /// The process group the session child leads (its pid).
    process_group: Option<i32>,
    /// Host-observed delivery of the most recent input action.
    last_input: Option<InputDelivery>,
    /// `(dx, dy)` from display/screen coordinates to the input channel's
    /// window-local convention, measured from the probe's `bounds_on_screen`
    /// minus `bounds` (normally the status-bar inset). The engine resolves tap
    /// points from screen bounds; `inject_tap` takes window-local points.
    input_offset: (i32, i32),
    /// Guest boot time: `acos-art` spawn to its ready line. Charged to the
    /// session, not to the `launch` step (the executor's clock starts later).
    session_start_ms: u64,
    /// Guest-side input settle window the engine asked for (seconds).
    frame_wait_secs: u64,
    /// Engine settle budget after a refused input frame.
    settle_budget: Duration,
    /// Identical captures that prove a refused input changed nothing.
    settle_polls: u32,
    /// The frame the executor will compare an input against (snapshotted in
    /// [`SessionBackend::begin_input`], before the traversal a selector tap
    /// needs to resolve its point): its content hash and its tree signature.
    input_baseline: Option<(String, u64)>,
    /// Input settle accounting, written next to the guest evidence.
    settle: SettleStats,
    closed: bool,
}

/// How the input steps of one session reached their bound frame.
///
/// Written to `<artifacts>/art-frames/session-timing.json` at every shutdown so
/// a run's cost breakdown can name the settle path instead of inferring it.
#[derive(Debug, Default, Clone)]
struct SettleStats {
    /// Replies the guest itself settled (the strong path).
    guest: u64,
    /// Refused replies the engine settled from its own bound captures.
    recovered: u64,
    /// Captures issued by the engine's recovery loop.
    recovery_polls: u64,
    /// Total wall spent in the recovery loop.
    recovery_ms: u64,
    /// Recovered inputs that did change the screen.
    recovered_changed: u64,
    /// Recovered inputs that changed nothing (focus-only taps).
    recovered_no_change: u64,
    /// Recoveries that hit the budget without a stable state.
    recovery_budget_hit: u64,
}

impl SettleStats {
    fn to_json(&self, session_start_ms: u64, shutdown_ms: u64, frame_wait_secs: u64) -> serde_json::Value {
        serde_json::json!({
            "session_start_ms": session_start_ms,
            "shutdown_ms": shutdown_ms,
            "guest_frame_wait_secs": frame_wait_secs,
            "input_settles": {
                "guest": self.guest,
                "recovered": self.recovered,
                "recovered_changed": self.recovered_changed,
                "recovered_no_change": self.recovered_no_change,
                "recovery_budget_hit": self.recovery_budget_hit,
                "recovery_polls": self.recovery_polls,
                "recovery_ms": self.recovery_ms,
            },
        })
    }
}

struct CachedFrame {
    seq: u64,
    md5: String,
    png: Vec<u8>,
    tree: UiTree,
    /// Structural signature of `tree` (see [`tree_signature`]).
    signature: u64,
    captured_at_ms: u64,
    captured_at: String,
}

impl CachedFrame {
    fn evidence(&self, previous_seq: u64) -> FrameEvidence {
        FrameEvidence {
            frame_seq: self.seq,
            previous_seq,
            captured_at_ms: self.captured_at_ms,
            captured_at: self.captured_at.clone(),
            md5: self.md5.clone(),
            png: self.png.clone(),
        }
    }

    fn observed(&self, previous_seq: u64) -> ObservedFrame {
        ObservedFrame {
            tree: self.tree.clone(),
            frame: self.evidence(previous_seq),
        }
    }
}

/// Measure the display-to-window offset from one probe tree: the difference
/// between a node's `bounds_on_screen` and its window-local `bounds`. The
/// decor root is the first node and carries both; a single offset is enough
/// because every node in one window shares the window origin.
fn probe_input_offset(tree: &serde_json::Value) -> Option<(i32, i32)> {
    fn walk(node: &serde_json::Value) -> Option<(i32, i32)> {
        let window = node
            .get("bounds")
            .and_then(|value| value.as_str())
            .and_then(parse_bounds);
        let screen = node
            .get("bounds_on_screen")
            .and_then(|value| value.as_str())
            .and_then(parse_bounds);
        if let (Some(window), Some(screen)) = (window, screen) {
            if screen.width() > 0 && screen.height() > 0 {
                return Some((screen.left() - window.left(), screen.top() - window.top()));
            }
        }
        node.get("children")
            .and_then(|children| children.as_array())
            .and_then(|children| children.iter().find_map(walk))
    }
    tree.get("nodes")
        .and_then(|nodes| nodes.as_array())
        .and_then(|nodes| nodes.iter().find_map(walk))
        .or_else(|| walk(tree))
}

/// Import acks from a reply's `gesture` record (guest-asserted).
fn acks_from_reply(reply: &serde_json::Value) -> Vec<InputAck> {
    reply
        .pointer("/gesture/acks")
        .and_then(|value| value.as_array())
        .map(|acks| {
            acks.iter()
                .map(|ack| InputAck {
                    seq: ack.get("seq").and_then(|v| v.as_u64()).unwrap_or(0),
                    handled: ack
                        .get("handled")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                    latency_us: ack.get("latency_us").and_then(|v| v.as_u64()),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Build a host-observed delivery record from a session reply.
///
/// `visual_change` is the engine's measured comparison of the post-action UI
/// tree with the pre-action one (see [`tree_signature`]). The acks are copied
/// verbatim and remain guest-asserted.
fn delivery(reply: &serde_json::Value, visual_change: bool) -> InputDelivery {
    InputDelivery {
        delivered: true,
        visual_change,
        acks: acks_from_reply(reply),
    }
}

/// Structural signature of a UI tree: its nodes and their attributes, with no
/// dependence on pixels, frame numbers or timestamps.
fn tree_signature(tree: &UiTree) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    if let Ok(json) = serde_json::to_string(&tree.nodes) {
        json.hash(&mut hasher);
    }
    hasher.finish()
}

/// Did an input change the state, measured so that the answer can never
/// disagree with the executor's `state_change` assertion?
///
/// Two independent signals are required, because each one alone is noisy in a
/// different direction:
///
/// * the **content hash** (SHA-256 of the encoded pixels) also changes when the
///   text cursor blinks between two captures of an unchanged screen;
/// * the **tree signature** also changes for attributes that paint nothing
///   (T07's submit tap flips a view attribute and leaves the pixels identical).
///
/// A change is claimed only when both moved: a blinking cursor or an unpainted
/// attribute is then "delivered, no visual change" (never a false claim), and a
/// real change satisfies the executor's own tree+screenshot requirement.
fn state_changed(baseline: Option<&(String, u64)>, current: Option<&CachedFrame>) -> bool {
    match (baseline, current) {
        (Some((md5, signature)), Some(frame)) => {
            frame.md5 != *md5 && frame.signature != *signature
        }
        // No baseline: never claim that nothing changed.
        _ => true,
    }
}

/// Host milliseconds since the Unix epoch.
fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// RFC 3339 UTC for a host millisecond timestamp.
fn rfc3339(unix_ms: u64) -> String {
    acos_verify::clock::rfc3339_from_unix_ms(unix_ms as i64)
}

impl ArtSession {
    /// Spawn `acos-art launch --apk <apk> --until activity-resumed` in its
    /// interactive mode (`ACOSLAUNCH_INTERACTIVE=1`) and wait for its ready
    /// line.
    ///
    /// This is the **real guest** path (W10): `LaunchMain` starts the real
    /// `ActivityThread`, the peer serves the real framework, C1 stores the
    /// app's BLAST buffers, and the launcher keeps the guest alive and serves
    /// the same line-JSON commands this engine already speaks (`tree`,
    /// `screenshot`, `tap`, `key`, `type_text`, `logs`, `close`). `tree`/`tap`
    /// replies carry the in-guest view-tree probe and a C1 buffer capture;
    /// `screenshot` is a C1 buffer capture labelled `acos-c1-buffer`; `logs` is
    /// the guest's logd snapshot (guest-asserted). `reset` is a host-side
    /// relaunch in a fresh guest process with an empty app-data directory.
    ///
    /// `artifacts` is where the engine mirrors captured frames. The binary is
    /// `$ACOS_ART_BIN`, else `acos-art` on `PATH`, else `target/{debug,release}/
    /// acos-art` next to the running `acos` binary.
    pub fn start(apk: &Path, artifacts: &Path) -> Result<ArtSession, BackendError> {
        if !apk.is_file() {
            return Err(BackendError::Failed {
                code: acos_protocol::ErrorCode::ApkIo,
                detail: format!("APK not found: {}", apk.display()),
            });
        }
        let binary = resolve_binary()?;
        let out = artifacts.join("art-frames");
        std::fs::create_dir_all(&out).map_err(|error| BackendError::Error {
            code: acos_protocol::ErrorCode::RenderIo,
            detail: format!("cannot create {}: {error}", out.display()),
        })?;
        let timeout = Duration::from_secs(
            std::env::var("ACOS_ART_TIMEOUT_SECS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(120),
        );
        // Cost knobs (engine-owned): see DEFAULT_FRAME_WAIT_SECS. The guest
        // variable is only overridden when the caller has not set it, so a
        // diagnostic run can still ask for the stock 15 s window.
        let frame_wait_secs = std::env::var("ACOS_ART_FRAME_WAIT_SECS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(DEFAULT_FRAME_WAIT_SECS);
        let settle_budget = Duration::from_millis(
            std::env::var("ACOS_ART_SETTLE_MS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(DEFAULT_SETTLE_BUDGET_MS),
        );
        let settle_polls = std::env::var("ACOS_ART_SETTLE_NOCHANGE_POLLS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(DEFAULT_SETTLE_NOCHANGE_POLLS)
            .max(1);

        let mut command = Command::new(&binary);
        command
            // The interactive launch session: one real guest, one line-JSON
            // command per step, same replies the engine already consumes.
            .arg("launch")
            .arg("--apk")
            .arg(apk)
            .arg("--until")
            .arg("activity-resumed")
            .arg("--out")
            .arg(&out)
            // The launcher reads this seam (no `acos-art` CLI flag change): a
            // live session instead of a one-shot launch. The guest publishes
            // the read-only view-tree probe the `tree`/`tap` replies carry.
            .env("ACOSLAUNCH_INTERACTIVE", "1")
            .env("ACOSLAUNCH_LIVE_PROBE", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if std::env::var_os("ACOSLAUNCH_FRAME_WAIT_SECS").is_none() {
            command.env("ACOSLAUNCH_FRAME_WAIT_SECS", frame_wait_secs.to_string());
        }
        // Own process group: a `close` that does not take must not leave ART or
        // its sandbox behind.
        command.process_group(0);
        let spawned_at = Instant::now();
        let mut child = command.spawn().map_err(|error| BackendError::Unsupported {
            capability: "engine.art".into(),
            detail: format!(
                "cannot spawn {} ({}): {error}. Install/build acos-art or set ACOS_ART_BIN",
                binary.display(),
                "the ART session engine is unavailable on this host"
            ),
        })?;
        let _pid = child.id();
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");

        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = Vec::new();
                match reader
                    .by_ref()
                    .take(MAX_LINE_BYTES as u64)
                    .read_until(b'\n', &mut line)
                {
                    Ok(0) => break,
                    Ok(_) => {
                        if line.len() > MAX_LINE_BYTES {
                            let _ = tx
                                .send(Err(format!("protocol line exceeds {MAX_LINE_BYTES} bytes")));
                            break;
                        }
                        let text = String::from_utf8_lossy(&line).trim().to_string();
                        if text.is_empty() {
                            continue;
                        }
                        if tx.send(Ok(text)).is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = tx.send(Err(format!("reading acos-art stdout: {error}")));
                        break;
                    }
                }
            }
        });
        let stderr_tail = Arc::new(Mutex::new(VecDeque::new()));
        let tail = Arc::clone(&stderr_tail);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stderr);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let mut tail = tail.lock().unwrap_or_else(|e| e.into_inner());
                        tail.extend(line.bytes());
                        while tail.len() > 8192 {
                            tail.pop_front();
                        }
                    }
                }
            }
        });

        let mut session = ArtSession {
            child: Some(child),
            stdin,
            lines,
            stderr_tail,
            timeout,
            apk: apk.to_path_buf(),
            artifacts: artifacts.to_path_buf(),
            package: String::new(),
            activity: None,
            width: 1080,
            height: 2400,
            density: 420,
            cached: None,
            seen_seq: 0,
            seq_offset: 0,
            log_offset: 0,
            logs_total_raw: 0,
            crash: None,
            process_group: Some(_pid as i32),
            last_input: None,
            input_offset: (0, 0),
            session_start_ms: 0,
            frame_wait_secs,
            settle_budget,
            settle_polls,
            input_baseline: None,
            settle: SettleStats::default(),
            closed: false,
        };
        // The ready line is the protocol's first reply. If the session dies
        // before it, there is no live frame at all: say so by name and keep the
        // child's own stderr tail (the crash cause) in the error.
        let ready = session
            .read_line(Duration::from_secs(90), "startup")
            .map_err(|error| match error {
                BackendError::Error { code, detail } => BackendError::Error {
                    code,
                    detail: format!(
                        "{LIVE_FRAME_UNAVAILABLE}: the ART session ended before its ready line \
                         (no live frame exists): {detail}"
                    ),
                },
                BackendError::Timeout { detail } => BackendError::Timeout {
                    detail: format!(
                        "{LIVE_FRAME_UNAVAILABLE}: the ART session produced no ready line: {detail}"
                    ),
                },
                other => other,
            })?;
        let ready: serde_json::Value =
            serde_json::from_str(&ready).map_err(|error| BackendError::Error {
                code: acos_protocol::ErrorCode::Internal,
                detail: format!("acos-art ready line is not JSON: {error}"),
            })?;
        if ready.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            let code = ready
                .get("code")
                .and_then(|v| v.as_str())
                .unwrap_or("STARTUP_FAILED");
            let message = ready
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("acos-art session did not start");
            return Err(BackendError::Unsupported {
                capability: "engine.art.session".into(),
                detail: format!("{code}: {message}; stderr tail: {}", session.stderr_tail()),
            });
        }
        if let Some(width) = ready.get("width").and_then(|v| v.as_u64()) {
            session.width = width as u32;
        }
        if let Some(height) = ready.get("height").and_then(|v| v.as_u64()) {
            session.height = height as u32;
        }
        if let Some(package) = ready.get("package").and_then(|v| v.as_str()) {
            session.package = package.to_string();
        }
        if let Some(activity) = ready.get("activity").and_then(|v| v.as_str()) {
            session.activity = Some(activity.to_string());
        }
        // `acos verify` starts the guest before its own step clock, so this is
        // the boot cost the verdict never showed.
        session.session_start_ms = spawned_at.elapsed().as_millis() as u64;
        Ok(session)
    }

    /// Guest boot time in milliseconds: `acos-art` spawn to its ready line.
    /// `verify` reports it separately because it happens before the executor's
    /// step clock starts, so no step could be charged with it.
    pub const fn session_start_ms(&self) -> u64 {
        self.session_start_ms
    }

    fn stderr_tail(&self) -> String {
        let tail = self.stderr_tail.lock().unwrap_or_else(|e| e.into_inner());
        let bytes: Vec<u8> = tail.iter().copied().collect();
        String::from_utf8_lossy(&bytes).trim().to_string()
    }

    fn read_line(&mut self, timeout: Duration, context: &str) -> Result<String, BackendError> {
        match self.lines.recv_timeout(timeout) {
            Ok(Ok(line)) => Ok(line),
            Ok(Err(error)) => Err(BackendError::Error {
                code: acos_protocol::ErrorCode::Internal,
                detail: format!("{context}: {error}; stderr tail: {}", self.stderr_tail()),
            }),
            Err(RecvTimeoutError::Timeout) => Err(BackendError::Timeout {
                detail: format!(
                    "{context}: no reply from acos-art within {}s; stderr tail: {}",
                    timeout.as_secs(),
                    self.stderr_tail()
                ),
            }),
            Err(RecvTimeoutError::Disconnected) => Err(BackendError::Error {
                code: acos_protocol::ErrorCode::Internal,
                detail: format!(
                    "{context}: acos-art session exited; stderr tail: {}",
                    self.stderr_tail()
                ),
            }),
        }
    }

    /// Send one command and keep the raw failure reply. The engine needs that
    /// reply for one case only: a delivered input whose action legitimately
    /// produced no pixel change. The guest reports the delivery evidence
    /// (`gesture` acks) in the same reply it uses to refuse the frame.
    fn command_raw(
        &mut self,
        request: serde_json::Value,
    ) -> Result<serde_json::Value, (ErrorCode, serde_json::Value, String)> {
        let stdin = self.stdin.as_mut().ok_or_else(|| {
            (
                ErrorCode::Internal,
                serde_json::Value::Null,
                "the ART session stdin is closed".to_string(),
            )
        })?;
        let line = request.to_string();
        if let Err(error) = writeln!(stdin, "{line}").and_then(|_| stdin.flush()) {
            return Err((
                ErrorCode::Internal,
                serde_json::Value::Null,
                format!("cannot write to acos-art: {error}"),
            ));
        }
        let timeout = self.timeout;
        let reply = self.read_line(timeout, "command").map_err(|error| {
            let code = error.code();
            let detail = error.to_protocol_error().message;
            (code, serde_json::Value::Null, detail)
        })?;
        let reply: serde_json::Value = serde_json::from_str(&reply).map_err(|error| {
            (
                ErrorCode::Internal,
                serde_json::Value::Null,
                format!("acos-art reply is not JSON: {error}"),
            )
        })?;
        if reply.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            let code = reply
                .get("code")
                .and_then(|v| v.as_str())
                .unwrap_or("COMMAND_FAILED");
            let message = reply
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("acos-art session command failed")
                .to_string();
            let code = ErrorCode::from_str_opt(code).unwrap_or(ErrorCode::Internal);
            return Err((code, reply, message));
        }
        Ok(reply)
    }

    fn command(&mut self, request: serde_json::Value) -> Result<serde_json::Value, BackendError> {
        self.command_raw(request)
            .map_err(|(code, _, message)| BackendError::Error {
                code,
                detail: format!("acos-art: {code}: {message}"),
            })
    }

    /// Send one input command. A guest reply that refuses the frame but carries
    /// a `gesture` record means the host-injected input was delivered and the
    /// guest could not settle an observation for it. That is **not** a stale
    /// frame: the engine settles it from its own bound captures in
    /// [`Self::settle_after_input`] and records the measured `visual_change` so
    /// no state change is claimed that the frames do not show. Every other
    /// `NO_FRESH_FRAME` stays a hard failure.
    fn input_command(
        &mut self,
        request: serde_json::Value,
        context: &str,
    ) -> Result<(), BackendError> {
        match self.command_raw(request) {
            Ok(reply) => {
                self.settle.guest += 1;
                let baseline = self.input_baseline.clone();
                self.absorb(&reply)?;
                // Compare exactly the pair the executor's `changed_from` will
                // compare: this action's frame against the frame it was handed
                // at the previous step (snapshotted by `begin_input` before the
                // point-resolution traversal). Both verdicts then agree by
                // construction: equal hashes are "delivered, no visual change",
                // different hashes are a state change the executor also sees.
                let visual_change = state_changed(baseline.as_ref(), self.cached.as_ref());
                self.last_input = Some(delivery(&reply, visual_change));
                Ok(())
            }
            Err((ErrorCode::NoFreshFrame, reply, _message)) if reply.get("gesture").is_some() => {
                if std::env::var_os("ACOS_ART_SETTLE_MODE").as_deref() == Some("guest-only".as_ref())
                {
                    // Diagnostic: the pre-W34 behaviour (one recovery capture,
                    // `visual_change: false`), for A/B cost runs.
                    let previous = self.seen_seq;
                    self.request_tree()?;
                    if let Some(cached) = self.cached.as_ref() {
                        if cached.seq <= previous {
                            return Err(stale_frame(
                                previous,
                                cached.seq,
                                &format!("{context} (no-pixel-change delivery)"),
                            ));
                        }
                    }
                    self.settle.recovered += 1;
                    self.settle.recovery_polls += 1;
                    self.settle.recovered_no_change += 1;
                    self.last_input = Some(delivery(&reply, false));
                    return Ok(());
                }
                self.settle_after_input(&reply, context)
            }
            Err((code, _, message)) => Err(BackendError::Error {
                code,
                detail: format!("acos-art: {code}: {message}"),
            }),
        }
    }

    /// Finish an input the guest could not settle, using bound captures.
    ///
    /// The guest refuses when its settle test (new buffer generation + new probe
    /// sample + changed pixels within its window) does not hold. Two causes are
    /// indistinguishable to the guest and both were charged the full window:
    /// a focus-only tap (no pixel change is the *correct* outcome) and a slow UI
    /// update. Both are decided here on what the frames actually show:
    ///
    /// * a recovery capture whose content hash differs from the pre-action frame
    ///   proves the action changed the screen, and a `tree` reply is itself a
    ///   traversal the guest settles (1.5 s internal window), so that capture is
    ///   the bound observation the following assertions need;
    /// * otherwise the input is a delivered, no-visual-change action once
    ///   `settle_polls` consecutive captures show that same content again.
    ///
    /// The comparison pair matters more than the signal: it is exactly the pair
    /// the executor's `SessionObservation::changed_from` compares (this action's
    /// frame vs the observation it handed back at the previous step). Comparing
    /// against the traversal a selector tap issued to resolve its point instead
    /// made T07's submit tap non-deterministic (frame 10 vs resolution frame 9
    /// differed by a cursor-blink phase while frame 10 vs the real baseline
    /// frame 8 were byte-identical), so the same tap was reported as a state
    /// change on some runs and as a no-op on others.
    ///
    /// The recovery never invents a frame: it keeps the sequence advancing, keeps
    /// the guest's acks, and reports the measured `visual_change`.
    fn settle_after_input(
        &mut self,
        reply: &serde_json::Value,
        context: &str,
    ) -> Result<(), BackendError> {
        let started = Instant::now();
        let previous_seq = self.seen_seq;
        // The baseline is the exact observation the executor compares against:
        // the one it handed back at the previous step (snapshotted before the
        // traversal a selector tap needs to resolve its point), falling back to
        // the current cache when the caller drives the session directly.
        let baseline = self
            .input_baseline
            .clone()
            .or_else(|| self.cached.as_ref().map(|frame| (frame.md5.clone(), frame.signature)));
        let mut polls = 0u64;
        let mut last_hash = baseline.clone();
        let mut visual_change = false;
        let mut budget_hit = false;
        loop {
            polls += 1;
            self.request_tree()?;
            let (seq, hash) = {
                let cached = self.require_cached_frame()?;
                (cached.seq, (cached.md5.clone(), cached.signature))
            };
            if seq <= previous_seq {
                return Err(stale_frame(
                    previous_seq,
                    seq,
                    &format!("{context} (recovery after refused frame)"),
                ));
            }
            if state_changed(baseline.as_ref(), self.cached.as_ref()) {
                visual_change = true;
            }
            let repeated = last_hash.as_ref() == Some(&hash);
            last_hash = Some(hash);
            if visual_change {
                // A traversal whose tree differs from the pre-action state is the
                // post-action state; `tree` already waited for the guest to settle
                // it, so this is the bound frame the next assertion reads.
                break;
            }
            if repeated && polls >= u64::from(self.settle_polls) {
                break;
            }
            let elapsed = started.elapsed();
            if elapsed >= self.settle_budget {
                budget_hit = true;
                break;
            }
            std::thread::sleep(
                Duration::from_millis(SETTLE_POLL_GAP_MS)
                    .min(self.settle_budget.saturating_sub(elapsed)),
            );
        }
        self.settle.recovered += 1;
        self.settle.recovery_polls += polls;
        self.settle.recovery_ms += started.elapsed().as_millis() as u64;
        if visual_change {
            self.settle.recovered_changed += 1;
        } else {
            self.settle.recovered_no_change += 1;
        }
        if budget_hit {
            self.settle.recovery_budget_hit += 1;
        }
        self.last_input = Some(InputDelivery {
            delivered: true,
            visual_change,
            acks: acks_from_reply(reply),
        });
        Ok(())
    }

    fn frame_meta(&self, seq: u64, md5: &str) -> FrameMeta {
        FrameMeta::new(
            seq,
            acos_verify::clock::rfc3339_from_unix_ms(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0),
            ),
            "art",
            self.width,
            self.height,
            self.density,
            md5,
            true,
        )
    }

    /// Absorb a reply's frame: sequence, PNG bytes and tree, all or nothing.
    ///
    /// A reply that carries a `frame_seq` also carries that frame's PNG; a tree
    /// reply carries the tree as well. A tree-less reply (a pure `screenshot`)
    /// only advances `seen_seq`; the bound tree keeps the sequence it was
    /// captured with, so a tree can never be bound to newer pixels.
    fn absorb(&mut self, reply: &serde_json::Value) -> Result<(), BackendError> {
        let raw_seq = reply.get("frame_seq").and_then(|v| v.as_u64());
        // A relaunched guest restarts its own sequence at 1; the engine's
        // sequence stays monotonic across the reset (see `seq_offset`).
        let seq = match raw_seq {
            Some(raw) => raw.saturating_add(self.seq_offset),
            None => self.cached.as_ref().map(|frame| frame.seq).unwrap_or(0),
        };
        let md5 = reply
            .get("md5")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let png = match reply.get("png").and_then(|v| v.as_str()) {
            Some(path) => read_png(Path::new(path))?,
            None => Vec::new(),
        };
        self.seen_seq = self.seen_seq.max(seq);
        let Some(tree_value) = reply.get("tree") else {
            // No tree in this reply (for example `key` or `screenshot`); keep the
            // cached tree only for the frame it was bound to.
            if let Some(cached) = self.cached.as_mut() {
                if cached.seq == seq && !png.is_empty() {
                    cached.png = png;
                    if !md5.is_empty() {
                        cached.md5 = md5;
                    }
                    cached.captured_at_ms = now_unix_ms();
                    cached.captured_at = rfc3339(cached.captured_at_ms);
                }
            }
            return Ok(());
        };
        if let Some(offset) = probe_input_offset(tree_value) {
            self.input_offset = offset;
        }
        let tree = ui_tree_from_json(&self.package, self.frame_meta(seq, &md5), tree_value)?;
        let signature = tree_signature(&tree);
        let captured_at_ms = now_unix_ms();
        self.cached = Some(CachedFrame {
            seq,
            md5,
            png,
            tree,
            signature,
            captured_at_ms,
            captured_at: rfc3339(captured_at_ms),
        });
        Ok(())
    }

    /// Ask for the live tree. The `tree` command re-traverses, draws and
    /// publishes a new frame, so the reply's tree and PNG share one `frame_seq`.
    fn request_tree(&mut self) -> Result<(), BackendError> {
        let reply = self.command(serde_json::json!({"cmd":"tree"}))?;
        self.absorb(&reply)
    }

    fn require_cached_tree(&mut self) -> Result<UiTree, BackendError> {
        if self.cached.is_none() {
            self.request_tree()?;
        }
        self.cached
            .as_ref()
            .map(|frame| frame.tree.clone())
            .ok_or_else(|| {
                live_frame_unavailable(
                    "the ART session has not completed a live frame yet (no tree)",
                )
            })
    }

    fn require_cached_frame(&self) -> Result<&CachedFrame, BackendError> {
        self.cached.as_ref().ok_or_else(|| {
            live_frame_unavailable("the ART session has not completed a live frame yet")
        })
    }

    fn kill_group(&mut self) {
        if let Some(mut child) = self.child.take() {
            let pid = child.id() as i32;
            let _ = child.kill();
            // SAFETY: `pid` is the direct child's pid and the child was spawned
            // into its own process group (`process_group(0)`), so `-pid` names
            // that group and no other process.
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
            let _ = child.wait();
        }
        self.stdin = None;
    }

    /// Kill any process left in the session's group, even after the leader
    /// exited: a guest that forked a helper must not outlive its session.
    fn kill_process_group(&self) {
        let Some(group) = self.process_group else {
            return;
        };
        if processes_in_group(group).is_empty() {
            return;
        }
        // SAFETY: `group` was the child's own process group id
        // (`process_group(0)`), so `-group` names that group and no other.
        unsafe {
            libc::kill(-group, libc::SIGKILL);
        }
    }

    /// Stop the guest and prove its whole process group is gone. Shared by
    /// `close` and `reset` (the latter relaunches in a fresh process).
    fn shutdown(&mut self) -> Result<(), BackendError> {
        let started = Instant::now();
        if let Some(stdin) = self.stdin.as_mut() {
            let _ = writeln!(stdin, "{}", serde_json::json!({"cmd":"close"}));
            let _ = stdin.flush();
        }
        self.stdin = None;
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut exited = false;
        if let Some(child) = self.child.as_mut() {
            while Instant::now() < deadline {
                match child.try_wait() {
                    Ok(Some(_)) => {
                        self.child = None;
                        exited = true;
                        break;
                    }
                    Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                    Err(_) => break,
                }
            }
        }
        if !exited {
            self.kill_group();
        }
        // Teardown guarantee: the whole process group is gone, not just the
        // direct child. A child the guest forked is killed here too; a survivor
        // that ignores SIGKILL is reported, never silently ignored.
        self.kill_process_group();
        let mut report = self.teardown_report();
        if !report.is_clean() {
            let grace = Instant::now() + Duration::from_secs(2);
            while Instant::now() < grace && !report.survivors.is_empty() {
                std::thread::sleep(Duration::from_millis(50));
                report = self.teardown_report();
            }
        }
        if !report.survivors.is_empty() {
            return Err(BackendError::Error {
                code: acos_protocol::ErrorCode::Internal,
                detail: format!(
                    "PROCESS_TREE_DIRTY: {} process(es) still alive in session group {:?}: {:?}",
                    report.survivors.len(),
                    report.process_group,
                    report.survivors
                ),
            });
        }
        self.write_timing(started.elapsed().as_millis() as u64);
        Ok(())
    }

    /// Records the session's cost split next to the guest evidence: guest boot
    /// (`session_start_ms`, charged to no step), quiet shutdown (`shutdown_ms`,
    /// outside the verdict clock) and how each input step reached its bound
    /// frame. Best effort: a write error never fails a run.
    fn write_timing(&self, shutdown_ms: u64) {
        let dir = self.artifacts.join("art-frames");
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        let body = self
            .settle
            .to_json(self.session_start_ms, shutdown_ms, self.frame_wait_secs);
        let _ = std::fs::write(
            dir.join("session-timing.json"),
            serde_json::to_string_pretty(&body).unwrap_or_default(),
        );
    }

    /// Processes still alive in the session's process group.
    ///
    /// Scans `/proc/<pid>/stat` for the group id (field 5). Zombies count as
    /// alive here: a process that has not been reaped still owns resources, and
    /// the caller must see it rather than a clean report.
    fn teardown_report(&self) -> TeardownReport {
        let Some(group) = self.process_group else {
            return TeardownReport::unverified();
        };
        TeardownReport {
            process_group: Some(group),
            survivors: processes_in_group(group),
            verified: true,
        }
    }
}

impl SessionBackend for ArtSession {
    fn engine(&self) -> Engine {
        Engine::Art
    }

    fn mode(&self) -> Mode {
        // This engine launches a real Activity; `render-app` (offline preview)
        // is a different command and never reaches this adapter.
        Mode::Live
    }

    fn capabilities(&self) -> SessionCapabilities {
        SessionCapabilities {
            launch: true,
            tree: true,
            screenshot: true,
            tap: true,
            // Host-side: resolve the node's text in the tree, tap its centre.
            tap_text: true,
            // W13: real key events on the focused window; `type_text` is one
            // key stroke per character (ASCII / US layout).
            type_text: true,
            key: true,
            // Host-side: stop the guest, relaunch the same APK in a fresh
            // process with an empty app-data directory (a real reset).
            reset: true,
            close: true,
            // The guest logd sink, snapshotted live (provenance guest-asserted).
            logs: true,
            environment: false,
            // Fatal guest log entries (ART LOG(FATAL)/CHECK/abort_message and
            // the JVM's `FATAL EXCEPTION` line) are a real crash signature.
            crash_reports: true,
        }
    }

    fn launch(&mut self, _apk: &Path) -> Result<LaunchOutcome, BackendError> {
        let previous = self.seen_seq;
        let reply = self.command(serde_json::json!({"cmd":"tree"}))?;
        self.absorb(&reply)?;
        let cached = self.require_cached_frame()?;
        let frame = cached.observed(previous);
        Ok(LaunchOutcome {
            session_id: format!(
                "s_art_{}",
                self.apk
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default()
            ),
            package: self.package.clone(),
            frame: Some(frame.tree.frame.clone()),
            screenshot_png: Some(frame.frame.png.clone()),
            crashed: None,
            detail: serde_json::json!({
                "engine": "art",
                "mode": "live",
                "activity": self.activity,
                "apk": self.apk.display().to_string(),
                "frame": frame.frame.label(),
                "provenance": {
                    "frame": "host-observed",
                    "tree": "guest-asserted",
                    "launch_markers": "guest-asserted",
                    "lifecycle_markers": "guest-asserted",
                },
            }),
        })
    }

    fn tree(&mut self) -> Result<UiTree, BackendError> {
        self.request_tree()?;
        self.require_cached_tree()
    }

    fn frame(&mut self) -> Result<ObservedFrame, BackendError> {
        // `tree` re-traverses and publishes a new frame; its reply carries both
        // the tree and the PNG of that frame.
        let previous = self.seen_seq;
        self.request_tree()?;
        let cached = self.require_cached_frame()?;
        if cached.png.is_empty() {
            return Err(live_frame_unavailable(
                "the ART tree reply carried no PNG; refusing to bind a tree to absent pixels",
            ));
        }
        Ok(cached.observed(previous))
    }

    fn last_frame(&self) -> Option<ObservedFrame> {
        self.cached
            .as_ref()
            .map(|frame| frame.observed(self.seen_seq.min(frame.seq.saturating_sub(1))))
    }

    fn last_input_delivery(&self) -> Option<InputDelivery> {
        self.last_input.clone()
    }

    fn last_crash(&self) -> Option<CrashRecord> {
        self.crash.clone()
    }

    /// Read the guest's own logd snapshot and map it to protocol log records.
    ///
    /// The logd sink drops oldest first when its cap is reached, so an entry's
    /// **absolute ordinal** is `total - kept + index`; that ordinal + 1 is the
    /// record `seq`, which stays stable across snapshots (the executor asks for
    /// "everything after `since`"). The timestamp is the host time at which the
    /// snapshot was read: the sink carries no guest timestamp, so this is an
    /// ordering fact, not an emission instant. Entries are guest-asserted
    /// (S8 §7): a trusted fixture may use them; an untrusted run must refuse
    /// them (`trust::TrustGuard`).
    fn logs_since(&mut self, since: u64) -> Result<Vec<LogRecord>, BackendError> {
        let reply = self.command(serde_json::json!({"cmd":"logs"}))?;
        let total = reply.get("total").and_then(|v| v.as_u64()).unwrap_or(0);
        self.logs_total_raw = self.logs_total_raw.max(total);
        if self.crash.is_none() {
            self.crash = fatal_log(&reply);
        }
        // `since` and the returned `seq` are both absolute (guest ordinal +
        // `log_offset`), so a cursor from before a reset stays meaningful.
        Ok(log_records_from_reply(
            &reply,
            since,
            now_unix_ms(),
            self.log_offset,
        ))
    }

    fn last_frame_seq(&self) -> u64 {
        self.seen_seq
    }

    fn screenshot(&mut self) -> Result<FrameEvidence, BackendError> {
        // A pure capture command: no tree is returned, so the bound tree keeps
        // its own sequence. Freshness is proven by the sequence advancing.
        let previous = self.seen_seq;
        let reply = self.command(serde_json::json!({"cmd":"screenshot"}))?;
        let raw_seq = reply
            .get("frame_seq")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| {
                live_frame_unavailable(
                    "the ART screenshot reply carried no frame_seq; it cannot be tied to a frame",
                )
            })?;
        let seq = raw_seq.saturating_add(self.seq_offset);
        let md5 = reply
            .get("md5")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let png_path = reply.get("png").and_then(|v| v.as_str()).ok_or_else(|| {
            live_frame_unavailable("the ART screenshot reply carried no PNG path")
        })?;
        if seq <= previous {
            return Err(stale_frame(previous, seq, "screenshot"));
        }
        let png = read_png(Path::new(png_path))?;
        if png.is_empty() {
            return Err(live_frame_unavailable(
                "the ART screenshot frame file is empty",
            ));
        }
        self.seen_seq = seq;
        let captured_at_ms = now_unix_ms();
        Ok(FrameEvidence {
            frame_seq: seq,
            previous_seq: previous,
            captured_at_ms,
            captured_at: rfc3339(captured_at_ms),
            md5,
            png,
        })
    }

    fn begin_input(&mut self) {
        self.input_baseline = self
            .cached
            .as_ref()
            .map(|frame| (frame.md5.clone(), frame.signature));
    }

    fn tap(&mut self, x: i32, y: i32) -> Result<(), BackendError> {
        // The engine's tap coordinates are display/screen coordinates (resolved
        // from the probe's `bounds_on_screen`). The interactive input channel's
        // transform is window-local (W10/I2), so subtract the measured window
        // origin here, once, in the backend that owns that convention.
        let (dx, dy) = self.input_offset;
        self.input_command(
            serde_json::json!({"cmd":"tap","x":x - dx,"y":y - dy}),
            "input.tap",
        )
    }

    fn tap_text(&mut self, text: &str) -> Result<(), BackendError> {
        let tree = self.require_cached_tree()?;
        let node = tree
            .find_one(&Selector::by_text(text))
            .ok_or_else(|| BackendError::Failed {
                code: acos_protocol::ErrorCode::AssertionFailed,
                detail: format!("no node with text {text:?} in the live ART tree"),
            })?
            .clone();
        let bounds = node.bounds;
        self.tap(
            (bounds.left() + bounds.right()) / 2,
            (bounds.top() + bounds.bottom()) / 2,
        )
    }

    fn type_text(&mut self, text: &str) -> Result<(), BackendError> {
        self.input_command(
            serde_json::json!({"cmd":"type_text","text":text}),
            "input.type_text",
        )
    }

    fn key(&mut self, key: &str) -> Result<(), BackendError> {
        let code = key_code(key);
        if code < 0 {
            return Err(BackendError::unsupported(
                "input.key",
                format!("unknown symbolic key {key:?}"),
            ));
        }
        self.input_command(serde_json::json!({"cmd":"key","code":code}), "input.key")
    }

    fn reset(&mut self) -> Result<(), BackendError> {
        // Honest reset on this channel: stop the guest and launch the same APK
        // in a fresh `acos-art` process. `acos-art` gives every launch its own
        // run directory (dalvik-cache, tmp, and an **empty** app-data), so this
        // is a fresh process, a fresh Activity and cleared app state — not the
        // app's own Reset control. The engine keeps its own frame sequence, so
        // the frame after the reset is provably newer than the one before it.
        let offset = self.seen_seq;
        // Flush once so the absolute log ordinal of the old guest is exact
        // before its sink dies with the process.
        let _ = self.logs_since(0);
        let log_offset = self.log_offset.saturating_add(self.logs_total_raw);
        let apk = self.apk.clone();
        let artifacts = self.artifacts.clone();
        let settle = self.settle.clone();
        let boot_ms = self.session_start_ms;
        // Same teardown rule as `close`: the old guest's whole process group
        // must be gone before the replacement starts (one ART guest at a time).
        self.closed = true;
        self.shutdown()?;
        let mut fresh = ArtSession::start(&apk, &artifacts)?;
        fresh.seq_offset = offset;
        fresh.log_offset = log_offset;
        // Session-level cost accounting spans the relaunch.
        fresh.settle = settle;
        fresh.session_start_ms = boot_ms.saturating_add(fresh.session_start_ms);
        // Leave a live frame bound: the step after `reset` may be a pure
        // assertion (`assert_text`), which reads the last completed frame.
        let reply = fresh.command(serde_json::json!({"cmd":"tree"}))?;
        fresh.absorb(&reply)?;
        fresh.last_input = None;
        *self = fresh;
        Ok(())
    }

    fn close(&mut self) -> Result<(), BackendError> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        self.shutdown()
    }

    fn teardown(&mut self) -> Result<TeardownReport, BackendError> {
        self.close()?;
        Ok(self.teardown_report())
    }
}
impl Drop for ArtSession {
    fn drop(&mut self) {
        let _ = self.close();
        self.kill_group();
    }
}

impl std::fmt::Debug for ArtSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArtSession")
            .field("apk", &self.apk)
            .field("package", &self.package)
            .field("closed", &self.closed)
            .finish()
    }
}

/// The first fatal entry in a logd snapshot, as a crash record.
///
/// The signature matches `acos-art`'s own `LogEntry::is_fatal`: ART's
/// `LOG(FATAL)`, `CHECK`/`CHECK_EQ` failures (tags `art_aborter`,
/// `abort_message`), and the JVM's uncaught-exception line. An ordinary `E`
/// entry (RippleDrawable, StrictMode, ...) is *not* a crash and must not be
/// reported as one.
fn fatal_log(reply: &serde_json::Value) -> Option<CrashRecord> {
    let entries = reply.get("entries").and_then(|v| v.as_array())?;
    for entry in entries {
        let prio = entry.get("prio").and_then(|v| v.as_i64()).unwrap_or(4);
        let tag = entry.get("tag").and_then(|v| v.as_str()).unwrap_or("");
        let message = entry.get("message").and_then(|v| v.as_str()).unwrap_or("");
        let fatal = prio >= 7
            || tag == "art_aborter"
            || tag == "abort_message"
            || message.contains("Check failed")
            || message.contains("CHECK failed")
            || (tag == "AndroidRuntime"
                && (message.starts_with("FATAL EXCEPTION")
                    || message.contains("FATAL EXCEPTION")));
        if fatal {
            return Some(CrashRecord {
                exception: if tag.is_empty() {
                    "FATAL".to_string()
                } else {
                    tag.to_string()
                },
                message: message.to_string(),
                stack: Vec::new(),
                fatal: true,
            });
        }
    }
    None
}

/// Map a `{"cmd":"logs"}` reply to protocol records.
///
/// The logd sink drops oldest first at its cap, so the absolute ordinal of
/// `entries[i]` is `total - kept + i`; ordinal + 1 is the record `seq`, stable
/// across snapshots. `timestamp_ms` is the host read time (the sink carries no
/// guest timestamp): it orders records, it is not an emission instant.
fn log_records_from_reply(
    reply: &serde_json::Value,
    since: u64,
    timestamp_ms: u64,
    seq_offset: u64,
) -> Vec<LogRecord> {
    let total = reply.get("total").and_then(|v| v.as_u64()).unwrap_or(0);
    let entries = reply
        .get("entries")
        .and_then(|v| v.as_array())
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let first = total.saturating_sub(entries.len() as u64);
    let mut records = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let seq = first
            .saturating_add(index as u64)
            .saturating_add(1)
            .saturating_add(seq_offset);
        if seq <= since {
            continue;
        }
        let level = match entry.get("prio").and_then(|v| v.as_i64()).unwrap_or(4) {
            2 => LogLevel::Verbose,
            3 => LogLevel::Debug,
            5 => LogLevel::Warn,
            6 | 7 => LogLevel::Error,
            _ => LogLevel::Info,
        };
        records.push(LogRecord {
            seq,
            timestamp_ms,
            level,
            tag: entry
                .get("tag")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            message: entry
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        });
    }
    records
}

fn resolve_binary() -> Result<PathBuf, BackendError> {
    if let Ok(value) = std::env::var("ACOS_ART_BIN") {
        let path = PathBuf::from(&value);
        if path.is_file() {
            return Ok(path);
        }
        return Err(BackendError::Unsupported {
            capability: "engine.art.binary".into(),
            detail: format!("ACOS_ART_BIN={value} is not a file"),
        });
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for candidate in [dir.join("acos-art"), dir.join("../acos-art")] {
                if candidate.is_file() {
                    return Ok(candidate);
                }
            }
        }
    }
    // Last resort: PATH lookup by spawning with a bare name. `Command` resolves
    // it; `start` turns a missing binary into `UNSUPPORTED_BACKEND`.
    Ok(PathBuf::from("acos-art"))
}

fn read_png(path: &Path) -> Result<Vec<u8>, BackendError> {
    let file = std::fs::File::open(path).map_err(|error| BackendError::Error {
        code: acos_protocol::ErrorCode::RenderIo,
        detail: format!("cannot open frame {}: {error}", path.display()),
    })?;
    let mut bytes = Vec::new();
    file.take(MAX_PNG_BYTES)
        .read_to_end(&mut bytes)
        .map_err(|error| BackendError::Error {
            code: acos_protocol::ErrorCode::RenderIo,
            detail: format!("cannot read frame {}: {error}", path.display()),
        })?;
    Ok(bytes)
}

/// Pids currently in process group `group`.
///
/// Reads field 5 (`pgrp`) of `/proc/<pid>/stat`. The command name can contain
/// spaces and parentheses, so parsing starts after the last `)`. `/proc` is
/// Linux-only, which is the only place ACOS runs.
pub(crate) fn processes_in_group(group: i32) -> Vec<u32> {
    let mut pids = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return pids;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|name| name.parse::<u32>().ok()) else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        let Some(rest) = stat.rfind(')').map(|index| &stat[index + 1..]) else {
            continue;
        };
        let mut fields = rest.split_whitespace();
        // state, ppid, pgrp
        let _state = fields.next();
        let _ppid = fields.next();
        if fields.next().and_then(|pgrp| pgrp.parse::<i32>().ok()) == Some(group) {
            pids.push(pid);
        }
    }
    pids.sort_unstable();
    pids
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn art_capabilities_name_the_protocol_gaps() {
        let caps = SessionCapabilities {
            launch: true,
            tree: true,
            screenshot: true,
            tap: true,
            tap_text: true,
            type_text: true,
            key: true,
            reset: true,
            close: true,
            logs: true,
            environment: false,
            // Fatal guest log entries (ART LOG(FATAL)/CHECK/abort_message and
            // the JVM's `FATAL EXCEPTION` line) are a real crash signature.
            crash_reports: true,
        };
        assert_eq!(caps.missing(), vec!["environment"]);
        // The executor sees the same two gaps through its capability names.
        let protocol = caps.to_protocol();
        assert!(protocol.logs && protocol.reset);
        assert!(!protocol.environment && protocol.crash_reports);
    }

    #[test]
    fn log_snapshot_maps_absolute_ordinals_not_array_indices() {
        // The sink drops oldest first, so `total` (5) is larger than `kept` (2).
        let reply = serde_json::json!({
            "ok": true, "cmd": "logs", "total": 5, "kept": 2, "dropped": 3,
            "entries": [
                {"prio": 4, "tag": "AcosForm", "message": "onCreate"},
                {"prio": 5, "tag": "AcosForm", "message": "onResume"}
            ]
        });
        let all = log_records_from_reply(&reply, 0, 7, 0);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].seq, 4, "first retained entry keeps its ordinal");
        assert_eq!(all[1].seq, 5);
        assert_eq!(all[0].level, LogLevel::Info);
        assert_eq!(all[1].level, LogLevel::Warn);
        assert_eq!(all[0].tag, "AcosForm");
        assert_eq!(all[0].timestamp_ms, 7);
        // A cursor inside the retained window returns only the newer records.
        let newer = log_records_from_reply(&reply, 4, 7, 0);
        assert_eq!(newer.len(), 1);
        assert_eq!(newer[0].message, "onResume");
        // Nothing new is an empty list, never a re-delivery.
        assert!(log_records_from_reply(&reply, 5, 7, 0).is_empty());
        // An empty snapshot is valid (no logs yet).
        let empty = serde_json::json!({"total": 0, "entries": []});
        assert!(log_records_from_reply(&empty, 0, 7, 0).is_empty());
    }

    #[test]
    fn probe_input_offset_reads_the_window_origin_from_the_tree_envelope() {
        // The probe wraps nodes in `{"nodes": [...]}`; the value is
        // window-local and `bounds_on_screen` adds the status-bar inset.
        let tree = serde_json::json!({
            "nodes": [{
                "class": "android.widget.FrameLayout",
                "id": "",
                "bounds": "[0,0][1080,2209]",
                "bounds_on_screen": "[0,128][1080,2337]",
                "children": [{
                    "class": "android.widget.TextView",
                    "id": "count",
                    "bounds": "[42,189][63,240]",
                    "bounds_on_screen": "[42,317][63,368]"
                }]
            }]
        });
        assert_eq!(probe_input_offset(&tree), Some((0, 128)));
        // A probe without `bounds_on_screen` has no measured offset.
        let legacy = serde_json::json!({
            "nodes": [{"class": "android.view.View", "bounds": "[0,0][10,10]"}]
        });
        assert_eq!(probe_input_offset(&legacy), None);
    }

    /// A fake `acos-art` that speaks just enough of the interactive protocol to
    /// prove `reset` really starts a new guest. Each process restarts its own
    /// frame sequence at 1 and reports one log line, so a reset that reused the
    /// old guest would be visible as a non-advancing engine sequence.
    fn fake_art_binary(dir: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let png = dir.join("tiny.png");
        std::fs::write(&png, super::super::fake::TINY_PNG).unwrap();
        let script = dir.join("fake-acos-art.sh");
        let mut body = String::new();
        body.push_str("#!/bin/sh\n");
        body.push_str("echo '{\"ok\":true,\"cmd\":\"ready\",\"package\":\"dev.test\",\"activity\":\"dev.test/.Main\",\"width\":1080,\"height\":2400,\"density\":420}'\n");
        body.push_str("seq=0\n");
        body.push_str("while IFS= read -r line; do\n");
        body.push_str("  case \"$line\" in\n");
        body.push_str("    *'\"cmd\":\"tree\"'*)\n");
        body.push_str("      seq=$((seq+1))\n");
        body.push_str(&format!(
            "      echo '{{\"ok\":true,\"cmd\":\"tree\",\"frame_seq\":1,\"md5\":\"m1\",\"png\":\"{}\",\"tree\":{{\"nodes\":[]}}}}'\n",
            png.display()
        ));
        body.push_str("      ;;\n");
        body.push_str("    *'\"cmd\":\"logs\"'*)\n");
        body.push_str("      echo '{\"ok\":true,\"cmd\":\"logs\",\"total\":1,\"kept\":1,\"dropped\":0,\"entries\":[{\"prio\":4,\"tag\":\"AcosTest\",\"message\":\"onCreate\"}]}'\n");
        body.push_str("      ;;\n");
        body.push_str("    *'\"cmd\":\"close\"'*)\n");
        body.push_str("      echo '{\"ok\":true,\"cmd\":\"close\"}'\n");
        body.push_str("      exit 0\n");
        body.push_str("      ;;\n");
        body.push_str("    *)\n");
        body.push_str("      echo '{\"ok\":false,\"code\":\"UNSUPPORTED_ANDROID_API\",\"error\":\"unsupported\"}'\n");
        body.push_str("      ;;\n");
        body.push_str("  esac\n");
        body.push_str("done\n");
        std::fs::write(&script, body).unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
        script
    }

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "acos-art-reset-{}-{}-{name}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn reset_relaunches_a_fresh_guest_and_keeps_frames_and_logs_monotonic() {
        let dir = scratch_dir("reset");
        let script = fake_art_binary(&dir);
        let apk = dir.join("fake.apk");
        std::fs::write(&apk, b"not-a-real-apk").unwrap();
        // `resolve_binary` is the only reader of this variable in this crate, and
        // only this test starts a session.
        std::env::set_var("ACOS_ART_BIN", &script);
        let mut session = ArtSession::start(&apk, &dir).expect("fake session starts");
        assert!(session.capabilities().reset);
        assert!(session.capabilities().logs);

        // Bind a first frame (seq 1 in the first guest) and read its log.
        let first = session.frame().expect("first frame");
        let first_seq = first.frame_seq();
        assert_eq!(first_seq, 1);
        let logs = session.logs_since(0).expect("first guest log");
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].seq, 1);
        assert_eq!(logs[0].tag, "AcosTest");

        // Reset: a new guest process, whose own frame/log sequences restart.
        session.reset().expect("reset relaunches");
        let after = session
            .last_frame()
            .expect("reset leaves a bound live frame");
        assert!(
            after.frame_seq() > first_seq,
            "engine frame sequence must advance across reset: {} -> {}",
            first_seq,
            after.frame_seq()
        );
        assert!(after.frame.is_fresh(), "{}", after.frame.label());

        // A log written by the new guest gets an ordinal after the old guest's,
        // so a cursor from before the reset cannot swallow it.
        let new_logs = session.logs_since(1).expect("new guest log");
        assert_eq!(
            new_logs.len(),
            1,
            "the post-reset line must not collide with the pre-reset seq"
        );
        assert_eq!(new_logs[0].seq, 2);
        assert!(session.logs_since(2).unwrap().is_empty());

        session.close().expect("clean close");
        std::env::remove_var("ACOS_ART_BIN");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A fake `acos-art` whose inputs are always refused on the guest side
    /// (the shape of a focus-only tap: generation and probe advance, the guest
    /// cannot settle a changed frame) and whose tree content changes only after
    /// `change_after` traversals. Records how many `tree` commands it served.
    fn refusing_art_binary(dir: &Path, change_after: u32) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let png = dir.join("tiny.png");
        std::fs::write(&png, super::super::fake::TINY_PNG).unwrap();
        let script = dir.join("fake-acos-art-refusing.sh");
        let mut body = String::new();
        body.push_str("#!/bin/sh\n");
        body.push_str(
            "echo '{\"ok\":true,\"cmd\":\"ready\",\"package\":\"dev.test\",\
             \"activity\":\"dev.test/.Main\",\"width\":1080,\"height\":2400,\
             \"density\":420}'\n",
        );
        body.push_str("trees=0\n");
        body.push_str("while IFS= read -r line; do\n");
        body.push_str("  case \"$line\" in\n");
        body.push_str("    *'\"cmd\":\"tree\"'*)\n");
        body.push_str("      trees=$((trees+1))\n");
        for (condition, text) in [("le", "before"), ("gt", "after")] {
            body.push_str(&format!(
                "      if [ \"$trees\" -{condition} {change_after} ]; then\n"
            ));
            body.push_str(&format!(
                "      echo '{{\"ok\":true,\"cmd\":\"tree\",\"frame_seq\":'\"$trees\"',\"md5\":\"m-{text}\",\"png\":\"{}\",\"tree\":{{\"nodes\":[{{\"class\":\"android.widget.TextView\",\"text\":\"{text}\",\"bounds\":\"[0,0][10,10]\"}}]}}}}'\n",
                png.display()
            ));
            body.push_str("      fi\n");
        }
        body.push_str("      ;;\n");
        body.push_str("    *'\"cmd\":\"tap\"'*)\n");
        body.push_str(
            "      echo '{\"ok\":false,\"cmd\":\"tap\",\"code\":\"NO_FRESH_FRAME\",\
             \"error\":\"no settled observation\",\"gesture\":{\"seqs\":[9],\
             \"acks\":[{\"seq\":9,\"handled\":true,\"latency_us\":1200}]}}'\n",
        );
        body.push_str("      ;;\n");
        body.push_str("    *'\"cmd\":\"logs\"'*)\n");
        body.push_str(
            "      echo '{\"ok\":true,\"cmd\":\"logs\",\"total\":0,\"kept\":0,\
             \"dropped\":0,\"entries\":[]}'\n",
        );
        body.push_str("      ;;\n");
        body.push_str("    *'\"cmd\":\"close\"'*)\n");
        body.push_str("      echo '{\"ok\":true,\"cmd\":\"close\"}'\n");
        body.push_str("      exit 0\n");
        body.push_str("      ;;\n");
        body.push_str("    *)\n");
        body.push_str(
            "      echo '{\"ok\":false,\"code\":\"UNSUPPORTED_ANDROID_API\",\"error\":\"unsupported\"}'\n",
        );
        body.push_str("      ;;\n");
        body.push_str("  esac\n");
        body.push_str("done\n");
        std::fs::write(&script, body).unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
        script
    }

    /// A refused input must be settled from the engine's own bound captures:
    /// the tap succeeds, the frame sequence advances, and `visual_change` is
    /// the *measured* comparison of the post-action tree with the pre-action
    /// tree — never the blind `false` the recovery used to record.
    #[test]
    fn refused_input_is_settled_from_bound_captures() {
        let dir = scratch_dir("settle");
        // The first traversal is `before`; the recovery traversal is `after`,
        // so the tap must be reported as a visual change.
        let script = refusing_art_binary(&dir, 1);
        let apk = dir.join("fake.apk");
        std::fs::write(&apk, b"not-a-real-apk").unwrap();
        std::env::set_var("ACOS_ART_BIN", &script);
        std::env::set_var("ACOS_ART_SETTLE_MS", "3000");
        let mut session = ArtSession::start(&apk, &dir).expect("fake session starts");
        let before = session.frame().expect("pre-action frame");
        let before_hash = before.tree.frame.content_hash.clone();
        session
            .tap(1, 1)
            .expect("a refused input is settled, not failed");
        let after = session.last_frame().expect("post-action frame");
        assert!(after.frame_seq() > before.frame_seq());
        assert_ne!(
            after.tree.frame.content_hash, before_hash,
            "the recovery must hand back the post-action traversal"
        );
        let delivery = session.last_input_delivery().expect("delivery record");
        assert!(delivery.delivered);
        assert!(
            delivery.visual_change,
            "the tree changed, so the change is claimed"
        );
        assert_eq!(delivery.acks.len(), 1, "guest acks survive the recovery");
        session.close().expect("clean close");
        // Cost accounting really is written (it is what the W34 table reads).
        let timing: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("art-frames/session-timing.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(timing["input_settles"]["recovered"], 1);
        assert_eq!(timing["input_settles"]["recovered_changed"], 1);
        assert_eq!(timing["guest_frame_wait_secs"], 3);

        // Negative case: an unchanged tree must NOT claim a visual change, and
        // the recovery must stop after two identical traversals.
        std::env::set_var("ACOS_ART_BIN", &refusing_art_binary(&dir, u32::MAX));
        let mut session = ArtSession::start(&apk, &dir).expect("second fake session starts");
        let _ = session.frame().expect("pre-action frame");
        session.tap(1, 1).expect("a no-change delivery still settles");
        let delivery = session.last_input_delivery().expect("delivery record");
        assert!(delivery.delivered);
        assert!(
            !delivery.visual_change,
            "no tree change means no claimed change"
        );
        assert_eq!(session.settle.recovered_no_change, 1);
        assert_eq!(session.settle.recovered_changed, 0);
        assert_eq!(
            session.settle.recovery_polls, 2,
            "two identical polls stop it"
        );
        session.close().expect("clean close");
        std::env::remove_var("ACOS_ART_BIN");
        std::env::remove_var("ACOS_ART_SETTLE_MS");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fatal_log_entries_are_crashes_and_ordinary_errors_are_not() {
        // Negative case first: a routine E line must never look like a crash.
        let benign = serde_json::json!({"entries": [
            {"prio": 6, "tag": "RippleDrawable", "message": "STYLE_PATTERNED not supported"},
            {"prio": 6, "tag": "OpenGLRenderer", "message": "Failed to initialize"}
        ]});
        assert!(fatal_log(&benign).is_none());
        assert!(fatal_log(&serde_json::json!({"entries": []})).is_none());

        // ART CHECK/abort (prio 7 and the abort tags).
        let art = serde_json::json!({"entries": [
            {"prio": 3, "tag": "art", "message": "start"},
            {"prio": 7, "tag": "art_aborter", "message": "Check failed: x"}
        ]});
        let crash = fatal_log(&art).expect("a prio-7 entry is fatal");
        assert_eq!(crash.exception, "art_aborter");
        assert!(crash.fatal);
        let check = serde_json::json!({"entries": [
            {"prio": 4, "tag": "art", "message": "Check failed: ART"}
        ]});
        assert!(fatal_log(&check).is_some());

        // The JVM's uncaught-exception line.
        let jvm = serde_json::json!({"entries": [
            {"prio": 6, "tag": "AndroidRuntime",
             "message": "FATAL EXCEPTION: main\njava.lang.IllegalStateException"}
        ]});
        let crash = fatal_log(&jvm).expect("FATAL EXCEPTION is a crash");
        assert_eq!(crash.exception, "AndroidRuntime");
    }

    #[test]
    fn keycodes_cover_the_art_protocol() {
        assert_eq!(key_code("back"), 4);
        assert_eq!(key_code("enter"), 66);
    }

    /// The teardown guarantee is a real scan of the whole process group: a
    /// direct child plus a grandchild, both killed, and no survivor left. This
    /// is the same check `close` performs on an ART session.
    #[test]
    fn teardown_scan_sees_the_whole_process_group_and_ends_clean() {
        let mut child = Command::new("/bin/sh")
            .arg("-c")
            .arg("sleep 30 & sleep 30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .expect("spawn a process group");
        let group = child.id() as i32;
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline && processes_in_group(group).len() < 2 {
            std::thread::sleep(Duration::from_millis(20));
        }
        let seen = processes_in_group(group);
        assert!(
            seen.contains(&(group as u32)) && seen.len() >= 2,
            "the scan must see the group leader and its child: {seen:?}"
        );
        // SAFETY: the child leads this group; `-group` names it and nothing else.
        unsafe {
            libc::kill(-group, libc::SIGKILL);
        }
        let _ = child.wait();
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline && !processes_in_group(group).is_empty() {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            processes_in_group(group),
            Vec::<u32>::new(),
            "a killed process group must leave no survivors"
        );
    }
}
