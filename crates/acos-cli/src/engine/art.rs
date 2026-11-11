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
//! The protocol has no `rotate` and no log command.
//! [`SessionCapabilities`] says so, and a scenario step that needs one fails with
//! `UNSUPPORTED_BACKEND` naming it. Nothing here renders offline: this engine is
//! [`Mode::Live`]; if the binary, the sysroot or a command is unavailable the
//! caller gets a structured error, never a preview pass.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use acos_protocol::ui::UiTree;
use acos_protocol::{ErrorCode, FrameMeta, Selector};
use acos_verify::backend::{InputAck, InputDelivery};
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

/// One ART session.
pub struct ArtSession {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    lines: Receiver<Result<String, String>>,
    stderr_tail: Arc<Mutex<VecDeque<u8>>>,
    timeout: Duration,
    /// The APK this session was started for (a session serves one app).
    apk: PathBuf,
    package: String,
    activity: Option<String>,
    width: u32,
    height: u32,
    density: u32,
    /// The newest *bound* frame seen (tree and PNG of one traversal).
    cached: Option<CachedFrame>,
    /// The highest frame sequence any reply has reported.
    seen_seq: u64,
    /// The process group the session child leads (its pid).
    process_group: Option<i32>,
    /// Host-observed delivery of the most recent input action.
    last_input: Option<InputDelivery>,
    /// `(dx, dy)` from display/screen coordinates to the input channel's
    /// window-local convention, measured from the probe's `bounds_on_screen`
    /// minus `bounds` (normally the status-bar inset). The engine resolves tap
    /// points from screen bounds; `inject_tap` takes window-local points.
    input_offset: (i32, i32),
    closed: bool,
}

struct CachedFrame {
    seq: u64,
    md5: String,
    png: Vec<u8>,
    tree: UiTree,
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

/// Build a host-observed delivery record from a session reply.
///
/// `previous_md5` is the content hash of the frame before the action. A normal
/// reply whose `md5` is unchanged is a delivered input with no visual change;
/// a recovery reply is always `visual_change: false`. The acks are copied
/// verbatim and remain guest-asserted.
fn input_delivery(
    reply: &serde_json::Value,
    previous_md5: Option<&str>,
    changed_if_unknown: bool,
) -> InputDelivery {
    let visual_change = match (reply.get("md5").and_then(|v| v.as_str()), previous_md5) {
        (Some(current), Some(previous)) => current != previous,
        _ => changed_if_unknown,
    };
    let acks = reply
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
        .unwrap_or_default();
    InputDelivery {
        delivered: true,
        visual_change,
        acks,
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
    /// `screenshot`, `tap`, `close`). `tree`/`tap` replies carry the in-guest
    /// view-tree probe and a C1 buffer capture; `screenshot` is a C1 buffer
    /// capture labelled `acos-c1-buffer`. `reset` and `key` are reported
    /// unsupported (the app's Reset control is a tap; key activation still
    /// needs the IME `EditorInfo` codec).
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
        // Own process group: a `close` that does not take must not leave ART or
        // its sandbox behind.
        command.process_group(0);
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
            package: String::new(),
            activity: None,
            width: 1080,
            height: 2400,
            density: 420,
            cached: None,
            seen_seq: 0,
            process_group: Some(_pid as i32),
            last_input: None,
            input_offset: (0, 0),
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
        Ok(session)
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
    /// a `gesture` record means the host-injected input was delivered and simply
    /// changed no pixels (focus-only tap, consumed key). That is **not** a stale
    /// frame: the engine re-binds a fresh tree/capture and records
    /// `visual_change: false` so no state change is claimed. Every other
    /// `NO_FRESH_FRAME` (or a missing new frame after the recovery capture) stays
    /// a hard failure.
    fn input_command(
        &mut self,
        request: serde_json::Value,
        context: &str,
    ) -> Result<(), BackendError> {
        let previous_md5 = self.cached.as_ref().map(|frame| frame.md5.clone());
        match self.command_raw(request) {
            Ok(reply) => {
                self.last_input = Some(input_delivery(&reply, previous_md5.as_deref(), true));
                self.absorb(&reply)
            }
            Err((ErrorCode::NoFreshFrame, reply, message)) if reply.get("gesture").is_some() => {
                let _ = message;
                // The guest session injected the input and then found no
                // changed pixels. Capture the current state as the action's
                // frame; the executor sees a fresh sequence and a delivery
                // record with no visual change.
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
                self.last_input = Some(input_delivery(&reply, None, false));
                Ok(())
            }
            Err((code, _, message)) => Err(BackendError::Error {
                code,
                detail: format!("acos-art: {code}: {message}"),
            }),
        }
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
        let seq = reply
            .get("frame_seq")
            .and_then(|v| v.as_u64())
            .or_else(|| self.cached.as_ref().map(|frame| frame.seq))
            .unwrap_or(0);
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
        let captured_at_ms = now_unix_ms();
        self.cached = Some(CachedFrame {
            seq,
            md5,
            png,
            tree,
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
            // The real launch session has no `reset` command; the scenario taps
            // the app's own Reset control instead.
            reset: false,
            close: true,
            logs: false,
            environment: false,
            crash_reports: false,
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

    fn last_frame_seq(&self) -> u64 {
        self.seen_seq
    }

    fn screenshot(&mut self) -> Result<FrameEvidence, BackendError> {
        // A pure capture command: no tree is returned, so the bound tree keeps
        // its own sequence. Freshness is proven by the sequence advancing.
        let previous = self.seen_seq;
        let reply = self.command(serde_json::json!({"cmd":"screenshot"}))?;
        let seq = reply
            .get("frame_seq")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| {
                live_frame_unavailable(
                    "the ART screenshot reply carried no frame_seq; it cannot be tied to a frame",
                )
            })?;
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
        let reply = self.command(serde_json::json!({"cmd":"reset"}))?;
        self.absorb(&reply)
    }

    fn close(&mut self) -> Result<(), BackendError> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
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
        Ok(())
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
            reset: false,
            close: true,
            logs: false,
            environment: false,
            crash_reports: false,
        };
        assert_eq!(
            caps.missing(),
            vec!["reset", "logs", "environment", "crash_reports"]
        );
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
