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

use std::collections::{HashMap, VecDeque};
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
    /// The newest *bound* frame seen (tree and, when the reply carried one,
    /// PNG of one traversal). A tree-only reply updates the tree but never
    /// invents pixels.
    cached: Option<CachedFrame>,
    /// The newest observation that actually carried pixels. A tree-only
    /// `acos_observe_tree` must not erase this (P3): `last_frame`/`bound_frame`
    /// and `observe_session` keep describing the last completed pixel frame.
    last_pixel: Option<CachedFrame>,
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
    /// minus `bounds` (normally the status-bar inset). This is the **first**
    /// window's offset: the fallback for an input that carries no window label
    /// (an explicit screen point). A selector tap uses `window_offsets`.
    input_offset: (i32, i32),
    /// Per-window `(dx, dy)` display-to-window offsets, keyed by the probe's
    /// `window` label. Every window has its own input channel and its own
    /// origin (a dialog sits at `[152,976]`, the Activity at `[0,128]`), so a
    /// single global offset sends the dialog's button to the wrong channel
    /// point (W36 §4.2).
    window_offsets: HashMap<String, (i32, i32)>,
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
    /// Host-observed phase timings of the most recent input command (W55).
    last_input_timing: Option<InputTiming>,
    /// Host-observed phase timings of the most recent tree/capture command.
    last_phase: Option<PhaseTiming>,
    /// P1: the guest's settle block for the most recent input (`mode`, `settled`,
    /// `ack_ms`, `first_change_ms`, `frame_ms`, `state_changed`). Opaque JSON:
    /// it is guest-reported and copied, never synthesised.
    last_input_settle: Option<serde_json::Value>,
    /// P1: `"fast"` or `"strict"` for the next input command. Serve sets it per
    /// verb; the default is strict.
    settle_mode: &'static str,
    /// P3b: the guest's `settle` block for the most recent tree request
    /// (`mode`, `settled`, `probe_seq`, `tree_age_ms`, `bound`). Copied into
    /// the `acos_observe_tree` result; never synthesised.
    last_tree_settle: Option<serde_json::Value>,
    /// P9: the settle mode the **next** `reset` binds its fresh tree with
    /// (`strict` by default). Serve sets it per `acos_reset` call; the reset
    /// body reads it before it replaces the session.
    reset_settle: &'static str,
    /// P9: whether that reset's tree request also captures a PNG.
    reset_pixels: bool,
    /// W7: `"carve"`/`"report"`, the session's launch-time app-window inset
    /// model. A `reset` relaunch must use the same model, and every reply may
    /// state it.
    insets: &'static str,
    /// P12: `"on"`/`"off"`, the session's launch-time animation scales. Every
    /// reply states it so a functional result is never mistaken for animation
    /// evidence.
    animations: &'static str,
    /// P12b: per-request strict/fast settle deadline (`settle_timeout_ms`).
    /// `None` keeps the engine's documented default; on timeout the guest still
    /// returns the latest tree and frame identity with `settled:false`.
    settle_timeout_ms: Option<u64>,
    /// P12b: deadline for the tree the next `reset` binds.
    reset_settle_timeout_ms: Option<u64>,
    closed: bool,
    /// GL1: how the last explicit destruction of this session ended
    /// (orderly activity lifecycle vs. killed after the bounded grace) and how
    /// long it took. Reported by `acos_destroy`; never guessed.
    last_teardown: Option<(TeardownPath, u64)>,
}

/// How an explicit session destruction ended (GL1).
///
/// Android separates the two: it delivers `onPause`/`onStop`/`onDestroy` and
/// then `Process.killProcess` — it never *joins* the app's threads (a React
/// Native process has many that never exit). ACOS names the same split, so a
/// `GUEST_EXITED` after a stop can be told apart from a stop that had to kill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TeardownPath {
    /// The guest ended by itself after the `close` command, inside the grace
    /// window: it received its activity lifecycle and exited.
    Lifecycle,
    /// The grace expired and the session's process tree was killed. The
    /// lifecycle may have been delivered but not completed; nothing waits for
    /// app threads.
    Killed,
}

impl TeardownPath {
    pub const fn as_str(self) -> &'static str {
        match self {
            TeardownPath::Lifecycle => "lifecycle",
            TeardownPath::Killed => "killed",
        }
    }
}

/// Host-observed phases of one input command (W55 latency decomposition).
///
/// The engine can only time the parts it sees: how long the guest took to
/// answer the single `tap`/`key`/`type_text` line (which is where the guest's
/// own delivery + settle window + capture happen), how long the host spent
/// absorbing the reply (PNG read + tree parse), and the guest's own ack
/// latency. The remainder is the settle wait, named as a measured estimate
/// rather than split with numbers the guest does not publish.
#[derive(Debug, Clone, Default)]
pub struct InputTiming {
    /// Write of the command line to `acos-art` → its reply line.
    pub guest_ms: u64,
    /// Host side of the reply: PNG read + tree parse + signature.
    pub absorb_ms: u64,
    /// The guest's own input delivery ack (`latency_us`), when it reported one.
    pub ack_ms: u64,
    /// Wall time the host's own recovery loop spent, when the guest refused the
    /// frame (`0` when the guest settled it).
    pub host_recovery_ms: u64,
    /// Bound frame sequence before / after the command.
    pub frame_seq_before: u64,
    pub frame_seq_after: u64,
    /// Commands the engine issued itself to settle a refused input.
    pub recovery_polls: u64,
}

impl InputTiming {
    /// The settle estimate: guest time that is neither the guest's ack nor the
    /// host's absorb. It bounds nothing by itself; it is what is left over.
    pub const fn settle_estimate_ms(&self) -> u64 {
        self.guest_ms
            .saturating_sub(self.ack_ms)
            .saturating_sub(self.absorb_ms)
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "guest_command_ms": self.guest_ms,
            "absorb_ms": self.absorb_ms,
            "input_ack_ms": self.ack_ms,
            "host_recovery_ms": self.host_recovery_ms,
            "recovery_polls": self.recovery_polls,
            "settle_estimate_ms": self.settle_estimate_ms(),
            "frame_seq_before": self.frame_seq_before,
            "frame_seq_after": self.frame_seq_after,
            "how": "guest_command_ms is the whole tap/key/type_text round trip (guest \
                     delivery + its settle window + C1 capture); input_ack_ms is the guest's \
                     own delivery ack; absorb_ms is host PNG read + tree parse; \
                     settle_estimate_ms is the remainder",
        })
    }
}

/// Host-observed phases of one read-only command (tree / capture).
#[derive(Debug, Clone, Default)]
pub struct PhaseTiming {
    /// Write of the command line to `acos-art` -> its reply line.
    pub guest_ms: u64,
    /// Host side of the reply: PNG read + tree parse.
    pub absorb_ms: u64,
}

impl PhaseTiming {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "guest_command_ms": self.guest_ms,
            "absorb_ms": self.absorb_ms,
        })
    }
}

/// A dead session's own exit evidence.
///
/// `state` is what the host can see: the child exited (a `try_wait` status), the
/// session was closed by the host, or there is no child at all. The protocol
/// code is decided by [`crate::failure::classify_session_exit`] and, when the
/// launcher died before writing its record,
/// [`crate::failure::classify_child_exit`] (W53).
#[derive(Debug, Clone)]
pub struct GuestExit {
    pub state: String,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
}

impl GuestExit {
    fn from_status(state: &str, status: Option<std::process::ExitStatus>) -> Self {
        use std::os::unix::process::ExitStatusExt;
        match status {
            Some(status) => GuestExit {
                state: state.to_string(),
                exit_code: status.code(),
                signal: status.signal(),
            },
            None => GuestExit {
                state: state.to_string(),
                exit_code: None,
                signal: None,
            },
        }
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "state": self.state,
            "exit_code": self.exit_code,
            "signal": self.signal,
        })
    }
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
    fn to_json(
        &self,
        session_start_ms: u64,
        shutdown_ms: u64,
        frame_wait_secs: u64,
    ) -> serde_json::Value {
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

/// First token of a frame-artifact error detail, so an agent can tell "the
/// pixels could not be proven" from "there is no frame yet".
const FRAME_ARTIFACT_UNAVAILABLE: &str = "FRAME_ARTIFACT_UNAVAILABLE";

/// The digest a launcher reports for the frame file it just wrote.
///
/// P2: the launcher hashes the PNG while writing it, so the engine no longer
/// re-reads 9.5 MiB to hash it again. The claimed digest is only trusted when it
/// is a well-formed MD5 and the file on disk has exactly the claimed size; the
/// digest is proved again on load.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FrameDigest {
    md5: String,
    size: u64,
}

impl FrameDigest {
    /// Take the digest from a reply's `md5` + `png_bytes`, if both are usable.
    fn from_reply(reply: &serde_json::Value, md5: &str) -> Option<Self> {
        if md5.len() != 32 || !md5.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let size = reply.get("png_bytes").and_then(|v| v.as_u64())?;
        if size == 0 || size > MAX_PNG_BYTES {
            return None;
        }
        Some(FrameDigest {
            md5: md5.to_ascii_lowercase(),
            size,
        })
    }
}

fn frame_artifact_error(detail: String) -> BackendError {
    BackendError::Error {
        code: ErrorCode::RenderIo,
        detail: format!("{FRAME_ARTIFACT_UNAVAILABLE}: {detail}"),
    }
}

/// One frame's PNG as an immutable on-disk artifact, not as retained bytes.
///
/// `acos serve` keeps one bound frame per held session for the session's whole
/// life. Retaining the encoded bytes cost ~9.5 MiB/session (the 1080x2209 PNG;
/// D-M2 `daemon retention` and D-M6): this descriptor is what is retained
/// instead — where the bytes are, the SHA-256 they had when the frame was bound,
/// their size, the frame sequence they belong to and the display geometry the
/// frame was captured against (W58 display space).
///
/// [`Self::load`] reads the bytes only when a caller actually needs PNG data and
/// verifies the recorded hash and size first, so a missing or altered file is a
/// structured error (`FRAME_ARTIFACT_UNAVAILABLE`), never silently stale pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FrameArtifact {
    path: PathBuf,
    /// The content hash the launcher reported for exactly these bytes (the same
    /// MD5 `content_hash` publishes), or, when the reply carried no trustworthy
    /// digest, the one taken from the file here.
    content_md5: String,
    size: u64,
    /// The engine frame sequence this artifact is bound to.
    frame_seq: u64,
    /// Display width in pixels (W58 display space) at capture time.
    display_width: u32,
    /// Display height in pixels (W58 display space) at capture time.
    display_height: u32,
    /// Display density in dots per inch at capture time.
    density: u32,
}

impl FrameArtifact {
    /// Record a frame's PNG path, size and hash without retaining its bytes.
    ///
    /// The hash is taken once, at bind time, so a later load can prove the file
    /// is exactly the artifact the frame was bound to. The read is streamed:
    /// a full PNG buffer is never allocated here.
    fn bind(
        path: &Path,
        frame_seq: u64,
        display_width: u32,
        display_height: u32,
        density: u32,
    ) -> Result<Self, BackendError> {
        let (size, content_md5) = Self::hash_file(path)?;
        Self::described(
            path,
            frame_seq,
            display_width,
            display_height,
            density,
            content_md5,
            size,
        )
    }

    /// Bind an artifact whose digest the launcher already computed.
    ///
    /// P2: the launcher hashes the PNG while it writes it (one pass, in
    /// process) and reports the digest for those exact bytes over the same
    /// control channel that carries `frame_seq` and the frame file's path. That
    /// channel is the session's own child process, which the engine already
    /// trusts for pixel identity, so re-reading and re-hashing the 9.5 MiB file
    /// here bought nothing but 5.3 ms. The claimed size is checked against the
    /// file on disk now; the claimed digest is proved again on every load, so an
    /// altered or truncated artifact still cannot be served as a frame.
    fn bind_claimed(
        path: &Path,
        frame_seq: u64,
        display_width: u32,
        display_height: u32,
        density: u32,
        claimed: &FrameDigest,
    ) -> Result<Self, BackendError> {
        let meta = std::fs::metadata(path).map_err(|error| {
            frame_artifact_error(format!(
                "cannot stat the frame artifact {}: {error}",
                path.display()
            ))
        })?;
        if meta.len() != claimed.size {
            // The file is not the one the launcher described: fall back to
            // proving it ourselves rather than trusting either side.
            return Self::bind(path, frame_seq, display_width, display_height, density);
        }
        Self::described(
            path,
            frame_seq,
            display_width,
            display_height,
            density,
            claimed.md5.clone(),
            claimed.size,
        )
    }

    fn described(
        path: &Path,
        frame_seq: u64,
        display_width: u32,
        display_height: u32,
        density: u32,
        content_md5: String,
        size: u64,
    ) -> Result<Self, BackendError> {
        if size > MAX_PNG_BYTES {
            return Err(frame_artifact_error(format!(
                "the frame artifact {} is {size} bytes, over the {MAX_PNG_BYTES} byte ceiling",
                path.display()
            )));
        }
        Ok(FrameArtifact {
            path: path.to_path_buf(),
            content_md5,
            size,
            frame_seq,
            display_width,
            display_height,
            density,
        })
    }

    /// `(size, md5)` of the file's bytes, streamed in 1 MiB chunks.
    fn hash_file(path: &Path) -> Result<(u64, String), BackendError> {
        use std::io::Read as _;
        let mut file = std::fs::File::open(path).map_err(|error| {
            frame_artifact_error(format!(
                "cannot open the frame artifact {}: {error}",
                path.display()
            ))
        })?;
        let mut hasher = acos_render::md5::Md5::new();
        let mut buffer = vec![0u8; 1 << 20];
        let mut size = 0u64;
        loop {
            let read = file.read(&mut buffer).map_err(|error| {
                frame_artifact_error(format!(
                    "cannot hash the frame artifact {}: {error}",
                    path.display()
                ))
            })?;
            if read == 0 {
                break;
            }
            size += read as u64;
            if size > MAX_PNG_BYTES {
                return Err(frame_artifact_error(format!(
                    "the frame artifact {} is over the {MAX_PNG_BYTES} byte ceiling",
                    path.display()
                )));
            }
            hasher.update(&buffer[..read]);
        }
        Ok((size, hasher.finish_hex()))
    }

    /// Read the bytes now, after proving they are the bound artifact.
    fn load(&self) -> Result<Vec<u8>, BackendError> {
        let file = std::fs::File::open(&self.path).map_err(|error| {
            frame_artifact_error(format!(
                "the frame artifact {} is gone ({error}); refusing to serve pixels that cannot \
                 be proven",
                self.path.display()
            ))
        })?;
        let mut bytes = Vec::with_capacity(self.size.min(MAX_PNG_BYTES) as usize);
        file.take(MAX_PNG_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| {
                frame_artifact_error(format!(
                    "cannot read the frame artifact {}: {error}",
                    self.path.display()
                ))
            })?;
        if bytes.len() as u64 != self.size {
            return Err(frame_artifact_error(format!(
                "the frame artifact {} is {} bytes but was bound at {}; the file changed",
                self.path.display(),
                bytes.len(),
                self.size
            )));
        }
        let content_md5 = acos_render::md5::md5_hex(&bytes);
        if content_md5 != self.content_md5 {
            return Err(frame_artifact_error(format!(
                "the frame artifact {} hashes to {content_md5} but was bound at {}; refusing to \
                 serve altered pixels as frame {}",
                self.path.display(),
                self.content_md5,
                self.frame_seq
            )));
        }
        Ok(bytes)
    }
}

/// One fresh screenshot: its identity and the artifact file that holds it.
///
/// P2: the daemon answers a screenshot request with this instead of the bytes,
/// so the caller's copy is one kernel-side file copy of the artifact rather than
/// a 9.5 MiB read into the daemon and a 9.5 MiB write back out.
pub(crate) struct CapturedFrame {
    pub frame_seq: u64,
    pub previous_seq: u64,
    pub captured_at: String,
    pub md5: String,
    pub path: PathBuf,
    /// Size of the artifact on disk, in bytes.
    pub bytes: u64,
}

#[derive(Clone)]
struct CachedFrame {
    seq: u64,
    /// C1 store counter the reply carried (tree-only metadata or capture).
    buffer_generation: Option<u64>,
    /// Producer frame number the reply carried.
    frame_number: Option<u64>,
    md5: String,
    /// The PNG of this traversal as an artifact reference (never its bytes).
    png: Option<FrameArtifact>,
    tree: UiTree,
    /// Structural signature of `tree` (see [`tree_signature`]).
    signature: u64,
    captured_at_ms: u64,
    captured_at: String,
}

impl CachedFrame {
    /// Load this frame's PNG, verifying that the artifact is the bound one.
    fn png_bytes(&self) -> Result<Vec<u8>, BackendError> {
        let artifact = self.png.as_ref().ok_or_else(|| {
            live_frame_unavailable(
                "the ART tree reply carried no PNG; refusing to bind a tree to absent pixels",
            )
        })?;
        if artifact.frame_seq != self.seq {
            return Err(frame_artifact_error(format!(
                "frame {} is bound to a PNG artifact of frame {}; refusing a mismatched binding",
                self.seq, artifact.frame_seq
            )));
        }
        artifact.load()
    }

    fn evidence(&self, previous_seq: u64) -> Result<FrameEvidence, BackendError> {
        Ok(FrameEvidence {
            frame_seq: self.seq,
            previous_seq,
            captured_at_ms: self.captured_at_ms,
            captured_at: self.captured_at.clone(),
            md5: self.md5.clone(),
            png: self.png_bytes()?,
        })
    }

    fn observed(&self, previous_seq: u64) -> Result<ObservedFrame, BackendError> {
        Ok(ObservedFrame {
            tree: self.tree.clone(),
            frame: self.evidence(previous_seq)?,
        })
    }
}

/// Measure one node's display-to-window offset: `bounds_on_screen` minus the
/// window-local `bounds`. Returns `None` when either bound is missing/empty.
fn node_input_offset(node: &serde_json::Value) -> Option<(i32, i32)> {
    let window = node
        .get("bounds")
        .and_then(|value| value.as_str())
        .and_then(parse_bounds)?;
    let screen = node
        .get("bounds_on_screen")
        .and_then(|value| value.as_str())
        .and_then(parse_bounds)?;
    if screen.width() <= 0 || screen.height() <= 0 {
        return None;
    }
    Some((screen.left() - window.left(), screen.top() - window.top()))
}

/// Per-window display-to-window offsets from one probe tree.
///
/// Each window has its own input channel with its own origin, so the engine
/// must subtract the origin of the window that contains the target node. The
/// probe labels every node with `window`; a probe that predates the field maps
/// its single root under `""`. The root of each window carries both coordinate
/// spaces; the first node that has both decides that window's origin.
fn probe_window_offsets(tree: &serde_json::Value) -> HashMap<String, (i32, i32)> {
    fn walk(
        node: &serde_json::Value,
        inherited_window: &str,
        offsets: &mut HashMap<String, (i32, i32)>,
    ) {
        let window = node
            .get("window")
            .and_then(|value| value.as_str())
            .unwrap_or(inherited_window);
        if !offsets.contains_key(window) {
            if let Some(offset) = node_input_offset(node) {
                offsets.insert(window.to_string(), offset);
            }
        }
        if let Some(children) = node.get("children").and_then(|value| value.as_array()) {
            for child in children {
                walk(child, window, offsets);
            }
        }
    }
    let mut offsets = HashMap::new();
    match tree.get("nodes").and_then(|nodes| nodes.as_array()) {
        Some(nodes) => {
            for node in nodes {
                walk(node, "", &mut offsets);
            }
        }
        None => walk(tree, "", &mut offsets),
    }
    offsets
}

/// The first window's offset, in document order (the fallback for an input
/// that carries no window label).
fn probe_input_offset(tree: &serde_json::Value) -> Option<(i32, i32)> {
    match tree.get("nodes").and_then(|nodes| nodes.as_array()) {
        Some(nodes) => nodes.iter().find_map(node_input_offset),
        None => node_input_offset(tree),
    }
}

/// The offset to subtract for one window. An unknown or empty label falls back
/// to the first window's measured offset (a single-window tree, or an explicit
/// screen point on a pre-multi-window probe).
fn offset_for_window(
    offsets: &HashMap<String, (i32, i32)>,
    fallback: (i32, i32),
    window: &str,
) -> (i32, i32) {
    if window.is_empty() {
        return fallback;
    }
    offsets.get(window).copied().unwrap_or(fallback)
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
        settle: reply.get("settle").cloned(),
    }
}

/// P1: the guest's own fast-mode verdict, when it reported one. `None` means
/// strict mode, where the host's pixel+tree comparison remains authoritative.
fn settle_state_changed(reply: &serde_json::Value) -> Option<bool> {
    let settle = reply.get("settle")?;
    if settle.get("mode").and_then(|v| v.as_str()) != Some("fast") {
        return None;
    }
    settle.get("state_changed").and_then(|v| v.as_bool())
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
        (Some((md5, signature)), Some(frame)) => frame.md5 != *md5 && frame.signature != *signature,
        // No baseline: never claim that nothing changed.
        _ => true,
    }
}

/// The one thread that is allowed to be the *parent thread* of a guest.
///
/// GL1: `PR_SET_PDEATHSIG` (set in [`ArtSession::start`]'s `pre_exec`) is
/// delivered when the parent **thread** exits, not when the parent process
/// exits — `PR_SET_PDEATHSIG` is per-thread state inherited across `fork`, and
/// the kernel signals the child from `exit_notify()` of the exiting task. The
/// daemon created ART sessions on the *per-connection* thread (`serve.rs`), so a
/// kept guest (`acos launch --keep`) was SIGKILLed the instant the launching
/// client disconnected: `acos session status/tap/observe` from another process
/// then answered `GUEST_EXITED` (signal 9). Spawning through this thread makes
/// the parent thread live exactly as long as the process, so the death signal
/// still means "the `acos` process really died" and nothing weaker.
///
/// The thread is created once per process and never exits; a guest whose parent
/// thread is gone would be killed by the kernel (that is the guarantee we are
/// keeping), so "never exits" is not an optimisation, it is the contract.
mod spawner {
    use std::process::{Child, Command};
    use std::sync::mpsc::channel;
    use std::sync::OnceLock;

    type Job = Box<dyn FnOnce() + Send + 'static>;

    fn job_sender() -> &'static std::sync::mpsc::Sender<Job> {
        static SENDER: OnceLock<std::sync::mpsc::Sender<Job>> = OnceLock::new();
        SENDER.get_or_init(|| {
            let (sender, receiver) = channel::<Job>();
            std::thread::Builder::new()
                .name("acos-guest-spawner".to_string())
                .spawn(move || {
                    // `SENDER` is a process-lifetime static, so this receiver
                    // never sees a disconnect: the thread outlives every call.
                    while let Ok(job) = receiver.recv() {
                        job();
                    }
                })
                .expect("spawn the acos-guest-spawner thread");
            sender
        })
    }

    /// `Command::spawn` executed on the long-lived spawner thread.
    ///
    /// The child's parent *thread* is therefore the spawner, not whichever
    /// thread (per-connection, CLI, verify) asked for it. Everything else about
    /// the spawn — the process group, the `pre_exec` hooks and their
    /// `PR_SET_PDEATHSIG` — is unchanged.
    pub fn spawn(command: Command) -> std::io::Result<Child> {
        let (reply_tx, reply_rx) = channel::<std::io::Result<Child>>();
        let job: Job = Box::new(move || {
            let mut command = command;
            let _ = reply_tx.send(command.spawn());
        });
        job_sender()
            .send(job)
            .map_err(|_| std::io::Error::other("the acos-guest-spawner thread is gone"))?;
        reply_rx
            .recv()
            .map_err(|_| std::io::Error::other("the acos-guest-spawner thread did not answer"))?
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

/// D-M6 (trace-only, env-gated): append one JSON line describing a host-side
/// stop phase. Off unless `DM6_RESET_TRACE` names a file. Never changes a reply,
/// an error or a control-flow decision.
fn dm6_trace(event: serde_json::Value) {
    if let Some(path) = std::env::var_os("DM6_RESET_TRACE") {
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(file, "{event}");
        }
    }
}

/// W7: the process's requested app-window inset model, as the env spelling.
#[must_use]
pub fn insets_mode_from_env() -> &'static str {
    match acos_services::wms::InsetsMode::from_env(acos_services::wms::DEFAULT_INSETS_MODE) {
        acos_services::wms::InsetsMode::Report => "report",
        acos_services::wms::InsetsMode::Carve => "carve",
    }
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
        Self::start_with_animations(apk, artifacts, "on")
    }

    /// P12: start a session with an explicit animation choice (`"on"` = real
    /// framework behaviour, `"off"` = the three Android animation scales at 0).
    /// The choice is launch-time: it is installed before the guest's first
    /// traversal and is stated in every reply as `animations`.
    pub fn start_with_animations(
        apk: &Path,
        artifacts: &Path,
        animations: &str,
    ) -> Result<ArtSession, BackendError> {
        let insets = std::env::var("ACOS_WM_INSETS").unwrap_or_else(|_| {
            acos_services::wms::DEFAULT_INSETS_MODE.as_str().to_string()
        });
        Self::start_with_options(apk, artifacts, animations, &insets)
    }

    /// W7: start a session with an explicit app-window inset model.
    /// `insets` is `carve` (pre-W7 carved frame) or `report` (AOSP full-display
    /// frame plus a reported `InsetsState`). It is a launch-time choice and is
    /// stated in every reply, like `animations`.
    pub fn start_with_options(
        apk: &Path,
        artifacts: &Path,
        animations: &str,
        insets: &str,
    ) -> Result<ArtSession, BackendError> {
        let insets: &'static str = match acos_services::wms::parse_insets_mode(insets) {
            Some(acos_services::wms::InsetsMode::Carve) => "carve",
            Some(acos_services::wms::InsetsMode::Report) => "report",
            None => {
                return Err(BackendError::Failed {
                    code: acos_protocol::ErrorCode::InvalidArgument,
                    detail: format!("insets must be \"carve\" or \"report\" (got {insets:?})"),
                })
            }
        };
        let animations: &'static str = match animations.trim().to_ascii_lowercase().as_str() {
            "on" => "on",
            "off" => "off",
            other => {
                return Err(BackendError::Failed {
                    code: acos_protocol::ErrorCode::InvalidArgument,
                    detail: format!("animations must be \"on\" or \"off\" (got {other:?})"),
                })
            }
        };
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
        // A previous guest in this artifacts directory may have left its own
        // `session-exit.json`. That record describes a *different* process and
        // must never classify this one (W55): a relaunch (`reset`, or a serve
        // relaunch after a death) reuses the directory, so the stale record is
        // removed before the new guest can die silently and inherit it.
        let _ = std::fs::remove_file(crate::failure::session_exit_path(artifacts));
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
            .arg("--animations")
            .arg(animations)
            .arg("--insets")
            .arg(insets)
            // The launcher reads this seam (no `acos-art` CLI flag change): a
            // live session instead of a one-shot launch. The guest publishes
            // the read-only view-tree probe the `tree`/`tap` replies carry.
            .env("ACOSLAUNCH_INTERACTIVE", "1")
            .env("ACOSLAUNCH_LIVE_PROBE", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Display geometry. The defaults are the API-34 emulator profile the
        // existing baselines were captured on; `ACOS_ART_*` lets a caller match
        // a *reference device* exactly (1080x2400 @ 400 dpi with a 108 px status
        // inset and no navigation bar, for the owner's Moto G35). The guest
        // reports back what it actually used in its ready line, and every reply
        // carries that.
        //
        // `ACOS_DISPLAY_OVERRIDE` is the same spec the WMS shim reads
        // (`acos-services::display`), so the window frame, the insets the app
        // sees and the DisplayInfo reply all come from one source instead of
        // the guest's reference constants.
        for (var, flag) in [
            ("ACOS_ART_WIDTH", "--width"),
            ("ACOS_ART_PANEL_HEIGHT", "--panel-height"),
            ("ACOS_ART_DENSITY", "--density"),
            ("ACOS_ART_STATUS_INSET", "--status-inset"),
            ("ACOS_ART_NAV_INSET", "--nav-inset"),
        ] {
            if let Ok(value) = std::env::var(var) {
                command.arg(flag).arg(value);
            }
        }
        if let (Ok(w), Ok(h), Ok(dpi), Ok(top), Ok(bottom)) = (
            std::env::var("ACOS_ART_WIDTH"),
            std::env::var("ACOS_ART_PANEL_HEIGHT"),
            std::env::var("ACOS_ART_DENSITY"),
            std::env::var("ACOS_ART_STATUS_INSET"),
            std::env::var("ACOS_ART_NAV_INSET"),
        ) {
            command.env("ACOS_DISPLAY_OVERRIDE", format!("{w}x{h}@{dpi}/{top}/{bottom}"));
        }
        if std::env::var_os("ACOSLAUNCH_FRAME_WAIT_SECS").is_none() {
            command.env("ACOSLAUNCH_FRAME_WAIT_SECS", frame_wait_secs.to_string());
        }
        // Own process group: a `close` that does not take must not leave ART or
        // its sandbox behind.
        command.process_group(0);
        // W50: the guest must not outlive the daemon. `acos serve` can be
        // SIGKILLed (benchmark cleanup, harness crash); with the parent-death
        // signal the launcher dies with it, and the launcher's own guest child
        // carries the same signal (app.rs), so the whole tree goes. This is a
        // backstop, not the teardown path: `close`/`teardown` still reap the
        // process group and prove it empty.
        //
        // GL1: the signal is per parent **thread**, so the spawn itself is
        // handed to the process-lifetime `spawner` thread below. Spawning on
        // the caller's thread made the daemon's per-connection thread the
        // parent, and a kept session died the moment its launching client
        // disconnected.
        unsafe {
            command.pre_exec(|| {
                libc::prctl(
                    libc::PR_SET_PDEATHSIG,
                    libc::SIGKILL as libc::c_ulong,
                    0,
                    0,
                    0,
                );
                Ok(())
            });
        }
        let spawned_at = Instant::now();
        // GL1: spawned from the long-lived spawner thread, so `PDEATHSIG` above
        // fires when this *process* dies, not when the calling thread ends.
        let mut child = spawner::spawn(command).map_err(|error| BackendError::Unsupported {
            capability: "engine.art".into(),
            detail: format!(
                "cannot spawn {} ({}): {error}. Install/build acos-art or set ACOS_ART_BIN",
                binary.display(),
                "the ART session engine is unavailable on this host"
            ),
        })?;
        let _pid = child.id();
        dm6_trace(serde_json::json!({
            "event": "spawn",
            "session": artifacts.display().to_string(),
            "pid": _pid,
            "process_group": _pid,
            "unix_ms": now_unix_ms(),
        }));
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
            last_pixel: None,
            seen_seq: 0,
            seq_offset: 0,
            log_offset: 0,
            logs_total_raw: 0,
            crash: None,
            process_group: Some(_pid as i32),
            last_input: None,
            input_offset: (0, 0),
            window_offsets: HashMap::new(),
            session_start_ms: 0,
            frame_wait_secs,
            settle_budget,
            settle_polls,
            input_baseline: None,
            settle: SettleStats::default(),
            last_input_timing: None,
            last_phase: None,
            last_input_settle: None,
            settle_mode: "strict",
            last_tree_settle: None,
            reset_settle: "strict",
            reset_pixels: true,
            insets,
            animations,
            settle_timeout_ms: None,
            reset_settle_timeout_ms: None,
            closed: false,
            last_teardown: None,
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
                BackendError::Protocol {
                    code,
                    detail,
                    hint,
                    details,
                } => BackendError::Protocol {
                    code,
                    detail: format!(
                        "{LIVE_FRAME_UNAVAILABLE}: the ART session ended before its ready line \
                         (no live frame exists): {detail}"
                    ),
                    hint,
                    details,
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
                detail: format!(
                    "acos-art ready line is not JSON: {error}; stderr tail: {}",
                    session.stderr_tail()
                ),
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
            // W46: the ready line names the cause (`HOST_DISK_FULL`, ...). Map it
            // to its protocol code and keep the session's own exit record. Folding
            // every refusal into `UNSUPPORTED_BACKEND` hid a full disk and a seccomp
            // refusal behind the same label.
            let mut error = crate::failure::art_error(code, message, &artifacts);
            if let Some(object) = error.details.as_mut().and_then(|d| d.as_object_mut()) {
                object.insert(
                    "stderr_tail".to_string(),
                    serde_json::json!(session.stderr_tail()),
                );
            }
            return Err(BackendError::from_protocol(error));
        }
        if let Some(width) = ready.get("width").and_then(|v| v.as_u64()) {
            session.width = width as u32;
        }
        if let Some(height) = ready.get("height").and_then(|v| v.as_u64()) {
            session.height = height as u32;
        }
        if let Some(density) = ready.get("density").and_then(|v| v.as_u64()) {
            session.density = density as u32;
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

    /// The bound frame's identity: `(frame_seq, md5, captured_at)`.
    ///
    /// Cheap (no PNG read or clone). `None` before the first completed traversal.
    pub fn frame_info(&self) -> Option<(u64, String, String)> {
        self.cached
            .as_ref()
            .map(|frame| (frame.seq, frame.md5.clone(), frame.captured_at.clone()))
    }

    /// Cheap C1 frame identity carried by the newest observation (a tree-only
    /// reply has no pixels but still says which stored buffer it belongs to).
    pub fn frame_identity_meta(&self) -> Option<(u64, Option<u64>, Option<u64>)> {
        self.cached
            .as_ref()
            .map(|frame| (frame.seq, frame.buffer_generation, frame.frame_number))
    }

    /// The most recent completed frame, loading its PNG artifact on demand.
    ///
    /// Unlike the trait's `last_frame`, this reports a missing or altered
    /// artifact as a structured `FRAME_ARTIFACT_UNAVAILABLE` error instead of
    /// folding it into "no frame yet". No command is issued.
    pub fn bound_frame(&self) -> Result<Option<ObservedFrame>, BackendError> {
        // W55 invariant: a dead session never serves a cached frame, even one
        // whose artifact is still on disk. Only metadata (`frame_info`) survives
        // a close; pixels and tree do not.
        if self.closed {
            return Ok(None);
        }
        let Some(cached) = self.last_pixel.as_ref() else {
            return Ok(None);
        };
        let previous = self.seen_seq.min(cached.seq.saturating_sub(1));
        cached.observed(previous).map(Some)
    }

    /// The bound tree of the most recent completed traversal, without issuing a
    /// command.
    #[allow(dead_code)]
    pub fn bound_tree(&self) -> Option<UiTree> {
        self.cached.as_ref().map(|frame| frame.tree.clone())
    }

    /// Ensure a bound tree exists: reuse the last completed traversal, or take
    /// one now if the session has not completed any. Never invents a frame.
    pub(crate) fn ensure_bound_tree(&mut self) -> Result<UiTree, BackendError> {
        self.require_cached_tree()
    }

    /// The launched package (empty before the ready line).
    pub fn package(&self) -> &str {
        &self.package
    }

    /// The launched Activity, when the guest reported one.
    pub fn activity(&self) -> Option<&str> {
        self.activity.as_deref()
    }

    /// The APK this session runs.
    pub fn apk_path(&self) -> &Path {
        &self.apk
    }

    /// Host-observed phase timings of the most recent input command.
    pub fn last_input_timing(&self) -> Option<&InputTiming> {
        self.last_input_timing.as_ref()
    }

    /// P3b: the guest's settle block for the most recent tree request.
    pub fn last_tree_settle(&self) -> Option<&serde_json::Value> {
        self.last_tree_settle.as_ref()
    }

    /// P1: set the settle mode for the next input command (`fast` navigation or
    /// the default `strict`). Serve owns the per-verb choice; assertions never
    /// call this with `fast`.
    pub fn set_settle_mode(&mut self, mode: &'static str) {
        self.settle_mode = if mode == "fast" { "fast" } else { "strict" };
    }

    /// P12b: the per-request settle deadline (`settle_timeout_ms`). `None` keeps
    /// the engine default. A timeout is still an observation: the guest returns
    /// the latest tree and frame identity with `settled:false`.
    pub fn set_settle_timeout(&mut self, timeout_ms: Option<u64>) {
        self.settle_timeout_ms = timeout_ms;
    }

    /// P12: the animation choice this session was launched with (`"on"`/`"off"`).
    pub fn animations(&self) -> &'static str {
        self.animations
    }

    /// W7: the app-window inset model this guest was launched with.
    pub fn insets(&self) -> &'static str {
        self.insets
    }

    /// P1: the guest's settle block for the most recent input, verbatim.
    pub fn last_input_settle(&self) -> Option<&serde_json::Value> {
        self.last_input_settle.as_ref()
    }

    /// Host-observed phase timings of the most recent tree/capture command.
    pub fn last_phase_timing(&self) -> Option<&PhaseTiming> {
        self.last_phase.as_ref()
    }

    /// Is the guest still alive? `Some(exit)` means it is not, and carries what
    /// the OS reports. Never blocks.
    ///
    /// This is the guard a daemon needs before it answers any verb from cached
    /// state: `cached` is a frame of a process that may no longer exist. A
    /// session the host closed counts as dead even if its pid lingers, because
    /// nothing can be served from it any more.
    pub fn liveness(&mut self) -> Option<GuestExit> {
        if let Some(child) = self.child.as_mut() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let exit = GuestExit::from_status("exited", Some(status));
                    dm6_trace(serde_json::json!({
                        "event": "liveness.dead",
                        "session": self.artifacts.display().to_string(),
                        "pid": child.id(),
                        "process_group": self.process_group,
                        "state": exit.state,
                        "exit_code": exit.exit_code,
                        "signal": exit.signal,
                        "unix_ms": now_unix_ms(),
                    }));
                    return Some(exit);
                }
                Ok(None) => {
                    if self.closed {
                        return Some(GuestExit::from_status("closed", None));
                    }
                    return None;
                }
                Err(error) => {
                    return Some(GuestExit {
                        state: format!("wait-error: {error}"),
                        exit_code: None,
                        signal: None,
                    })
                }
            }
        }
        Some(GuestExit::from_status(
            if self.closed { "closed" } else { "no-child" },
            None,
        ))
    }

    /// Drop the bound frame and report what was dropped.
    ///
    /// Called when a session is known dead: a cached frame from a dead guest is
    /// exactly the stale evidence the agent must never receive. The returned
    /// tuple is the discarded frame's identity, kept for the error details.
    pub fn invalidate_frame(&mut self) -> Option<(u64, String, String)> {
        self.last_pixel = None;
        let cached = self.cached.take()?;
        Some((cached.seq, cached.md5, cached.captured_at))
    }

    /// The art child's exit status, if it has already exited (never blocks).
    fn child_exit_status(&mut self) -> Option<std::process::ExitStatus> {
        self.child.as_mut().and_then(|c| match c.try_wait() {
            Ok(Some(status)) => Some(status),
            _ => None,
        })
    }

    /// The display width in pixels.
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// The display height in pixels.
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// The display density (dpi).
    #[allow(dead_code)]
    pub const fn density(&self) -> u32 {
        self.density
    }

    /// Press a raw Android keycode (the protocol takes integers).
    ///
    /// `key()` maps symbolic names; a socket caller may send `{"code": 66}`
    /// directly, so the numeric path must exist too.
    pub fn key_code_i32(&mut self, code: i32) -> Result<(), BackendError> {
        if code < 0 {
            return Err(BackendError::unsupported(
                "input.key",
                format!("invalid keycode {code}"),
            ));
        }
        self.input_command(serde_json::json!({"cmd":"key","code":code}), "input.key")
    }

    fn stderr_tail(&self) -> String {
        let tail = self.stderr_tail.lock().unwrap_or_else(|e| e.into_inner());
        let bytes: Vec<u8> = tail.iter().copied().collect();
        String::from_utf8_lossy(&bytes).trim().to_string()
    }

    fn read_line(&mut self, timeout: Duration, context: &str) -> Result<String, BackendError> {
        match self.lines.recv_timeout(timeout) {
            Ok(Ok(line)) => Ok(line),
            Ok(Err(error)) => Err(self.session_lost_mut(context, &error.to_string())),
            Err(RecvTimeoutError::Timeout) => Err(BackendError::Timeout {
                detail: format!(
                    "{context}: no reply from acos-art within {}s; stderr tail: {}",
                    timeout.as_secs(),
                    self.stderr_tail()
                ),
            }),
            Err(RecvTimeoutError::Disconnected) => {
                Err(self.session_lost_mut(context, "acos-art session exited"))
            }
        }
    }

    /// A lost session is classified from its own exit record when there is one
    /// (W46): a full disk, a panic, an OOM kill or a guest exit is a known cause
    /// and must never surface as `INTERNAL: session exited`.
    ///
    /// When the launcher died before it could write `session-exit.json`, the
    /// engine still holds its `Child`, so the child's own exit status is the
    /// evidence (W53): a SIGKILLed launcher is `GUEST_EXITED` with
    /// `details.session_exit.guest_signal`, never `INTERNAL`.
    fn session_lost_mut(&mut self, context: &str, detail: &str) -> BackendError {
        if let Some((code, facts)) = crate::failure::classify_session_exit(&self.artifacts) {
            return BackendError::Protocol {
                code,
                detail: format!(
                    "{context}: {detail} (classified from {}); stderr tail: {}",
                    crate::failure::session_exit_path(&self.artifacts).display(),
                    self.stderr_tail()
                ),
                hint: None,
                details: Some(serde_json::json!({"session_exit": facts})),
            };
        }
        if let Some((code, facts)) = self
            .child_exit_status()
            .and_then(crate::failure::classify_child_exit)
        {
            return BackendError::Protocol {
                code,
                detail: format!(
                    "{context}: {detail} (no {}; the acos-art child's own exit status was \
                     classified instead); stderr tail: {}",
                    crate::failure::session_exit_path(&self.artifacts).display(),
                    self.stderr_tail()
                ),
                hint: Some(
                    "The launcher died before it could write session-exit.json. The signal or \
                     exit code in details.session_exit is the evidence; check the host kill \
                     paths (memcg OOM, the launcher RSS watchdog, guarded.sh's wall clock) and \
                     the session's logcat/live-session.json."
                        .to_string(),
                ),
                details: Some(serde_json::json!({"session_exit": facts})),
            };
        }
        BackendError::Error {
            code: acos_protocol::ErrorCode::Internal,
            detail: format!("{context}: {detail}; stderr tail: {}", self.stderr_tail()),
        }
    }

    /// Send one command and keep the raw failure reply. The engine needs that
    /// reply for one case only: a delivered input whose action legitimately
    /// produced no pixel change. The guest reports the delivery evidence
    /// (`gesture` acks) in the same reply it uses to refuse the frame.
    ///
    /// A failure reply carries the mapped protocol code, its class/retryability
    /// and the raw reply in `details.reply` (W46).
    fn command_raw(
        &mut self,
        request: serde_json::Value,
    ) -> Result<serde_json::Value, acos_protocol::ProtocolError> {
        let artifacts = self.artifacts.clone();
        if self.stdin.is_none() {
            // A session the host closed (or one whose guest was reaped) has
            // nothing to answer with. Classify it from the guest's own evidence
            // instead of reporting a bare harness INTERNAL (W53/W55).
            return Err(self
                .session_lost_mut("command", "the ART session stdin is closed")
                .to_protocol_error());
        }
        let stdin = self.stdin.as_mut().expect("checked above");
        let line = request.to_string();
        let cmd = request
            .get("cmd")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string();
        let t_write = m5_now_us();
        if p3_enabled() && cmd == "tree" {
            p3_mark("cli", "cli.cmd_request", &format!("bytes={}", line.len()));
        }
        if let Err(error) = writeln!(stdin, "{line}").and_then(|_| stdin.flush()) {
            if crate::failure::is_disk_full(&error) {
                return Err(acos_protocol::ProtocolError::new(
                    ErrorCode::HostDiskFull,
                    format!("cannot write to acos-art: {error}"),
                ));
            }
            // EPIPE/ENOTCONN: the session is already gone. Classify it exactly
            // like a disconnected read (W53): a launcher death is never
            // `INTERNAL`, and a disk-full/write failure stays its own code.
            self.stdin = None;
            let detail = format!("cannot write to acos-art: {error}");
            return Err(self
                .session_lost_mut("command", &detail)
                .to_protocol_error());
        }
        let t_written = m5_now_us();
        m5_trace(
            "cli.art_cmd_write",
            t_write,
            t_written,
            &format!("cmd={cmd} bytes={}", line.len()),
        );
        if p3_enabled() && cmd == "tree" {
            p3_trace(
                "cli",
                "cli.cmd_write",
                t_write,
                t_written,
                &format!("bytes={}", line.len()),
            );
        }
        let timeout = self.timeout;
        let reply = self
            .read_line(timeout, "command")
            .map_err(|error| error.to_protocol_error())?;
        let t_reply = m5_now_us();
        m5_trace(
            "cli.art_cmd_wait",
            t_written,
            t_reply,
            &format!("cmd={cmd}"),
        );
        m5_trace(
            "cli.art_cmd_roundtrip",
            t_write,
            t_reply,
            &format!("cmd={cmd} reply_bytes={}", reply.len()),
        );
        if cmd == "tree" && p3_enabled() {
            p3_trace(
                "cli",
                "cli.cmd_wait",
                t_written,
                t_reply,
                &format!("reply_bytes={}", reply.len()),
            );
        }
        let reply: serde_json::Value = serde_json::from_str(&reply).map_err(|error| {
            acos_protocol::ProtocolError::new(
                ErrorCode::Internal,
                format!("acos-art reply is not JSON: {error}"),
            )
            .with_hint(
                "The line-JSON protocol is broken (a guest or harness wrote a non-JSON line). \
                 Capture the session's stderr tail and report the harness bug.",
            )
        })?;
        let t_parsed = m5_now_us();
        if cmd == "tree" && p3_enabled() {
            p3_trace(
                "cli",
                "cli.reply_json_parse",
                t_reply,
                t_parsed,
                &format!("reply_bytes={}", reply.to_string().len()),
            );
        }
        if reply.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            let code = reply
                .get("code")
                .and_then(|v| v.as_str())
                .unwrap_or("COMMAND_FAILED");
            let message = reply
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("acos-art session command failed");
            let mut error = crate::failure::art_error(code, message, &artifacts);
            if let Some(object) = error.details.as_mut().and_then(|d| d.as_object_mut()) {
                object.insert("reply".to_string(), reply.clone());
            }
            return Err(error);
        }
        Ok(reply)
    }

    fn command(&mut self, request: serde_json::Value) -> Result<serde_json::Value, BackendError> {
        self.command_raw(request)
            .map_err(BackendError::from_protocol)
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
        let mut request = request;
        if let Some(object) = request.as_object_mut() {
            object.insert("settle".into(), serde_json::json!(self.settle_mode));
            if let Some(timeout) = self.settle_timeout_ms {
                object.insert("settle_timeout_ms".into(), serde_json::json!(timeout));
            }
        }
        self.last_input_settle = None;
        let started = Instant::now();
        let mut timing = InputTiming {
            frame_seq_before: self.cached.as_ref().map(|frame| frame.seq).unwrap_or(0),
            ..InputTiming::default()
        };
        let outcome = self.input_command_inner(request, context, started, &mut timing);
        timing.frame_seq_after = self.cached.as_ref().map(|frame| frame.seq).unwrap_or(0);
        // The guest's own delivery ack (host-measured round trip, guest-reported
        // latency) is what separates "input delivery" from "settle" in the
        // reply's phase evidence.
        timing.ack_ms = if outcome.is_ok() {
            self.last_input
                .as_ref()
                .map(|delivery| {
                    delivery
                        .acks
                        .iter()
                        .filter_map(|ack| ack.latency_us)
                        .map(|us| us / 1000)
                        .max()
                        .unwrap_or(0)
                })
                .unwrap_or(0)
        } else {
            0
        };
        self.last_input_timing = Some(timing);
        outcome
    }

    fn input_command_inner(
        &mut self,
        request: serde_json::Value,
        context: &str,
        started: Instant,
        timing: &mut InputTiming,
    ) -> Result<(), BackendError> {
        match self.command_raw(request) {
            Ok(reply) => {
                timing.guest_ms = started.elapsed().as_millis() as u64;
                self.settle.guest += 1;
                let baseline = self.input_baseline.clone();
                let absorb_started = Instant::now();
                self.absorb(&reply)?;
                timing.absorb_ms = absorb_started.elapsed().as_millis() as u64;
                // Compare exactly the pair the executor's `changed_from` will
                // compare: this action's frame against the frame it was handed
                // at the previous step (snapshotted by `begin_input` before the
                // point-resolution traversal). Both verdicts then agree by
                // construction: equal hashes are "delivered, no visual change",
                // different hashes are a state change the executor also sees.
                let mut visual_change = state_changed(baseline.as_ref(), self.cached.as_ref());
                // P1: in fast mode the guest bound a probe sample that differs
                // from the pre-action tree (or explicitly fell back without a
                // change). That verdict wins so a mid-animation frame is never
                // reported as a state change the tree did not show.
                self.last_input_settle = reply.get("settle").cloned();
                if let Some(changed) = settle_state_changed(&reply) {
                    visual_change = changed;
                }
                self.last_input = Some(delivery(&reply, visual_change));
                Ok(())
            }
            Err(error) if error.code == ErrorCode::NoFreshFrame => {
                timing.guest_ms = started.elapsed().as_millis() as u64;
                // Recover the guest's delivery evidence from the structured reply.
                let reply = error
                    .details
                    .as_ref()
                    .and_then(|details| details.get("reply"))
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                if reply.get("gesture").is_none() {
                    return Err(BackendError::from_protocol(error));
                }
                let recovery_started = Instant::now();
                let polls_before = self.settle.recovery_polls;
                if std::env::var_os("ACOS_ART_SETTLE_MODE").as_deref()
                    == Some("guest-only".as_ref())
                {
                    // Diagnostic: the pre-W34 behaviour (one recovery capture,
                    // `visual_change: false`), for A/B cost runs.
                    let previous = self.seen_seq;
                    self.request_tree()?;
                    timing.recovery_polls = 1;
                    if let Some(cached) = self.cached.as_ref() {
                        if cached.seq <= previous {
                            timing.host_recovery_ms = recovery_started.elapsed().as_millis() as u64;
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
                    self.last_input_settle = reply.get("settle").cloned();
                    self.last_input = Some(delivery(&reply, false));
                } else {
                    self.settle_after_input(&reply, context)?;
                    timing.recovery_polls = self.settle.recovery_polls - polls_before;
                }
                timing.host_recovery_ms = recovery_started.elapsed().as_millis() as u64;
                Ok(())
            }
            Err(error) => Err(BackendError::from_protocol(error)),
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
        let baseline = self.input_baseline.clone().or_else(|| {
            self.cached
                .as_ref()
                .map(|frame| (frame.md5.clone(), frame.signature))
        });
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
        if self.last_input_settle.is_none() {
            self.last_input_settle = reply.get("settle").cloned();
        }
        self.last_input = Some(InputDelivery {
            delivered: true,
            visual_change,
            acks: acks_from_reply(reply),
            settle: reply.get("settle").cloned(),
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

    /// Absorb a reply's frame: sequence, PNG artifact and tree, all or nothing.
    ///
    /// A reply that carries a `frame_seq` also carries that frame's PNG; a tree
    /// reply carries the tree as well. A tree-less reply (a pure `screenshot`)
    /// only advances `seen_seq`; the bound tree keeps the sequence it was
    /// captured with, so a tree can never be bound to newer pixels.
    ///
    /// The PNG is recorded as an immutable artifact reference (path, content
    /// hash, size, sequence, W58 display geometry), never as retained bytes: the
    /// daemon holds one bound frame per session, and a 1080x2209 PNG is ~9.5 MiB
    /// (D-M2/D-M6).
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
        let digest = FrameDigest::from_reply(reply, &md5);
        let buffer_generation = reply.get("buffer_generation").and_then(|v| v.as_u64());
        let frame_number = reply.get("frame_number").and_then(|v| v.as_u64());
        let png = match reply.get("png").and_then(|v| v.as_str()) {
            Some(path) => Some(match digest {
                Some(claimed) => FrameArtifact::bind_claimed(
                    Path::new(path),
                    seq,
                    self.width,
                    self.height,
                    self.density,
                    &claimed,
                ),
                None => {
                    FrameArtifact::bind(Path::new(path), seq, self.width, self.height, self.density)
                }
            }?),
            None => None,
        };
        self.seen_seq = self.seen_seq.max(seq);
        let Some(tree_value) = reply.get("tree") else {
            // No tree in this reply (for example `key` or `screenshot`); keep the
            // cached tree only for the frame it was bound to.
            if let Some(cached) = self.cached.as_mut() {
                if cached.seq == seq {
                    if let Some(artifact) = png {
                        cached.png = Some(artifact);
                        if !md5.is_empty() {
                            cached.md5 = md5;
                        }
                        cached.captured_at_ms = now_unix_ms();
                        cached.captured_at = rfc3339(cached.captured_at_ms);
                    }
                }
            }
            return Ok(());
        };
        let offsets = probe_window_offsets(tree_value);
        if !offsets.is_empty() {
            if let Some(offset) = probe_input_offset(tree_value) {
                self.input_offset = offset;
            }
            self.window_offsets = offsets;
        }
        let tree = {
            let t_tree = m5_now_us();
            let tree = ui_tree_from_json(&self.package, self.frame_meta(seq, &md5), tree_value)?;
            if p3_enabled() {
                p3_trace(
                    "cli",
                    "cli.tree_parse",
                    t_tree,
                    m5_now_us(),
                    &format!("frame_seq={seq}"),
                );
            }
            tree
        };
        let signature = tree_signature(&tree);
        let captured_at_ms = now_unix_ms();
        let frame = CachedFrame {
            seq,
            buffer_generation,
            frame_number,
            md5,
            png,
            tree,
            signature,
            captured_at_ms,
            captured_at: rfc3339(captured_at_ms),
        };
        if frame.png.is_some() {
            self.last_pixel = Some(frame.clone());
        }
        self.cached = Some(frame);
        Ok(())
    }

    /// Take a fresh screenshot and return its identity plus where its PNG
    /// already is, without reading the bytes.
    ///
    /// P2: a caller that only needs the file (a daemon writing it to the
    /// caller's path) can copy the artifact instead of reading 9.5 MiB into the
    /// daemon and writing it out again. Freshness is unchanged: the frame's
    /// `frame_seq` must advance, and the sequence is only advanced once the
    /// reply's frame is fully described.
    pub(crate) fn capture_frame(&mut self) -> Result<CapturedFrame, BackendError> {
        // A pure capture command: no tree is returned, so the bound tree keeps
        // its own sequence. Freshness is proven by the sequence advancing.
        let previous = self.seen_seq;
        let started = Instant::now();
        let reply = self.command(serde_json::json!({"cmd":"screenshot"}))?;
        let mut phase = PhaseTiming {
            guest_ms: started.elapsed().as_millis() as u64,
            absorb_ms: 0,
        };
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
        let path = PathBuf::from(png_path);
        let bytes = std::fs::metadata(&path)
            .map_err(|error| {
                live_frame_unavailable(format!(
                    "the ART screenshot frame {} cannot be stat'ed: {error}",
                    path.display()
                ))
            })?
            .len();
        if bytes == 0 {
            return Err(live_frame_unavailable(
                "the ART screenshot frame file is empty",
            ));
        }
        phase.absorb_ms = started.elapsed().as_millis() as u64;
        self.last_phase = Some(phase);
        self.seen_seq = seq;
        let captured_at_ms = now_unix_ms();
        Ok(CapturedFrame {
            frame_seq: seq,
            previous_seq: previous,
            captured_at: rfc3339(captured_at_ms),
            md5,
            path,
            bytes,
        })
    }

    /// Ask for the live tree. The `tree` command re-traverses, draws and
    /// publishes a new frame, so the reply's tree and PNG share one `frame_seq`.
    fn request_tree(&mut self) -> Result<(), BackendError> {
        self.request_tree_with("strict", true)
    }

    /// P3b tree-only request: no pixel clone/encode/md5/write, and the reply is
    /// not required to carry a PNG. `settle` is P1's vocabulary; `fast` returns
    /// the newest probe bound to a frame at least as new as the store current
    /// at request entry, `strict` keeps the 150 ms quiet window.
    fn request_tree_only(&mut self, settle: &str) -> Result<(), BackendError> {
        self.request_tree_with(settle, false)
    }

    fn request_tree_with(&mut self, settle: &str, pixels: bool) -> Result<(), BackendError> {
        let started = Instant::now();
        let t0 = m5_now_us();
        let mut request = serde_json::json!({"cmd":"tree", "settle": settle});
        if let Some(timeout) = self.settle_timeout_ms {
            request["settle_timeout_ms"] = serde_json::json!(timeout);
        }
        if !pixels {
            request["pixels"] = serde_json::json!(false);
        }
        let reply = self.command(request)?;
        self.last_tree_settle = reply.get("settle").cloned();
        let t1 = m5_now_us();
        let mut timing = PhaseTiming {
            guest_ms: started.elapsed().as_millis() as u64,
            absorb_ms: 0,
        };
        let absorb_started = Instant::now();
        let result = self.absorb(&reply);
        timing.absorb_ms = absorb_started.elapsed().as_millis() as u64;
        if p3_enabled() {
            p3_trace(
                "cli",
                "cli.request_tree",
                t0,
                m5_now_us(),
                &format!("reply_us={}", t1.saturating_sub(t0)),
            );
        }
        self.last_phase = Some(timing);
        result
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
            dm6_trace(serde_json::json!({
                "event": "drop.kill",
                "session": self.artifacts.display().to_string(),
                "pid": pid,
                "unix_ms": now_unix_ms(),
            }));
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

    /// Members of the session's process group (empty when it has no group).
    fn group_survivors(&self) -> Vec<u32> {
        self.process_group
            .map(processes_in_group)
            .unwrap_or_default()
    }

    /// SIGKILL the session's whole process group (idempotent; `ESRCH` is fine).
    fn sigkill_group(&self) {
        let Some(group) = self.process_group else {
            return;
        };
        dm6_trace(serde_json::json!({
            "event": "sigkill.group",
            "session": self.artifacts.display().to_string(),
            "process_group": group,
            "unix_ms": now_unix_ms(),
        }));
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
        // GL1: explicit destruction is **bounded**. The guest's own `close`
        // handler delivers the activity lifecycle (onPause/onStop/onDestroy)
        // and then ends its process, which normally takes ~10 ms (measured:
        // counter p50 18 ms, `thread-heavy` p50 18 ms end to end). An app whose
        // threads never exit must not be waited for: Android's AMS does not
        // join them either, it SIGKILLs the process after the lifecycle. So the
        // grace below bounds the *lifecycle*, never the app's threads: a guest
        // still alive afterwards is killed with its whole process tree, and the
        // reply says which path was taken (`lifecycle` vs `killed`).
        let grace = Duration::from_millis(teardown_grace_ms());
        let deadline = Instant::now() + grace;
        let exited = self.wait_for_exit(deadline);
        let path = if exited {
            TeardownPath::Lifecycle
        } else {
            TeardownPath::Killed
        };
        let exit_ms = started.elapsed().as_millis() as u64;
        // Teardown guarantee: the whole process group is gone, not just the
        // direct child. A child the guest forked is killed here too; a survivor
        // that ignores SIGKILL is reported, never silently ignored. When the
        // wait hit its deadline the waiter thread still owns the child and reaps
        // it once the kernel delivers the signal below.
        //
        // D-M6/P4: the kernel answers "is the group empty?" directly with
        // `kill(-pgid, 0)` (ESRCH = no member, zombies included), which is both
        // atomic and free compared with reading `/proc/<pid>/stat` for every
        // process on the host. That walk cost more than the guest's own exit
        // (~65 ms of every stop). The `/proc` scan is kept, but only when the
        // kernel says a member still exists: then it names the survivors for
        // the teardown report and for the SIGKILL/grace path below.
        let survivors = if self.group_is_empty() {
            Vec::new()
        } else {
            self.group_survivors()
        };
        if !survivors.is_empty() {
            self.sigkill_group();
        }
        let mut report = self.teardown_report_with(survivors);
        if !report.is_clean() {
            // The group proof must not quantise to a coarse tick either: after
            // SIGKILL the bwrap/dalvik leaves are usually reaped within a few
            // milliseconds, so poll adaptively from 1 ms (capped at 10 ms) and
            // the whole stop stays well under the old 50 ms step.
            let grace = Instant::now() + Duration::from_secs(2);
            let mut step = Duration::from_millis(1);
            while Instant::now() < grace && !report.survivors.is_empty() {
                std::thread::sleep(step);
                step = (step * 2).min(Duration::from_millis(10));
                report = self.teardown_report();
            }
        }
        if !report.survivors.is_empty() {
            return Err(BackendError::Protocol {
                code: acos_protocol::ErrorCode::Internal,
                detail: format!(
                    "PROCESS_TREE_DIRTY: {} process(es) still alive in session group {:?}: {:?}",
                    report.survivors.len(),
                    report.process_group,
                    report.survivors
                ),
                hint: Some(
                    "The session's process group still has live processes after SIGKILL and a \
                     2 s grace window. Do not start another session until the PIDs are gone; \
                     inspect /proc and report the harness bug."
                        .to_string(),
                ),
                details: Some(serde_json::json!({
                    "process_group": report.process_group,
                    "survivors": report.survivors,
                })),
            });
        }
        // D-M6 (trace-only, env-gated): the stop cost this session actually paid
        // (close write -> child reaped -> group proven empty). `exit_ms` is the
        // wait alone, so the poll replacement and the /proc proof can be told
        // apart. Inert unless `DM6_RESET_TRACE` names a file; no reply or
        // control flow changes.
        dm6_trace(serde_json::json!({
            "event": "stop",
            "session": self.artifacts.display().to_string(),
            "process_group": self.process_group,
            "path": path.as_str(),
            "exit_ms": exit_ms,
            "stop_ms": started.elapsed().as_millis() as u64,
            "reaped_before_deadline": exited,
            "unix_ms": now_unix_ms(),
        }));
        self.last_teardown = Some((path, started.elapsed().as_millis() as u64));
        self.write_timing(started.elapsed().as_millis() as u64);
        Ok(())
    }

    /// GL1: how the last explicit destruction ended and how long it took.
    ///
    /// `acos_destroy` reports it so a caller can tell an orderly lifecycle
    /// (`lifecycle`) from a bounded kill (`killed`) without inspecting logs.
    pub fn last_teardown(&self) -> Option<(TeardownPath, u64)> {
        self.last_teardown
    }

    /// GL1: processes that still name this session's artifacts directory on
    /// their command line (the `acos-art` launcher and anything it left).
    ///
    /// The process-group proof above only covers the group the engine spawned;
    /// this is the *session*-wide check the destroy reply reports, so a leftover
    /// launcher from the same session cannot be missed.
    pub fn session_leftovers(&self) -> Vec<u32> {
        processes_mentioning(&self.artifacts)
    }

    /// Whether no process is left in the session's process group, asked of the
    /// kernel instead of walking `/proc` (D-M6 candidate 3, P4).
    ///
    /// `kill(-group, 0)` runs the same existence and permission checks as a
    /// real signal without delivering one: `ESRCH` proves the group is empty
    /// (a zombie is still a member, so it is not hidden), `EPERM`/success means
    /// a member exists. Anything else falls back to the `/proc` scan, which is
    /// then the proof and the survivor list. No group recorded (`None`) keeps
    /// the old unverified-report behaviour.
    fn group_is_empty(&self) -> bool {
        self.process_group.map(group_is_empty).unwrap_or(true)
    }

    /// Wait for the child to exit, without polling, up to `deadline`.
    ///
    /// A blocking `wait()` runs on a short-lived thread and signals a channel, so
    /// the stop returns the moment the guest exits instead of on the next tick of
    /// a poll. The previous `try_wait()` loop slept 50 ms between probes, which
    /// quantised every stop to ~55/105/155 ms and cost 105 ms p50 (D-M6). The
    /// waiter owns the `Child` (so it still reaps after the SIGKILL below); no
    /// `try_wait` races it.
    ///
    /// Returns true when the child was reaped before `deadline`.
    fn wait_for_exit(&mut self, deadline: Instant) -> bool {
        let Some(mut child) = self.child.take() else {
            return true;
        };
        let (tx, rx) = mpsc::sync_channel::<()>(1);
        std::thread::spawn(move || {
            let _ = child.wait();
            let _ = tx.send(());
        });
        let remaining = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(remaining) {
            Ok(()) => true,
            // The waiter ended without signalling (it panicked). Treat the child
            // as unreaped so the caller SIGKILLs the group and proves it empty.
            Err(RecvTimeoutError::Disconnected) => false,
            // Deadline passed: the caller kills the group; the waiter reaps the
            // child when the kernel delivers the signal, and the teardown proof
            // below waits for the group to disappear.
            Err(RecvTimeoutError::Timeout) => false,
        }
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

    /// A teardown report from a `/proc` scan already taken by the caller.
    fn teardown_report_with(&self, survivors: Vec<u32>) -> TeardownReport {
        let Some(group) = self.process_group else {
            return TeardownReport::unverified();
        };
        TeardownReport {
            process_group: Some(group),
            survivors,
            verified: true,
        }
    }
}

impl ArtSession {
    /// A tree-only traversal with an explicit P1 settle mode. `fast` is for
    /// navigation/inspection and is labelled in the reply's `settle` block;
    /// `strict` is the default evidence mode.
    pub fn tree_with_settle(&mut self, settle: &'static str) -> Result<UiTree, BackendError> {
        let settle = if settle == "fast" { "fast" } else { "strict" };
        self.request_tree_only(settle)?;
        self.require_cached_tree()
    }

    /// P9: the tree a **launch** reply binds.
    ///
    /// A live session already has a bound tree, and a relaunch of the same
    /// guest must hand that tree back (it is the newest traversal this guest
    /// made; re-traversing would only add latency). A fresh guest has none, so
    /// one traversal is taken with the caller's `settle`:
    ///
    /// * `strict` (default) — the unchanged settled-frame rule (150 ms quiet
    ///   window), and `pixels:true` keeps P7's launch reply, whose `frame.md5`
    ///   is the captured PNG's hash.
    /// * `fast` — P3b's fast tree: a probe sample bound to a C1 generation at
    ///   least as new as the store current at request entry, no quiet window.
    ///   `pixels` is always `false` here (the launcher refuses fast + capture),
    ///   so the reply's frame identity is C1 metadata only.
    pub(crate) fn ensure_launch_tree(
        &mut self,
        settle: &'static str,
        pixels: bool,
    ) -> Result<UiTree, BackendError> {
        if self.cached.is_some() {
            return self.require_cached_tree();
        }
        let settle = if settle == "fast" { "fast" } else { "strict" };
        self.request_tree_with(settle, pixels)?;
        self.require_cached_tree()
    }

    /// P9: choose the settle mode and pixel capture the next `reset` binds its
    /// relaunched guest's tree with (serve's `acos_reset` arguments).
    pub(crate) fn set_reset_bind(
        &mut self,
        settle: &'static str,
        pixels: bool,
        timeout_ms: Option<u64>,
    ) {
        self.reset_settle = if settle == "fast" { "fast" } else { "strict" };
        self.reset_pixels = pixels;
        self.reset_settle_timeout_ms = timeout_ms;
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
        let frame = cached.observed(previous)?;
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
        self.tree_with_settle("strict")
    }

    fn set_input_settle_mode(&mut self, fast: bool) {
        self.settle_mode = if fast { "fast" } else { "strict" };
    }

    fn frame(&mut self) -> Result<ObservedFrame, BackendError> {
        // `tree` re-traverses and publishes a new frame; its reply carries both
        // the tree and the PNG of that frame. The PNG bytes are loaded from the
        // artifact now (and hash-verified); the session retains only the
        // descriptor between calls.
        let previous = self.seen_seq;
        self.request_tree()?;
        let cached = self.require_cached_frame()?;
        let observed = cached.observed(previous)?;
        if observed.png().is_empty() {
            return Err(live_frame_unavailable(
                "the ART tree reply carried no PNG; refusing to bind a tree to absent pixels",
            ));
        }
        Ok(observed)
    }

    fn last_frame(&self) -> Option<ObservedFrame> {
        match self.bound_frame() {
            Ok(frame) => frame,
            Err(error) => {
                // `last_frame` is the trait's infallible "last completed frame"
                // accessor; an artifact that can no longer be proven must not
                // masquerade as a frame. Say why on the daemon's stderr and
                // answer "no frame"; `frame()`/`launch()` return the structured
                // error to a caller that is asking for pixels.
                eprintln!("acos serve: ART bound frame unavailable: {error}");
                None
            }
        }
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
        let captured = self.capture_frame()?;
        let absorb_started = Instant::now();
        let t_read = m5_now_us();
        let png = read_png(&captured.path)?;
        if let Some(phase) = self.last_phase.as_mut() {
            phase.absorb_ms = absorb_started.elapsed().as_millis() as u64;
        }
        m5_trace(
            "cli.png_read",
            t_read,
            m5_now_us(),
            &format!("png_bytes={}", png.len()),
        );
        if png.is_empty() {
            return Err(live_frame_unavailable(
                "the ART screenshot frame file is empty",
            ));
        }
        Ok(FrameEvidence {
            frame_seq: captured.frame_seq,
            previous_seq: captured.previous_seq,
            captured_at_ms: now_unix_ms(),
            captured_at: captured.captured_at,
            md5: captured.md5,
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
        // No window label: subtract the first window's measured origin. This is
        // the pre-multi-window behaviour and is right for a single-window tree.
        let (dx, dy) = self.input_offset;
        self.input_command(
            serde_json::json!({"cmd":"tap","x":x - dx,"y":y - dy}),
            "input.tap",
        )
    }

    fn tap_in_window(&mut self, window: &str, x: i32, y: i32) -> Result<(), BackendError> {
        // The engine's tap coordinates are display/screen coordinates (resolved
        // from the probe's `bounds_on_screen`). The interactive input channel's
        // transform is window-local (W10/I2), so subtract the origin of the
        // window that contains the node, measured from the same probe tree.
        let (dx, dy) = offset_for_window(&self.window_offsets, self.input_offset, window);
        self.input_command(
            serde_json::json!({"cmd":"tap","x":x - dx,"y":y - dy}),
            "input.tap",
        )
    }

    fn tap_text(&mut self, text: &str) -> Result<(), BackendError> {
        let tree = self.require_cached_tree()?;
        let selector = Selector::by_text(text);
        let node = match tree.find_one(&selector) {
            Some(node) => node,
            None => {
                if let Some(offscreen) = tree.find_invisible(&selector) {
                    return Err(BackendError::Failed {
                        code: acos_protocol::ErrorCode::ElementOffscreen,
                        detail: format!(
                            "text {text:?} matches only a node that is not visible to the user (bounds {}, visible_bounds {}); scroll it into view first",
                            offscreen.bounds,
                            offscreen.visible_bounds
                                .map_or_else(|| "<none>".to_string(), |b| b.to_string())
                        ),
                    });
                }
                return Err(BackendError::Failed {
                    code: acos_protocol::ErrorCode::ElementNotFound,
                    detail: format!("no node with text {text:?} in the live ART tree"),
                });
            }
        };
        let (x, y) = node.tap_point();
        self.tap_in_window(&node.window, x, y)
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
        //
        // W55: if any part of the relaunch fails, `self` is left with the OLD
        // guest already stopped. It must then be a **provably dead** session:
        // closed, with no bound frame, so no verb can answer from the stale
        // frame and `liveness()` says exactly why. Before W55 a failed relaunch
        // left a closed-but-held session whose cached tree `acos_launch` served
        // for the rest of the daemon's life (W50 §7 item 3).
        self.closed = true;
        let phase_started = Instant::now();
        if let Err(error) = self.shutdown() {
            self.invalidate_frame();
            return Err(error);
        }
        let stop_ms = phase_started.elapsed().as_millis() as u64;
        let relaunch_started = Instant::now();
        let mut fresh = match ArtSession::start_with_options(
            &apk,
            &artifacts,
            self.animations,
            self.insets(),
        ) {
            Ok(fresh) => fresh,
            Err(error) => {
                self.invalidate_frame();
                return Err(error);
            }
        };
        fresh.settle_timeout_ms = self.reset_settle_timeout_ms;
        let start_ms = relaunch_started.elapsed().as_millis() as u64;
        fresh.seq_offset = offset;
        fresh.log_offset = log_offset;
        // Session-level cost accounting spans the relaunch.
        fresh.settle = settle;
        fresh.session_start_ms = boot_ms.saturating_add(fresh.session_start_ms);
        // Leave a live frame bound: the step after `reset` may be a pure
        // assertion (`assert_text`), which reads the last completed frame.
        //
        // P9: the bind uses the mode/pixels the caller chose before the reset
        // (`set_reset_bind`). `fast` is P3b's probe-fresh tree: the relaunched
        // guest's own first traversal, bound to a C1 generation at least as new
        // as the store current at request entry, with no 150 ms quiet window and
        // no PNG; `strict` (default) is the unchanged 150 ms settled rule.
        let (bind_settle, bind_pixels) = (self.reset_settle, self.reset_pixels);
        let bind_started = Instant::now();
        let bound = fresh
            .request_tree_with(bind_settle, bind_pixels)
            .and_then(|()| fresh.require_cached_tree());
        if let Err(error) = bound {
            let _ = fresh.close();
            self.invalidate_frame();
            return Err(error);
        }
        let bind_ms = bind_started.elapsed().as_millis() as u64;
        fresh.last_input = None;
        *self = fresh;
        // P4 (trace-only, env-gated): reset's phase split on the host clock.
        // Inert unless `DM6_RESET_TRACE` names a file; no reply or control
        // flow changes.
        dm6_trace(serde_json::json!({
            "event": "reset",
            "stop_ms": stop_ms,
            "relaunch_ms": start_ms,
            "bind_ms": bind_ms,
            "engine_total_ms": phase_started.elapsed().as_millis() as u64,
            "unix_ms": now_unix_ms(),
        }));
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

    fn input_settle_timeout(&self) -> Option<serde_json::Value> {
        timed_out_settle(self.last_input_settle.as_ref())
    }

    fn tree_settle_timeout(&self) -> Option<serde_json::Value> {
        timed_out_settle(self.last_tree_settle.as_ref())
    }
}
/// P12b: did this reply's settle block time out?
///
/// Only the structured timeout shape counts (`settled:false` with
/// `bound:"timeout"`); an old or absent block is not a timeout.
fn timed_out_settle(settle: Option<&serde_json::Value>) -> Option<serde_json::Value> {
    let settle = settle?;
    if settle.get("settled").and_then(serde_json::Value::as_bool) == Some(false)
        && settle.get("bound").and_then(serde_json::Value::as_str) == Some("timeout")
    {
        Some(settle.clone())
    } else {
        None
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
                && (message.starts_with("FATAL EXCEPTION") || message.contains("FATAL EXCEPTION")));
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

/// D-M5: microseconds since the Unix epoch (realtime). Shared by the serve and
/// engine timers so one screenshot's phases line up on one clock.
pub(crate) fn m5_now_us() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros())
        .unwrap_or(0)
}

/// D-M5: env-gated per-phase trace for one screenshot.
///
/// `DM5_TRACE=<path>` appends one line per phase to that file (append-only,
/// opened once); unset means no work at all. Trace only: no behaviour change.
pub(crate) fn m5_trace(event: &str, t0: u128, t1: u128, extra: &str) {
    use std::io::Write;
    static SINK: std::sync::OnceLock<Option<std::sync::Mutex<std::fs::File>>> =
        std::sync::OnceLock::new();
    let sink = SINK.get_or_init(|| {
        std::env::var_os("DM5_TRACE")
            .and_then(|path| {
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .ok()
            })
            .map(std::sync::Mutex::new)
    });
    if let Some(file) = sink {
        if let Ok(mut file) = file.lock() {
            let _ = writeln!(
                file,
                "DM5TRACE proc=cli ev={event} t_us={t1} dur_us={} {extra}",
                t1.saturating_sub(t0)
            );
            let _ = file.flush();
        }
    }
}

/// P3: is the tree-diagnosis trace on? Cached once.
pub(crate) fn p3_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("P3_TRACE").is_some())
}

/// P3: env-gated phase trace for one fresh tree request. `P3_TRACE=<path>`
/// appends one line per phase; unset means no work at all. Trace only.
pub(crate) fn p3_trace(proc_name: &str, event: &str, t0: u128, t1: u128, extra: &str) {
    use std::io::Write;
    static SINK: std::sync::OnceLock<Option<std::sync::Mutex<std::fs::File>>> =
        std::sync::OnceLock::new();
    let sink = SINK.get_or_init(|| {
        std::env::var_os("P3_TRACE")
            .and_then(|path| {
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .ok()
            })
            .map(std::sync::Mutex::new)
    });
    if let Some(file) = sink {
        if let Ok(mut file) = file.lock() {
            // One write per line: several host processes append to the same
            // file, so a fragmented `write!` would interleave two lines.
            let line = format!(
                "P3TRACE proc={proc_name} ev={event} t_us={t1} dur_us={} {extra}\n",
                t1.saturating_sub(t0)
            );
            let _ = file.write_all(line.as_bytes());
            let _ = file.flush();
        }
    }
}

/// P3: a single timestamped marker (duration 0).
pub(crate) fn p3_mark(proc_name: &str, event: &str, extra: &str) {
    let t = m5_now_us();
    p3_trace(proc_name, event, t, t, extra);
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

/// Whether process group `group` has no member left, asked of the kernel.
///
/// `kill(-group, 0)` runs the same existence and permission checks as a real
/// signal without delivering one: `ESRCH` proves the group is empty (a zombie
/// is still a member, so one cannot hide here), `EPERM`/success means a member
/// exists. Any other errno answers `false`, so the caller falls back to the
/// `/proc` scan and never claims a proof it did not get.
pub(crate) fn group_is_empty(group: i32) -> bool {
    // SAFETY: `group` is the caller's own child's process group
    // (`process_group(0)` at spawn); signal 0 performs only the check.
    let rc = unsafe { libc::kill(-group, 0) };
    if rc == 0 {
        return false;
    }
    std::io::Error::last_os_error()
        .raw_os_error()
        .map(|errno| errno == libc::ESRCH)
        .unwrap_or(false)
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

/// GL1: how long a guest gets to finish its activity lifecycle after the
/// explicit `close`, in milliseconds.
///
/// Android's own bound is `ActivityManagerService`'s stop timeout, which is
/// seconds — far more than a device needs. Measured on this host, a guest's own
/// `close` (lifecycle + process exit) returns in ~10 ms for both the `counter`
/// fixture and the thread-heavy one, so 300 ms is ~30x the observed cost and
/// still keeps `acos_destroy` under the 500 ms "feels instant" bound. Anything
/// slower is an app that is not going to finish on its own (`PR_SET_PDEATHSIG`
/// find, `Thread.join` of non-daemon threads), and waiting longer cannot help.
const DEFAULT_TEARDOWN_GRACE_MS: u64 = 300;

fn teardown_grace_ms() -> u64 {
    std::env::var("ACOS_ART_TEARDOWN_GRACE_MS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_TEARDOWN_GRACE_MS)
}

/// Live (non-zombie) processes whose command line names `dir`.
///
/// The process-group proof is scoped to the group the engine spawned; this is
/// the wider check the destroy reply reports (a launcher left behind by the
/// same session is named on its own command line: `--out <dir>/art-frames`).
fn processes_mentioning(dir: &Path) -> Vec<u32> {
    let needle = dir.display().to_string();
    let mut pids = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return pids;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|name| name.parse::<u32>().ok()) else {
            continue;
        };
        let Ok(raw) = std::fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        if raw.is_empty() {
            continue;
        }
        let cmdline = String::from_utf8_lossy(&raw).replace('\0', " ");
        if !cmdline.contains("acos-art") || !cmdline.contains(&needle) {
            continue;
        }
        // A zombie is not running code; init/the subreaper still reaps it.
        let state = std::fs::read_to_string(entry.path().join("status"))
            .ok()
            .and_then(|status| {
                status
                    .lines()
                    .find(|line| line.starts_with("State:"))
                    .and_then(|line| line.split_whitespace().nth(1).map(str::to_string))
            });
        if state.as_deref() == Some("Z") {
            continue;
        }
        pids.push(pid);
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

    #[test]
    fn per_window_offsets_map_a_dialog_tap_into_the_dialog_window() {
        // The W36 measurement: Activity window at screen offset `[0,128]`, the
        // AlertDialog's frame at `[152,976]`. Each node carries its `window`
        // label; `bounds` is local to that window.
        let tree = serde_json::json!({
            "nodes": [{
                "class": "android.widget.FrameLayout",
                "window": "main",
                "bounds": "[0,0][1080,2209]",
                "bounds_on_screen": "[0,128][1080,2337]",
                "children": [{
                    "class": "android.widget.TextView",
                    "id": "count",
                    "window": "main",
                    "bounds": "[42,189][63,240]",
                    "bounds_on_screen": "[42,317][63,368]"
                }]
            }, {
                "class": "android.widget.FrameLayout",
                "window": "window-2#1",
                "bounds": "[0,0][720,480]",
                "bounds_on_screen": "[152,976][872,1456]",
                "children": [{
                    "class": "android.widget.Button",
                    "id": "button1",
                    "window": "window-2#1",
                    "bounds": "[534,318][702,460]",
                    "bounds_on_screen": "[686,1294][854,1436]"
                }]
            }]
        });
        let offsets = probe_window_offsets(&tree);
        assert_eq!(offsets.get("main"), Some(&(0, 128)));
        assert_eq!(offsets.get("window-2#1"), Some(&(152, 976)));
        assert_eq!(probe_input_offset(&tree), Some((0, 128)));

        // Screen point of the dialog's OK button -> dialog-local point that the
        // dialog's own InputChannel receives, not a point offset by `[0,128]`.
        let ok_center_screen = (770, 1365);
        let (dx, dy) = offset_for_window(&offsets, (0, 128), "window-2#1");
        assert_eq!(
            (ok_center_screen.0 - dx, ok_center_screen.1 - dy),
            (618, 389)
        );
        // The Activity's button keeps the Activity origin.
        let (dx, dy) = offset_for_window(&offsets, (0, 128), "main");
        assert_eq!((540 - dx, 1290 - dy), (540, 1162));
        // An explicit screen point (no label) and an unknown label fall back to
        // the first window, never to a guessed dialog origin.
        assert_eq!(offset_for_window(&offsets, (0, 128), ""), (0, 128));
        assert_eq!(offset_for_window(&offsets, (0, 128), "gone"), (0, 128));
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
        // P9: every command the engine sends is appended here, so a test can
        // assert the *request* shape (`settle`, `pixels`), not just the reply.
        body.push_str(&format!("requests='{}/requests.log'\n", dir.display()));
        body.push_str("while IFS= read -r line; do\n");
        body.push_str("  printf '%s\\n' \"$line\" >> \"$requests\"\n");
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

    /// P9: `settle:"fast"` on launch/reset must reach acos-art as a **tree-only
    /// fast request** (`settle:fast`, `pixels:false`) and `strict` must keep the
    /// unchanged request shape (strict, capture by launcher default).
    #[test]
    fn launch_and_reset_bind_forward_the_settle_mode_and_pixel_choice() {
        let dir = scratch_dir("launch-bind");
        let script = fake_art_binary(&dir);
        let apk = dir.join("fake.apk");
        std::fs::write(&apk, b"not-a-real-apk").unwrap();
        let _art_bin_guard = art_bin_env_lock();
        std::env::set_var("ACOS_ART_BIN", &script);
        let mut session = ArtSession::start(&apk, &dir).expect("fake session starts");
        let requests = dir.join("requests.log");

        // A fresh session has no bound tree: fast binds one from the probe.
        session
            .ensure_launch_tree("fast", false)
            .expect("fast bind");
        let first = std::fs::read_to_string(&requests).unwrap();
        assert!(
            first.contains("\"settle\":\"fast\""),
            "fast launch must ask for the fast tree: {first}"
        );
        assert!(
            first.contains("\"pixels\":false"),
            "fast launch must be pixel-free: {first}"
        );
        let _ = std::fs::remove_file(&requests);

        session.set_reset_bind("strict", true, None);
        session.reset().expect("strict reset");
        let strict = std::fs::read_to_string(&requests).unwrap();
        assert!(
            strict.contains("\"settle\":\"strict\""),
            "the default reset bind stays strict: {strict}"
        );
        assert!(
            !strict.contains("\"pixels\":false"),
            "the strict default still captures pixels (launcher default): {strict}"
        );
        let _ = std::fs::remove_file(&requests);

        session.set_reset_bind("fast", false, None);
        session.reset().expect("fast reset");
        let fast = std::fs::read_to_string(&requests).unwrap();
        assert!(
            fast.contains("\"settle\":\"fast\"") && fast.contains("\"pixels\":false"),
            "a fast reset binds a tree-only fast tree: {fast}"
        );
        let _ = session.close();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reset_relaunches_a_fresh_guest_and_keeps_frames_and_logs_monotonic() {
        let dir = scratch_dir("reset");
        let script = fake_art_binary(&dir);
        let apk = dir.join("fake.apk");
        std::fs::write(&apk, b"not-a-real-apk").unwrap();
        // `resolve_binary` is the only reader of this variable in this crate, and
        // only this test starts a session.
        let _art_bin_guard = art_bin_env_lock();
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
        let _art_bin_guard = art_bin_env_lock();
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
        session
            .tap(1, 1)
            .expect("a no-change delivery still settles");
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

    /// GL1: a guest must not die when the *thread that asked for it* exits.
    ///
    /// `PR_SET_PDEATHSIG` is delivered on the death of the parent **thread**.
    /// The daemon creates sessions on the per-connection thread, so spawning
    /// there made a kept guest SIGKILL itself as soon as the launching client
    /// disconnected. This reproduces the daemon's shape without any guest: a
    /// throwaway thread asks for a child with the same `pre_exec`, then exits;
    /// the child must still be running afterwards (and it must be gone when the
    /// process dies, which is what the real daemon-SIGKILL test asserts).
    #[test]
    fn a_child_spawned_through_the_spawner_survives_the_asking_thread() {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut command = Command::new("/bin/sleep");
            command
                .arg("30")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .process_group(0);
            unsafe {
                command.pre_exec(|| {
                    libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL as libc::c_ulong, 0, 0, 0);
                    Ok(())
                });
            }
            let _ = tx.send(spawner::spawn(command).map(|child| child.id()));
        });
        let pid = rx
            .recv()
            .expect("the asking thread answers")
            .expect("the spawner starts the child") as i32;
        // The asking thread is long gone now; the child must still be alive.
        std::thread::sleep(Duration::from_millis(400));
        // SAFETY: signal 0 only performs the existence/permission check.
        let alive = unsafe { libc::kill(pid, 0) == 0 };
        // SAFETY: the child leads its own group (`process_group(0)`).
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
        assert!(
            alive,
            "the child was killed with the thread that asked for it (GL1 regression)"
        );
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

    /// P4: the kernel's own group check must say "live" for a live group and
    /// "empty" once the group is gone, without reading `/proc`. This is the
    /// proof `shutdown` now uses on the clean path; the `/proc` scan remains
    /// only for naming survivors when the check says a member exists.
    #[test]
    fn kernel_group_check_agrees_with_the_scan() {
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
        assert!(
            !group_is_empty(group),
            "a live process group must not read as empty"
        );
        assert!(
            !processes_in_group(group).is_empty(),
            "the scan agrees the group is live"
        );
        // SAFETY: the child leads this group; `-group` names it and nothing else.
        unsafe {
            libc::kill(-group, libc::SIGKILL);
        }
        let _ = child.wait();
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline && !group_is_empty(group) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            group_is_empty(group),
            "a killed and reaped process group must read as empty"
        );
        assert_eq!(
            processes_in_group(group),
            Vec::<u32>::new(),
            "the scan agrees the group is gone"
        );
    }

    /// A launcher that dies without writing `session-exit.json` (SIGKILL, no
    /// record) must be classified from its own exit status: `GUEST_EXITED`,
    /// never `INTERNAL` (W53 hunk, applied in W55).
    #[test]
    fn a_killed_launcher_is_classified_from_its_exit_status() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch_dir("killed-launcher");
        let script = dir.join("fake-acos-art-dies.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\n\
             echo '{\"ok\":true,\"cmd\":\"ready\",\"package\":\"dev.test\",\"width\":1080,\"height\":2400,\"density\":420}'\n\
             kill -9 $$\n",
        )
        .unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
        let apk = dir.join("fake.apk");
        std::fs::write(&apk, b"not-a-real-apk").unwrap();
        let _art_bin_guard = art_bin_env_lock();
        std::env::set_var("ACOS_ART_BIN", &script);
        let mut session = ArtSession::start(&apk, &dir).expect("ready line arrives first");
        // The child is already gone: the write fails with EPIPE or the read sees
        // EOF. Either way the error is the classified guest exit.
        let error = session.tree().expect_err("a dead launcher cannot answer");
        assert_eq!(
            error.code(),
            ErrorCode::GuestExited,
            "a killed launcher is never INTERNAL: {error}"
        );
        let details = error
            .to_protocol_error()
            .details
            .unwrap_or(serde_json::Value::Null);
        assert_eq!(
            details["session_exit"]["source"],
            serde_json::json!("acos-art child exit status (no session-exit.json)"),
            "{details}"
        );
        assert_eq!(
            details["session_exit"]["guest_signal"],
            serde_json::json!(9)
        );
        assert!(session.liveness().is_some(), "liveness agrees");
        std::env::remove_var("ACOS_ART_BIN");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A reset whose replacement guest fails must NOT leave a closed-but-held
    /// session with a cached frame: that is the W50 stale-frame bug. The
    /// session is dead, has no bound frame, and says why.
    #[test]
    fn a_failed_reset_leaves_a_provably_dead_session() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch_dir("failed-reset");
        let png = dir.join("tiny.png");
        std::fs::write(&png, super::super::fake::TINY_PNG).unwrap();
        let marker = dir.join("first-guest-ran");
        let script = dir.join("fake-acos-art-reset-fails.sh");
        let mut body = String::new();
        body.push_str("#!/bin/sh\n");
        body.push_str(&format!(
            "if [ -f '{m}' ]; then echo 'acos-art: replacement guest failed' >&2; exit 1; fi\n",
            m = marker.display()
        ));
        body.push_str(&format!(": > '{}'\n", marker.display()));
        body.push_str("echo '{\"ok\":true,\"cmd\":\"ready\",\"package\":\"dev.test\",\"width\":1080,\"height\":2400,\"density\":420}'\n");
        body.push_str("while IFS= read -r line; do\n");
        body.push_str("  case \"$line\" in\n");
        body.push_str(&format!(
            "    *'\"cmd\":\"tree\"'*) echo '{{\"ok\":true,\"cmd\":\"tree\",\"frame_seq\":1,\"md5\":\"m1\",\"png\":\"{}\",\"tree\":{{\"nodes\":[]}}}}' ;;\n",
            png.display()
        ));
        body.push_str("    *'\"cmd\":\"logs\"'*) echo '{\"ok\":true,\"cmd\":\"logs\",\"total\":0,\"kept\":0,\"dropped\":0,\"entries\":[]}' ;;\n");
        body.push_str("    *'\"cmd\":\"close\"'*) exit 0 ;;\n");
        body.push_str(
            "    *) echo '{\"ok\":false,\"code\":\"BAD_REQUEST\",\"error\":\"no\"}' ;;\n",
        );
        body.push_str("  esac\n");
        body.push_str("done\n");
        std::fs::write(&script, body).unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
        let apk = dir.join("fake.apk");
        std::fs::write(&apk, b"not-a-real-apk").unwrap();
        // A stale record from an earlier guest in the same directory must not
        // survive into this session.
        let stale = dir.join("art-frames/session-exit.json");
        std::fs::create_dir_all(dir.join("art-frames")).unwrap();
        std::fs::write(
            &stale,
            br#"{"outcome":"crash","code":"GUEST_EXITED","stage":"stale"}"#,
        )
        .unwrap();
        let _art_bin_guard = art_bin_env_lock();
        std::env::set_var("ACOS_ART_BIN", &script);
        let mut session = ArtSession::start(&apk, &dir).expect("fake session starts");
        assert!(
            !stale.exists(),
            "a stale session-exit.json must be removed at start"
        );
        let before = session.frame().expect("first frame");
        assert!(before.frame_seq() >= 1);
        let error = session.reset().expect_err("the replacement guest dies");
        assert_ne!(error.code(), ErrorCode::Internal, "{error}");
        assert!(
            session.liveness().is_some(),
            "a failed reset leaves a provably dead session"
        );
        assert!(
            session.frame_info().is_none(),
            "the dead session's cached frame is discarded, never served"
        );
        assert!(session.bound_tree().is_none());
        assert!(
            session.bound_frame().expect("no artifact error").is_none(),
            "a failed reset must leave no loadable frame artifact"
        );
        // Closing it is still clean (idempotent), so the daemon can drop it.
        let _ = session.close();
        std::env::remove_var("ACOS_ART_BIN");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Scratch under the worktree `.cache/` (never `/tmp`), per O2.
    /// Serialises tests that point `ACOS_ART_BIN` at a fake launcher: the
    /// variable is process-global, so parallel tests would race on it.
    fn art_bin_env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn o2_scratch_dir(name: &str) -> PathBuf {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.cache/o2-tests");
        let dir = root.join(format!(
            "{}-{}-{name}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The retained descriptor is a reference (path/hash/size/seq/geometry), and
    /// loading verifies the hash before handing any pixels back.
    #[test]
    fn frame_artifact_loads_lazily_and_verifies_its_hash() {
        let dir = o2_scratch_dir("artifact-hash");
        let png = dir.join("frame.png");
        std::fs::write(&png, super::super::fake::TINY_PNG).unwrap();
        let artifact = FrameArtifact::bind(&png, 7, 1080, 2400, 420).expect("bind");
        // What is retained: identity, not pixels.
        assert_eq!(artifact.size, super::super::fake::TINY_PNG.len() as u64);
        assert_eq!(
            artifact.content_md5,
            acos_render::md5::md5_hex(super::super::fake::TINY_PNG)
        );
        assert_eq!(artifact.frame_seq, 7);
        assert_eq!(
            (
                artifact.display_width,
                artifact.display_height,
                artifact.density
            ),
            (1080, 2400, 420)
        );
        assert_eq!(
            artifact.load().expect("verified load"),
            super::super::fake::TINY_PNG
        );

        // Same size, different bytes: the recorded hash catches it.
        let mut altered = super::super::fake::TINY_PNG.to_vec();
        altered[20] ^= 0xff;
        std::fs::write(&png, &altered).unwrap();
        let error = artifact.load().expect_err("altered pixels must not load");
        assert_eq!(error.code(), ErrorCode::RenderIo, "{error}");
        assert!(
            format!("{error}").contains("FRAME_ARTIFACT_UNAVAILABLE"),
            "{error}"
        );
        assert!(format!("{error}").contains("hashes to"), "{error}");

        // A truncated file is caught by the recorded size.
        std::fs::write(&png, &super::super::fake::TINY_PNG[..20]).unwrap();
        let error = artifact.load().expect_err("truncated pixels must not load");
        assert_eq!(error.code(), ErrorCode::RenderIo, "{error}");
        assert!(
            format!("{error}").contains("bytes but was bound at"),
            "{error}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// P2: the launcher's own digest over a trusted channel is what gets bound,
    /// and it is still proved against the bytes before any pixel is served.
    #[test]
    fn a_launcher_claimed_digest_is_bound_and_still_proved_on_load() {
        let dir = o2_scratch_dir("artifact-claimed");
        let png = dir.join("frame.png");
        std::fs::write(&png, super::super::fake::TINY_PNG).unwrap();
        let claimed = FrameDigest {
            md5: acos_render::md5::md5_hex(super::super::fake::TINY_PNG),
            size: super::super::fake::TINY_PNG.len() as u64,
        };
        let artifact = FrameArtifact::bind_claimed(&png, 9, 1080, 2400, 420, &claimed)
            .expect("a matching claim binds");
        // The bound descriptor carries the launcher's hash, and the bytes load.
        assert_eq!(artifact.content_md5, claimed.md5);
        assert_eq!(artifact.size, claimed.size);
        assert_eq!(
            artifact.load().expect("verified load"),
            super::super::fake::TINY_PNG
        );

        // Same size, different bytes: the claim does not excuse altered pixels.
        let mut altered = super::super::fake::TINY_PNG.to_vec();
        altered[20] ^= 0xff;
        std::fs::write(&png, &altered).unwrap();
        let error = artifact.load().expect_err("altered pixels must not load");
        assert_eq!(error.code(), ErrorCode::RenderIo, "{error}");
        assert!(
            format!("{error}").contains("FRAME_ARTIFACT_UNAVAILABLE"),
            "{error}"
        );

        // A file whose size contradicts the reply is not trusted: the engine
        // hashes it itself and binds what it can prove.
        let short = &super::super::fake::TINY_PNG[..20];
        std::fs::write(&png, short).unwrap();
        let rebound = FrameArtifact::bind_claimed(&png, 9, 1080, 2400, 420, &claimed)
            .expect("the engine re-binds what it can prove");
        assert_eq!(rebound.size, short.len() as u64);
        assert_eq!(rebound.content_md5, acos_render::md5::md5_hex(short));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A missing artifact is a structured error, never an empty/stale frame.
    #[test]
    fn frame_artifact_missing_file_is_a_structured_error() {
        let dir = o2_scratch_dir("artifact-missing");
        let png = dir.join("frame.png");
        std::fs::write(&png, super::super::fake::TINY_PNG).unwrap();
        let artifact = FrameArtifact::bind(&png, 3, 1080, 2400, 420).expect("bind");
        std::fs::remove_file(&png).unwrap();
        let error = artifact.load().expect_err("a missing artifact cannot load");
        assert_eq!(error.code(), ErrorCode::RenderIo, "{error}");
        assert!(
            format!("{error}").contains("FRAME_ARTIFACT_UNAVAILABLE"),
            "{error}"
        );
        assert!(format!("{error}").contains("is gone"), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A held session keeps only the descriptor: the bytes are read from the
    /// artifact on demand, so removing the file after the bind makes the bound
    /// frame unavailable (not silently reused) and restoring it works again.
    #[test]
    fn a_held_session_loads_its_bound_frame_from_the_artifact_on_demand() {
        let dir = o2_scratch_dir("artifact-lazy");
        let script = fake_art_binary(&dir);
        let apk = dir.join("fake.apk");
        std::fs::write(&apk, b"not-a-real-apk").unwrap();
        let _art_bin_guard = art_bin_env_lock();
        std::env::set_var("ACOS_ART_BIN", &script);
        let mut session = ArtSession::start(&apk, &dir).expect("fake session starts");
        session.tree().expect("bind a tree");
        // Binding recorded the artifact but did not keep its bytes: the file is
        // the only home of the pixels.
        let png = dir.join("tiny.png");
        std::fs::remove_file(&png).unwrap();
        let error = session
            .bound_frame()
            .expect_err("the artifact is gone, so the frame cannot be served");
        assert!(
            format!("{error}").contains("FRAME_ARTIFACT_UNAVAILABLE"),
            "{error}"
        );
        assert!(
            session.last_frame().is_none(),
            "the infallible accessor must answer no frame, never stale pixels"
        );
        // The metadata is still retained (cheap identity) while the pixels are not.
        assert!(session.frame_info().is_some());
        // Restoring the exact bytes makes the same bound frame load again.
        std::fs::write(&png, super::super::fake::TINY_PNG).unwrap();
        let frame = session
            .bound_frame()
            .expect("the restored artifact loads")
            .expect("a frame is bound");
        assert_eq!(frame.png(), super::super::fake::TINY_PNG);
        session.close().expect("clean close");
        // W55: a closed session never serves its bound frame.
        assert!(session.last_frame().is_none());
        assert!(session.bound_frame().expect("no artifact error").is_none());
        std::env::remove_var("ACOS_ART_BIN");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A reset rebuilds the bound frame from the replacement guest's artifact:
    /// the old cache (including its hash) is gone, and the new frame loads.
    #[test]
    fn reset_replaces_the_bound_frame_artifact() {
        let dir = o2_scratch_dir("artifact-reset");
        let script = fake_art_binary(&dir);
        let apk = dir.join("fake.apk");
        std::fs::write(&apk, b"not-a-real-apk").unwrap();
        let _art_bin_guard = art_bin_env_lock();
        std::env::set_var("ACOS_ART_BIN", &script);
        let mut session = ArtSession::start(&apk, &dir).expect("fake session starts");
        let first = session.frame().expect("first frame");
        let first_seq = first.frame_seq();
        assert_eq!(first.png(), super::super::fake::TINY_PNG);

        // A different artifact for the replacement guest (same path, new bytes).
        let png = dir.join("tiny.png");
        let replacement = [super::super::fake::TINY_PNG, b"-replacement"].concat();
        std::fs::write(&png, &replacement).unwrap();
        session.reset().expect("reset relaunches");
        let after = session
            .bound_frame()
            .expect("the new artifact loads")
            .expect("reset leaves a bound frame");
        assert!(after.frame_seq() > first_seq, "{}", after.frame.label());
        assert_eq!(
            after.png(),
            replacement.as_slice(),
            "the reset must re-bind the replacement guest's artifact, not keep the old bytes"
        );
        session.close().expect("clean close");
        std::env::remove_var("ACOS_ART_BIN");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The stop must return when the guest exits, not on the next tick of a
    /// 50 ms poll (D-M6: 105 ms p50 for a ~5 ms exit), and still prove the
    /// whole process group empty.
    #[test]
    fn stop_returns_promptly_and_leaves_no_survivor() {
        use std::os::unix::fs::PermissionsExt;
        let dir = o2_scratch_dir("stop-wait");
        let script = dir.join("fake-acos-art-slow-close.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\n\
             echo '{\"ok\":true,\"cmd\":\"ready\",\"package\":\"dev.test\",\"width\":1080,\"height\":2400,\"density\":420}'\n\
             while IFS= read -r line; do\n\
               case \"$line\" in\n\
                 *'\"cmd\":\"close\"'*) sleep 0.010; exit 0 ;;\n\
                 *) echo '{\"ok\":false,\"code\":\"BAD_REQUEST\",\"error\":\"no\"}' ;;\n\
               esac\n\
             done\n",
        )
        .unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
        let apk = dir.join("fake.apk");
        std::fs::write(&apk, b"not-a-real-apk").unwrap();
        let _art_bin_guard = art_bin_env_lock();
        std::env::set_var("ACOS_ART_BIN", &script);
        let mut session = ArtSession::start(&apk, &dir).expect("fake session starts");
        let started = Instant::now();
        let report = session.teardown().expect("clean stop");
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_millis(45),
            "stop quantised to the old 50 ms poll: {elapsed:?}"
        );
        assert!(report.is_clean(), "{report:?}");
        assert!(report.survivors.is_empty(), "{report:?}");
        assert!(report.verified, "the group must be proven, not assumed");
        std::env::remove_var("ACOS_ART_BIN");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
