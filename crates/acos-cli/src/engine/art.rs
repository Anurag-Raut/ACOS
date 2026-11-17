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
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use acos_protocol::ui::{UiNode, UiTree};
use acos_protocol::{ErrorCode, FrameMeta, LogLevel, Selector};
use acos_verify::backend::{CrashRecord, InputAck, InputDelivery, LogRecord};
use acos_verify::{BackendError, LaunchOutcome};

use super::host_mem;
use super::zygote_host::{reply_fields, zygote_dead, ZygoteFacts, ZygoteHost};
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
/// SE2: how long a `settle: content` step may keep traversing before its
/// content signature must have been stable for the window. The window itself
/// is ~300 ms; the budget bounds the wait, and running out is an honest
/// `TIMEOUT` (the screen kept changing its content), never a pass.
const DEFAULT_CONTENT_SETTLE_TIMEOUT_MS: u64 = 10_000;

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
    /// ADR-0029 item 4: the `services:` specs this session was launched with,
    /// kept so a `reset`/relaunch attaches the same modules.
    services: Vec<serde_json::Value>,
    /// The modules the guest's ready line reported as attached (verdict rows).
    modules: Vec<serde_json::Value>,
    /// The same rows, shared with `EngineBackend` so the verdict can cite the
    /// session's **final** module state (delivered fixes included) instead of the
    /// launch snapshot.
    module_rows: Arc<Mutex<Vec<serde_json::Value>>>,
    /// H13b: the boot image this session's ready line reported —
    /// `{mode, cache: hit|built|fallback|external|none, reason}` — so a serve's
    /// launch reply and session list can say whether the shared-image cache hit.
    boot_image: Option<serde_json::Value>,
    /// VIEW1a: the live-view stream socket the ready line reported
    /// (`docs/LIVE_VIEW.md`); `None` when the launcher could not bind one.
    stream_socket: Option<PathBuf>,
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
    /// SERVELEAK: the process group the **guest** leads, which `acos-art` puts in
    /// its own group (`launcher.rs: process_group(0)`), so it is *not* a member of
    /// [`Self::process_group`]. Reported by the launcher's ready line as
    /// `guest_process_group`. Without it a teardown proof covers only the launcher
    /// and a live bwrap/dalvikvm64 could be reported as a clean tree.
    guest_group: Option<i32>,
    /// SERVELEAK2/F1: the pid of the `acos-art` launcher this session spawned.
    ///
    /// Ownership of a leftover is proved from *this*, never from a command-line
    /// substring: two agents running `acos verify counter.yaml` in different
    /// directories both name `out/counter`, so a substring match let one
    /// session's teardown SIGKILL the other agent's guest.
    launcher_pid: Option<i32>,
    /// SERVELEAK2/F1: a random per-session token exported as
    /// [`SESSION_TOKEN_ENV`] into the launcher's environment. Every process the
    /// launcher starts (`bwrap`, `dalvikvm64`) inherits it, and it survives the
    /// launcher's death (a guest reparented to init keeps its environment), so
    /// it is the one ownership proof that is stable for the session's whole
    /// life. A process can only carry it if this session started it.
    session_token: String,
    /// SERVELEAK2/F4: a pidfd for the launcher. A pid **number** can be recycled
    /// once the leader is reaped; a pidfd pins the process, not the number, so
    /// liveness and the leader's SIGKILL cannot hit somebody else's process.
    leader_pidfd: Option<OwnedFd>,
    /// SERVELEAK2/F4: the group id the session still names in its teardown
    /// report after [`Self::proven_empty`] cleared the live records.
    reported_group: Option<i32>,
    /// SERVELEAK2/F4: true once a teardown proved every group this session owns
    /// empty. The group ids are cleared at that point, so neither a later
    /// `Drop` nor a repeat `shutdown` can signal an id the kernel may since
    /// have recycled for another agent's `setsid`/`bwrap`.
    proven_empty: bool,
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
    /// NET4: the id this session is addressed by — the `acos serve` session id,
    /// or the run's own id for a one-shot `verify`. It is published to the
    /// launcher (`ACOS_ART_SESSION_ID`) and names the relay session
    /// (`acos-<id>`), the same rule `acos_net` resolves, so **every** session's
    /// network is reachable, including a one-shot run's.
    session_id: String,
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
    /// FB3: the tree the session held immediately before the current input
    /// action, snapshotted by [`Self::begin_input`] from the already-bound
    /// frame (no traversal, no extra guest round trip). `None` when the action
    /// asked for no diff, when no baseline exists, or before any action.
    diff_baseline: Option<DiffBaseline>,
    /// FB3: whether the reply for the *current* action carries `changes`.
    /// Serve sets it per verb (`diff: true` is the default).
    diff_requested: bool,
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
    /// REL3A: the settle rule the next **observation** (`frame()`) binds with.
    /// `"strict"` (default) or `"content"` (ADR-0026 decision 3). The verifier
    /// sets it per step (`set_observation_settle`); the rule is one-shot - `frame()`
    /// consumes it and restores `strict`, so a content observation can never leak
    /// into a later capture/pixel claim. Serve's own verbs never ask for content
    /// observations and reset this to `strict` at the start of every call.
    observation_settle: &'static str,
    /// P3b: the guest's `settle` block for the most recent tree request
    /// (`mode`, `settled`, `probe_seq`, `tree_age_ms`, `bound`). Copied into
    /// the `acos_observe_tree` result; never synthesised.
    last_tree_settle: Option<serde_json::Value>,
    /// G3f1: the raw probe tree of the most recent tree reply, byte-for-byte as
    /// the guest produced it. The launch's content wait hands this back to the
    /// guest as `baseline_tree`, so "the next tree state" is an exact guest-side
    /// comparison (no host re-serialisation, no signature that could differ).
    last_tree_json: Option<serde_json::Value>,
    /// G3c: how the most recent launch bound its first frame (traversals, wait,
    /// node count and whether it is only the window skeleton). Serve publishes
    /// it in the launch reply so a placeholder is visible, never silent.
    first_frame_report: Option<FirstFrameReport>,
    /// G3f1: `(settled, bound)` of the frame a launch finally bound, so serve
    /// can publish `result.settle` for exactly that frame (a `change-draw` launch
    /// frame says `settled:false` instead of inheriting the pre-launch block).
    first_frame_bound: Option<(bool, Option<String>)>,
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
    /// MPROC1: the extra APKs this session installed beside the launched one,
    /// as the caller named them. Launch-time: a live session cannot gain a
    /// package (the same rule `animations`/`insets` follow), and a `reset`
    /// relaunch installs exactly this list again.
    extra_apks: Vec<PathBuf>,
    /// MPROC7: the launcher's idle kill in seconds (`None` keeps the launcher
    /// default of 30; `Some(0)` disables it). Launch-time, like `extra_apks`.
    idle_kill_seconds: Option<u64>,
    /// P12b: per-request strict/fast settle deadline (`settle_timeout_ms`).
    /// `None` keeps the engine's documented default; on timeout the guest still
    /// returns the latest tree and frame identity with `settled:false`.
    settle_timeout_ms: Option<u64>,
    /// SE2: how the bound frame was settled (`strict`, `fast` or `content`).
    /// Published on the observation so the verifier can refuse a pixel claim
    /// that only content stability backs.
    bound_settle: Option<serde_json::Value>,
    /// P12b: deadline for the tree the next `reset` binds.
    reset_settle_timeout_ms: Option<u64>,
    /// SE2b: the probe sample floor for the *next* tree request. The guest's
    /// fast tree only answers with a sample strictly newer than this, so a
    /// content-settle traversal can never be the sample an earlier traversal
    /// (or the pre-input state) already consumed.
    next_min_probe_seq: Option<u64>,
    /// SE2b: the newest `probe_seq` this session has *consumed* (any tree
    /// reply). A content window starts strictly above it.
    last_probe_seq: Option<u64>,
    /// SE2b: the `probe_seq` of the most recent tree reply, `None` when the
    /// reply carried none. Read straight from the reply (not carried over), so
    /// a reply without a sample is detectable and fails closed.
    last_reply_probe_seq: Option<u64>,
    closed: bool,
    /// GL1: how the last explicit destruction of this session ended
    /// (orderly activity lifecycle vs. killed after the bounded grace) and how
    /// long it took. Reported by `acos_destroy`; never guessed.
    last_teardown: Option<(TeardownPath, u64)>,
    /// ZPROD3: the keeper this session runs on. `None` with `ACOS_ART_ZYGOTE`
    /// off (the exec path, byte-identical to `main`) and also on every session
    /// that failed to get a zygote child (that session does not exist: the
    /// failure is a structured `ZYGOTE_DEAD`, never a silent exec fallback).
    zygote: Option<ZygoteBinding>,
    /// The pid the most recent zygote reset killed (reply field only).
    zygote_previous_child_pid: Option<u32>,
    /// S3b: a reset that failed once for a host-transport reason and then
    /// succeeded. The first failure's evidence is copied to
    /// `<artifacts>/relaunch-retry-1/` and this record is carried in the reset
    /// reply. `None` on every ordinary relaunch.
    relaunch_retry: Option<serde_json::Value>,
}

/// One session's binding to a keeper child (P2, design §3/§4).
struct ZygoteBinding {
    host: Arc<ZygoteHost>,
    key: acos_art::zygote_prod::ZygoteKey,
    facts: ZygoteFacts,
}

/// How a session asks for a zygote child. Built by
/// [`ArtSession::start_with_zygote`], and again by `reset` with
/// `reuse_tag: Some(..)` so the reset refork reuses the killed tag (§4).
pub struct ZygoteStart {
    host: Arc<ZygoteHost>,
    key: acos_art::zygote_prod::ZygoteKey,
    reuse_tag: Option<u32>,
}

impl Clone for ZygoteStart {
    fn clone(&self) -> Self {
        Self {
            host: Arc::clone(&self.host),
            key: self.key.clone(),
            reuse_tag: self.reuse_tag,
        }
    }
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

/// FB3: the tree a session held before the current input action.
///
/// Cloned from the already-bound frame in [`ArtSession::begin_input`]: the diff
/// must never cost a traversal or a guest round trip.
struct DiffBaseline {
    tree: UiTree,
    seq: u64,
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
    /// ROT8b: this observation is the task-finished (`window-gone`) state: an
    /// empty window forest with NO pixels. It carries no PNG and never becomes
    /// `last_pixel`; only the state proof is claimed.
    gone: bool,
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
            png: if self.gone && self.png.is_none() {
                Vec::new()
            } else {
                self.png_bytes()?
            },
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
// LAYER1: taps now carry display coordinates and the guest picks the window by
// policy, so this helper is only used by the probe-offset tests. Keep it: it is
// the documented mapping for a future node -> window conversion.
#[allow(dead_code)]
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
    let (handled, published) = handled_from_reply(reply);
    // REL3I3: the host's own record that the window this input went to was
    // removed during the step. A dismissed dialog's `InputEventReceiver` is
    // disposed before it can finish the event, so there is no ack to wait for;
    // the removal *is* the proof that the press acted on that window.
    let window_removed = reply
        .pointer("/gesture/window_removed")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    // REL3I3/MERGE3I3: the same precedence `handled_source` applies - the
    // delivered window's removal answers `true` unless a guest ack already
    // claimed the event.
    let any_handled = acks_from_reply(reply).iter().any(|ack| ack.handled);
    let handled =
        acos_protocol::ui::handled_answer(handled, published, any_handled, window_removed);
    InputDelivery {
        delivered: true,
        visual_change,
        handled,
        published,
        acks: acks_from_reply(reply),
        window_removed,
        target: reply.get("target").cloned(),
        settle: reply.get("settle").cloned(),
    }
}

/// REL3E: the app window's own answer, read from the guest's `gesture` record.
///
/// The guest acks every published `InputMessage` with the `handled` bit its
/// `InputEventReceiver.finishInputEvent` was called with (AOSP `ViewRootImpl`
/// passes `FLAG_FINISHED_HANDLED`, and drops with `false`), so this is the
/// framework's verdict, never a heuristic:
///
/// * **any** unhandled ack proves nothing consumed that event → `Some(false)`;
/// * every published event acked handled → `Some(true)`;
/// * a short or missing ack list → `None` (unknown; the reply says so, it never
///   claims "not handled" for an event the guest never answered).
fn handled_from_reply(reply: &serde_json::Value) -> (Option<bool>, usize) {
    let published = reply
        .pointer("/gesture/seqs")
        .and_then(|value| value.as_array())
        .map(Vec::len)
        .unwrap_or(0);
    let acks = acks_from_reply(reply);
    let handled = if acks.iter().any(|ack| !ack.handled) {
        Some(false)
    } else if published > 0 && acks.len() == published {
        Some(true)
    } else {
        None
    };
    (handled, published)
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

/// G3c: how a launch bound its first frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FirstFrameReport {
    /// Traversals spent on the launch frame (1 = the first one was content).
    pub traversals: usize,
    /// Wall time those traversals took.
    pub waited_ms: u64,
    /// G3f1: how many of those traversals were event-gated content waits (a
    /// request that waits for the guest's next committed tree state, not a
    /// timed re-traversal). `0` means the app's first frame was already content
    /// and the launch paid nothing extra.
    pub event_waits: usize,
    /// G3f1: did the guest report the bound traversal as *settled* (no pending
    /// framework work)? A `change-draw` bind is fresh but not settled, and the
    /// reply says so instead of implying a quiet screen.
    pub settled: bool,
    /// G3f1: the guest's own bind rule for the bound frame
    /// (`strict-event`, `change-draw`, `probe-fresh`), when it reported one.
    pub bound: Option<String>,
    /// Nodes in the bound tree.
    pub nodes: usize,
    /// True when the bound tree is still only the window skeleton after the
    /// budget **and** the frame moved no content: no newer frame identity and
    /// the same pixels (see [`first_frame_placeholder_verdict`]). Only then is
    /// the frame a placeholder, never a claim.
    pub placeholder: bool,
    /// FB2c: the bound tree is still the window skeleton, but the frame *did*
    /// move (a newer frame identity and/or changed pixels). That is the normal
    /// frame of a text-free screen (a custom View, a SurfaceView/GL surface, an
    /// image-only activity, Flutter without semantics): a pass that carries a
    /// warning, never a blocked verdict.
    pub suspect: bool,
    /// The frame the report describes (`cached.seq` at bind time).
    pub frame_seq: Option<u64>,
    /// The guest's own frame number for it, when the reply carried one.
    pub frame_number: Option<u64>,
    /// Frames (tree replies) the launch bound, including the launch's own.
    pub frames: usize,
    /// Did the frame identity advance across the content wait?
    pub frames_advanced: bool,
    /// Were the pixels comparable (both frames carried a frame digest)?
    pub pixels_compared: bool,
    /// Did the pixels change across the content wait?
    pub pixels_changed: bool,
    /// The budget the content wait was allowed (`ACOS_ART_FIRST_FRAME_MS`).
    pub budget_ms: u64,
    /// GATEFIX (F1): the content window the wait had to cover before a
    /// placeholder verdict (the budget, from the end of the launch's own
    /// traversal).
    pub window_ms: u64,
    /// GATEFIX (F1): did the wait observe the skeleton for that whole window?
    /// `false` means the wait ended early (guest error / protocol failure), so
    /// neither a placeholder nor a pass may be claimed from it.
    pub window_spent: bool,
}

impl Default for FirstFrameReport {
    fn default() -> Self {
        FirstFrameReport {
            traversals: 0,
            waited_ms: 0,
            event_waits: 0,
            settled: true,
            bound: None,
            nodes: 0,
            placeholder: false,
            suspect: false,
            frame_seq: None,
            frame_number: None,
            frames: 0,
            frames_advanced: false,
            pixels_compared: false,
            pixels_changed: false,
            budget_ms: 0,
            window_ms: 0,
            window_spent: false,
        }
    }
}

impl FirstFrameReport {
    /// SE2c: point the report at the tree the reply actually publishes.
    ///
    /// A content-settled launch binds its frame with the guest's `fast` rule and
    /// only then proves the tree content stable, so this report was built for the
    /// pre-settle probe. Publishing it unchanged alongside the *content-settled*
    /// tree described a tree the reply no longer carries (its `nodes` count and
    /// `placeholder` flag). Re-describing it keeps `first_frame` evidence and
    /// `frame.meaningful`/`frame.placeholder` about the same tree.
    fn describes_tree(mut self, tree: &UiTree) -> Self {
        let skeleton = first_frame_is_bare_skeleton(tree);
        self.nodes = ui_tree_node_count(&tree.nodes);
        // FB2c: the verdict (`placeholder` / `suspect`) is frame evidence taken
        // on the pre-settle traversal; the settled tree can only *clear* it
        // (the app's content mounted), never raise it from the node count alone.
        self.placeholder = self.placeholder && skeleton;
        self.suspect = self.suspect && skeleton;
        self
    }

    /// The evidence object a launch reply publishes (`result.first_frame`).
    pub(crate) fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "traversals": self.traversals,
            "waited_ms": self.waited_ms,
            "event_waits": self.event_waits,
            "settled": self.settled,
            "bound": self.bound,
            "nodes": self.nodes,
            "placeholder": self.placeholder,
            "placeholder_suspect": self.suspect,
            "budget_ms": self.budget_ms,
            "frame_seq": self.frame_seq,
            "frame_number": self.frame_number,
            "frames": self.frames,
            "frames_advanced": self.frames_advanced,
            "pixels_compared": self.pixels_compared,
            "pixels_changed": self.pixels_changed,
            "window_ms": self.window_ms,
            "window_spent": self.window_spent,
        })
    }
}

/// FB2c: the frame identity a launch baseline was bound to.
///
/// The placeholder verdict needs evidence *beyond the tree*: a node count alone
/// cannot tell an unmounted window from a text-free mounted screen.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct FrameIdentity {
    seq: Option<u64>,
    frame_number: Option<u64>,
    md5: Option<String>,
}

/// FB2c: what the frame did while the launch's content wait ran.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct FirstFrameEvidence {
    /// Frames bound by the launch (the launch's own traversal counts).
    frames: usize,
    /// A newer frame identity was published (`frame_number` or `frame_seq`).
    frames_advanced: bool,
    /// Both the baseline and the bound frame carried a digest.
    pixels_compared: bool,
    /// The digest changed (the frame's pixels are not the skeleton's).
    pixels_changed: bool,
    /// GATEFIX (F1): did the wait give the app its whole chance — the content
    /// window (`ACOS_ART_FIRST_FRAME_MS`) elapsed while the skeleton was
    /// observed? A placeholder is a claim about the app ("it never mounted"),
    /// so it may only be made after the app had that window to run; samples
    /// taken within the first few milliseconds are not evidence of absence.
    window_spent: bool,
}

fn frame_identity(frame: Option<&CachedFrame>) -> FrameIdentity {
    match frame {
        Some(frame) => FrameIdentity {
            seq: Some(frame.seq),
            frame_number: frame.frame_number,
            md5: (!frame.md5.is_empty()).then(|| frame.md5.clone()),
        },
        None => FrameIdentity::default(),
    }
}

fn frame_evidence(
    baseline: &FrameIdentity,
    current: &FrameIdentity,
    frames: usize,
    window_spent: bool,
) -> FirstFrameEvidence {
    let seq_advanced = match (baseline.seq, current.seq) {
        (Some(before), Some(after)) => after > before,
        _ => false,
    };
    let number_advanced = match (baseline.frame_number, current.frame_number) {
        (Some(before), Some(after)) => after > before,
        _ => false,
    };
    let pixels_compared = baseline.md5.is_some() && current.md5.is_some();
    let pixels_changed = match (&baseline.md5, &current.md5) {
        (Some(before), Some(after)) => before != after,
        _ => false,
    };
    FirstFrameEvidence {
        frames,
        frames_advanced: seq_advanced || number_advanced,
        pixels_compared,
        pixels_changed,
        window_spent,
    }
}

/// FB2c: is this bound frame a placeholder, and is it merely *suspect*?
///
/// * a mounted frame (text, a description/hint, or more nodes than the skeleton)
///   is never either;
/// * a skeleton whose **pixels changed** while the content wait ran is the app's
///   frame — a text-free screen is a real screen — so it is a pass with a
///   `placeholder_suspect` warning, never a blocked verdict;
/// * a skeleton whose frame identity advanced but whose pixels are the same is
///   *still* the window (the launcher re-published the same buffer), so the
///   pixels decide when they are comparable;
/// * a skeleton that nothing moved **for the whole content window** is the
///   placeholder: the app never mounted. The window is what makes it evidence:
///   the launch's own traversal plus a burst of samples taken within a few
///   milliseconds of the window mount is exactly how a fast host produced a
///   false `PLACEHOLDER_CONTENT` on a launch whose app did mount 300 ms later
///   (v0.3.0 gate F1). `window_spent == false` therefore yields neither a
///   placeholder nor a pass; the caller fails closed instead.
fn first_frame_placeholder_verdict(
    tree: &UiTree,
    evidence: &FirstFrameEvidence,
) -> (bool, bool) {
    if !first_frame_is_bare_skeleton(tree) {
        return (false, false);
    }
    let moved = if evidence.pixels_compared {
        evidence.pixels_changed
    } else {
        evidence.frames_advanced
    };
    (!moved && evidence.window_spent, moved)
}

/// FB2c: what the launch actually measured, in one sentence.
///
/// Never asserts an unmeasured fact: a frame number is quoted only when the
/// guest reported one, and pixel identity is only claimed when both frames
/// carried a digest.
pub(crate) fn placeholder_evidence_sentence(report: &FirstFrameReport) -> String {
    let mut facts: Vec<String> = Vec::new();
    match (report.frame_number, report.frames_advanced) {
        (Some(number), true) => facts.push(format!("frame_number advanced to {number}")),
        (Some(number), false) => facts.push(format!("frame_number stayed {number}")),
        (None, true) => facts.push("the frame sequence advanced".to_string()),
        (None, false) => facts.push("no newer frame was published".to_string()),
    }
    match (report.pixels_compared, report.pixels_changed) {
        (true, true) => facts.push("the pixels changed".to_string()),
        (true, false) => facts.push("the pixels are identical to the skeleton's".to_string()),
        (false, _) => facts.push("no pixel digest was comparable".to_string()),
    }
    format!(
        "{} frame(s) over {} traversal(s)/{} ms ({} event wait(s), budget ACOS_ART_FIRST_FRAME_MS={} ms, window {} ms spent: {}): {}",
        report.frames,
        report.traversals,
        report.waited_ms,
        report.event_waits,
        report.budget_ms,
        report.window_ms,
        report.window_spent,
        facts.join(", ")
    )
}

/// FB2c: the `PLACEHOLDER_CONTENT` detail for the verify/launch route.
pub(crate) fn placeholder_evidence_detail(report: &FirstFrameReport) -> String {
    format!(
        "launch reached the window skeleton and nothing about the frame moved, so the app's content \
         never mounted ({} node(s), no app text); {}; reason: placeholder-content. Read the guest log \
         for the first UnsatisfiedLinkError / SoLoader / fatal JS line.",
        report.nodes,
        placeholder_evidence_sentence(report)
    )
}

/// G3c: node count of a tree (all windows, all depths).
fn ui_tree_node_count(nodes: &[UiNode]) -> usize {
    nodes
        .iter()
        .map(|node| 1 + ui_tree_node_count(&node.children))
        .sum()
}

/// G3c: does any node carry something an app put on screen (text, a content
/// description or an editor hint)?
fn ui_tree_has_app_text(nodes: &[UiNode]) -> bool {
    nodes.iter().any(|node| {
        !node.text.trim().is_empty()
            || !node.content_desc.trim().is_empty()
            || !node.hint.trim().is_empty()
            || ui_tree_has_app_text(&node.children)
    })
}

/// G3c: the largest tree that can still be *only* the window skeleton.
///
/// Measured on Gaten: the unmounted skeleton (DecorView -> `action_bar_root` ->
/// `android.R.id.content` -> an empty `ReactRootView`, plus the decor's
/// `ViewStub`s) is **10 nodes**, while the painted welcome screen is **243**.
/// 40 leaves room for a window skeleton with dialogs/stubs without ever
/// reaching a mounted app screen.
const FIRST_FRAME_SKELETON_MAX_NODES: usize = 40;

/// GATEFIX (F1): how long the mount wait sleeps between two observations when
/// the guest answered *before* the window ran out.
///
/// G3f1 made the wait event-driven and G3c capped it at a traversal count
/// (`FIRST_FRAME_MAX_ATTEMPTS = 8`). The cap was the bug: on a quiet host eight
/// samples of the guest's *cached* skeleton cost ~0 ms, so the launch concluded
/// `placeholder` before the app ever got CPU time (v0.3.0 gate F1). The window
/// is now the binding budget and this pace only keeps a fast-answering guest
/// (an old launcher, a fake) from burning it in a burst; the placeholder
/// verdict requires the whole window (see `first_frame_placeholder_verdict`).
const FIRST_FRAME_PROBE_PACE_MS: u64 = 25;

/// G3c: at or below this node count a text-free tree is the *bare* window
/// skeleton measured on Gaten (1–10 nodes), so a launch frame that never grew
/// past it is published as a `placeholder` with `meaningful: false`. Kept well
/// below [`FIRST_FRAME_SKELETON_MAX_NODES`]: a dense but text-free screen is
/// waited for, never labelled.
const FIRST_FRAME_PLACEHOLDER_MAX_NODES: usize = 16;

/// G3c/G3f1: the launch's first-frame content budget (`ACOS_ART_FIRST_FRAME_MS`,
/// default 5000 ms).
///
/// G3f1: measured **from the end of the launch's own traversal**, because the
/// wait it bounds is event-driven: it ends as soon as the guest commits the
/// app's next content state (a changed tree after a real draw, plus its settle
/// when the screen goes quiet), and is only fully spent by an app that never
/// gains content at all. The pre-G3f1 value (900 ms, from launch entry) was the
/// binding constraint: two strict re-traversals cost ~1.63 s on Gaten, so the
/// retry landed *before* the first content paint and the launch published a
/// labelled placeholder (E13.8, 9/30 launches). Measured on Gaten in this
/// branch, the RN bundle's first content paint lands 1–3 s after the skeleton,
/// so a smaller budget only reproduces the placeholder.
fn first_frame_budget_ms() -> u64 {
    std::env::var("ACOS_ART_FIRST_FRAME_MS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(5000)
}

/// G3c: is this tree the window skeleton with nothing of the app's mounted yet?
///
/// Deliberately conservative: a tree that carries *any* text/content-description
/// or that has more than [`FIRST_FRAME_SKELETON_MAX_NODES`] nodes is app content
/// and is never delayed or labelled, so a legitimately small screen cannot be
/// mistaken for an unmounted one (and a blank screen that never mounts is
/// labelled `placeholder` instead of being claimed as the app's first frame).
fn first_frame_is_unmounted(tree: &UiTree) -> bool {
    ui_tree_node_count(&tree.nodes) <= FIRST_FRAME_SKELETON_MAX_NODES
        && !ui_tree_has_app_text(&tree.nodes)
}

/// G3c: is this tree the *bare* window skeleton (never gained app content)?
///
/// Only such a frame is labelled `placeholder` / `meaningful: false`; a
/// text-free screen that is bigger than the skeleton is returned as-is, so a
/// legitimately non-textual app is never turned into a failed assertion.
fn first_frame_is_bare_skeleton(tree: &UiTree) -> bool {
    ui_tree_node_count(&tree.nodes) <= FIRST_FRAME_PLACEHOLDER_MAX_NODES
        && !ui_tree_has_app_text(&tree.nodes)
}

/// G3f1: did the guest report this traversal as settled?
///
/// Missing/unknown means settled, which is the pre-existing reading of a reply
/// that carries no settle block (the launch's own traversal is then treated
/// exactly as it always was). A `change-draw` bind is explicitly **not** settled
/// (fresh, not quiet) and says so.
fn tree_settle_settled(settle: &Option<serde_json::Value>) -> bool {
    settle
        .as_ref()
        .and_then(|block| block.get("settled"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true)
}

/// G3f1: the guest's own bind rule for the most recent traversal, if it named
/// one (`strict-event`, `change-draw`, `probe-fresh`, `timeout`).
fn settle_bound(settle: &Option<serde_json::Value>) -> Option<String> {
    settle
        .as_ref()
        .and_then(|block| block.get("bound"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

/// G3f1: did the guest bind this traversal to the app's next **draw commit**
/// after the tree we handed it (`bound:"change-draw"`)?
///
/// That is the launch's content event on a screen that never goes quiet: the
/// tree changed and its draw is stored by C1, while the framework may still be
/// animating. It is a fresh frame (P9's launch contract), never a settled one,
/// and the reply's settle block keeps `settled:false` so no assertion can use it.
fn tree_bound_draw_commit(settle: &Option<serde_json::Value>) -> bool {
    settle
        .as_ref()
        .and_then(|block| block.get("bound"))
        .and_then(serde_json::Value::as_str)
        == Some("change-draw")
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
        let insets = std::env::var("ACOS_WM_INSETS")
            .unwrap_or_else(|_| acos_services::wms::DEFAULT_INSETS_MODE.as_str().to_string());
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
        Self::start_with_apks(apk, &[], artifacts, animations, insets)
    }

    /// MPROC1: start a session that installs `extra_apks` beside the launched
    /// APK. Each extra APK is staged into this session's own tree, bound at its
    /// guest path, installed at its own uid and seeded into the activity
    /// manager, so `PackageManager`/`createPackageContext` (and, later, a second
    /// process) see every package. An empty list is exactly
    /// [`ArtSession::start_with_options`]: no staging, no bind, no install.
    pub fn start_with_apks(
        apk: &Path,
        extra_apks: &[PathBuf],
        artifacts: &Path,
        animations: &str,
        insets: &str,
    ) -> Result<ArtSession, BackendError> {
        Self::start_with_apks_and_session(
            apk,
            extra_apks,
            artifacts,
            animations,
            insets,
            None,
            None,
            None,
            &[],
        )
    }

    /// ZPROD3: start a session that must run on a persistent zygote child
    /// (`ACOS_ART_ZYGOTE=on`). With the flag off (or a disabled host) this is
    /// exactly [`ArtSession::start_with_options`]: the launcher argv, the call
    /// order and the replies are unchanged.
    ///
    /// With the flag on the order is the design's (§2/§4.3/§4.4): prepare the
    /// session dir → spawn the launcher with `ACOS_ZYGOTE_SESSION` → wait for
    /// its `argv` and publish `binds` → `spawn <tag>` on the keeper → only then
    /// read the launcher's ready line (the ready line means the forked child
    /// connected to this session's own sockets). Any zygote failure returns a
    /// structured `ZYGOTE_DEAD`; the launcher is killed and no exec-path session
    /// is ever substituted.
    pub fn start_with_zygote(
        apk: &Path,
        artifacts: &Path,
        animations: &str,
        insets: &str,
        zygote: Option<Arc<ZygoteHost>>,
        session_id: Option<&str>,
    ) -> Result<ArtSession, BackendError> {
        Self::start_with_session(apk, artifacts, animations, insets, zygote, session_id, &[])
    }

    /// ADR-0029 item 4: [`Self::start_with_zygote`] plus the `services:` specs
    /// the session attaches at launch. The specs are forwarded to `acos-art` in
    /// its environment and echoed in the launch reply from the ready line, so a
    /// reply reports what was attached, never what was requested.
    ///
    /// NET3/NET5: `session_id` is the addressable id of the session (the serve
    /// daemon's own id, or `verify-<pid>` for a one-shot run): the module's
    /// relay session is `acos-<id>`, the same rule `acos_net` resolves, so a
    /// client can address the network of the session it holds.
    #[allow(clippy::too_many_arguments)]
    pub fn start_with_session(
        apk: &Path,
        artifacts: &Path,
        animations: &str,
        insets: &str,
        zygote: Option<Arc<ZygoteHost>>,
        session_id: Option<&str>,
        services: &[serde_json::Value],
    ) -> Result<ArtSession, BackendError> {
        Self::start_with_apks_and_session(
            apk,
            &[],
            artifacts,
            animations,
            insets,
            None,
            zygote,
            session_id,
            services,
        )
    }

    /// MPROC1: [`ArtSession::start_with_zygote`] plus a list of extra APKs.
    /// The extras are part of the zygote key: a keeper child's pre-fork heap is
    /// only valid for the package set it was started for, so a session with
    /// different extras never reuses another session's child.
    /// Every constructor funnels here: the one place that builds the
    /// `ZygoteStart`/launcher command and calls [`Self::start_inner`].
    #[allow(clippy::too_many_arguments)]
    pub fn start_with_apks_and_session(
        apk: &Path,
        extra_apks: &[PathBuf],
        artifacts: &Path,
        animations: &str,
        insets: &str,
        idle_kill_seconds: Option<u64>,
        zygote: Option<Arc<ZygoteHost>>,
        session_id: Option<&str>,
        services: &[serde_json::Value],
    ) -> Result<ArtSession, BackendError> {
        let start = zygote.map(|host| ZygoteStart {
            key: zygote_key_of(apk, insets, animations, extra_apks),
            host,
            reuse_tag: None,
        });
        Self::start_inner(
            apk,
            extra_apks,
            artifacts,
            animations,
            insets,
            idle_kill_seconds,
            start,
            session_id,
            services,
        )
    }

    fn start_inner(
        apk: &Path,
        extra_apks: &[PathBuf],
        artifacts: &Path,
        animations: &str,
        insets: &str,
        idle_kill_seconds: Option<u64>,
        zygote: Option<ZygoteStart>,
        session_id: Option<&str>,
        services: &[serde_json::Value],
    ) -> Result<ArtSession, BackendError> {
        // NET4: every session has an id. `acos serve` supplies its own (the id
        // a client addresses); a one-shot run gets one from this process, which
        // is what makes its network addressable at all (`art-<pid>` was a name
        // no client could ever hold).
        let session_id = match session_id {
            Some(id) if !id.is_empty() => id.to_string(),
            _ => format!("verify-{}", std::process::id()),
        };
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
        // Host memory admission (owner directive, 2026-11-15). Every ART guest
        // this CLI starts passes through here: `acos launch`, `acos verify`,
        // `acos serve`/MCP sessions and a `reset` relaunch. There is no queue
        // and no slot file — a launch either has room right now or it fails
        // fast with a structured, retryable `HOST_MEMORY_LOW`. The per-session
        // cgroup cap that `acos-art` applies to the run is unchanged; this only
        // refuses to *start* a guest that would leave the host under 10% of
        // MemTotal available. `ACOS_MEMORY_GUARD=off` disables the check.
        if let Err(denied) = host_mem::check_host() {
            // Carried as a structured protocol failure so the measured numbers
            // reach `error.details` unchanged (CLI JSON and MCP `notes`).
            return Err(BackendError::Protocol {
                code: ErrorCode::HostMemoryLow,
                detail: denied.describe(),
                hint: Some(ErrorCode::HostMemoryLow.hint().to_string()),
                details: Some(denied.details()),
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
            // ADR-0029 item 4: the module specs. The launcher attaches them
            // before the guest boots; an invalid spec is refused there with a
            // structured error instead of a silently defaulted session.
            .env(
                "ACOS_ART_SERVICES",
                serde_json::to_string(&services).unwrap_or_else(|_| "[]".to_string()),
            )
            // The launcher reads this seam (no `acos-art` CLI flag change): a
            // live session instead of a one-shot launch. The guest publishes
            // the read-only view-tree probe the `tree`/`tap` replies carry.
            .env("ACOSLAUNCH_INTERACTIVE", "1")
            .env("ACOSLAUNCH_LIVE_PROBE", "1")
            // The relay session is named `acos-<id>` from this id (`acos_net`
            // resolves the same rule). Always set: a one-shot run publishes its
            // own id, so no session's network is unreachable by construction.
            .env(acos_art::spike_net::SESSION_ID_ENV, &session_id);
        // MPROC1: the extra packages this session installs. Both spellings name
        // the same list: the repeatable flags the launcher parses, and the
        // `ACOSLAUNCH_EXTRA_APKS` env a frozen host-template child reads.
        for extra in extra_apks {
            command.arg("--extra-apk").arg(extra);
        }
        if !extra_apks.is_empty() {
            command.env(
                "ACOSLAUNCH_EXTRA_APKS",
                std::env::join_paths(extra_apks.iter()).map_err(|error| BackendError::Failed {
                    code: acos_protocol::ErrorCode::InvalidArgument,
                    detail: format!(
                        "extra APK paths cannot be forwarded in one environment value: {error}"
                    ),
                })?,
            );
        }
        // MPROC7: the session's idle kill. Both spellings: the launcher flag for
        // the exec path, and the env a frozen host-template child reads
        // (`host_zygote` builds the child's `LaunchOpts` from it).
        if let Some(seconds) = idle_kill_seconds {
            command.arg("--idle-kill-seconds").arg(seconds.to_string());
            command.env("ACOSLAUNCH_IDLE_KILL_SECS", seconds.to_string());
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // ZPROD3 (design §4.3): the session launcher runs in *foreign-guest*
        // mode: it builds its own sandbox, peer, `ServiceHost` and contract, but
        // does not start ART — the keeper forks the child that connects to these
        // sockets. `prepare` boots the keeper on first use and wipes this
        // session's run root/sock dir, so nothing the previous child wrote can
        // survive a reset. Every failure here is `ZYGOTE_DEAD`; there is no
        // fallback to the exec path while the flag is on.
        let mut zygote_prepared: Option<(Arc<ZygoteHost>, acos_art::zygote_prod::ZygoteKey, u32)> =
            None;
        if let Some(start) = zygote.as_ref() {
            if start.host.enabled() {
                let tag = start
                    .host
                    .prepare(&start.key, &binary, apk, start.reuse_tag)
                    .map_err(|error| zygote_dead(&error.to_string()))?;
                let session_dir = start.host.session_dir(&start.key, tag);
                command
                    .env("ACOS_ZYGOTE_SESSION", &session_dir)
                    // Z4: the forked child publishes the read-only tree probe.
                    .env("ACOS_ZYGOTE_PROBE", "1");
                zygote_prepared = Some((Arc::clone(&start.host), start.key.clone(), tag));
            }
        }
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
            command.env(
                "ACOS_DISPLAY_OVERRIDE",
                format!("{w}x{h}@{dpi}/{top}/{bottom}"),
            );
        }
        if std::env::var_os("ACOSLAUNCH_FRAME_WAIT_SECS").is_none() {
            command.env("ACOSLAUNCH_FRAME_WAIT_SECS", frame_wait_secs.to_string());
        }
        // SERVELEAK2/F1: a per-session ownership token in the environment.
        //
        // The teardown may only kill processes this session provably started.
        // A command-line match cannot prove that (two agents verifying
        // `scenarios/counter.yaml` both name `out/counter`), so every process
        // the launcher starts carries this random token; its presence in
        // `/proc/<pid>/environ` is the proof that our launcher started it.
        let session_token = new_session_token(artifacts);
        command.env(SESSION_TOKEN_ENV, &session_token);
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
        // SERVELEAK2/F4: pin the launcher **process** (not its pid number) so
        // liveness checks and the leader's SIGKILL cannot hit a recycled pid.
        let leader_pidfd = open_pidfd(_pid);
        // ZPROD5: the launcher's stderr is drained **before** the zygote contract
        // wait, so a launcher that dies before it publishes `<plan>/<tag>/argv`
        // reports *why* instead of the engine waiting out the 180 s contract
        // timeout with only the keeper's log (measured: `acos serve`'s bench
        // cohort, where the session dir pushed `sock/peer.sock` past `sun_path`
        // and the launcher printed `io: path must be shorter than SUN_LEN` and
        // exited, invisible to the engine).
        let stderr_tail = Arc::new(Mutex::new(VecDeque::new()));
        {
            let tail = Arc::clone(&stderr_tail);
            let stderr = child.stderr.take().expect("piped stderr");
            // Keep the whole launcher stderr in the run's artifacts as well as
            // the bounded tail the replies carry. The launcher's own decisions
            // (spawns, idle kills, spawn failures) and the guest's tee are what
            // explains a process-level failure; an 8 KiB tail of the guest trace
            // is not enough. Bounded at 16 MiB so a chatty guest cannot fill the
            // disk.
            let log_path = artifacts.join("launcher.stderr.log");
            std::thread::spawn(move || {
                let mut file = std::fs::File::create(&log_path).ok();
                let mut written = 0usize;
                let mut reader = BufReader::new(stderr);
                let mut line = String::new();
                // NET3 left a hole here: `acos-art` writes its `stage()` markers to
                // stderr and this drain swallowed them, so the documented
                // `ACOS_ART_STAGE_TIMING=1` stage table never appeared in a
                // `verify`/`launch` transcript (the NET3 report concluded "none
                // fire on this path"). Forward the markers to this process's
                // stderr when the caller asked for them; the tail keeps every
                // line either way.
                let forward_stages = std::env::var_os("ACOS_ART_STAGE_TIMING").is_some();
                loop {
                    line.clear();
                    match reader.read_line(&mut line) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {
                            if forward_stages && line.contains(": stage ") {
                                eprint!("{line}");
                            }
                            if let Some(file) = file.as_mut() {
                                if written < 16 * 1024 * 1024 {
                                    use std::io::Write as _;
                                    let _ = file.write_all(line.as_bytes());
                                    written += line.len();
                                }
                            }
                            let mut tail = tail.lock().unwrap_or_else(|e| e.into_inner());
                            tail.extend(line.bytes());
                            while tail.len() > 8192 {
                                tail.pop_front();
                            }
                        }
                    }
                }
            });
        }
        // ZPROD3 (design §4.3/§4.4): the launcher published `argv` during its
        // own launch; publish this session's `binds` and fork **one** fresh child
        // from the keeper, all before the launcher's ready line (that line means
        // the child connected to this session's sockets). A dead keeper or a
        // missing ack is a structured `ZYGOTE_DEAD` and the launcher is killed —
        // never an exec-path session reported as zygote isolation (§6).
        let mut zygote_facts: Option<ZygoteFacts> = None;
        if let Some((host, key, tag)) = zygote_prepared.as_ref() {
            let tail = Arc::clone(&stderr_tail);
            let mut launcher_gone = || match child.try_wait() {
                Ok(Some(status)) => {
                    let text = {
                        let tail = tail.lock().unwrap_or_else(|e| e.into_inner());
                        let bytes: Vec<u8> = tail.iter().copied().collect();
                        String::from_utf8_lossy(&bytes).trim().to_string()
                    };
                    Some(format!(
                        "the session launcher exited ({status}) before it published the contract \
                         (its stderr: {})",
                        if text.is_empty() { "<empty>" } else { &text }
                    ))
                }
                Ok(None) => None,
                Err(error) => Some(format!(
                    "the session launcher could not be observed: {error}"
                )),
            };
            match host.wait_contract_and_spawn(
                key,
                *tag,
                Duration::from_secs(180),
                &mut launcher_gone,
            ) {
                Ok(spawned) => {
                    zygote_facts = Some(ZygoteFacts::of(
                        &spawned,
                        host.boot_epoch(key).unwrap_or_default(),
                    ));
                }
                Err(error) => {
                    // SAFETY: the launcher was spawned with `process_group(0)`,
                    // so `-_pid` names its group and no other.
                    note_group_signal();
                    unsafe {
                        libc::kill(-(_pid as i32), libc::SIGKILL);
                    }
                    let _ = child.wait();
                    return Err(zygote_dead(&error.to_string()));
                }
            }
        }
        dm6_trace(serde_json::json!({
            "event": "spawn",
            "session": artifacts.display().to_string(),
            "pid": _pid,
            "process_group": _pid,
            "pidfd": leader_pidfd.is_some(),
            "unix_ms": now_unix_ms(),
        }));
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("piped stdout");

        let stderr_tail2 = Arc::clone(&stderr_tail);
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

        let mut session = ArtSession {
            child: Some(child),
            stdin,
            lines,
            stderr_tail: stderr_tail2,
            timeout,
            apk: apk.to_path_buf(),
            artifacts: artifacts.to_path_buf(),
            package: String::new(),
            activity: None,
            width: 1080,
            height: 2400,
            density: 420,
            services: services.to_vec(),
            modules: Vec::new(),
            module_rows: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            boot_image: None,
            stream_socket: None,
            cached: None,
            last_pixel: None,
            seen_seq: 0,
            seq_offset: 0,
            log_offset: 0,
            logs_total_raw: 0,
            crash: None,
            process_group: Some(_pid as i32),
            guest_group: None,
            launcher_pid: Some(_pid as i32),
            session_token,
            leader_pidfd,
            reported_group: Some(_pid as i32),
            proven_empty: false,
            last_input: None,
            input_offset: (0, 0),
            window_offsets: HashMap::new(),
            session_start_ms: 0,
            session_id,
            frame_wait_secs,
            settle_budget,
            settle_polls,
            input_baseline: None,
            diff_baseline: None,
            diff_requested: false,
            settle: SettleStats::default(),
            last_input_timing: None,
            last_phase: None,
            last_input_settle: None,
            settle_mode: "strict",
            observation_settle: "strict",
            last_tree_settle: None,
            last_tree_json: None,
            first_frame_report: None,
            first_frame_bound: None,
            reset_settle: "strict",
            reset_pixels: true,
            next_min_probe_seq: None,
            last_probe_seq: None,
            last_reply_probe_seq: None,
            insets,
            animations,
            extra_apks: extra_apks.to_vec(),
            idle_kill_seconds,
            settle_timeout_ms: None,
            bound_settle: None,
            reset_settle_timeout_ms: None,
            closed: false,
            last_teardown: None,
            zygote: match (zygote_prepared, zygote_facts) {
                (Some((host, key, _tag)), Some(facts)) => Some(ZygoteBinding { host, key, facts }),
                _ => None,
            },
            zygote_previous_child_pid: None,
            relaunch_retry: None,
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
        // SERVELEAK: the guest's own process group, so `shutdown` can kill and
        // prove the *guest*, not just the launcher that spawned it.
        session.guest_group = ready
            .get("guest_process_group")
            .and_then(|v| v.as_i64())
            .and_then(|v| i32::try_from(v).ok())
            .filter(|group| *group > 1);
        if let Some(width) = ready.get("width").and_then(|v| v.as_u64()) {
            session.width = width as u32;
        }
        if let Some(height) = ready.get("height").and_then(|v| v.as_u64()) {
            session.height = height as u32;
        }
        if let Some(density) = ready.get("density").and_then(|v| v.as_u64()) {
            session.density = density as u32;
        }
        // H13b: the boot-image mode and cache outcome of this guest.
        if let Some(boot) = ready.get("boot_image") {
            session.boot_image = Some(boot.clone());
        }
        if let Some(modules) = ready.get("modules").and_then(|v| v.as_array()) {
            session.modules = modules.clone();
        }
        if let Some(package) = ready.get("package").and_then(|v| v.as_str()) {
            session.package = package.to_string();
        }
        if let Some(activity) = ready.get("activity").and_then(|v| v.as_str()) {
            session.activity = Some(activity.to_string());
        }
        // VIEW1a: the session's live-view stream socket (protocol v1). The
        // registry publishes it; a session without one still runs normally.
        if let Some(path) = ready.get("stream_socket").and_then(|v| v.as_str()) {
            if !path.is_empty() {
                session.stream_socket = Some(PathBuf::from(path));
            }
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

    /// NET4: the id this session is addressed by (the `acos serve` id, or the
    /// one-shot run's own id). The relay session is `acos-<id>`, which is what
    /// `acos_net` resolves, so a client can control this session's network.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// The guest's own pid: the process group `acos-art` started for it
    /// (`guest_process_group` on the ready line; the guest is its own group
    /// leader, so the group id is the pid). `None` before a ready line or for a
    /// launcher that does not report one. FB5: `session_list` reports it so the
    /// owning MCP server can say how much memory a session's guest holds.
    pub fn guest_pid(&self) -> Option<u32> {
        self.guest_group
            .and_then(|group| u32::try_from(group).ok())
            .filter(|pid| *pid > 1)
    }

    /// ZPROD3: the zygote child facts this session really runs on, or `None`
    /// with `ACOS_ART_ZYGOTE` off. Facts are the keeper's observed acks, never
    /// host guesses, so a reply built from them cannot claim a zygote it did not
    /// get.
    pub fn zygote_facts(&self) -> Option<&ZygoteFacts> {
        self.zygote.as_ref().map(|binding| &binding.facts)
    }

    /// The child pid the most recent zygote reset killed.
    pub fn zygote_previous_child_pid(&self) -> Option<u32> {
        self.zygote_previous_child_pid
    }

    /// ZPROD3: the zygote reply fields (`isolation`, `child_pid`,
    /// `previous_child_pid`, `run_root`, `zygote_boot_epoch`, `memory_scope`).
    /// `None` when the session is not a zygote child, so the flag-off reply is
    /// unchanged byte for byte.
    pub fn zygote_reply_fields(&self) -> Option<serde_json::Map<String, serde_json::Value>> {
        reply_fields(
            self.zygote.as_ref().map(|binding| &binding.facts),
            self.zygote_previous_child_pid,
        )
    }

    /// H13b: the `boot_image` record the guest's ready line carried.
    #[must_use]
    pub fn boot_image(&self) -> Option<&serde_json::Value> {
        self.boot_image.as_ref()
    }

    /// ADR-0029 item 4: the modules the guest's ready line reported attached.
    #[must_use]
    pub fn modules(&self) -> &[serde_json::Value] {
        &self.modules
    }

    /// ADR-0029 item 4: the `services:` specs this session was launched with.
    #[must_use]
    pub fn services(&self) -> &[serde_json::Value] {
        &self.services
    }

    /// ADR-0029 item 4: one `acos_services` operation (list/status/attach/
    /// detach) on this live guest. The launcher's peer thread owns the service
    /// table, so this is a command, not a local mutation.
    pub fn service_op(
        &mut self,
        request: serde_json::Value,
    ) -> Result<serde_json::Value, BackendError> {
        let mut request = request;
        request["cmd"] = serde_json::json!("services");
        let reply = self.command(request)?;
        if let Some(modules) = reply.get("modules").and_then(|v| v.as_array()) {
            self.modules = modules.clone();
            self.publish_module_rows();
        }
        Ok(reply)
    }

    /// The live module rows (the verdict cites these after the run).
    #[must_use]
    pub fn module_rows(&self) -> std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>> {
        std::sync::Arc::clone(&self.module_rows)
    }

    fn publish_module_rows(&self) {
        if let Ok(mut rows) = self.module_rows.lock() {
            *rows = self.modules.clone();
        }
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
        // ROT8b: a task-finished observation is the newest state (no pixels);
        // otherwise the last frame that really drew.
        let source = match self.cached.as_ref() {
            Some(gone) if gone.gone => Some(gone),
            _ => self.last_pixel.as_ref(),
        };
        let Some(cached) = source else {
            return Ok(None);
        };
        let previous = self.seen_seq.min(cached.seq.saturating_sub(1));
        cached.observed(previous).map(Some)
    }

    /// REL3E: attach the resolved-target/hit-test block to the action's own
    /// delivery record, so `verify` publishes the same `target` the MCP reply
    /// carries instead of re-deriving it.
    pub fn attach_input_target(&mut self, target: serde_json::Value) {
        if let Some(input) = self.last_input.as_mut() {
            input.target = Some(target);
        }
    }

    /// The bound tree of the most recent completed traversal, without issuing a
    /// command.
    pub fn bound_tree(&self) -> Option<UiTree> {
        self.cached.as_ref().map(|frame| frame.tree.clone())
    }

    /// Ensure a bound tree exists: reuse the last completed traversal, or take
    /// one now if the session has not completed any. Never invents a frame.
    pub(crate) fn ensure_bound_tree(&mut self) -> Result<UiTree, BackendError> {
        self.require_cached_tree()
    }

    /// SE2c: one **fresh** tree-only traversal for target resolution under
    /// `settle: content`.
    ///
    /// The bound tree of the previous step is not position evidence for the next
    /// input: the content signature never hashes `bounds` (a view translated by an
    /// animation reports a rect that drifts ~1 px per fresh traversal), so a
    /// layout shift between steps would compute the tap point from a stale rect
    /// and land on a neighbour.
    ///
    /// This takes exactly one `fast` traversal (`pixels:false`, no quiet window -
    /// a forever-animating screen would never satisfy one) with the one-shot
    /// `min_probe_seq` floor set above every sample this session has consumed, so
    /// the guest either answers with a provably newer sample or refuses with
    /// `NO_FRESH_TREE`. No cached tree is used, and no sample is silently reused.
    /// The input itself still binds its own content-settled frame afterwards.
    pub(crate) fn fresh_input_tree(&mut self) -> Result<UiTree, BackendError> {
        let floor = self.last_probe_seq.unwrap_or(0);
        self.next_min_probe_seq = Some(floor);
        let result = self.request_tree_with("fast", false);
        // The floor is one-shot; never leave it armed for the next request.
        self.next_min_probe_seq = None;
        result?;
        if self.last_reply_probe_seq.is_none() {
            return Err(BackendError::Protocol {
                code: ErrorCode::NoFreshFrame,
                detail: format!(
                    "settle content: the target-resolution traversal returned no `probe_seq`, so \
                     it cannot be proven newer than the sample at or below {floor}; the target is \
                     not resolved on an unproven tree"
                ),
                hint: Some(
                    "A content-mode input resolves its selector on a provably fresh traversal. \
                     Re-run on a guest that reports `probe_seq`."
                        .to_string(),
                ),
                details: Some(serde_json::json!({
                    "reason": "content_resolution_unproven",
                    "min_probe_seq": floor,
                })),
            });
        }
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

    /// VIEW1a: the live-view stream socket this session's ready line reported.
    #[must_use]
    pub fn stream_socket(&self) -> Option<&Path> {
        self.stream_socket.as_deref()
    }

    /// VIEW1a: the launcher (`acos-art`) pid of the current guest, so the live
    /// registry can name it and a reader can tell a live entry from a stale one.
    #[must_use]
    pub fn launcher_pid(&self) -> Option<u32> {
        self.process_group.map(|pid| pid as u32)
    }

    /// VIEW1a: the session's display size as the guest booted it.
    #[must_use]
    pub const fn display_size(&self) -> (u32, u32) {
        (self.width, self.height)
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
        self.settle_mode = match mode {
            "fast" => "fast",
            "content" => "content",
            _ => "strict",
        };
    }

    /// P12b: the per-request settle deadline (`settle_timeout_ms`). `None` keeps
    /// the engine default. A timeout is still an observation: the guest returns
    /// the latest tree and frame identity with `settled:false`.
    pub fn set_settle_timeout(&mut self, timeout_ms: Option<u64>) {
        self.settle_timeout_ms = timeout_ms;
    }

    /// SE2: the settle rule the bound frame was proven by, as reported.
    pub fn bound_settle(&self) -> Option<&serde_json::Value> {
        self.bound_settle.as_ref()
    }

    /// REL3A: the settle rule the next observation binds with
    /// (`"strict"` default, `"content"` for a screen that animates forever).
    ///
    /// It is deliberately separate from [`Self::settle_mode`]: an input's rule
    /// (`strict`/`fast`/`content`) and an observation's rule are chosen per step
    /// by the caller, and a `settle: content` tap must not silently make every
    /// later `frame()` content-settled (or the reverse). Only `strict` and
    /// `content` are observation rules; `fast` is navigation-only and never
    /// backs an observation, so anything else falls back to `strict`.
    pub fn set_observation_settle(&mut self, mode: &str) {
        self.observation_settle = if mode == "content" { "content" } else { "strict" };
    }

    /// The observation settle rule in force (`"strict"`/`"content"`).
    pub fn observation_settle(&self) -> &'static str {
        self.observation_settle
    }

    /// SE2: ask the next input action for the content-stable rule instead of
    /// the strict (or fast) one. Mutually exclusive with `set_settle_mode`.
    pub fn set_content_settle(&mut self, content: bool) {
        if content {
            self.settle_mode = "content";
        } else if self.settle_mode == "content" {
            self.settle_mode = "strict";
        }
    }

    /// P12: the animation choice this session was launched with (`"on"`/`"off"`).
    pub fn animations(&self) -> &'static str {
        self.animations
    }

    /// MPROC1: the extra APKs this session installed beside the launched one,
    /// as the caller named them (launch-time; a `reset` relaunch reinstalls
    /// exactly this list).
    #[must_use]
    pub fn extra_apks(&self) -> &[PathBuf] {
        &self.extra_apks
    }

    /// MPROC7: the idle kill this session launched with (`None` = the
    /// launcher's default 30 s, `Some(0)` = disabled).
    #[must_use]
    pub fn idle_kill_seconds(&self) -> Option<u64> {
        self.idle_kill_seconds
    }

    /// MPROC1: run one session control op (`acos_service_ctl`).
    ///
    /// `module` names a session service (`intents` today); `op` is that
    /// service's own operation and `args` its arguments. The launcher answers
    /// with its own record, so the reply is the service's, not a translation:
    /// an unknown module/op is a structured `UNSUPPORTED_ANDROID_API`/
    /// `BAD_REQUEST` from the session, never a local guess.
    pub fn ctl(
        &mut self,
        module: &str,
        op: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, BackendError> {
        let request = serde_json::json!({
            "cmd": "ctl",
            "module": module,
            "op": op,
            "args": args,
        });
        let reply = self.command(request)?;
        if reply.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
            let code = reply
                .get("code")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("INTERNAL");
            let detail = reply
                .get("error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("the session refused the control op");
            return Err(BackendError::Failed {
                code: match code {
                    "BAD_REQUEST" => acos_protocol::ErrorCode::InvalidArgument,
                    "UNSUPPORTED_ANDROID_API" => acos_protocol::ErrorCode::UnsupportedAndroidApi,
                    _ => acos_protocol::ErrorCode::Internal,
                },
                detail: detail.to_string(),
            });
        }
        Ok(reply)
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

    /// The child's exit status once its session is known to be lost.
    ///
    /// The kernel closes a dying process's pipes before the process becomes
    /// reapable, so a stdout EOF can be observed while `try_wait` still says
    /// "running". Poll for a bounded moment so a SIGKILLed launcher is
    /// classified from its real status instead of falling through to INTERNAL.
    fn lost_child_exit_status(&mut self) -> Option<std::process::ExitStatus> {
        const GRACE: Duration = Duration::from_millis(500);
        let deadline = std::time::Instant::now() + GRACE;
        loop {
            if let Some(status) = self.child_exit_status() {
                return Some(status);
            }
            if self.child.is_none() || std::time::Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
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
            .lost_child_exit_status()
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
            // SE2: the guest knows `strict` and `fast` only. A content-stable
            // step delivers the action with the guest's fast rule (no quiet
            // window) and proves content stability on the host's own fresh
            // traversals below; the report it publishes says `content`.
            let guest_settle = if self.settle_mode == "content" {
                "fast"
            } else {
                self.settle_mode
            };
            object.insert("settle".into(), serde_json::json!(guest_settle));
            // SE2b: in content mode the step's `settle_timeout_ms` is the budget
            // for the *content window* (the rule the caller asked for), not for
            // the guest's fast delivery. Forwarding it would make the guest's
            // navigation settle wait the whole content budget on a screen that
            // never goes quiet (a 20 s tap on the reference screen). The guest
            // keeps its own fast deadline; the host bounds the window.
            if self.settle_mode != "content" {
                if let Some(timeout) = self.settle_timeout_ms {
                    object.insert("settle_timeout_ms".into(), serde_json::json!(timeout));
                }
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
                // SE2: a content-stable input is only settled once the host has
                // proven the tree content unchanged across its own traversals.
                // The guest's fast block is replaced by the content report, so
                // the step (and every observation it backs) records the rule
                // that actually justified it.
                if self.settle_mode == "content" {
                    let report = self.content_settle()?;
                    self.last_input_settle = Some(report.clone());
                    self.bound_settle = Some(report.clone());
                    if let Some(input) = self.last_input.as_mut() {
                        input.settle = Some(report);
                    }
                }
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
                // SE2b: the recovery path (guest refused the settle, the host
                // recovered from its own captures) must apply the same content
                // rule. Before this, a content action that recovered returned a
                // strict/fast record and only failed closed by accident.
                if self.settle_mode == "content" {
                    let report = self.content_settle()?;
                    self.last_input_settle = Some(report.clone());
                    self.bound_settle = Some(report.clone());
                    if let Some(input) = self.last_input.as_mut() {
                        input.settle = Some(report);
                    }
                }
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
        // One constructor for both paths: the host facts (including REL3I3's
        // window-removal evidence) are read in exactly one place.
        self.last_input = Some(delivery(reply, visual_change));
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
        // ROT8: the guest proved the app's task finished (its last Activity
        // is destroyed, no window root remains). Nothing draws then, so the
        // reply carries no frame: the observation is the empty window forest
        // bound to the LAST COMMITTED pixels (the display keeps its last frame;
        // ACOS has no launcher to draw). `settle.task_finished` and
        // `pixels_changed:false` say so; no frame is invented.
        // ROT8b: the destroyed window's PNG is NOT rebound under the new
        // frame sequence; the gone observation carries no PNG and `last_pixel`
        // stays what it was (the last frame that really drew).
        let window_gone = reply.get("window_gone").and_then(|v| v.as_bool()) == Some(true)
            && reply.get("tree").is_some()
            && reply.get("png").is_none();
        let md5 = reply
            .get("md5")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_default();
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
        if reply.get("tree").is_some() {
            if let Some(settle) = reply.get("settle") {
                self.bound_settle = Some(settle.clone());
            }
        }
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
            gone: window_gone,
        };
        if frame.png.is_some() {
            self.last_pixel = Some(frame.clone());
        }
        self.cached = Some(frame);
        // FB2c: the placeholder label describes the frame it was bound to. A
        // later traversal that binds app content clears it, so a sticky report
        // can never block a launch/screenshot of an app that has since mounted.
        if self
            .first_frame_report
            .as_ref()
            .is_some_and(|report| report.placeholder)
            && self
                .cached
                .as_ref()
                .is_some_and(|frame| !first_frame_is_bare_skeleton(&frame.tree))
        {
            self.first_frame_report = None;
        }
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
        //
        // SE2b: and it carries no settle record either, so it must not inherit
        // the previous reply's (a content-settled record would otherwise outlive
        // the pixels it never described).
        self.bound_settle = None;
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
        self.request_tree_params(settle, pixels, None, None)
    }

    /// G3f1: a tree request that waits for the guest's **next** tree state.
    ///
    /// `baseline_tree` is the raw probe tree this engine already holds; the
    /// guest only reports settled when the sample's tree differs from it, so the
    /// reply is the app's next committed state (a real draw the guest itself
    /// observed), not the same skeleton again. `timeout_ms` bounds the wait from
    /// now; the reply still carries the latest tree with `settled:false` when it
    /// expires, exactly as the P12b rule says.
    fn request_tree_after(
        &mut self,
        settle: &str,
        pixels: bool,
        baseline_tree: serde_json::Value,
        timeout_ms: u64,
    ) -> Result<(), BackendError> {
        self.request_tree_params(settle, pixels, Some(baseline_tree), Some(timeout_ms))
    }

    fn request_tree_params(
        &mut self,
        settle: &str,
        pixels: bool,
        baseline_tree: Option<serde_json::Value>,
        timeout_ms: Option<u64>,
    ) -> Result<(), BackendError> {
        let started = Instant::now();
        let t0 = m5_now_us();
        let mut request = serde_json::json!({"cmd":"tree", "settle": settle});
        let settle_timeout = timeout_ms.or(self.settle_timeout_ms);
        if let Some(timeout) = settle_timeout {
            request["settle_timeout_ms"] = serde_json::json!(timeout);
        }
        if !pixels {
            request["pixels"] = serde_json::json!(false);
        }
        // SE2b: one-shot sample floor. The guest's fast tree refuses to answer
        // with a sample at or below it (`NO_FRESH_TREE`), which is what proves a
        // content-settle traversal is a *new* observation.
        if let Some(floor) = self.next_min_probe_seq.take() {
            request["min_probe_seq"] = serde_json::json!(floor);
        }
        if let Some(baseline) = baseline_tree {
            request["baseline_tree"] = baseline;
        }
        let reply = self.command(request)?;
        // SE2b: read the sample identity from this reply only. A reply without
        // `probe_seq` cannot be proven fresh, and the content settle refuses it.
        self.last_reply_probe_seq = reply.get("probe_seq").and_then(|v| v.as_u64());
        if let Some(seq) = self.last_reply_probe_seq {
            self.last_probe_seq = Some(self.last_probe_seq.map_or(seq, |seen| seen.max(seq)));
        }
        self.last_tree_settle = reply.get("settle").cloned();
        self.last_tree_json = reply.get("tree").cloned();
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
        // SERVELEAK2/F4: a teardown that proved the tree empty cleared every
        // group id, so there is nothing left to signal — and, more importantly,
        // nothing left for a recycled id to be confused with.
        if self.proven_empty {
            self.stdin = None;
            return;
        }
        if let Some(mut child) = self.child.take() {
            let pid = child.id() as i32;
            dm6_trace(serde_json::json!({
                "event": "drop.kill",
                "session": self.artifacts.display().to_string(),
                "pid": pid,
                "pidfd": self.leader_pidfd.is_some(),
                "unix_ms": now_unix_ms(),
            }));
            // F4: SIGKILL the leader **through its pidfd** when there is one, so
            // the signal names the process this session spawned and never a
            // recycled pid number.
            match self.leader_pidfd.as_ref() {
                Some(fd) => {
                    pidfd_send_signal(fd, libc::SIGKILL);
                }
                None => {
                    let _ = child.kill();
                }
            }
            // The leader is still **unreaped** here (`child` was not waited
            // yet), so its pid and group id cannot have been recycled: `-pid`
            // still names this session's group and nothing else.
            // SAFETY: the child leads its own process group
            // (`process_group(0)`) and is still a zombie of this process.
            unsafe {
                note_group_signal();
                libc::kill(-pid, libc::SIGKILL);
            }
            let _ = child.wait();
        }
        // SERVELEAK: the guest has its own group (`acos-art` spawns it with
        // `process_group(0)`), so a drop that only killed the launcher's group
        // left bwrap/dalvikvm64 running. Kill everything this session provably
        // owns, always.
        //
        // Only *proved* ownership is killed here: `*self = fresh` drops the
        // previous incarnation after `reset` has already spawned the
        // replacement, which names the same artifacts directory, so a
        // directory-wide sweep would kill the guest that just started.
        self.sigkill_group();
        self.stdin = None;
    }

    /// Every process group this session owns, guarded.
    ///
    /// The launcher's group and the guest's group are distinct by design
    /// (`crates/acos-art/src/launcher.rs` spawns the guest with
    /// `process_group(0)`). A group id is only ever signalled when it is a real
    /// positive pid-like value and **not** this process's own group, so a
    /// malformed or stale record can never signal the daemon's (or a caller's)
    /// group.
    fn own_groups(&self) -> Vec<i32> {
        // SAFETY: `getpgrp` takes no arguments and cannot fail.
        let own = unsafe { libc::getpgrp() };
        let mut groups: Vec<i32> = Vec::new();
        for group in [self.process_group, self.guest_group].into_iter().flatten() {
            if group > 1 && group != own && !groups.contains(&group) {
                groups.push(group);
            }
        }
        groups
    }

    /// Pids this session can **prove** it owns (SERVELEAK2/F1).
    ///
    /// Three independent proofs, and nothing else:
    ///
    /// 1. membership of a process group this session recorded (the launcher's
    ///    and the guest's);
    /// 2. the `/proc` ppid chain from the launcher pid this session spawned
    ///    (walked while the launcher lives);
    /// 3. [`SESSION_TOKEN_ENV`] carrying this session's random token — the only
    ///    proof that survives the launcher's death and a guest reparented to
    ///    init, and the one a *different* session (even one running the same
    ///    scenario into a path-identical artifacts directory) can never carry.
    ///
    /// A command-line substring is deliberately **not** a proof: two agents
    /// running `acos verify counter.yaml` in different worktrees both have
    /// `out/counter` on their launcher's command line, and treating that as
    /// ownership let one teardown SIGKILL the other's live guest.
    fn owned_pids(&self) -> Vec<u32> {
        let me = std::process::id();
        let mut pids: Vec<u32> = Vec::new();
        // Proof 1 — membership of a process group this session recorded. This is
        // the widest proof and the one that makes the teardown *complete*: a
        // guest whose environment was cleared (`bwrap --clearenv`) and whose
        // parent already died is still a member of the group this session
        // started, so it must be named in the teardown proof rather than
        // reported as a clean tree over a live process.
        for group in self.own_groups() {
            pids.extend(processes_in_group(group));
        }
        pids.extend(self.provable_pids());
        pids.retain(|pid| *pid != me);
        pids.sort_unstable();
        pids.dedup();
        pids
    }

    /// Pids this session can prove it started **independently of a group id**
    /// (SERVELEAK2/F1 proofs 2 and 3).
    ///
    /// These, and only these, are what a signal is aimed at: a recorded group id
    /// can be recycled by another agent once its leader is reaped (F4), so a
    /// group is signalled only while it holds one of these pids (see
    /// [`Self::owned_live_groups`]). A member that cannot be attributed this way
    /// is still *reported* as a survivor — never silently dropped, never killed
    /// on a guess.
    fn provable_pids(&self) -> Vec<u32> {
        let mut pids: Vec<u32> = Vec::new();
        if let Some(launcher) = self.launcher_pid {
            pids.extend(descendants_of(launcher as u32));
        }
        if !self.session_token.is_empty() {
            let needle = format!("{}={}", SESSION_TOKEN_ENV, self.session_token);
            for pid in all_pids() {
                if environ_has(pid, &needle) {
                    pids.push(pid);
                }
            }
        }
        pids
    }

    /// The process groups that currently hold at least one **provably owned**
    /// pid (SERVELEAK2/F1, F4).
    ///
    /// `kill(-group)` is only ever issued for one of these. A recorded group id
    /// whose members are no longer ours (the leader was reaped and the id was
    /// recycled by another agent's `setsid`/`bwrap`) therefore dies here
    /// without being signalled at all: nobody in it is ours to kill.
    fn owned_live_groups(&self) -> Vec<i32> {
        // SAFETY: `getpgrp` takes no arguments and cannot fail.
        let own = unsafe { libc::getpgrp() };
        let mut groups: Vec<i32> = Vec::new();
        for pid in self.provable_pids() {
            if let Some((_ppid, pgrp, _alive)) = proc_ppid_pgrp(pid) {
                if pgrp > 1 && pgrp != own && !groups.contains(&pgrp) {
                    groups.push(pgrp);
                }
            }
        }
        groups.sort_unstable();
        groups
    }

    /// SIGKILL every process this session provably owns. Idempotent.
    ///
    /// SERVELEAK2/F1: the old shape also SIGKILLed every process whose command
    /// line *mentioned* the session's artifacts directory. That is not
    /// ownership: a second agent verifying the same scenario into a
    /// path-identical directory was killed by this session's teardown. Only the
    /// three proofs in [`Self::owned_pids`] are used now.
    fn reap_session_leftovers(&self) -> usize {
        // A signal is aimed only at a pid this session can prove it started
        // (F1: not at a command-line name, not at a recycled group id).
        let owned: Vec<u32> = self
            .provable_pids()
            .into_iter()
            .filter(|pid| *pid != std::process::id())
            .collect();
        for pid in &owned {
            // SAFETY: `pid` is a live process this session proved it owns; a
            // process that died in between answers `ESRCH`.
            unsafe {
                libc::kill(*pid as i32, libc::SIGKILL);
            }
        }
        owned.len()
    }

    /// Members of every process group the session owns, plus any other process
    /// it provably started and still has alive.
    ///
    /// SERVELEAK: the launcher and the guest are in *different* groups
    /// (`crates/acos-art/src/launcher.rs` gives the guest its own with
    /// `process_group(0)`), so a proof that only walked the launcher's group
    /// reported "clean" while bwrap/dalvikvm64 kept running.
    fn group_survivors(&self) -> Vec<u32> {
        self.owned_pids()
    }

    /// SIGKILL every process this session provably owns (idempotent; `ESRCH` is
    /// fine).
    fn sigkill_group(&self) {
        // Groups first: `kill(-group)` reaches a whole tree in one call. Only
        // groups with an owned member are signalled (F1/F4).
        for group in self.owned_live_groups() {
            dm6_trace(serde_json::json!({
                "event": "sigkill.group",
                "session": self.artifacts.display().to_string(),
                "process_group": group,
                "unix_ms": now_unix_ms(),
            }));
            // SAFETY: every id here holds at least one process this session
            // proved it owns, and is never this process's own group; `-group`
            // names that group and no other.
            unsafe {
                note_group_signal();
                libc::kill(-group, libc::SIGKILL);
            }
        }
        // Then every owned process whose group was not signalled above (a child
        // that called `setsid`, or a guest whose group id was recorded before a
        // relaunch). Guarded by ownership, never by a name match.
        self.reap_session_leftovers();
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
        //
        // ZPROD6: a **zygote** session has no lifecycle to wait for. Its `child`
        // here is the session launcher — a foreign-guest shim that owns the
        // sockets and the `ServiceHost` and starts no ART at all (the app child
        // was forked inside the keeper). Measured with `DM6_RESET_TRACE` on the
        // serve path: the shim never exits after `close` (`exit_ms=300`,
        // `reaped_before_deadline=false` on every ON stop), so the 300 ms grace
        // bought nothing and every ON `acos_destroy` cost 307 ms against the OFF
        // arm's 13 ms — and every ON reset paid it too, inside its stop phase.
        // The real death proof for a zygote session is the keeper's `reaped`
        // ack (`kill(-child, SIGKILL)` + `wait4`, written by `ZygoteApp.z4KillOne`),
        // which `close`/`reset` obtain right after this stop. So: signal, then
        // prove — never wait out a lifecycle the shim cannot deliver.
        let grace = Duration::from_millis(teardown_grace_ms_for(self.zygote.is_some()));
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
        // SERVELEAK: a survivor is not reported until it has been **killed**.
        // The old code returned `Err(PROCESS_TREE_DIRTY)` here without ever
        // signalling the guest group, so a destroy could fail *and* leave the
        // guest running. Every group the session owns is SIGKILLed first (the
        // guest's own group included), then the proof is re-taken.
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
                // A process may have forked between the scan and the signal;
                // re-signal the whole tree before re-taking the proof.
                self.sigkill_group();
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
        //
        // SERVELEAK2/F4: once the teardown has *proven* the tree empty the group
        // ids are dropped. Holding them any longer is a hazard, not a
        // convenience: the kernel may hand the id to another agent's
        // `setsid`/`bwrap`, and a later `Drop` or repeat `shutdown` would then
        // SIGKILL a stranger. Every later signal path derives its target from
        // ownership (`owned_live_groups`), so a cleared record is also the
        // cheapest correct state.
        // SERVELEAK2/F1: a command-line mention of this session's directory by
        // a process this session does **not** own (another agent verifying the
        // same scenario). Reported in the trace, never signalled: a name match
        // is not ownership.
        let foreign_mentions = self.foreign_artifacts_mentions();
        self.proven_empty = true;
        self.reported_group = self.process_group.or(self.reported_group);
        self.process_group = None;
        self.guest_group = None;
        self.launcher_pid = None;
        self.session_token = String::new();
        self.leader_pidfd = None;
        dm6_trace(serde_json::json!({
            "event": "stop",
            "session": self.artifacts.display().to_string(),
            "process_group": self.reported_group,
            "path": path.as_str(),
            "zygote": self.zygote.is_some(),
            "grace_ms": grace.as_millis() as u64,
            "exit_ms": exit_ms,
            "foreign_mentions": foreign_mentions,
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

    /// S3b: the bounded relaunch retry this session carries, if any. The reset
    /// reply states it so a host-transport retry is never a silent pass.
    pub fn relaunch_retry(&self) -> Option<&serde_json::Value> {
        self.relaunch_retry.as_ref()
    }

    /// GL1: processes this session **provably** started and that are still
    /// alive (SERVELEAK2/F1).
    ///
    /// The process-group proof above only covers the groups the engine recorded;
    /// this is the *session*-wide check the destroy reply reports. It is derived
    /// from ownership (groups, the launcher's descendant chain, the inherited
    /// session token), never from a command-line match: a second agent verifying
    /// the same scenario into a path-identical artifacts directory is not this
    /// session's leftover and must not be named or killed as one.
    pub fn session_leftovers(&self) -> Vec<u32> {
        if self.proven_empty {
            return Vec::new();
        }
        self.owned_pids()
    }

    /// Processes whose command line **names** this session's artifacts
    /// directory but which this session does not own (SERVELEAK2/F1: report
    /// only, never a signal target).
    ///
    /// This is the case a substring match got wrong: another agent running the
    /// same scenario from another directory. Canonicalising the directory and
    /// requiring a whole argv element to be it (or sit under it) makes the
    /// report precise; ownership is still what decides a kill.
    fn foreign_artifacts_mentions(&self) -> Vec<u32> {
        let owned = self.owned_pids();
        processes_mentioning(&self.artifacts)
            .into_iter()
            .filter(|pid| !owned.contains(pid))
            .collect()
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
        // Every group the session owns, not just the launcher's (SERVELEAK).
        let groups = self.own_groups();
        groups.is_empty() || groups.iter().all(|group| group_is_empty(*group))
    }

    /// Kill every process this session provably owns and report what is still
    /// alive afterwards. Called by `acos_destroy` (SERVELEAK): a destroy that
    /// names a leftover must kill it, never only report it. (SERVELEAK2/F1: the
    /// leftover set is the ownership set, so a stranger that merely names the
    /// same directory is neither killed nor reported as a survivor.)
    pub fn reap_leftovers(&self) -> Vec<u32> {
        self.reap_session_leftovers();
        let mut alive = self.session_leftovers();
        let deadline = Instant::now() + Duration::from_millis(500);
        let mut step = Duration::from_millis(1);
        while !alive.is_empty() && Instant::now() < deadline {
            std::thread::sleep(step);
            step = (step * 2).min(Duration::from_millis(10));
            self.sigkill_group();
            alive = self.session_leftovers();
        }
        alive
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
        if self.proven_empty {
            return TeardownReport {
                process_group: self.reported_group,
                survivors: Vec::new(),
                verified: true,
            };
        }
        let Some(group) = self.process_group else {
            return TeardownReport::unverified();
        };
        TeardownReport {
            process_group: Some(group),
            survivors: self.group_survivors(),
            verified: true,
        }
    }

    /// A teardown report from a `/proc` scan already taken by the caller.
    fn teardown_report_with(&self, survivors: Vec<u32>) -> TeardownReport {
        if self.proven_empty {
            return TeardownReport {
                process_group: self.reported_group,
                survivors: Vec::new(),
                verified: true,
            };
        }
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

/// S3b: is this failed relaunch a **host transport/host-resource** failure
/// rather than the app's own startup? Only the named host-class strings are
/// accepted; a generic `GUEST_EXITED` (a real app wall) never matches.
fn relaunch_transport_reason(error: &BackendError, artifacts: &Path) -> Option<String> {
    let mut text = error.to_string();
    if let Some(doc) = crate::failure::read_session_exit(artifacts) {
        text.push('\n');
        text.push_str(&doc.to_string());
    }
    let lower = text.to_ascii_lowercase();
    for (needle, label) in [
        (
            "vsync thread:",
            "host thread limit: vsync thread could not spawn",
        ),
        (
            "display event receiver",
            "host frame clock: DisplayEventReceiver init failed",
        ),
        (
            "ipcsetdatareference",
            "host transport: malformed binder reply parcel",
        ),
        (
            "unsupported type object",
            "host transport: malformed binder object table",
        ),
        (
            "adding window failed",
            "host window add failed (remote reply malformed)",
        ),
        ("host_panic", "host launcher panic (harness/host resource)"),
    ] {
        if lower.contains(needle) {
            return Some(label.to_string());
        }
    }
    None
}

/// Copy the failed relaunch's small evidence aside before the retry replaces
/// `session-exit.json` in the shared artifacts directory. Returns the directory.
fn preserve_failed_relaunch(
    artifacts: &Path,
    error: &BackendError,
    reason: &str,
) -> Option<String> {
    let dest = artifacts.join("relaunch-retry-1");
    if std::fs::create_dir_all(&dest).is_err() {
        return None;
    }
    let frames = artifacts.join("art-frames");
    for name in [
        "session-exit.json",
        "live-session.json",
        "session-timing.json",
        "native-crash.json",
        "native-crash.txt",
        "session.json",
        "argv",
        "unsupported.jsonl",
    ] {
        let src = frames.join(name);
        if src.is_file() {
            let _ = std::fs::copy(&src, dest.join(name));
        }
    }
    let doc = serde_json::json!({
        "utc_ms": now_unix_ms(),
        "reason": reason,
        "error": error.to_string(),
        "note": "S3b: the first relaunch failed for a host transport/host-resource reason; \
                 the same APK was relaunched once. The copied files are the first attempt's \
                 own evidence (session-exit.json holds the guard line and the guest log).",
    });
    let path = dest.join("relaunch-retry.json");
    let _ = std::fs::write(&path, serde_json::to_vec_pretty(&doc).unwrap_or_default());
    let _ = std::fs::write(
        artifacts.join("relaunch-retry.json"),
        serde_json::to_vec_pretty(&doc).unwrap_or_default(),
    );
    Some(path.display().to_string())
}

impl ArtSession {
    /// A tree-only traversal with an explicit P1 settle mode. `fast` is for
    /// navigation/inspection and is labelled in the reply's `settle` block;
    /// `strict` is the default evidence mode.
    pub fn tree_with_settle(&mut self, settle: &'static str) -> Result<UiTree, BackendError> {
        let settle = match settle {
            "fast" => "fast",
            "content" => "content",
            _ => "strict",
        };
        if settle == "content" {
            self.content_settle()?;
            return self.require_cached_tree();
        }
        self.request_tree_only(settle)?;
        self.require_cached_tree()
    }

    /// SE2: prove the UI tree content stable, then report it (`settle: content`).
    ///
    /// Every traversal is a **fast, tree-only** request (`settle:fast`,
    /// `pixels:false`): the guest answers with a probe sample that is
    /// *strictly newer* than the floor this call passes
    /// (`min_probe_seq`), or refuses with `NO_FRESH_TREE`. That freshness floor
    /// is what makes the window an observation of the screen rather than of the
    /// guest's latest (possibly pre-request) sample: two traversals can never be
    /// the same sample, and a sample that predates the window never counts.
    ///
    /// Only the traversal's **content signature** is compared
    /// ([`acos_verify::settle::content_signature`]: class, resource-id, text,
    /// content-desc, hint, bounds, visible-bounds, visible/enabled/focused/
    /// clickable/scrollable/checked/selected, window, package + the tree's
    /// package/truncated/text_available). Pixels are never consulted.
    ///
    /// A different signature restarts the window. Settled means the same
    /// signature was seen across at least two **distinct fresh** traversals
    /// spanning at least the window ([`acos_verify::settle::DEFAULT_CONTENT_WINDOW_MS`],
    /// configurable upward with `ACOS_CONTENT_SETTLE_WINDOW_MS`, never below
    /// 300 ms). Running out of [`DEFAULT_CONTENT_SETTLE_TIMEOUT_MS`] (or the
    /// step's `settle_timeout_ms`) is a `TIMEOUT` carrying the last signature in
    /// `details.settle`, never a pass - including when the signature changes on
    /// every traversal (a countdown), because the deadline is checked
    /// independently of the window's traversal count.
    ///
    /// Once settled, one **pixel-bound** traversal is taken so the settled
    /// content is also the bound frame (the verifier binds a frame, tree and
    /// PNG together). That traversal is only accepted when its tree hashes to
    /// the settled signature; if the screen moved meanwhile the window simply
    /// continues. Its pixels are never pixel evidence (`pixel_evidence:false`).
    pub fn content_settle(&mut self) -> Result<serde_json::Value, BackendError> {
        use acos_verify::settle::{content_window_ms, ContentProbeWindow};
        let requested = std::env::var("ACOS_CONTENT_SETTLE_WINDOW_MS")
            .ok()
            .and_then(|value| value.trim().parse::<u64>().ok());
        let window_ms = content_window_ms(requested);
        let timeout_ms = self
            .settle_timeout_ms
            .unwrap_or(DEFAULT_CONTENT_SETTLE_TIMEOUT_MS);
        if std::env::var_os("ACOS_CONTENT_TRACE").is_some() {
            eprintln!(
                "content_settle: step timeout {:?} -> {timeout_ms} ms",
                self.settle_timeout_ms
            );
        }
        let started = Instant::now();
        // The floor: the newest sample this session already consumed. The first
        // traversal of the window must be newer than it, so the pre-request /
        // pre-input sample can never be part of the verdict.
        let floor_probe_seq = self.last_probe_seq.unwrap_or(0);
        let mut window = ContentProbeWindow::new(window_ms, floor_probe_seq);
        let caller_timeout = self.settle_timeout_ms;
        let result = self.content_settle_loop(&mut window, started, timeout_ms);
        self.settle_timeout_ms = caller_timeout;
        self.next_min_probe_seq = None;
        match result {
            Ok(report) => {
                self.last_tree_settle = Some(report.clone());
                self.bound_settle = Some(report.clone());
                Ok(report)
            }
            Err(error) => {
                // A timeout is still an observation of *why*: keep the last
                // signature, the traversal count and the sample range.
                if let BackendError::Protocol {
                    details: Some(details),
                    ..
                } = &error
                {
                    self.last_tree_settle = details.get("settle").cloned();
                }
                Err(error)
            }
        }
    }

    fn content_settle_loop(
        &mut self,
        window: &mut acos_verify::settle::ContentProbeWindow,
        started: Instant,
        timeout_ms: u64,
    ) -> Result<serde_json::Value, BackendError> {
        use acos_verify::settle::{content_signature, ContentAttempt, SIGNATURE_FIELDS};
        // At least one traversal is always attempted: the deadline is enforced
        // *after* a traversal (see `ContentAttempt`), so a 1 ms budget is an
        // honest "not settled", never a verdict with no sample at all.
        loop {
            // One fresh, tree-only traversal. A per-traversal deadline keeps a
            // single slow walk from eating the whole window.
            let remaining = timeout_ms.saturating_sub(started.elapsed().as_millis() as u64);
            self.settle_timeout_ms = Some(remaining.clamp(50, 750));
            self.next_min_probe_seq = Some(window.next_floor());
            self.request_tree_with("fast", false)?;
            let probe_seq = self.last_reply_probe_seq;
            let tree = self.require_cached_tree()?;
            let signature = content_signature(&tree);
            let at_ms = started.elapsed().as_millis() as u64;
            match window.attempt(signature, probe_seq, at_ms, timeout_ms) {
                ContentAttempt::Settled => {}
                ContentAttempt::Continue => continue,
                ContentAttempt::TimedOut(reason) => {
                    return Err(self.content_timeout(window, started, reason));
                }
            }
            // Bind the settled content to pixels: one traversal with a capture
            // whose tree must still hash to the settled signature.
            self.settle_timeout_ms = Some(1);
            // This traversal may reuse the settled sample (it only has to be
            // no older than it); the *content* is what is being bound.
            self.next_min_probe_seq = None;
            if let Err(error) = self.request_tree_with("strict", true) {
                // No pixels for the bound frame: the content verdict stands but
                // there is nothing to bind it to, so fail closed.
                return Err(error);
            }
            let bound_probe = self.last_reply_probe_seq;
            let bound_tree = self.require_cached_tree()?;
            let bound_signature = content_signature(&bound_tree);
            if !window.binds_settled_content(bound_signature, bound_probe) {
                // The screen moved while the pixels were captured: keep waiting.
                // The deadline is enforced at the top of the loop.
                continue;
            }
            let frame_seq = self.cached.as_ref().map(|frame| frame.seq);
            return Ok(serde_json::json!({
                "mode": "content",
                "settled": true,
                "bound": "content-stable",
                "window_ms": window.window_ms(),
                "traversals": window.fresh_traversals(),
                "signature": format!("{:016x}", bound_signature),
                "frame_seq": frame_seq,
                "probe_seq": bound_probe,
                "probe_seq_first": window.first_probe_seq(),
                "probe_seq_floor": window.floor_probe_seq(),
                "elapsed_ms": started.elapsed().as_millis() as u64,
                "pixel_evidence": false,
                "signature_fields": SIGNATURE_FIELDS,
                "signature_tree_fields": acos_verify::settle::SIGNATURE_TREE_FIELDS,
                "freshness": "each traversal's probe_seq strictly increased (guest-side                               min_probe_seq floor + host-side check); the pre-window sample                               never counts",
            }));
        }
    }

    fn content_timeout(
        &self,
        window: &acos_verify::settle::ContentProbeWindow,
        started: Instant,
        reason: &'static str,
    ) -> BackendError {
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let traversals = window.fresh_traversals();
        let detail = if reason == acos_verify::settle::CONTENT_PROBE_UNPROVEN {
            format!(
                "settle content: a traversal could not be proven fresh (no `probe_seq`, or a                  sample at/below the floor {}), so the content window has no evidence;                  after {traversals} proven traversal(s) / {elapsed_ms} ms",
                window.next_floor()
            )
        } else {
            format!(
                "settle content: the UI tree content signature was still changing after                  {traversals} proven traversal(s) / {elapsed_ms} ms (window {} ms); the screen                  is not content-stable, so nothing it shows is settled",
                window.window_ms()
            )
        };
        BackendError::Protocol {
            code: ErrorCode::Timeout,
            detail,
            hint: Some(
                "Content settle needs the tree's text/content-desc/bounds/state to stop                  changing for the window, on traversals that are provably fresh samples. If                  the screen updates a value continuously (a countdown or a progress                  percentage), no settle rule can prove it: use `settle: fast` for navigation                  only, then assert once you know the value."
                    .to_string(),
            ),
            details: Some(serde_json::json!({
                "reason": reason,
                "settle": {
                    "mode": "content",
                    "settled": false,
                    "bound": "content-timeout",
                    "window_ms": window.window_ms(),
                    "traversals": traversals,
                    "signature": window.signature().map(|signature| format!("{signature:016x}")),
                    "probe_seq": window.last_probe_seq(),
                    "probe_seq_first": window.first_probe_seq(),
                    "probe_seq_floor": window.floor_probe_seq(),
                    "unproven_replies": window.unproven(),
                    "elapsed_ms": elapsed_ms,
                    "pixel_evidence": false,
                    "signature_fields": acos_verify::settle::SIGNATURE_FIELDS,
                },
                "traversals": traversals,
                "window_ms": window.window_ms(),
                "reason": reason,
            })),
        }
    }

    /// FB2c item 6: a tree traversal that **captures** the frame in the same
    /// request when the caller wants a screenshot, so the tree and the PNG of one
    /// reply are one frame (`screenshot.frame_seq == tree frame_seq`). Without a
    /// screenshot it is exactly [`ArtSession::tree_with_settle`] (tree-only).
    pub fn tree_with_settle_pixels(
        &mut self,
        settle: &'static str,
        pixels: bool,
    ) -> Result<UiTree, BackendError> {
        let settle = if settle == "fast" { "fast" } else { "strict" };
        self.request_tree_with(settle, pixels)?;
        self.require_cached_tree()
    }

    /// G3c: how the most recent launch bound its first frame (serve publishes
    /// it in the launch reply, so a placeholder is visible and never silent).
    pub(crate) fn first_frame_report(&self) -> Option<FirstFrameReport> {
        self.first_frame_report.clone()
    }

    /// FB2: is the bound launch frame still the bare window skeleton?
    ///
    /// True only when the launch waited its whole content budget and the frame
    /// never grew past the skeleton **and** nothing about the frame moved (see
    /// [`first_frame_placeholder_verdict`]). A screenshot of such a frame is not
    /// a screenshot of the app, so the session verbs refuse it instead of
    /// returning splash pixels.
    ///
    /// FB2c: the label is not sticky. Every later traversal that binds a frame
    /// with content clears it (see [`ArtSession::absorb`]), so a second `launch`, a
    /// `reset` or a screenshot after the app mounted is judged on the current
    /// frame, never on a report from the failed launch.
    pub(crate) fn placeholder_frame(&self) -> bool {
        self.placeholder_frame_report().is_some()
    }

    /// FB2c: the placeholder report, when the **current** bound frame is still
    /// the placeholder (the reply publishes it; `None` once content was seen).
    pub(crate) fn placeholder_frame_report(&self) -> Option<&FirstFrameReport> {
        let report = self.first_frame_report.as_ref()?;
        if !report.placeholder {
            return None;
        }
        match self.cached.as_ref() {
            Some(frame) => {
                if report.frame_seq.is_some() && report.frame_seq != Some(frame.seq) {
                    // A newer frame is bound: its content decides, not the
                    // launch report. `absorb` clears the report on content; this
                    // is the same rule for a frame bound by another path.
                    if first_frame_is_bare_skeleton(&frame.tree) {
                        return Some(report);
                    }
                    return None;
                }
                Some(report)
            }
            None => Some(report),
        }
    }

    /// FB2: the PNG artifact the session's current frame is bound to.
    ///
    /// The bound traversal's own PNG (the normal case: a strict tree traversal
    /// captures). The returned path is the launcher's artifact — the SHOTZ
    /// encoder already wrote it, so a caller copies the compressed bytes and
    /// never re-encodes.
    ///
    /// FB2c item 6: never a *newer* frame than the tree in the same reply. This
    /// used to fall back to `capture_frame()`, so a `pixels:false` tree reply got
    /// a screenshot whose `frame_seq` was newer than the tree's and the two
    /// disagreed about what was on screen. A tree without its own pixels is now
    /// a structured refusal.
    pub(crate) fn screenshot_source(
        &mut self,
    ) -> Result<(PathBuf, u64, String, u64), BackendError> {
        if let Some(cached) = self.cached.as_ref() {
            if let Some(artifact) = cached.png.as_ref() {
                if artifact.frame_seq == cached.seq {
                    return Ok((
                        artifact.path.clone(),
                        cached.seq,
                        cached.md5.clone(),
                        artifact.size,
                    ));
                }
            }
            return Err(live_frame_unavailable(format!(
                "the bound tree's frame (frame_seq {}) carries no pixel artifact of its own, so a \
                 screenshot would have to be a *different* frame than the tree in this reply; \
                 refusing to pair them. Request the tree with pixels (the default strict \
                 traversal) and ask for the screenshot again",
                cached.seq
            )));
        }
        Err(live_frame_unavailable(
            "no frame is bound to the session, so there is nothing to screenshot",
        ))
    }

    /// W58/FB2: the bound frame's display-space origin, in pixels.
    ///
    /// Measured from the probe tree (`bounds_on_screen` minus window-local
    /// `bounds`), the same offset the input channel already applies. A PNG
    /// covers `[display_origin, display_origin + png size)` of the display, so
    /// `png_y = display_y - display_origin.y`; publishing it is what stops an
    /// agent from mis-tapping a screenshot by the status-bar inset.
    pub(crate) fn display_origin(&self) -> (i32, i32) {
        self.input_offset
    }

    /// FB2: the producer frame number of the bound frame, when the reply
    /// carried one. A placeholder launch is reported with it, because a number
    /// that never advances is what "the app never mounted" looks like from the
    /// frame store.
    pub(crate) fn last_frame_number(&self) -> Option<u64> {
        self.cached.as_ref().and_then(|frame| frame.frame_number)
    }

    /// G3f1: the settle verdict of the frame the last launch bound
    /// (`(settled, bound)`), so serve publishes `result.settle` for that frame
    /// rather than for the pre-launch traversal.
    pub(crate) fn first_frame_bound(&self) -> Option<(bool, Option<String>)> {
        self.first_frame_bound.clone()
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
    ///
    /// G3c: a launch frame that is still the *window skeleton* is re-read (see
    /// [`ArtSession::bind_first_frame`]) so the first frame is the app's content
    /// frame, not the empty window that exists a moment before it mounts.
    pub(crate) fn ensure_launch_tree(
        &mut self,
        settle: &'static str,
        pixels: bool,
    ) -> Result<UiTree, BackendError> {
        if self.cached.is_some() {
            return self.require_cached_tree();
        }
        let settle = match settle {
            "fast" => "fast",
            "content" => "content",
            _ => "strict",
        };
        if settle == "content" {
            // SE2: bind the app's content frame with the guest's fast rule, then
            // prove the tree content stable on the host. No PNG is claimed for a
            // content-bound launch (the pixels may still be animating).
            //
            // SE2c: the reply must carry the **content-settled** tree. The fast
            // bind above only proves the app mounted; its skeleton-or-not state is
            // not what this launch publishes. `content_settle` binds the settled
            // content to its own frame, so that is the tree both the reply and the
            // `first_frame` evidence have to describe.
            let (_pre_settle, report) = self.bind_first_frame("fast", false, false)?;
            self.content_settle()?;
            let tree = self.require_cached_tree()?;
            self.first_frame_report = Some(report.describes_tree(&tree));
            return Ok(tree);
        }
        let (tree, report) = self.bind_first_frame(settle, pixels, true)?;
        self.first_frame_report = Some(report);
        Ok(tree)
    }

    /// FB2c: the frame identity the placeholder verdict compares.
    ///
    /// The tree's own identity (`frame_number`/`frame_seq`) is always available;
    /// the pixel digest only when the traversal captured one. A bare-skeleton
    /// traversal that did not capture is captured here: the pixel comparison is
    /// what keeps a text-free **mounted** screen (SurfaceView/GL/image-only) from
    /// being called a placeholder, so the one extra capture is paid only on the
    /// path where the verdict is genuinely in doubt.
    fn verdict_identity(&mut self, tree: &UiTree) -> FrameIdentity {
        let mut identity = frame_identity(self.cached.as_ref());
        if first_frame_is_bare_skeleton(tree) && identity.md5.is_none() {
            if let Ok(captured) = self.capture_frame() {
                if !captured.md5.is_empty() {
                    identity.md5 = Some(captured.md5.clone());
                }
            }
        }
        identity
    }

    /// G3c/G3f1: bind a launch's first frame, waiting past an unmounted window
    /// skeleton.
    ///
    /// One traversal is the normal case (`settle` already waits for the frame
    /// the request produced) and an app whose first frame already carries
    /// content pays **nothing** extra: no second request is ever sent.
    ///
    /// When that traversal returns the window skeleton — no text/content-
    /// description anywhere and at most [`FIRST_FRAME_SKELETON_MAX_NODES`] nodes
    /// — the app's own mounting work is still in flight (measured on Gaten: 10
    /// nodes at the launch frame, 243 two seconds later). The launch then waits
    /// for an **event**, not a traversal count and not a fixed number of samples:
    /// it sends the current tree back as `baseline_tree` and the guest's strict
    /// settle rule only accepts a sample whose tree differs from it (one real
    /// draw, its own confirmation window, no pending framework work — the same
    /// signal the launch's own traversal uses), bounded by the
    /// `ACOS_ART_FIRST_FRAME_MS` window (default 5000 ms, measured from the end
    /// of the launch's own traversal).
    ///
    /// GATEFIX (F1): the mount wait is a **change** wait for every settle rule.
    /// The caller's rule (`content`) is proven *after* the app mounted; the
    /// question here is only "did the guest commit the app's own frame yet?". A
    /// `fast` first traversal has no gate, and the old wait inherited that: its
    /// eight samples of the guest's cached skeleton cost ~0 ms, so a launch
    /// concluded `placeholder` before the app could draw (v0.3.0 gate F1, 2/32
    /// matrix negatives). Two rules follow from that and are enforced here:
    ///
    /// * the wait is bounded by the **window**, never by a traversal count, and
    ///   a guest that answers before the window has run out (an old launcher, a
    ///   fake, a timeout that returned early) is simply observed again after
    ///   [`FIRST_FRAME_PROBE_PACE_MS`];
    /// * `placeholder` is only claimed once the skeleton was observed for that
    ///   whole window (see [`first_frame_placeholder_verdict`]). A wait that
    ///   ended early (guest error) yields neither a placeholder nor a pass.
    ///
    /// Every guarantee of the pre-G3f1 wait is preserved:
    ///
    /// * a frame that never gains content is still published with
    ///   `placeholder: true` and `meaningful: false` (fail-closed, never a
    ///   claim, never an assertion's evidence);
    /// * a wait that does not settle (an endlessly repainting screen, a guest
    ///   error, the budget) rolls the frame **and** its settle verdict back to
    ///   the last settled traversal, so the wait can never turn a passing launch
    ///   into a `TIMEOUT`;
    /// * the launch step's P9 contract is untouched: the caller restores the
    ///   pre-launch settle block, so the launch step is judged exactly as before.
    fn bind_first_frame(
        &mut self,
        settle: &'static str,
        pixels: bool,
        warn_placeholder: bool,
    ) -> Result<(UiTree, FirstFrameReport), BackendError> {
        let mut traversals = 0usize;
        let mut waited_ms = 0u64;
        let mut event_waits = 0usize;

        // 1. The launch's own traversal. Its failure is the launch's failure,
        // exactly as before.
        let started = Instant::now();
        let first = self.request_tree_with(settle, pixels);
        traversals += 1;
        waited_ms += started.elapsed().as_millis() as u64;
        first?;
        let mut tree = self.require_cached_tree()?;
        // FB2c: the frame the launch's own traversal bound is the baseline for
        // the placeholder evidence. A skeleton tree alone is not evidence of an
        // unmounted app (a custom View / SurfaceView / GL / image-only / Flutter
        // screen is text-free too), so the verdict compares the frame identity
        // and the pixels across the content wait.
        let baseline_identity = self.verdict_identity(&tree);
        let budget_ms = first_frame_budget_ms();
        // FB2c item 2: an unsettled first traversal is **not** an early return.
        // It used to return `one_traversal` (waited_ms 0, placeholder from the
        // bare skeleton), so an animating splash answered a placeholder verdict
        // before the content budget ever ran. It is the rollback baseline here
        // and the wait below still gets its budget.
        let mut settled_binding = (
            self.require_cached_frame()?.clone(),
            self.last_tree_settle.clone(),
        );

        // 2. The mount wait. Only an unmounted skeleton reaches it.
        let mut wait_error: Option<BackendError> = None;
        let mut window_spent = false;
        if first_frame_is_unmounted(&tree) {
            let deadline = Instant::now() + Duration::from_millis(budget_ms);
            loop {
                if !first_frame_is_unmounted(&tree) {
                    break;
                }
                let now = Instant::now();
                if now >= deadline {
                    // The app had its whole window and the guest never committed
                    // anything else: that is what a placeholder is.
                    window_spent = true;
                    break;
                }
                let remaining = deadline.saturating_duration_since(now);
                let budget = match self.settle_timeout_ms {
                    // A caller's own settle deadline is a ceiling on the wait.
                    Some(caller) => (remaining.as_millis() as u64).min(caller),
                    None => remaining.as_millis().max(1) as u64,
                };
                // GATEFIX (F1): hand the guest the tree it just published, so
                // the request waits for the app's *next committed state* whatever
                // the caller's settle rule is. A `fast` request ignores a
                // baseline, so the gated wait asks with `strict` (tree-only when
                // the launch is tree-only); the caller's own rule is proven on
                // the mounted tree afterwards.
                let baseline = self.last_tree_json.clone();
                let wait_settle: &'static str = if baseline.is_some() { "strict" } else { settle };
                let gated = baseline.is_some();
                let started = Instant::now();
                let result = match baseline {
                    Some(baseline) => {
                        self.request_tree_after(wait_settle, pixels, baseline, budget)
                    }
                    None => self.request_tree_params(wait_settle, pixels, None, Some(budget)),
                };
                traversals += 1;
                if gated {
                    event_waits += 1;
                }
                waited_ms += started.elapsed().as_millis() as u64;
                if let Err(error) = result {
                    // The guest stopped answering. Keep the last settled binding
                    // (never a new failure mode) but do not pretend the window
                    // ran: the verdict below fails closed instead.
                    wait_error = Some(error);
                    break;
                }
                let settled_now = tree_settle_settled(&self.last_tree_settle);
                tree = self.require_cached_tree()?;
                if settled_now {
                    settled_binding = (
                        self.require_cached_frame()?.clone(),
                        self.last_tree_settle.clone(),
                    );
                }
                // G3f1: a draw-committed frame is accepted as the launch frame
                // (P9's contract is a *fresh* frame, and the guest's settle
                // verdict stays in the reply as `settled:false`).
                if !first_frame_is_unmounted(&tree) {
                    break;
                }
                // Still the skeleton: keep observing the rest of the window.
                // A guest that answered before the window ran out (an early
                // timeout, an old launcher, a fake) is paced, so a burst of
                // immediate samples can never decide the verdict.
                let now = Instant::now();
                if now >= deadline {
                    window_spent = true;
                    break;
                }
                let pause = deadline
                    .saturating_duration_since(now)
                    .min(Duration::from_millis(FIRST_FRAME_PROBE_PACE_MS));
                std::thread::sleep(pause);
            }
        }

        // FB2c: the evidence is what the *latest* traversal saw, taken before the
        // rollback below can restore an older bound frame (an animating skeleton
        // that never settles is rolled back to the baseline, which would make
        // "nothing moved" true by construction).
        let observed_identity = self.verdict_identity(&tree);

        // 3. Keep the settled binding as the launch frame whenever the last
        // traversal did not leave a usable one of its own (a settle timeout or a
        // guest error). A `change-draw` bind *is* usable and stays.
        let mut settled = tree_settle_settled(&self.last_tree_settle);
        let mut bound = settle_bound(&self.last_tree_settle);
        if !settled && !tree_bound_draw_commit(&self.last_tree_settle) {
            self.cached = Some(settled_binding.0.clone());
            self.last_tree_settle = settled_binding.1;
            tree = settled_binding.0.tree.clone();
            settled = tree_settle_settled(&self.last_tree_settle);
            bound = settle_bound(&self.last_tree_settle);
        }
        let current_identity = self.verdict_identity(&tree);
        let evidence = frame_evidence(
            &baseline_identity,
            &observed_identity,
            traversals,
            window_spent,
        );
        let (placeholder, suspect) = first_frame_placeholder_verdict(&tree, &evidence);
        if !placeholder && !suspect && first_frame_is_unmounted(&tree) && !evidence.window_spent {
            // GATEFIX (F1): a window skeleton the wait could not observe for the
            // whole window is neither a placeholder (unproven) nor a pass. The
            // guest error that ended the wait is the honest answer; without one,
            // the launch still fails closed rather than claim a first frame it
            // never observed.
            if let Some(error) = wait_error {
                return Err(error);
            }
            return Err(BackendError::Protocol {
                code: ErrorCode::Timeout,
                detail: format!(
                    "launch bound only the window skeleton and the first-frame window ({} ms) did \
                     not run out, so neither the app's content nor a placeholder can be claimed",
                    budget_ms
                ),
                hint: Some(
                    "Re-run the launch; a window that is not observed for the whole \
                     ACOS_ART_FIRST_FRAME_MS window is not evidence of an unmounted app."
                        .to_string(),
                ),
                details: Some(serde_json::json!({ "reason": "first_frame_window_unproven" })),
            });
        }
        self.first_frame_bound = Some((settled, bound.clone()));
        let report = FirstFrameReport {
            traversals,
            waited_ms,
            event_waits,
            settled,
            bound,
            nodes: ui_tree_node_count(&tree.nodes),
            placeholder,
            suspect,
            frame_seq: current_identity.seq,
            frame_number: current_identity.frame_number,
            frames: evidence.frames,
            frames_advanced: evidence.frames_advanced,
            pixels_compared: evidence.pixels_compared,
            pixels_changed: evidence.pixels_changed,
            budget_ms,
            window_ms: budget_ms,
            window_spent: evidence.window_spent,
        };
        if suspect {
            eprintln!(
                "acos serve: launch first frame is a text-free skeleton ({} nodes) but the frame \
                 moved ({} frame(s), identity advanced: {}, pixels compared: {}, pixels changed: \
                 {}); reported as placeholder_suspect, never blocked",
                report.nodes,
                report.frames,
                report.frames_advanced,
                report.pixels_compared,
                report.pixels_changed
            );
        }
        if report.placeholder && warn_placeholder {
            eprintln!(
                "acos serve: launch first frame is a window skeleton ({} nodes, no app text) \
                 after {} traversal(s)/{} ms ({} event wait(s), budget {} ms, window spent: {}); \
                 nothing about the frame moved (identity advanced: {}, pixels compared: {}, pixels \
                 changed: {}), so it is reported as a placeholder, never as app content",
                report.nodes,
                report.traversals,
                report.waited_ms,
                report.event_waits,
                report.budget_ms,
                report.window_spent,
                report.frames_advanced,
                report.pixels_compared,
                report.pixels_changed
            );
        }
        // FB2c: the report is session state (serve publishes it, the screenshot
        // verbs read it, `reset` replaces it). Storing it here means every route
        // that binds a first frame — serve, verify, reset — keeps it honest.
        self.first_frame_report = Some(report.clone());
        Ok((tree, report))
    }

    /// G3f1: keep the evidence of a launch whose guest died **before its first
    /// frame**, and classify it in the reply.
    ///
    /// A pre-fix Gaten run lost 1/60 launches to `GUEST_EXITED` exit 1 before
    /// any frame with no evidence kept (E13.8, unclassified). The launcher does
    /// write `session-exit.json` (guest exit status, logd tail, stdout/stderr
    /// tails, native-crash record) — but only as it tears down, i.e. *after* the
    /// `GUEST_EXITED` reply the engine already classified, so the reply could
    /// carry `session_exit: null`. This closes and reaps the launcher first
    /// (bounded, idempotent), then reads the record, writes a consolidated
    /// `launch-failure.json` next to it under the session's artifacts, and
    /// returns the facts an envelope can carry.
    ///
    /// Nothing here invents a cause: a status without a crash record is
    /// reported as exactly that (`class: "guest-exited-before-first-frame"`).
    pub(crate) fn keep_launch_failure_evidence(
        &mut self,
        error: &BackendError,
    ) -> serde_json::Value {
        // Bounded close: it delivers the lifecycle, kills the group if needed
        // and reaps the launcher, which is what makes its record readable.
        let _ = self.close();
        let out = self.artifacts.join("art-frames");
        let exit_doc = crate::failure::read_session_exit(&self.artifacts);
        let protocol = error.to_protocol_error();
        let child = self.lost_child_exit_status();
        let facts = crate::failure::prelaunch_exit_facts(
            exit_doc.as_ref(),
            child.and_then(|status| status.code()),
            {
                use std::os::unix::process::ExitStatusExt;
                child.and_then(|status| status.signal())
            },
        );
        let evidence_files = {
            let mut files = serde_json::Map::new();
            for name in [
                "session-exit.json",
                "live-session.json",
                "native-crash.json",
                "native-crash.txt",
                "session-timing.json",
                "unsupported.jsonl",
            ] {
                let path = out.join(name);
                if path.is_file() {
                    files.insert(
                        name.to_string(),
                        serde_json::json!(path.display().to_string()),
                    );
                }
            }
            files
        };
        let logd_tail = exit_doc
            .as_ref()
            .and_then(|doc| doc.get("logd_tail"))
            .and_then(|value| value.as_array())
            .map(|lines| {
                let start = lines.len().saturating_sub(12);
                lines[start..].to_vec()
            })
            .unwrap_or_default();
        let doc = serde_json::json!({
            "utc_ms": crate::failure::host_ms_now(),
            "route": "launch",
            "stage": "before-first-frame",
            "error_code": protocol.code.as_str(),
            "error": protocol.message,
            "session_exit_path": crate::failure::session_exit_path(&self.artifacts)
                .display().to_string(),
            "classification": facts,
            "logd_tail": logd_tail,
            "session_exit": exit_doc.as_ref().map(|doc| {
                crate::failure::session_exit_facts_for(doc, &self.artifacts)
            }),
            "evidence_files": evidence_files.clone(),
        });
        let kept = out.join("launch-failure.json");
        let _ = std::fs::create_dir_all(&out);
        if serde_json::to_vec_pretty(&doc)
            .ok()
            .and_then(|bytes| std::fs::write(&kept, bytes).ok())
            .is_none()
        {
            eprintln!(
                "acos serve: cannot write the launch-failure evidence at {}",
                kept.display()
            );
        }
        serde_json::json!({
            "kept_path": kept.display().to_string(),
            "kept": kept.is_file(),
            "session_exit_path": crate::failure::session_exit_path(&self.artifacts)
                .display().to_string(),
            "classification": facts,
            "logd_tail": logd_tail,
            "evidence_files": evidence_files,
        })
    }

    /// P9: choose the settle mode and pixel capture the next `reset` binds its
    /// relaunched guest's tree with (serve's `acos_reset` arguments).
    pub(crate) fn set_reset_bind(
        &mut self,
        settle: &'static str,
        pixels: bool,
        timeout_ms: Option<u64>,
    ) {
        // SE2b: only fast/strict can bind a reset frame. `serve::reset` refuses
        // `content` before it gets here; this is the second, explicit guard so a
        // future caller cannot silently downgrade it to strict.
        debug_assert!(
            settle != "content",
            "acos_reset cannot bind a content-stable frame"
        );
        self.reset_settle = if settle == "fast" { "fast" } else { "strict" };
        self.reset_pixels = pixels;
        self.reset_settle_timeout_ms = timeout_ms;
    }
}

impl ArtSession {
    /// FB3: ask the next input command's reply for a settled tree diff.
    pub fn set_diff_requested(&mut self, requested: bool) {
        self.diff_requested = requested;
    }

    /// FB3: whether the current action's reply carries `changes`.
    pub fn diff_requested(&self) -> bool {
        self.diff_requested
    }

    /// FB3: the settled tree diff of the action that just ran.
    ///
    /// `before` is the tree snapshotted before the input was injected and
    /// `after` is the tree the action's own settle bound -- both were already in
    /// the session, so this reads and compares, nothing more. With no pre-action
    /// tree the reply says `baseline: "none"`; no diff is ever invented.
    pub fn tree_changes(&self) -> acos_protocol::TreeChanges {
        let after = self.cached.as_ref();
        let frames = acos_protocol::DiffFrames {
            before: self.diff_baseline.as_ref().map_or(0, |b| b.seq),
            after: after.map_or(0, |frame| frame.seq),
        };
        let activity = self.activity.as_deref().unwrap_or("");
        match (self.diff_baseline.as_ref(), after) {
            (Some(baseline), Some(after)) => acos_protocol::TreeChanges::diff(
                Some(&baseline.tree),
                &after.tree,
                frames,
                activity,
            ),
            // A tree is required on both sides. Without an `after` tree the
            // action bound no observation, so the honest answer is "no
            // baseline" rather than a diff against nothing.
            _ => acos_protocol::TreeChanges::none(frames, activity),
        }
    }
}

/// REL3A: refuse a capture whose bound frame was only content-settled.
///
/// `settle: content` proves the UI tree's text/nodes/state stopped changing; it
/// says nothing about the pixels, which may still be animating. A capture is
/// pixel evidence, so it may never be bound by a content record. The executor
/// resets the observation rule to strict before a capture and `frame()` is
/// one-shot; this refuses the remaining case (a caller that asked for content
/// and then captured) with the same `content_settle_pixel` reason the pixel
/// assertions use.
fn content_capture_refusal(settle: Option<&serde_json::Value>) -> Option<BackendError> {
    let settle = settle?;
    if settle.get("mode").and_then(serde_json::Value::as_str) != Some("content") {
        return None;
    }
    Some(BackendError::Protocol {
        code: ErrorCode::UnsupportedAssertion,
        detail: "capture: the frame was bound by `settle: content` (UI tree content stable), \
                 which is not pixel evidence - the pixels may still be animating"
            .to_string(),
        hint: Some(
            "A capture needs a pixel-settled frame: use `settle: strict` (the default) and let \
             the screen stop animating, or assert on the UI tree (assert_text/assert_visible/\
             assert_state) which content settle may back."
                .to_string(),
        ),
        details: Some(serde_json::json!({
            "reason": "content_settle_pixel",
            "step_kind": "capture",
            "settle": settle,
            "pixel_evidence": false,
        })),
    })
}

impl SessionBackend for ArtSession {
    fn engine(&self) -> Engine {
        Engine::Art
    }

    fn modules(&self) -> Vec<serde_json::Value> {
        self.modules.clone()
    }

    /// ADR-0029 item 4: the `acos_services`/`acos_service_ctl` command, forwarded
    /// to the launcher's peer thread that owns the module registry. Without this
    /// forward every `service_ctl` step is `unsupported` (the trait default),
    /// which would read as "this engine cannot" instead of the module's answer.
    ///
    /// The forwarded reply is the launcher's own: `{op, id, ctl_op, value, ...}`
    /// for a control op, `{attached, modules}` for attach/detach/list/status. The
    /// inherent [`ArtSession::service_op`] refreshes this adapter's `modules`
    /// rows from it, so a live attach/detach is visible to the next launch
    /// reply without a second query.
    fn service_op(
        &mut self,
        request: serde_json::Value,
    ) -> Result<serde_json::Value, BackendError> {
        ArtSession::service_op(self, request)
    }

    /// The live module rows, so a verdict cites the session's final module
    /// state (a detached module's honest row) instead of the launch snapshot.
    fn module_rows_handle(
        &self,
    ) -> Option<std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>> {
        Some(self.module_rows())
    }

    fn bound_tree(&self) -> Option<UiTree> {
        ArtSession::bound_tree(self)
    }

    fn attach_input_target(&mut self, target: serde_json::Value) {
        ArtSession::attach_input_target(self, target);
    }

    fn mode(&self) -> Mode {
        // This engine launches a real Activity; `render-app` (offline preview)
        // is a different command and never reaches this adapter.
        Mode::Live
    }

    fn display_size(&self) -> Option<(u32, u32)> {
        Some(ArtSession::display_size(self))
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
            // REL3S: a real interpolated MotionEvent stream on the live input
            // channel (DOWN, MOVEs at the refresh cadence, UP), the same
            // delivery path a tap uses. Scroll/fling physics are the app's own.
            swipe: true,
            // ROT4: a real display rotation through the guest's
            // `ActivityThread` configuration path (WMS DisplaySpec + the
            // manifest's `configChanges` decide relaunch vs
            // `onConfigurationChanged`). Proven end to end before the flag.
            rotate: true,
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
            // ADR-0029 item 4: the launcher's peer thread owns the module
            // registry, so `service_ctl` is a command to it (attach/detach and
            // a module's own control ops).
            services: true,
        }
    }

    fn launch(&mut self, _apk: &Path) -> Result<LaunchOutcome, BackendError> {
        let content = self.settle_mode == "content";
        // G3c: the launch frame must be the app's *content* frame, not the window
        // skeleton that exists a moment before the app mounts its own views (see
        // `bind_first_frame`).
        let previous = self.seen_seq;
        // P9 (unchanged): a launch's contract is a **fresh** frame, not a
        // settled one. A screen that keeps animating (Gaten's welcome timeline)
        // must not fail the launch step — that verdict is `wait_for`'s or
        // `capture`'s to make, and the settle semantics of an endlessly
        // animating screen are GR3/E13.7's open owner decision (G3e). The
        // content-aware retry below traverses with `request_tree_with`, which
        // records each traversal's settle block; restore the pre-launch value so
        // the launch step is judged exactly as it was before G3c.
        let settle_before = self.last_tree_settle.clone();
        let (_tree, report) =
            self.bind_first_frame(if content { "fast" } else { "strict" }, !content, true)?;
        self.last_tree_settle = settle_before;
        if report.placeholder {
            // FB2: the false green reported by agents. `reached_stage:
            // first_frame` with `placeholder: true` is *not* a launch pass: the
            // window exists, the app's own content never mounted (measured:
            // 9-node empty RN surface, `frame_number` stuck at 1 for 40+ s while
            // SoLoader looped on a missing `libworklets.so`). A launch step that
            // reached only the skeleton is `blocked` / `placeholder-content`.
            return Err(BackendError::Error {
                code: ErrorCode::PlaceholderContent,
                detail: placeholder_evidence_detail(&report),
            });
        }
        // SE2: a content-settled launch proves the tree's content only. Its
        // frame is never pixel evidence, so no PNG is claimed for it and the
        // reply carries the measured content record.
        let content_settle = if content {
            let record = self.content_settle()?;
            self.bound_settle = Some(record.clone());
            Some(record)
        } else {
            None
        };
        let frame = self.require_cached_frame()?.observed(previous)?;
        // SE2c: the published frame is the content-settled traversal, so the
        // `first_frame` report must describe *it*, not the fast pre-settle probe
        // `bind_first_frame` returned (whose node count and placeholder flag
        // describe a tree this reply no longer carries).
        let report = if content {
            report.describes_tree(&frame.tree)
        } else {
            report
        };
        let mut meta = frame.tree.frame.clone();
        // SE2: a content-settled launch binds the content-stable traversal, not
        // the fast skeleton probe; its meaning is the content tree's own. (A
        // placeholder never reaches here: FB2c blocks it above.)
        if content {
            meta.meaningful = !first_frame_is_bare_skeleton(&frame.tree);
        }
        Ok(LaunchOutcome {
            session_id: format!(
                "s_art_{}",
                self.apk
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default()
            ),
            package: self.package.clone(),
            frame: Some(meta),
            screenshot_png: if content {
                None
            } else {
                Some(self.require_cached_frame()?.png_bytes()?)
            },
            crashed: None,
            detail: serde_json::json!({
                "engine": "art",
                "mode": "live",
                "activity": self.activity,
                "apk": self.apk.display().to_string(),
                "first_frame": report.to_json(),
                "settle": content_settle,
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

    /// SE2b: forward the step's `settle_timeout_ms` to the session.
    ///
    /// The verifier's `Backend::set_settle_timeout` reaches
    /// `EngineBackend::Session { session: Box<dyn SessionBackend> }`, so without
    /// this override the trait's default (a no-op) swallowed it: every scenario's
    /// declared settle budget was silently ignored and the engine default was
    /// used instead. A declared budget that is not applied is exactly the kind of
    /// quiet downgrade a verification tool must not have.
    fn set_settle_timeout(&mut self, timeout_ms: Option<u64>) {
        ArtSession::set_settle_timeout(self, timeout_ms);
    }

    fn set_input_settle_mode(&mut self, fast: bool) {
        self.settle_mode = if fast { "fast" } else { "strict" };
    }

    fn input_settle_mode(&self) -> &'static str {
        self.settle_mode
    }

    fn fresh_input_tree(&mut self) -> Result<UiTree, BackendError> {
        ArtSession::fresh_input_tree(self)
    }

    fn set_input_content_settle(&mut self, content: bool) {
        ArtSession::set_content_settle(self, content);
    }

    fn set_launch_settle_mode(&mut self, mode: &'static str) {
        self.settle_mode = mode;
    }

    fn set_observation_settle(&mut self, mode: acos_verify::settle::ObservationSettle) {
        ArtSession::set_observation_settle(self, mode.as_str());
    }

    fn bound_settle(&self) -> Option<&serde_json::Value> {
        self.bound_settle.as_ref()
    }

    fn frame(&mut self) -> Result<ObservedFrame, BackendError> {
        // `tree` re-traverses and publishes a new frame; its reply carries both
        // the tree and the PNG of that frame. The PNG bytes are loaded from the
        // artifact now (and hash-verified); the session retains only the
        // descriptor between calls.
        //
        // REL3A: an observation asked for the content-stable rule binds the
        // content window's own pixel-bound traversal (a strict capture of the
        // frame the stable content was proven on), so `refresh()`/`capture()`
        // report the rule they actually applied. A content settle that cannot
        // prove stability is a structured TIMEOUT, never an observation.
        //
        // The rule is **one-shot**: it is consumed here and restored to strict
        // before any command runs, so an error or an early return cannot leave
        // the session content-sticky for the next step's capture/pixel claim.
        let content = self.observation_settle == "content";
        self.observation_settle = "strict";
        let previous = self.seen_seq;
        if content {
            self.content_settle()?;
        } else {
            self.request_tree()?;
        }
        let cached = self.require_cached_frame()?;
        let observed = cached.observed(previous)?;
        if observed.png().is_empty() {
            return Err(live_frame_unavailable(
                "the ART tree reply carried no PNG; refusing to bind a tree to absent pixels",
            ));
        }
        Ok(observed)
    }

    /// REL3A: a fresh capture is pixel evidence; it may never be bound by the
    /// content-stable rule. The executor resets the observation rule to strict
    /// before a capture, and `frame()` is one-shot, so this refuses the remaining
    /// case (a caller that asked for content and then captured): a content
    /// record says the *tree* was stable while the pixels may still animate.
    fn capture(&mut self) -> Result<ObservedFrame, BackendError> {
        let previous = self.last_frame_seq();
        let observed = self.frame()?;
        if let Some(error) = content_capture_refusal(self.bound_settle.as_ref()) {
            return Err(error);
        }
        if observed.frame_seq() <= previous {
            return Err(stale_frame(previous, observed.frame_seq(), "capture"));
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
        // SE2b: the bound settle record describes the *last* tree reply. A new
        // action invents a new frame, so a previous (possibly content-settled)
        // record must not outlive it: strict/fast actions clear it here and let
        // their own reply replace it; a content action proves a new record.
        if self.settle_mode != "content" {
            self.bound_settle = None;
        }
        self.input_baseline = self
            .cached
            .as_ref()
            .map(|frame| (frame.md5.clone(), frame.signature));
        // FB3: the "before" side of the action's diff is the tree this session
        // already holds -- the bound frame of the previous observation. It is
        // only cloned when the caller asked for a diff (`diff:false` costs
        // nothing), and it never triggers a traversal or a guest round trip.
        self.diff_baseline = if self.diff_requested {
            self.cached.as_ref().map(|frame| DiffBaseline {
                tree: frame.tree.clone(),
                seq: frame.seq,
            })
        } else {
            None
        };
    }

    fn tap(&mut self, x: i32, y: i32) -> Result<(), BackendError> {
        // LAYER1: send display coordinates. The guest hit-tests them against the
        // WMS layer stack and converts to the target window's own input space,
        // so a tap beside an overlay reaches the activity below it and a
        // `FLAG_NOT_TOUCHABLE` overlay passes the touch through. The CLI must
        // not pre-subtract a window origin: it does not know which window the
        // policy will pick.
        self.input_command(serde_json::json!({"cmd":"tap","x":x,"y":y}), "input.tap")
    }

    fn tap_in_window(&mut self, _window: &str, x: i32, y: i32) -> Result<(), BackendError> {
        // The node's window label selects the *node*; the tap still carries
        // display coordinates and the guest routes it by the layer stack (a
        // point the policy sends to another window is a real disagreement, not
        // something this layer hides).
        self.tap(x, y)
    }

    /// REL3S: one real swipe/scroll gesture through the guest's live input
    /// channel, delivered exactly like a tap (same channel, same ack
    /// collection, same settle rule).
    ///
    /// `from`/`to` are display coordinates; the input channel's transform is
    /// window-local, so both ends are shifted by the first window's measured
    /// origin (the single-window rule `tap` uses).
    fn swipe(
        &mut self,
        from: (f32, f32),
        to: (f32, f32),
        duration_ms: u64,
        _fling: bool,
    ) -> Result<(), BackendError> {
        let (dx, dy) = self.input_offset;
        self.input_command(
            serde_json::json!({
                "cmd": "swipe",
                "x1": from.0.round() as i64 - dx as i64,
                "y1": from.1.round() as i64 - dy as i64,
                "x2": to.0.round() as i64 - dx as i64,
                "y2": to.1.round() as i64 - dy as i64,
                "duration_ms": duration_ms.max(1),
            }),
            "input.swipe",
        )
    }

    fn tap_text(&mut self, text: &str) -> Result<(), BackendError> {
        let tree = self.require_cached_tree()?;
        let selector = Selector::by_text(text);
        // Input resolution, not evidence resolution: the target is the node the
        // platform's *accessible name* designates (`text`, else
        // `contentDescription`) among the nodes visible to the user -- see
        // `UiTree::find_input_target`. A React Native pressable carries its whole
        // name in the content description and no text at all, so a tap by the
        // label an agent reads on screen must reach it. A node that is not
        // visible to the user is never tapped: it stays `ELEMENT_OFFSCREEN`.
        let node = match tree.find_input_target(&selector) {
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

    fn rotate(&mut self, rotation: u8) -> Result<(), BackendError> {
        // ROT4: one display rotation. The host rotates the live WMS display
        // (so the next `relayout` reply carries the rotated frame and
        // `MergedConfiguration`), the guest builds the new `Configuration` and
        // lets the real `ActivityThread` decide relaunch vs
        // `onConfigurationChanged` from the manifest's `ActivityInfo.configChanges`.
        // The reply is bound to a probe sample *newer* than the request, but it
        // carries no pixels, and this action owes the caller a completed frame:
        // one traversal follows and is the action's frame (which also refreshes
        // the input mapping for the new geometry).
        if rotation > 3 {
            return Err(BackendError::unsupported(
                "input.rotate",
                format!("rotation {rotation} is not a quarter-turn value (0..3)"),
            ));
        }
        let reply = self.command(serde_json::json!({
            "cmd": "rotate",
            "rotation": rotation,
        }))?;
        if reply.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
            let detail = reply
                .get("error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("the session did not confirm the rotation");
            return Err(BackendError::unsupported(
                "input.rotate",
                format!("the ART session did not apply the rotation: {detail}"),
            ));
        }
        if let Some(applied) = reply.get("rotation").and_then(serde_json::Value::as_u64) {
            if applied != u64::from(rotation) {
                return Err(BackendError::unsupported(
                    "input.rotate",
                    format!("asked for rotation {rotation}, the session applied {applied}"),
                ));
            }
        }
        // The frame's own size is the rotated one; frame artifacts are stamped
        // with it, so a later capture is bound to the geometry it really has.
        if let Some(width) = reply.get("width").and_then(serde_json::Value::as_u64) {
            self.width = width as u32;
        }
        if let Some(height) = reply.get("height").and_then(serde_json::Value::as_u64) {
            self.height = height as u32;
        }
        let settle = if self.settle_mode == "fast" {
            "fast"
        } else {
            "strict"
        };
        self.request_tree_with(settle, true)
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
        // ZPROD3 (design §4): a zygote reset is `kill <tag>` + a fresh launcher
        // for the *same* tag + `spawn <tag>`. The zygote itself, its pid and its
        // boot epoch are untouched; a reset that silently restarted the keeper
        // would change the epoch, which the reply states.
        let zygote_link = self.zygote.as_ref().map(|binding| {
            (
                Arc::clone(&binding.host),
                binding.key.clone(),
                binding.facts.tag,
                binding.facts.boot_epoch.clone(),
            )
        });
        let zygote_previous_child_pid = self.zygote.as_ref().map(|b| b.facts.child_pid);
        let zygote_start = self.zygote.as_ref().map(|binding| ZygoteStart {
            host: Arc::clone(&binding.host),
            key: binding.key.clone(),
            reuse_tag: Some(binding.facts.tag),
        });
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
        // The old launcher's group is gone; now end the old child's process
        // group inside the keeper before its run root is wiped (§4.1/§4.2). A
        // keeper that cannot ack the kill is `ZYGOTE_DEAD` and this session stays
        // provably closed (W55): no bound frame, no verb answers from it.
        // ZPROD4: if the keeper is gone (or was restarted with a new boot epoch)
        // this session is *lost*: `verb_error` says so structurally and never
        // re-points it at the new keeper.
        if let Some((host, key, tag, epoch)) = zygote_link.as_ref() {
            if let Err(error) = host.kill(key, *tag) {
                self.invalidate_frame();
                return Err(host.verb_error(key, *tag, epoch, &error));
            }
        }
        let relaunch_started = Instant::now();
        // S3b: a relaunch can fail because the *host* could not complete a
        // transport/host resource step (the binder reply was malformed, or the
        // vsync thread hit the task limit). The guest then exits 1 before its
        // first frame. That is not app behaviour, so one bounded retry is
        // allowed; the first failure is preserved verbatim under
        // `<artifacts>/relaunch-retry-1/` and carried in the reset reply. Any
        // other startup failure (a real app wall, a broken APK) is returned
        // immediately and is never retried.
        //
        // ZPROD4: the loop covers both arms. With `ACOS_ART_ZYGOTE` off
        // `zygote_start` is `None` and this is exactly `start_with_options`;
        // with it on `start_inner` spawns the child on the keeper (same tag,
        // same epoch) and a retry re-spawns on that same tag.
        let mut retry_info: Option<serde_json::Value> = None;
        let mut fresh = None;
        for attempt in 1..=2 {
            match ArtSession::start_inner(
                &apk,
                &self.extra_apks.clone(),
                &artifacts,
                self.animations,
                self.insets(),
                self.idle_kill_seconds,
                zygote_start.clone(),
                // A relaunch is the same session: it keeps the id (and therefore
                // the relay session name) the client already holds.
                Some(&self.session_id),
                &self.services,
            ) {
                Ok(session) => {
                    fresh = Some(session);
                    break;
                }
                Err(error) => {
                    if attempt == 1 {
                        if let Some(reason) = relaunch_transport_reason(&error, &artifacts) {
                            let evidence = preserve_failed_relaunch(&artifacts, &error, &reason);
                            eprintln!(
                                "acos: reset: first relaunch failed for a host transport reason \
                                 ({reason}); retrying once (evidence: {})",
                                evidence.as_deref().unwrap_or("<not written>")
                            );
                            retry_info = Some(serde_json::json!({
                                "attempts": 2,
                                "first_failure": error.to_string(),
                                "reason": reason,
                                "evidence": evidence,
                                "utc_ms": now_unix_ms(),
                            }));
                            continue;
                        }
                    }
                    self.invalidate_frame();
                    return Err(error);
                }
            }
        }
        let mut fresh = fresh.expect("the relaunch loop either returns or yields a session");
        fresh.settle_timeout_ms = self.reset_settle_timeout_ms;
        let start_ms = relaunch_started.elapsed().as_millis() as u64;
        fresh.seq_offset = offset;
        fresh.log_offset = log_offset;
        fresh.zygote_previous_child_pid = zygote_previous_child_pid;
        fresh.relaunch_retry = retry_info;
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
        // FB2c item 3: the reset binds the relaunched guest's **first frame**
        // through the same rule a launch uses — the content wait and the
        // placeholder verdict included. Before this, reset bound one plain
        // traversal and `acos_reset` hardcoded `stage_reached: first_frame`, so
        // a relaunched app that never mounted passed the step and every later
        // screenshot served splash pixels.
        let bound = fresh.bind_first_frame(bind_settle, bind_pixels, true);
        let report = match bound {
            Ok((_tree, report)) => report,
            Err(error) => {
                let _ = fresh.close();
                self.invalidate_frame();
                return Err(error);
            }
        };
        if report.placeholder {
            let detail = format!(
                "reset relaunched the app and its first frame is still the window skeleton: {}",
                placeholder_evidence_detail(&report)
            );
            let _ = fresh.close();
            self.invalidate_frame();
            self.first_frame_report = None;
            return Err(BackendError::Error {
                code: ErrorCode::PlaceholderContent,
                detail,
            });
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
        // ADR-0029: the verdict cites the session's final module rows, so ask
        // the launcher for them once before the process goes away (the rows
        // carry the delivered-fix record, which grows while the app runs).
        if !self.modules.is_empty() {
            let _ = self.service_op(serde_json::json!({"op": "list"}));
        }
        let stopped = self.shutdown();

        // ZPROD3: a zygote session's app child lives inside the keeper's
        // sandbox, so closing the session must end *that child* too (by its
        // process group). A keeper that is already gone is not an error here:
        // `shutdown_all` on serve exit ends every child with its zygote.
        if let Some(binding) = self.zygote.as_ref() {
            let _ = binding.host.kill(&binding.key, binding.facts.tag);
        }
        stopped
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

    /// REL3H2: the guest's own settle record for the most recent traversal,
    /// settled or not. `None` means the reply carried no `settle` block at all,
    /// which a `scroll_until` hit must not treat as settled.
    fn tree_settle_record(&self) -> Option<serde_json::Value> {
        self.last_tree_settle.clone()
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

/// ZPROD3: the keeper key a session needs (design §3: a zygote's pre-fork heap
/// is only valid for one APK + boot-image mode + geometry + insets + animations
/// tuple). Values are the launch-time ones the engine already decided, never a
/// new source of truth.
fn zygote_key_of(
    apk: &Path,
    insets: &str,
    animations: &str,
    extra_apks: &[PathBuf],
) -> acos_art::zygote_prod::ZygoteKey {
    let num = |name: &str, default: u32| {
        std::env::var(name)
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(default)
    };
    acos_art::zygote_prod::ZygoteKey {
        apk: apk.display().to_string(),
        boot_image: std::env::var("ACOS_ART_BOOT_IMAGE").unwrap_or_else(|_| "default".to_string()),
        width: num("ACOS_ART_WIDTH", 1080),
        height: num("ACOS_ART_PANEL_HEIGHT", 2400),
        density: num("ACOS_ART_DENSITY", 420),
        insets: insets.trim().to_ascii_lowercase(),
        animations: animations.trim().to_ascii_lowercase(),
        // MPROC1: the installed package set is part of the key. A keeper
        // child forked for one set must never serve a session that asks for a
        // different one.
        extra_apks: extra_apks
            .iter()
            .map(|path| path.display().to_string())
            .collect(),
    }
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

/// ZPROD6: the lifecycle grace for a session whose ART guest is **not** this
/// process's child (a zygote session: the guest was forked inside the keeper and
/// the engine's own `child` is the foreign-guest session launcher).
///
/// Zero, because there is nothing to wait for and waiting was measured to cost
/// exactly the grace: with `DM6_RESET_TRACE` on the live serve path every ON
/// stop read `path=killed reaped_before_deadline=false exit_ms=300` — the shim
/// never exits on `close` — which is 100 % of the ON arm's teardown regression
/// (307 ms vs the OFF arm's 13 ms) and, inside reset's stop phase, all of its
/// reset regression (307 ms vs 14 ms). The death proof for a zygote session is
/// the keeper's `reaped` ack instead (`kill(-child, SIGKILL)` + `wait4`, written
/// by `ZygoteApp.z4KillOne`), which `close`/`reset` fetch immediately after this
/// stop; the process-group proof below is unchanged and still runs.
const DEFAULT_ZYGOTE_TEARDOWN_GRACE_MS: u64 = 0;

fn teardown_grace_ms() -> u64 {
    std::env::var("ACOS_ART_TEARDOWN_GRACE_MS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_TEARDOWN_GRACE_MS)
}

/// SERVELEAK2/F1: the environment variable a session's launcher — and every
/// process it starts — inherits, naming the session that owns it. Its presence
/// in `/proc/<pid>/environ` is proof that this session started that process; no
/// other session, and no command-line coincidence, can produce it.
pub(crate) const SESSION_TOKEN_ENV: &str = "ACOS_ART_SESSION_TOKEN";

/// SERVELEAK2/F4: how many `kill(-group)` signals this process has issued. A
/// test seam: the F4 regression asserts that a teardown which already proved
/// the tree empty issues **no** further group signal (a recycled group id is
/// then never signalled).
static GROUP_SIGNALS: AtomicUsize = AtomicUsize::new(0);

fn note_group_signal() {
    GROUP_SIGNALS.fetch_add(1, AtomicOrdering::Relaxed);
}

/// How many `kill(-group)` signals this process has issued (F4 test seam).
pub(crate) fn kill_group_signals() -> usize {
    GROUP_SIGNALS.load(AtomicOrdering::Relaxed)
}

/// The grace this session's stop may spend waiting for its own child to exit.
///
/// The operator knob still wins (so an experiment can restore the old bound for
/// either arm); otherwise the arm decides: an exec-path session keeps GL1's
/// 300 ms lifecycle bound, a zygote session has none (see
/// [`DEFAULT_ZYGOTE_TEARDOWN_GRACE_MS`]).
fn teardown_grace_ms_for(zygote: bool) -> u64 {
    if std::env::var("ACOS_ART_TEARDOWN_GRACE_MS").is_ok() {
        return teardown_grace_ms();
    }
    if zygote {
        DEFAULT_ZYGOTE_TEARDOWN_GRACE_MS
    } else {
        DEFAULT_TEARDOWN_GRACE_MS
    }
}

/// A fresh, unguessable-per-session ownership token (F1).
///
/// Read from `/dev/urandom` (never a clock alone: two agents can start in the
/// same millisecond, and the token is a capability that must not collide). The
/// artifacts directory name is mixed in for a readable `ps e` / trace label.
fn new_session_token(artifacts: &Path) -> String {
    let mut bytes = [0u8; 16];
    let read = std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .is_ok();
    if !read {
        // `/dev/urandom` is present on every host ACOS runs on; a clock+pid mix
        // is a weaker but still per-session-unique fallback.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        bytes[..8].copy_from_slice(&(nanos as u64).to_le_bytes());
        bytes[8..].copy_from_slice(&(std::process::id() as u64).to_le_bytes());
    }
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    let label = artifacts
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("session");
    format!("acos-{label}-{hex}")
}

/// Every pid the kernel currently lists (F1's ownership walk).
fn all_pids() -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
        .collect()
}

/// `(ppid, pgrp, alive)` for `pid`, from `/proc/<pid>/stat`.
///
/// The command name can contain spaces and parentheses, so parsing starts after
/// the last `)`. `alive` is false for a zombie (it still occupies its group).
fn proc_ppid_pgrp(pid: u32) -> Option<(i32, i32, bool)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = &stat[stat.rfind(')')? + 1..];
    let mut fields = rest.split_whitespace();
    let state = fields.next()?;
    let ppid = fields.next()?.parse::<i32>().ok()?;
    let pgrp = fields.next()?.parse::<i32>().ok()?;
    Some((ppid, pgrp, state != "Z"))
}

/// Every live process descended from `root`, walked through `/proc`'s ppid
/// links (F1 proof 2). Empty once the root itself is gone.
fn descendants_of(root: u32) -> Vec<u32> {
    let mut children: HashMap<i32, Vec<u32>> = HashMap::new();
    for pid in all_pids() {
        if let Some((ppid, _pgrp, _alive)) = proc_ppid_pgrp(pid) {
            children.entry(ppid).or_default().push(pid);
        }
    }
    let mut out: Vec<u32> = Vec::new();
    let mut stack = vec![root as i32];
    while let Some(parent) = stack.pop() {
        for child in children.get(&parent).cloned().unwrap_or_default() {
            if child == root || out.contains(&child) {
                continue;
            }
            out.push(child);
            stack.push(child as i32);
        }
    }
    out.sort_unstable();
    out
}

/// Whether `pid`'s environment contains the exact `NAME=value` entry `needle`
/// (F1 proof 3; `\0`-separated, so a prefix/suffix can never match).
fn environ_has(pid: u32, needle: &str) -> bool {
    let Ok(raw) = std::fs::read(format!("/proc/{pid}/environ")) else {
        return false;
    };
    raw.split(|byte| *byte == 0)
        .any(|entry| entry == needle.as_bytes())
}

/// A pidfd for `pid`, or `None` where the kernel or the pid does not allow one
/// (F4: a stable handle on the process, not on its pid number).
fn open_pidfd(pid: u32) -> Option<OwnedFd> {
    // SAFETY: `pidfd_open` takes the pid and flags; a valid fd is wrapped in an
    // `OwnedFd` immediately, so it is closed exactly once.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::c_int, 0) as libc::c_int };
    if fd < 0 {
        return None;
    }
    // SAFETY: `fd` is a fresh, owned pidfd returned by `pidfd_open`.
    Some(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Send `signal` through a pidfd (F4). Returns false when the process is gone.
fn pidfd_send_signal(fd: &OwnedFd, signal: libc::c_int) {
    // SAFETY: `fd` is an open pidfd this session owns; the info argument is NULL
    // and ignored, which the kernel documents for this call.
    unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            fd.as_raw_fd(),
            signal,
            std::ptr::null::<libc::siginfo_t>(),
            0,
        );
    }
    if signal == libc::SIGKILL {
        note_group_signal();
    }
}

/// Live (non-zombie) processes whose command line names `dir` (SERVELEAK2/F1).
///
/// **Report only** — ownership is decided by [`ArtSession::owned_pids`], never
/// by this function; a name match is not proof that this session started a
/// process. Two agents running `acos verify counter.yaml` from different
/// directories both name `out/counter`, and `s1` is a prefix of `s10`.
///
/// The directory is canonicalised and must match a **whole argv element** (it, or
/// a path under it), so `out/counter` can never match `other/out/counter2`.
fn processes_mentioning(dir: &Path) -> Vec<u32> {
    let key = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let key = key.display().to_string();
    let mut pids = Vec::new();
    for pid in all_pids() {
        let Ok(raw) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
            continue;
        };
        if raw.is_empty() {
            continue;
        }
        let names_acos_art = raw
            .split(|byte| *byte == 0)
            .any(|arg| String::from_utf8_lossy(arg).contains("acos-art"));
        if !names_acos_art {
            continue;
        }
        let names_dir = raw.split(|byte| *byte == 0).any(|arg| {
            let arg = String::from_utf8_lossy(arg);
            // Canonicalise the argv path when it exists, so `/a/out/counter` and
            // `out/counter` are the same key; otherwise fall back to the text.
            let arg_key = std::fs::canonicalize(arg.as_ref())
                .map(|path| path.display().to_string())
                .unwrap_or_else(|_| arg.to_string());
            arg_key == key || arg_key.starts_with(&format!("{key}/"))
        });
        if !names_dir {
            continue;
        }
        match proc_ppid_pgrp(pid) {
            Some((_ppid, _pgrp, true)) => pids.push(pid),
            _ => continue,
        }
    }
    pids.sort_unstable();
    pids
}

#[cfg(test)]
mod tests {
    use super::*;

    /// REL3H2: the guest and the verifier must name the same settle reason. The
    /// verifier reads it to refuse an end-of-list inference from a gesture the
    /// app never consumed; `acos-art` cannot depend on `acos-protocol` (it is the
    /// guest side of the wire), so the two literals are pinned equal here - in
    /// the one crate that sees both.
    #[test]
    fn the_unhandled_input_settle_reason_is_one_string() {
        assert_eq!(
            acos_art::launch::SETTLE_UNHANDLED_INPUT,
            acos_protocol::SETTLE_UNHANDLED_INPUT_REASON,
            "a renamed settle reason would silently stop the end-of-list refusal"
        );
    }

    /// REL3A: a capture is pixel evidence and may never be bound by a content
    /// settle record. The refusal is the same fail-closed reason the pixel
    /// assertions use (`content_settle_pixel`), with no frame handed back.
    #[test]
    fn a_content_settled_frame_is_refused_as_capture_evidence() {
        let content = serde_json::json!({
            "mode": "content",
            "settled": true,
            "window_ms": 300,
            "traversals": 3,
            "pixel_evidence": false,
        });
        let error = content_capture_refusal(Some(&content)).expect("content is refused");
        let protocol = error.to_protocol_error();
        assert_eq!(protocol.code, ErrorCode::UnsupportedAssertion);
        let details = protocol.details.expect("details");
        assert_eq!(details["reason"], "content_settle_pixel");
        assert_eq!(details["pixel_evidence"], false);
        assert_eq!(details["settle"]["mode"], "content");

        // Strict and absent records are not refused.
        let strict = serde_json::json!({"mode": "strict", "settled": true});
        assert!(content_capture_refusal(Some(&strict)).is_none());
        assert!(content_capture_refusal(None).is_none());
    }

    /// G3c: the launch first-frame predicate must reject the *window skeleton*
    /// (decor + `action_bar_root` + `content` + an empty app root, no text) and
    /// must never touch a mounted screen — including a small, text-free one.
    #[test]
    fn the_launch_first_frame_predicate_only_rejects_an_unmounted_skeleton() {
        use acos_protocol::ui::Bounds;
        let node = |class: &str, children: Vec<UiNode>| {
            let mut node = UiNode::new("n", class, Bounds::new(0, 0, 1080, 2209));
            node.children = children;
            node
        };
        let tree = |nodes: Vec<UiNode>| {
            UiTree::new(
                FrameMeta::new(1, "t", "c", 1080, 2400, 420, "h", true),
                "com.gaten",
                nodes,
            )
        };
        // Gaten's measured unmounted skeleton: 10 nodes, no text anywhere.
        let app_root = node("android.widget.FrameLayout", vec![]);
        let content = node("android.widget.FrameLayout", vec![app_root.clone()]);
        let action_bar_root = node(
            "android.widget.LinearLayout",
            vec![content, node("android.view.ViewStub", vec![])],
        );
        let mut skeleton = vec![node(
            "com.android.internal.policy.DecorView",
            vec![action_bar_root],
        )];
        for _ in 0..5 {
            skeleton.push(node("android.view.ViewStub", vec![]));
        }
        assert!(
            first_frame_is_unmounted(&tree(skeleton.clone())),
            "a 10-node, text-free window skeleton is not app content"
        );
        assert!(
            first_frame_is_bare_skeleton(&tree(skeleton.clone())),
            "and a frame that never grows past it is a placeholder"
        );

        // The same skeleton with one app text (the painted welcome screen) is
        // content: it must never be delayed or labelled as a placeholder.
        let mut painted = skeleton.clone();
        painted[0].children[0].children[0].children[0].text = "Set up Gaten".to_string();
        assert!(!first_frame_is_unmounted(&tree(painted)));

        // A content description or an editor hint counts too.
        let mut described = skeleton.clone();
        described[0].children[0].children[0].children[0].content_desc = "Skip".to_string();
        assert!(!first_frame_is_unmounted(&tree(described)));
        let mut hinted = skeleton.clone();
        hinted[0].children[0].children[0].children[0].hint = "Search".to_string();
        assert!(!first_frame_is_unmounted(&tree(hinted)));

        // A big, text-free tree is a mounted screen (canvas/custom views): the
        // node budget alone must not make it a placeholder.
        let wide = vec![node(
            "android.widget.FrameLayout",
            (0..FIRST_FRAME_SKELETON_MAX_NODES)
                .map(|_| node("android.view.View", vec![]))
                .collect(),
        )];
        assert!(!first_frame_is_unmounted(&tree(wide.clone())));
        // ... and it is never labelled a placeholder either (it is bigger than
        // the bare skeleton), so a text-free canvas screen keeps its meaning.
        assert!(!first_frame_is_bare_skeleton(&tree(wide)));
        // An empty tree (no windows yet) is a skeleton, not content.
        assert!(first_frame_is_unmounted(&tree(vec![])));
    }

    /// SE2c: a content-settled launch publishes the **content-settled** tree, so
    /// its `first_frame` evidence must describe that tree, not the fast
    /// pre-settle probe it was built from.
    ///
    /// Before the fix `ensure_launch_tree` handed the pre-settle skeleton tree
    /// back in the launch reply (and the report stayed a skeleton report), so the
    /// reply's tree and its `first_frame.nodes`/`placeholder` described two
    /// different traversals.
    #[test]
    fn a_content_launch_report_describes_the_tree_the_reply_publishes() {
        use acos_protocol::ui::Bounds;
        let node = |class: &str, children: Vec<UiNode>| {
            let mut node = UiNode::new("n", class, Bounds::new(0, 0, 1080, 2209));
            node.children = children;
            node
        };
        let tree = |nodes: Vec<UiNode>| {
            UiTree::new(
                FrameMeta::new(1, "t", "c", 1080, 2400, 420, "h", true),
                "com.gaten",
                nodes,
            )
        };
        // The window skeleton the fast launch bind sees before the app mounts.
        let skeleton = tree(vec![node("com.android.internal.policy.DecorView", vec![])]);
        let report = FirstFrameReport {
            traversals: 1,
            settled: true,
            bound: Some("probe-fresh".into()),
            nodes: ui_tree_node_count(&skeleton.nodes),
            placeholder: first_frame_is_bare_skeleton(&skeleton),
            ..FirstFrameReport::default()
        };
        assert!(report.placeholder, "the pre-settle probe is a placeholder");
        assert_eq!(report.nodes, 1);

        // The content-settled tree the reply actually carries.
        let mut painted = node("com.android.internal.policy.DecorView", vec![]);
        painted.text = "Set up Gaten".to_string();
        painted.children = vec![node("android.widget.FrameLayout", vec![])];
        let settled = tree(vec![painted]);
        let report = report.describes_tree(&settled);
        assert!(
            !report.placeholder,
            "the published content tree is not a placeholder"
        );
        assert_eq!(report.nodes, ui_tree_node_count(&settled.nodes));
        assert_eq!(report.nodes, 2);
    }

    /// FB2c item 1: the placeholder verdict is frame evidence, not a node count.
    ///
    /// A synthetic window skeleton whose pixels **change** (a text-free mounted
    /// screen) is a pass with a `placeholder_suspect` warning; the same tree whose
    /// frame never moves is the placeholder. A content tree is neither.
    #[test]
    fn the_placeholder_verdict_needs_frame_evidence_not_a_node_count() {
        use acos_protocol::ui::Bounds;
        let node = |class: &str, children: Vec<UiNode>| {
            let mut node = UiNode::new("n", class, Bounds::new(0, 0, 1080, 2209));
            node.children = children;
            node
        };
        let tree = |nodes: Vec<UiNode>| {
            UiTree::new(
                FrameMeta::new(1, "t", "c", 1080, 2400, 420, "h", true),
                "com.gaten",
                nodes,
            )
        };
        let app_root = node("android.widget.FrameLayout", vec![]);
        let content = node("android.widget.FrameLayout", vec![app_root]);
        let skeleton = vec![node(
            "com.android.internal.policy.DecorView",
            vec![node(
                "android.widget.LinearLayout",
                vec![content, node("android.view.ViewStub", vec![])],
            )],
        )];
        let identity = |seq: u64, md5: &str| FrameIdentity {
            seq: Some(seq),
            frame_number: Some(seq),
            md5: Some(md5.to_string()),
        };
        // GATEFIX (F1): a placeholder verdict also needs the wait to have
        // observed the skeleton for the whole content window, so the evidence of
        // a wait that actually ran carries `window_spent: true`.
        let evidence = |before: &FrameIdentity, after: &FrameIdentity| {
            frame_evidence(before, after, 2, true)
        };

        // Skeleton, same pixels, no newer frame: the placeholder.
        let still = evidence(&identity(1, "aa"), &identity(1, "aa"));
        assert_eq!(
            first_frame_placeholder_verdict(&tree(skeleton.clone()), &still),
            (true, false),
            "a skeleton that nothing moved is the placeholder"
        );
        // Skeleton, same pixels but a republished frame: the pixels decide, so it
        // is still the placeholder (the launcher re-published the window).
        let republished = evidence(&identity(1, "aa"), &identity(2, "aa"));
        assert_eq!(
            first_frame_placeholder_verdict(&tree(skeleton.clone()), &republished),
            (true, false)
        );
        // Skeleton, changed pixels: a text-free mounted screen — pass + suspect.
        let animating = evidence(&identity(1, "aa"), &identity(2, "bb"));
        assert_eq!(
            first_frame_placeholder_verdict(&tree(skeleton.clone()), &animating),
            (false, true),
            "changing pixels are the app's frame, never a blocked verdict"
        );
        // No comparable digest: the frame identity decides.
        let no_digest_moved = evidence(
            &FrameIdentity {
                seq: Some(1),
                frame_number: Some(1),
                md5: None,
            },
            &FrameIdentity {
                seq: Some(2),
                frame_number: Some(2),
                md5: None,
            },
        );
        assert_eq!(
            first_frame_placeholder_verdict(&tree(skeleton.clone()), &no_digest_moved),
            (false, true)
        );
        let no_digest_still = evidence(
            &FrameIdentity {
                seq: Some(1),
                frame_number: Some(1),
                md5: None,
            },
            &FrameIdentity {
                seq: Some(1),
                frame_number: Some(1),
                md5: None,
            },
        );
        assert_eq!(
            first_frame_placeholder_verdict(&tree(skeleton.clone()), &no_digest_still),
            (true, false)
        );
        // Content is never a placeholder, whatever the pixels did.
        let mut painted = skeleton.clone();
        painted[0].children[0].children[0].text = "Set up Gaten".to_string();
        assert_eq!(
            first_frame_placeholder_verdict(&tree(painted), &still),
            (false, false)
        );
        // GATEFIX (F1): the same unchanged skeleton observed for less than the
        // window is NOT a placeholder. A launch may only claim that the app never
        // mounted after the app had the whole `ACOS_ART_FIRST_FRAME_MS` window to
        // mount; eight cached samples in ~0 ms are not that evidence.
        let premature = frame_evidence(&identity(1, "aa"), &identity(1, "aa"), 8, false);
        assert_eq!(
            first_frame_placeholder_verdict(&tree(skeleton.clone()), &premature),
            (false, false),
            "an unproven skeleton is neither a placeholder nor a pass"
        );
        // …and moving pixels are still the app's frame, window or not.
        let animating_early = frame_evidence(&identity(1, "aa"), &identity(2, "bb"), 2, false);
        assert_eq!(
            first_frame_placeholder_verdict(&tree(skeleton.clone()), &animating_early),
            (false, true)
        );
    }

    #[test]
    fn art_capabilities_name_the_protocol_gaps() {
        let caps = SessionCapabilities {
            launch: true,
            tree: true,
            screenshot: true,
            tap: true,
            tap_text: true,
            swipe: true,
            type_text: true,
            key: true,
            rotate: true,
            reset: true,
            close: true,
            logs: true,
            environment: false,
            // Fatal guest log entries (ART LOG(FATAL)/CHECK/abort_message and
            // the JVM's `FATAL EXCEPTION` line) are a real crash signature.
            crash_reports: true,
            // ADR-0029 item 4: the launcher's peer thread owns the module
            // registry, so `service_ctl` is a command.
            services: true,
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
            "      echo '{{\"ok\":true,\"cmd\":\"tree\",\"frame_seq\":1,\"md5\":\"m1\",\"png\":\"{}\",\"tree\":{{\"nodes\":[{{\"class\":\"android.widget.TextView\",\"text\":\"Counter\",\"children\":[]}}]}}}}'\n",
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
            "    *'\"cmd\":\"tree\"'*) echo '{{\"ok\":true,\"cmd\":\"tree\",\"frame_seq\":1,\"md5\":\"m1\",\"png\":\"{}\",\"tree\":{{\"nodes\":[{{\"class\":\"android.widget.TextView\",\"text\":\"Counter\",\"children\":[]}}]}}}}' ;;\n",
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
    /// SERVELEAK: a fake `acos-art` whose guest runs in its **own** process group
    /// (exactly like the real launcher, `crates/acos-art/src/launcher.rs` uses
    /// `process_group(0)`), reports it in the ready line, and — this is the bug —
    /// leaves it running when `close` arrives.
    ///
    /// `ignore_close` makes the launcher ignore `close` entirely, which forces
    /// `shutdown`'s bounded grace to expire and take the killed path.
    fn fake_art_with_own_guest_group(dir: &Path, ignore_close: bool) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join(if ignore_close {
            "fake-acos-art-guest-ignores-close.sh"
        } else {
            "fake-acos-art-guest-outlives-close.sh"
        });
        let mut body = String::new();
        body.push_str("#!/bin/sh\n");
        // The guest pid goes under the `--out` directory this launcher was given
        // (the session's own artifacts), so two sessions in one test file cannot
        // share a pid file.
        body.push_str("out=''\nprev=''\nfor a in \"$@\"; do\n");
        body.push_str("  [ \"$prev\" = \"--out\" ] && out=\"$a\"\n  prev=\"$a\"\ndone\n");
        body.push_str("mkdir -p \"$out\" 2>/dev/null || true\n");
        body.push_str("guest_pid_file=\"${out:-.}/guest.pid\"\n");
        // The guest: its own session (hence its own process group), the way the
        // real launcher spawns bwrap.
        body.push_str("setsid sleep 600 &\n");
        body.push_str("guest=$!\n");
        body.push_str("echo \"$guest\" > \"$guest_pid_file\"\n");
        body.push_str(&format!(
            "echo '{{\"ok\":true,\"cmd\":\"ready\",\"package\":\"dev.test\",\"activity\":\"dev.test/.Main\",\"width\":1080,\"height\":2400,\"density\":420,\"guest_process_group\":'\"$guest\"'}}'\n"
        ));
        body.push_str("while IFS= read -r line; do\n");
        body.push_str("  case \"$line\" in\n");
        body.push_str("    *'\"cmd\":\"close\"'*)\n");
        body.push_str("      echo '{\"ok\":true,\"cmd\":\"close\"}'\n");
        if ignore_close {
            // Keep running (and past the EOF `shutdown` causes by closing the
            // pipe): the caller's grace must expire and kill the tree.
            body.push_str("      sleep 600\n");
            body.push_str("      ;;\n");
        } else {
            // The graceful path replies and exits, and the guest it started is
            // left behind — the leaked-guest shape SERVELEAK reported.
            body.push_str("      exit 0\n");
            body.push_str("      ;;\n");
        }
        body.push_str("    *'\"cmd\":\"tree\"'*)\n");
        body.push_str("      echo '{\"ok\":true,\"cmd\":\"tree\",\"frame_seq\":1,\"md5\":\"m1\",\"tree\":{\"nodes\":[]}}'\n");
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

    /// Is `pid` still a live (non-zombie) process?
    fn process_is_alive(pid: u32) -> bool {
        match std::fs::read_to_string(format!("/proc/{pid}/status")) {
            Ok(status) => !status
                .lines()
                .find(|line| line.starts_with("State:"))
                .map(|line| line.split_whitespace().nth(1) == Some("Z"))
                .unwrap_or(false),
            Err(_) => false,
        }
    }

    fn wait_until_gone(pid: u32, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            if !process_is_alive(pid) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        !process_is_alive(pid)
    }

    /// SERVELEAK regression 1 — the bench's shape: the graceful `close` path
    /// succeeds (the launcher exits by itself, `lifecycle`), but the guest it
    /// started is in its own process group and is still running. Before the fix
    /// the destroy's proof only covered the launcher's group, so it either
    /// reported a clean tree over a live guest or failed with `INTERNAL` and
    /// killed nothing. A destroy must leave no guest process behind.
    #[test]
    fn destroy_kills_the_guest_group_the_launcher_reports() {
        let dir = scratch_dir("guest-group-lifecycle");
        let script = fake_art_with_own_guest_group(&dir, false);
        let apk = dir.join("fake.apk");
        std::fs::write(&apk, b"not-a-real-apk").unwrap();
        let guest_pid_file = dir.join("art-frames").join("guest.pid");
        let _guard = art_bin_env_lock();
        std::env::set_var("ACOS_ART_BIN", &script);
        let mut session = ArtSession::start(&apk, &dir).expect("fake session starts");
        let guest: u32 = std::fs::read_to_string(&guest_pid_file)
            .expect("the fake launcher wrote its guest pid")
            .trim()
            .parse()
            .expect("guest pid");
        assert!(
            process_is_alive(guest),
            "the fake guest must be running before the destroy"
        );
        assert_eq!(
            session.last_teardown(),
            None,
            "no teardown has happened yet"
        );
        let report = session.teardown().expect("teardown reports no survivors");
        assert!(
            report.is_clean(),
            "the destroy proof must be clean once the guest group is killed: {report:?}"
        );
        assert_eq!(
            session.last_teardown().map(|(path, _)| path),
            Some(TeardownPath::Lifecycle),
            "the launcher itself ended by lifecycle"
        );
        assert!(
            wait_until_gone(guest, Duration::from_secs(3)),
            "the guest process {guest} must not survive its session's destroy"
        );
        assert!(
            session.session_leftovers().is_empty(),
            "no launcher leftover may remain: {:?}",
            session.session_leftovers()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// SERVELEAK regression 2 — the graceful path fails outright: the launcher
    /// ignores `close`, so the bounded grace expires and the killed path runs.
    /// Both groups (launcher and guest) must die, and the reply must still say
    /// which path was taken.
    #[test]
    fn destroy_kills_the_whole_tree_when_close_is_ignored() {
        let dir = scratch_dir("guest-group-killed");
        let script = fake_art_with_own_guest_group(&dir, true);
        let apk = dir.join("fake.apk");
        std::fs::write(&apk, b"not-a-real-apk").unwrap();
        let guest_pid_file = dir.join("art-frames").join("guest.pid");
        let _guard = art_bin_env_lock();
        std::env::set_var("ACOS_ART_BIN", &script);
        std::env::set_var("ACOS_ART_TEARDOWN_GRACE_MS", "150");
        let mut session = ArtSession::start(&apk, &dir).expect("fake session starts");
        let guest: u32 = std::fs::read_to_string(&guest_pid_file)
            .expect("the fake launcher wrote its guest pid")
            .trim()
            .parse()
            .expect("guest pid");
        let report = session.teardown().expect("teardown reports no survivors");
        std::env::remove_var("ACOS_ART_TEARDOWN_GRACE_MS");
        assert!(
            report.is_clean(),
            "a killed tree is still a clean teardown: {report:?}"
        );
        assert_eq!(
            session.last_teardown().map(|(path, _)| path),
            Some(TeardownPath::Killed),
            "an ignored close takes the bounded-kill path"
        );
        assert!(
            wait_until_gone(guest, Duration::from_secs(3)),
            "the guest process {guest} must not survive the killed path"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// SERVELEAK2/F1 regression — two sessions whose artifacts directories are
    /// prefixes/substrings of each other.
    ///
    /// Agent A runs `acos verify counter.yaml` (artifacts `out/counter`), agent B
    /// runs the same scenario from another directory (artifacts
    /// `<dir>/x/out/counter`). Both launchers therefore name `out/counter` on
    /// their command line. The old teardown SIGKILLed every `acos-art` whose
    /// command line *contained* the session's artifacts path, so destroying A
    /// killed B's live guest (and, inside one daemon, `s1` matched
    /// `…/art-sessions/s10`). This asserts the opposite: destroying one session
    /// leaves the other session's guest running, and both teardowns are clean.
    #[test]
    fn destroy_kills_only_its_own_guest_when_artifact_paths_share_a_prefix() {
        let dir = o2_scratch_dir("f1-prefix");
        let script = fake_art_with_own_guest_group(&dir, false);
        let apk = dir.join("fake.apk");
        std::fs::write(&apk, b"not-a-real-apk").unwrap();
        let _guard = art_bin_env_lock();
        std::env::set_var("ACOS_ART_BIN", &script);

        // `out/counter` (agent A) and `<dir>/x/out/counter` (agent B): one path
        // is a suffix of the other, so a substring match cannot tell them apart.
        let artifacts_a = dir.join("out").join("counter");
        let artifacts_b = dir.join("x").join("out").join("counter");
        std::fs::create_dir_all(&artifacts_a).unwrap();
        std::fs::create_dir_all(&artifacts_b).unwrap();
        let mut session_a = ArtSession::start(&apk, &artifacts_a).expect("session A starts");
        let mut session_b = ArtSession::start(&apk, &artifacts_b).expect("session B starts");
        let guest = |artifacts: &Path| -> u32 {
            std::fs::read_to_string(artifacts.join("art-frames").join("guest.pid"))
                .expect("the fake launcher wrote its guest pid")
                .trim()
                .parse()
                .expect("guest pid")
        };
        let guest_a = guest(&artifacts_a);
        let guest_b = guest(&artifacts_b);
        assert!(process_is_alive(guest_a) && process_is_alive(guest_b));

        // A's teardown must not touch B's guest.
        let report_a = session_a.teardown().expect("A tears down cleanly");
        assert!(
            report_a.is_clean(),
            "A's own tree is proved gone: {report_a:?}"
        );
        assert!(
            wait_until_gone(guest_a, Duration::from_secs(3)),
            "A's guest must be gone"
        );
        assert!(
            process_is_alive(guest_b),
            "destroying session A killed session B's live guest (F1 regression)"
        );
        // And B's own teardown still works afterwards.
        let report_b = session_b.teardown().expect("B tears down cleanly");
        assert!(report_b.is_clean(), "{report_b:?}");
        assert!(wait_until_gone(guest_b, Duration::from_secs(3)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The same shape one level down: `art-sessions/s1` and
    /// `art-sessions/s10` share a directory, and `s1` is a *prefix* of `s10`.
    #[test]
    fn destroy_kills_only_its_own_guest_when_session_dirs_are_prefixes() {
        let dir = o2_scratch_dir("f1-s1-s10");
        let script = fake_art_with_own_guest_group(&dir, false);
        let apk = dir.join("fake.apk");
        std::fs::write(&apk, b"not-a-real-apk").unwrap();
        let _guard = art_bin_env_lock();
        std::env::set_var("ACOS_ART_BIN", &script);
        let root = dir.join("art-sessions");
        let artifacts_1 = root.join("s1");
        let artifacts_10 = root.join("s10");
        std::fs::create_dir_all(&artifacts_1).unwrap();
        std::fs::create_dir_all(&artifacts_10).unwrap();
        let mut session_1 = ArtSession::start(&apk, &artifacts_1).expect("s1 starts");
        let mut session_10 = ArtSession::start(&apk, &artifacts_10).expect("s10 starts");
        let guest = |artifacts: &Path| -> u32 {
            std::fs::read_to_string(artifacts.join("art-frames").join("guest.pid"))
                .expect("guest pid")
                .trim()
                .parse()
                .expect("guest pid")
        };
        let guest_1 = guest(&artifacts_1);
        let guest_10 = guest(&artifacts_10);
        assert!(session_1.teardown().expect("s1 tears down").is_clean());
        assert!(wait_until_gone(guest_1, Duration::from_secs(3)));
        assert!(
            process_is_alive(guest_10),
            "destroying s1 killed s10's guest (prefix-match regression)"
        );
        assert!(session_10.teardown().expect("s10 tears down").is_clean());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// SERVELEAK2/F4 regression — once a teardown has proved the tree empty the
    /// session holds **no** group id, so no later `kill(-group)` is issued and a
    /// recycled group id (another agent's `setsid`/`bwrap`) can never be hit.
    ///
    /// The seam is the process-wide count of group signals: a proven-empty
    /// session's second `teardown` and its `Drop` must both leave it unchanged.
    #[test]
    fn a_proven_empty_session_issues_no_further_group_signal() {
        let dir = o2_scratch_dir("f4-no-resignal");
        let script = fake_art_with_own_guest_group(&dir, false);
        let apk = dir.join("fake.apk");
        std::fs::write(&apk, b"not-a-real-apk").unwrap();
        let _guard = art_bin_env_lock();
        std::env::set_var("ACOS_ART_BIN", &script);
        let mut session = ArtSession::start(&apk, &dir).expect("fake session starts");
        let guest: u32 = std::fs::read_to_string(dir.join("art-frames").join("guest.pid"))
            .expect("guest pid")
            .trim()
            .parse()
            .expect("guest pid");
        assert!(session.teardown().expect("teardown is clean").is_clean());
        assert!(wait_until_gone(guest, Duration::from_secs(3)));
        let settled = kill_group_signals();
        // A repeat teardown of the same session is still a clean proof...
        let again = session
            .teardown()
            .expect("a repeat teardown is still clean");
        assert!(again.is_clean(), "{again:?}");
        assert_eq!(
            kill_group_signals(),
            settled,
            "a proven-empty session must not signal a process group again (F4)"
        );
        // ...and so is dropping it.
        drop(session);
        assert_eq!(
            kill_group_signals(),
            settled,
            "Drop of a proven-empty session must not signal a process group (F4)"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

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

    /// ZPROD6: the lifecycle grace each arm gets. A zygote session has no
    /// lifecycle to wait for (its app child is the keeper's, and the shim that
    /// *is* the engine's child never exits on `close`), so its stop must not
    /// spend GL1's 300 ms bound; the exec path keeps it. The operator knob still
    /// wins for both arms, so an experiment can restore the old bound.
    #[test]
    fn only_the_exec_path_pays_the_lifecycle_grace() {
        let guard = art_bin_env_lock();
        std::env::remove_var("ACOS_ART_TEARDOWN_GRACE_MS");
        assert_eq!(teardown_grace_ms_for(false), DEFAULT_TEARDOWN_GRACE_MS);
        assert_eq!(
            teardown_grace_ms_for(true),
            DEFAULT_ZYGOTE_TEARDOWN_GRACE_MS
        );
        assert_eq!(teardown_grace_ms_for(true), 0, "no wait is the whole point");
        std::env::set_var("ACOS_ART_TEARDOWN_GRACE_MS", "120");
        assert_eq!(
            teardown_grace_ms_for(true),
            120,
            "the knob wins on both arms"
        );
        assert_eq!(teardown_grace_ms_for(false), 120);
        std::env::remove_var("ACOS_ART_TEARDOWN_GRACE_MS");
        drop(guard);
    }

    /// ZPROD6, the measured regression: the ON arm's `acos_destroy` cost 307 ms
    /// (OFF: 13 ms) and every ON reset paid the same 307 ms inside its stop
    /// phase, because the engine waited GL1's 300 ms lifecycle grace for a
    /// *shim* that never exits on `close` (`reaped_before_deadline=false`,
    /// `exit_ms=300`). With a zygote binding the stop must signal and prove
    /// instead — and still leave the whole process group gone.
    #[test]
    fn a_zygote_session_signals_and_proves_instead_of_waiting_the_grace() {
        use std::os::unix::fs::PermissionsExt;
        let dir = o2_scratch_dir("zygote-stop");
        // A session launcher in foreign-guest mode: it serves the protocol line
        // for line and **ignores** `close` — the app child lives inside the
        // keeper, and this process is only the sockets/ServiceHost shim. This is
        // the shape measured on the serve path, not a synthetic never-exiter.
        let script = dir.join("fake-acos-art-shim.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\n\
             echo '{\"ok\":true,\"cmd\":\"ready\",\"package\":\"dev.test\",\"width\":1080,\"height\":2400,\"density\":420}'\n\
             while IFS= read -r line; do\n\
               case \"$line\" in\n\
                 *'\"cmd\":\"close\"'*) : ;;\n\
                 *) echo '{\"ok\":false,\"code\":\"BAD_REQUEST\",\"error\":\"no\"}' ;;\n\
               esac\n\
             done\n\
             sleep 300\n",
        )
        .unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
        let apk = dir.join("fake.apk");
        std::fs::write(&apk, b"not-a-real-apk").unwrap();
        let _art_bin_guard = art_bin_env_lock();
        std::env::set_var("ACOS_ART_BIN", &script);
        std::env::remove_var("ACOS_ART_TEARDOWN_GRACE_MS");

        // The exec path keeps GL1's bound: the same shim costs the full grace.
        let mut exec = ArtSession::start(&apk, &dir.join("exec")).expect("exec session starts");
        assert!(exec.zygote.is_none());
        let started = Instant::now();
        let report = exec.teardown().expect("clean stop");
        let exec_ms = started.elapsed();
        assert!(
            exec_ms >= Duration::from_millis(250),
            "the exec arm still bounds the lifecycle at {} ms (waited {exec_ms:?})",
            DEFAULT_TEARDOWN_GRACE_MS
        );
        assert!(report.is_clean(), "{report:?}");
        assert_eq!(
            exec.last_teardown().map(|(path, _)| path),
            Some(TeardownPath::Killed),
            "a guest that never exits is killed, and the reply says so"
        );

        // A zygote session: the same shim, but the app child is the keeper's.
        let mut zg =
            ArtSession::start(&apk, &dir.join("zg")).expect("zygote-shaped session starts");
        zg.zygote = Some(ZygoteBinding {
            host: Arc::new(ZygoteHost::disabled()),
            key: zygote_key_of(&apk, "report", "on", &[]),
            facts: ZygoteFacts {
                tag: 0,
                child_pid: 0,
                run_root: dir.join("zg/run"),
                boot_epoch: "test-epoch".into(),
            },
        });
        let started = Instant::now();
        let report = zg.teardown().expect("clean zygote stop");
        let zg_ms = started.elapsed();
        assert!(
            zg_ms < Duration::from_millis(100),
            "a zygote stop must not wait a lifecycle its shim cannot deliver: {zg_ms:?}"
        );
        assert!(report.is_clean(), "{report:?}");
        assert!(report.survivors.is_empty(), "{report:?}");
        assert!(report.verified, "the group is still proven, never assumed");
        assert_eq!(
            zg.last_teardown().map(|(path, _)| path),
            Some(TeardownPath::Killed)
        );
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
    /// S3b: a fake `acos-art` that fails its **second** start (the first reset
    /// attempt) exactly like the N=20 class-B incident: it writes the session's
    /// own `session-exit.json` (guest exit 1 after resume, malformed
    /// `addToDisplayAsUser` reply) and exits 1 before its ready line. Every
    /// other start serves the ordinary interactive protocol, so the third
    /// spawn is the retry and is observable through `<dir>/starts.log`.
    fn transport_failing_art_binary(dir: &Path, attempt_exit_json: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let png = dir.join("tiny.png");
        std::fs::write(&png, super::super::fake::TINY_PNG).unwrap();
        let starts = dir.join("starts.log");
        let script = dir.join("fake-acos-art-s3b.sh");
        let mut body = String::new();
        body.push_str("#!/bin/sh\n");
        body.push_str(&format!("starts='{}'\n", starts.display()));
        body.push_str("out=''\n");
        body.push_str("while [ $# -gt 0 ]; do\n");
        body.push_str("  case \"$1\" in\n");
        body.push_str("    --out) out=$2; shift 2 ;;\n");
        body.push_str("    *) shift ;;\n");
        body.push_str("  esac\n");
        body.push_str("done\n");
        body.push_str("n=0\n");
        body.push_str("[ -f \"$starts\" ] && n=$(cat \"$starts\")\n");
        body.push_str("n=$((n+1))\n");
        body.push_str("printf '%s\\n' \"$n\" > \"$starts\"\n");
        body.push_str("if [ \"$n\" = 2 ]; then\n");
        body.push_str(&format!(
            "  cp '{}' \"$out/session-exit.json\"\n",
            attempt_exit_json.display()
        ));
        body.push_str(&format!(
            "  cp '{}' \"$out/live-session.json\"\n",
            attempt_exit_json.display()
        ));
        body.push_str("  exit 1\n");
        body.push_str("fi\n");
        body.push_str("echo '{\"ok\":true,\"cmd\":\"ready\",\"package\":\"dev.test\",\"activity\":\"dev.test/.Main\",\"width\":1080,\"height\":2400,\"density\":420}'\n");
        body.push_str("while IFS= read -r line; do\n");
        body.push_str("  case \"$line\" in\n");
        body.push_str("    *'\"cmd\":\"tree\"'*)\n");
        body.push_str(&format!(
            "      echo '{{\"ok\":true,\"cmd\":\"tree\",\"frame_seq\":1,\"md5\":\"m1\",\"png\":\"{}\",\"tree\":{{\"nodes\":[{{\"class\":\"android.widget.TextView\",\"text\":\"Counter\",\"children\":[]}}]}}}}'\n",
            png.display()
        ));
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

    /// S3b: the bounded relaunch retry must **actually fire**. The classifier
    /// test above only proves the string match; this proves the whole path:
    /// a host-transport relaunch failure (the N=20 class-B shape) is retried
    /// exactly once, the first attempt's own evidence is preserved verbatim,
    /// the reset succeeds on the retry and the retry is stated, never silent.
    #[test]
    fn reset_retries_a_host_transport_relaunch_once_and_preserves_the_first_failure() {
        let dir = o2_scratch_dir("relaunch-retry-fires");
        // The N=20 evidence verbatim (see results/bench/S3ab-evidence/
        // repro-n20-binder-parcel/session-exit.json): the guest could not parse
        // the addToDisplayAsUser reply and exited 1 after the Activity resumed.
        let injected = concat!(
            "{\"code\":\"GUEST_EXIT_AFTER_RESUME\",\"guest_exit\":1,\"guest_signal\":null,",
            "\"stdout_tail\":[\"E/Parcel: ipcSetDataReference: unsupported type object ",
            "(7209057) at offset 16\",\"Adding window failed\"]}\n"
        );
        let exit_json = dir.join("first-attempt-session-exit.json");
        std::fs::write(&exit_json, injected).unwrap();
        let script = transport_failing_art_binary(&dir, &exit_json);
        let apk = dir.join("fake.apk");
        std::fs::write(&apk, b"not-a-real-apk").unwrap();
        let starts = dir.join("starts.log");
        let _art_bin_guard = art_bin_env_lock();
        std::env::set_var("ACOS_ART_BIN", &script);

        let mut session = ArtSession::start(&apk, &dir).expect("fake session starts");
        assert_eq!(std::fs::read_to_string(&starts).unwrap().trim(), "1");
        assert!(
            session.relaunch_retry().is_none(),
            "a first start never retries"
        );
        let before = session.frame().expect("first frame").frame_seq();

        session
            .reset()
            .expect("the host-transport relaunch failure is retried and succeeds");
        // Three spawns: the original guest, the injected failure, the retry.
        assert_eq!(
            std::fs::read_to_string(&starts).unwrap().trim(),
            "3",
            "the retry must be a real new acos-art process"
        );
        let retry = session
            .relaunch_retry()
            .expect("the reset must state that it retried");
        assert_eq!(retry.get("attempts").and_then(|v| v.as_u64()), Some(2));
        let reason = retry
            .get("reason")
            .and_then(|v| v.as_str())
            .expect("the retry names its reason");
        assert!(
            reason.contains("binder") || reason.contains("window add"),
            "unexpected retry reason: {reason}"
        );
        assert!(
            retry
                .get("first_failure")
                .and_then(|v| v.as_str())
                .is_some_and(|text| text.contains("LIVE_FRAME_UNAVAILABLE")),
            "the first failure must be carried, not replaced: {retry}"
        );
        // The first attempt's own evidence is byte-identical, and the retry
        // record exists both beside it and at the artifacts root.
        let preserved = dir.join("relaunch-retry-1/session-exit.json");
        assert_eq!(
            std::fs::read(&preserved).expect("preserved session-exit.json"),
            injected.as_bytes(),
            "the first failure's session-exit.json must be preserved verbatim"
        );
        assert!(dir.join("relaunch-retry-1/relaunch-retry.json").is_file());
        assert!(dir.join("relaunch-retry.json").is_file());

        // The relaunched guest is live: the retry is a real recovery, not a pass.
        let after = session.frame().expect("the retried guest serves a frame");
        assert!(
            after.frame_seq() > before,
            "{} -> {}",
            before,
            after.frame_seq()
        );
        session.close().expect("clean close");
        std::env::remove_var("ACOS_ART_BIN");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// S3b: the retry is bounded to **host transport**. An ordinary app startup
    /// wall must fail the reset on the first attempt, with no second spawn and
    /// no retry record — a bounded retry must never hide an app failure.
    #[test]
    fn reset_does_not_retry_an_app_startup_wall() {
        let dir = o2_scratch_dir("relaunch-retry-app-wall");
        let injected = concat!(
            "{\"code\":\"GUEST_EXIT_AFTER_RESUME\",\"guest_exit\":1,\"guest_signal\":null,",
            "\"stdout_tail\":[\"java.lang.RuntimeException: Application onCreate threw\"]}\n"
        );
        let exit_json = dir.join("first-attempt-session-exit.json");
        std::fs::write(&exit_json, injected).unwrap();
        let script = transport_failing_art_binary(&dir, &exit_json);
        let apk = dir.join("fake.apk");
        std::fs::write(&apk, b"not-a-real-apk").unwrap();
        let starts = dir.join("starts.log");
        let _art_bin_guard = art_bin_env_lock();
        std::env::set_var("ACOS_ART_BIN", &script);

        let mut session = ArtSession::start(&apk, &dir).expect("fake session starts");
        let error = session
            .reset()
            .expect_err("an app wall must fail the reset");
        assert!(
            error.to_string().contains("LIVE_FRAME_UNAVAILABLE"),
            "the failure must stay visible: {error}"
        );
        assert_eq!(
            std::fs::read_to_string(&starts).unwrap().trim(),
            "2",
            "an app wall must not be retried: only the original start and one relaunch"
        );
        assert!(session.relaunch_retry().is_none());
        assert!(!dir.join("relaunch-retry-1").exists());
        assert!(!dir.join("relaunch-retry.json").exists());
        // A failed relaunch is a provably dead session (W55), never a stale frame.
        assert!(session.last_frame().is_none());
        std::env::remove_var("ACOS_ART_BIN");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_host_transport_startup_failures_are_retryable() {
        let dir = scratch_dir("relaunch-retry-classify");
        std::fs::create_dir_all(dir.join("art-frames")).unwrap();
        let write = |body: &str| {
            std::fs::write(dir.join("art-frames/session-exit.json"), body).unwrap();
        };
        let error = |text: &str| BackendError::Protocol {
            code: acos_protocol::ErrorCode::GuestExited,
            detail: text.to_string(),
            hint: None,
            details: None,
        };
        // A real app wall is never retried.
        write(
            r#"{"code":"GUEST_EXITED","guest_exit":1,"stdout_tail":["Application onCreate threw"]}"#,
        );
        assert!(relaunch_transport_reason(&error("GUEST_EXITED"), &dir).is_none());
        // The S3b host classes are.
        write(
            r#"{"code":"GUEST_EXIT_AFTER_RESUME","guest_exit":1,"logd_tail":["vsync thread: Resource temporarily unavailable (os error 11)"]}"#,
        );
        assert!(relaunch_transport_reason(&error("GUEST_EXITED"), &dir)
            .unwrap()
            .contains("vsync thread"));
        write(
            r#"{"code":"GUEST_EXIT_AFTER_RESUME","guest_exit":1,"stdout_tail":["E/Parcel: ipcSetDataReference: unsupported type object (7209057) at offset 16","Adding window failed"]}"#,
        );
        assert!(relaunch_transport_reason(&error("GUEST_EXITED"), &dir).is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- G3f1: the launch's content wait ---------------------------------- //
    //
    // A fake `acos-art` that answers the interactive protocol with the *launch
    // skeleton* first and, for a request that carries `baseline_tree`, with the
    // app's content — or with nothing at all. It records every request so a test
    // can prove how many traversals a launch actually paid for.

    const FAKE_SKELETON: &str = concat!(
        r#"{"nodes":[{"class":"com.android.internal.policy.DecorView","children":[{"class":"android.widget.FrameLayout","children":[{"class":"android.widget.LinearLayout","children":[{"class":"android.widget.FrameLayout","children":[{"class":"com.facebook.react.ReactRootView"}]}]}]}]}]}"#
    );
    const FAKE_CONTENT: &str = concat!(
        r#"{"nodes":[{"class":"com.android.internal.policy.DecorView","children":[{"class":"android.widget.FrameLayout","children":[{"class":"android.widget.TextView","text":"Set up Gaten","children":[]}]}]}]}"#
    );

    fn g3f1_scratch(name: &str) -> PathBuf {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.cache/g3f1-tests");
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

    /// `mode`: `content-first` | `late` | `never` | `unsettled-change` |
    /// `skeleton-animating` | `unsettled-first`.
    ///
    /// FB2c: the frame identity (`frame_seq`/`md5`) is what the placeholder
    /// verdict measures, so the fake is explicit about it: `never` re-publishes
    /// one stored frame (constant identity, like an app whose SoLoader loop never
    /// mounts), every other mode publishes a new one.
    fn fake_first_frame_art(dir: &Path, mode: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let png = dir.join("tiny.png");
        std::fs::write(&png, super::super::fake::TINY_PNG).unwrap();
        let script = dir.join("fake-acos-art.sh");
        let requests = dir.join("requests.log");
        let mut body = String::new();
        body.push_str("#!/bin/sh\n");
        body.push_str("echo '{\"ok\":true,\"cmd\":\"ready\",\"package\":\"dev.test\",\"activity\":\"dev.test/.Main\",\"width\":1080,\"height\":2400,\"density\":420}'\n");
        body.push_str("seq=0\ngated=0\nstartns=$(date +%s%N)\n");
        body.push_str(&format!("requests='{}'\n", requests.display()));
        body.push_str(&format!("png='{}'\n", png.display()));
        // The mode lives in a file so a test can change the app's behaviour
        // mid-session (FB2c: a placeholder that later mounts).
        let mode_file = dir.join("mode.txt");
        std::fs::write(&mode_file, mode).unwrap();
        body.push_str(&format!("mode='{mode}'\n"));
        body.push_str(&format!("mode_file='{}'\n", mode_file.display()));
        body.push_str(&format!("skeleton='{FAKE_SKELETON}'\n"));
        body.push_str(&format!("content='{FAKE_CONTENT}'\n"));
        body.push_str("while IFS= read -r line; do\n");
        body.push_str("  printf '%s\\n' \"$line\" >> \"$requests\"\n");
        body.push_str("  mode=$(cat \"$mode_file\")\n");
        body.push_str("  case \"$line\" in\n");
        body.push_str("    *'\"cmd\":\"tree\"'*)\n");
        body.push_str("      seq=$((seq+1))\n");
        // A `pixels:false` request gets no PNG (like the real launcher).
        body.push_str("      case \"$line\" in *'\"pixels\":false'*) pngf= ;; *) pngf=\"\\\"png\\\":\\\"$png\\\",\" ;; esac\n");
        body.push_str("      fseq=$seq\n");
        body.push_str("      fmd5=m$seq\n");
        body.push_str("      if [ \"$mode\" = never ]; then fseq=1; fmd5=m1; fi\n");
        // GATEFIX (F1): the real launcher answers with its *cached* frame while
        // the app is still booting, so the frame identity does not move until the
        // app draws. `slow-mount` reproduces that: a constant skeleton identity
        // for the first 400 ms, then the app's content.
        body.push_str("      elapsed=0\n");
        body.push_str("      if [ \"$mode\" = slow-mount ]; then now=$(date +%s%N); elapsed=$(( (now - startns) / 1000000 )); fi\n");
        body.push_str("      if [ \"$mode\" = slow-mount ] && [ \"$elapsed\" -lt 400 ]; then fseq=1; fmd5=m1; fi\n");
        body.push_str("      case \"$line\" in\n");
        body.push_str("        *'baseline_tree'*)\n");
        body.push_str("          gated=$((gated+1))\n");
        body.push_str("          case \"$mode\" in\n");
        body.push_str("            late)\n");
        body.push_str("              echo \"{\\\"ok\\\":true,\\\"cmd\\\":\\\"tree\\\",\\\"frame_seq\\\":$fseq,\\\"md5\\\":\\\"$fmd5\\\",$pngf\\\"tree\\\":$content,\\\"settle\\\":{\\\"mode\\\":\\\"strict\\\",\\\"settled\\\":true,\\\"bound\\\":\\\"strict-event\\\",\\\"awaited\\\":\\\"tree-change\\\"}}\"\n");
        body.push_str("              ;;\n");
        body.push_str("            never)\n");
        body.push_str("              echo \"{\\\"ok\\\":true,\\\"cmd\\\":\\\"tree\\\",\\\"frame_seq\\\":$fseq,\\\"md5\\\":\\\"$fmd5\\\",$pngf\\\"tree\\\":$skeleton,\\\"settle\\\":{\\\"mode\\\":\\\"strict\\\",\\\"settled\\\":false,\\\"bound\\\":\\\"timeout\\\",\\\"reason\\\":\\\"tree-unchanged\\\",\\\"awaited\\\":\\\"tree-change\\\"}}\"\n");
        body.push_str("              ;;\n");
        body.push_str("            skeleton-animating)\n");
        body.push_str("              echo \"{\\\"ok\\\":true,\\\"cmd\\\":\\\"tree\\\",\\\"frame_seq\\\":$fseq,\\\"md5\\\":\\\"$fmd5\\\",$pngf\\\"tree\\\":$skeleton,\\\"settle\\\":{\\\"mode\\\":\\\"strict\\\",\\\"settled\\\":false,\\\"bound\\\":\\\"timeout\\\",\\\"reason\\\":\\\"animating\\\",\\\"awaited\\\":\\\"tree-change\\\"}}\"\n");
        body.push_str("              ;;\n");
        body.push_str("            late-change-draw)\n");
        body.push_str("              echo \"{\\\"ok\\\":true,\\\"cmd\\\":\\\"tree\\\",\\\"frame_seq\\\":$fseq,\\\"md5\\\":\\\"$fmd5\\\",$pngf\\\"tree\\\":$content,\\\"settle\\\":{\\\"mode\\\":\\\"strict\\\",\\\"settled\\\":false,\\\"bound\\\":\\\"change-draw\\\",\\\"awaited\\\":\\\"tree-change\\\",\\\"changed_since_baseline\\\":true}}\"\n");
        body.push_str("              ;;\n");
        body.push_str("            unsettled-first)\n");
        body.push_str("              echo \"{\\\"ok\\\":true,\\\"cmd\\\":\\\"tree\\\",\\\"frame_seq\\\":$fseq,\\\"md5\\\":\\\"$fmd5\\\",$pngf\\\"tree\\\":$content,\\\"settle\\\":{\\\"mode\\\":\\\"strict\\\",\\\"settled\\\":true,\\\"bound\\\":\\\"strict-event\\\",\\\"awaited\\\":\\\"tree-change\\\"}}\"\n");
        body.push_str("              ;;\n");
        body.push_str("            slow-mount)\n");
        body.push_str("              if [ \"$elapsed\" -ge 400 ]; then\n");
        body.push_str("                echo \"{\\\"ok\\\":true,\\\"cmd\\\":\\\"tree\\\",\\\"frame_seq\\\":$fseq,\\\"md5\\\":\\\"$fmd5\\\",$pngf\\\"tree\\\":$content,\\\"settle\\\":{\\\"mode\\\":\\\"strict\\\",\\\"settled\\\":true,\\\"bound\\\":\\\"strict-event\\\",\\\"awaited\\\":\\\"tree-change\\\"}}\"\n");
        body.push_str("              else\n");
        body.push_str("                echo \"{\\\"ok\\\":true,\\\"cmd\\\":\\\"tree\\\",\\\"frame_seq\\\":$fseq,\\\"md5\\\":\\\"$fmd5\\\",$pngf\\\"tree\\\":$skeleton,\\\"settle\\\":{\\\"mode\\\":\\\"strict\\\",\\\"settled\\\":false,\\\"bound\\\":\\\"timeout\\\",\\\"reason\\\":\\\"tree-unchanged\\\",\\\"awaited\\\":\\\"tree-change\\\"}}\"\n");
        body.push_str("              fi\n");
        body.push_str("              ;;\n");
        body.push_str("            wait-dies)\n");
        body.push_str("              echo '{\"ok\":false,\"cmd\":\"tree\",\"code\":\"INTERNAL\",\"error\":\"the guest stopped answering\"}'\n");
        body.push_str("              ;;\n");
        body.push_str("            unsettled-change)\n");
        body.push_str("              echo \"{\\\"ok\\\":true,\\\"cmd\\\":\\\"tree\\\",\\\"frame_seq\\\":$fseq,\\\"md5\\\":\\\"$fmd5\\\",$pngf\\\"tree\\\":$content,\\\"settle\\\":{\\\"mode\\\":\\\"strict\\\",\\\"settled\\\":false,\\\"bound\\\":\\\"timeout\\\",\\\"reason\\\":\\\"animating\\\",\\\"awaited\\\":\\\"tree-change\\\"}}\"\n");
        body.push_str("              ;;\n");
        body.push_str("          esac\n");
        body.push_str("          ;;\n");
        body.push_str("        *)\n");
        body.push_str("          tree=$skeleton\n");
        body.push_str("          [ \"$mode\" = content-first ] && tree=$content\n");
        body.push_str("          if [ \"$mode\" = slow-mount ] && [ \"$elapsed\" -ge 400 ]; then tree=$content; fi\n");
        body.push_str("          if [ \"$mode\" = wait-dies ]; then\n");
        body.push_str("            echo \"{\\\"ok\\\":true,\\\"cmd\\\":\\\"tree\\\",\\\"frame_seq\\\":$fseq,\\\"md5\\\":\\\"$fmd5\\\",$pngf\\\"tree\\\":$skeleton,\\\"settle\\\":{\\\"mode\\\":\\\"fast\\\",\\\"settled\\\":true,\\\"bound\\\":\\\"probe-fresh\\\"}}\"\n");
        body.push_str("            continue\n");
        body.push_str("          fi\n");
        body.push_str("          if [ \"$mode\" = unsettled-first ]; then\n");
        body.push_str("            echo \"{\\\"ok\\\":true,\\\"cmd\\\":\\\"tree\\\",\\\"frame_seq\\\":$fseq,\\\"md5\\\":\\\"$fmd5\\\",$pngf\\\"tree\\\":$tree,\\\"settle\\\":{\\\"mode\\\":\\\"strict\\\",\\\"settled\\\":false,\\\"bound\\\":\\\"timeout\\\",\\\"reason\\\":\\\"animating\\\",\\\"awaited\\\":\\\"tree-change\\\"}}\"\n");
        body.push_str("          else\n");
        body.push_str("            echo \"{\\\"ok\\\":true,\\\"cmd\\\":\\\"tree\\\",\\\"frame_seq\\\":$fseq,\\\"md5\\\":\\\"$fmd5\\\",$pngf\\\"tree\\\":$tree,\\\"settle\\\":{\\\"mode\\\":\\\"strict\\\",\\\"settled\\\":true,\\\"bound\\\":\\\"strict-event\\\"}}\"\n");
        body.push_str("          fi\n");
        body.push_str("          ;;\n");
        body.push_str("      esac\n");
        body.push_str("      ;;\n");
        body.push_str("    *'\"cmd\":\"close\"'*)\n");
        body.push_str("      echo '{\"ok\":true,\"cmd\":\"close\"}'\n");
        body.push_str("      exit 0\n");
        body.push_str("      ;;\n");
        body.push_str("    *)\n");
        body.push_str("      echo '{\"ok\":false,\"code\":\"BAD_REQUEST\",\"error\":\"no\"}'\n");
        body.push_str("      ;;\n");
        body.push_str("  esac\n");
        body.push_str("done\n");
        std::fs::write(&script, body).unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
        script
    }

    fn start_fake_first_frame(dir: &Path, mode: &str) -> ArtSession {
        let script = fake_first_frame_art(dir, mode);
        let apk = dir.join("fake.apk");
        std::fs::write(&apk, b"not-a-real-apk").unwrap();
        std::env::set_var("ACOS_ART_BIN", &script);
        ArtSession::start(&apk, dir).expect("fake session starts")
    }

    /// Late content: the launch's own traversal binds the skeleton, the event
    /// wait returns the app's content frame, and the launch reports one wait.
    #[test]
    fn g3f1_late_content_binds_the_content_frame() {
        let dir = g3f1_scratch("late");
        let _guard = art_bin_env_lock();
        let mut session = start_fake_first_frame(&dir, "late");
        let tree = session
            .ensure_launch_tree("strict", true)
            .expect("launch binds");
        let report = session.first_frame_report().expect("report");
        assert_eq!(report.traversals, 2, "{report:?}");
        assert_eq!(report.event_waits, 1, "{report:?}");
        assert!(
            !report.placeholder,
            "content must not be a placeholder: {report:?}"
        );
        assert!(report.settled, "a settled content bind says so: {report:?}");
        assert_eq!(report.bound.as_deref(), Some("strict-event"), "{report:?}");
        assert_eq!(report.nodes, ui_tree_node_count(&tree.nodes));
        assert!(
            ui_tree_has_app_text(&tree.nodes),
            "the bound tree is the content frame"
        );
        assert!(
            !first_frame_is_unmounted(&tree),
            "content must not be treated as unmounted"
        );
        // The wait is a *change* request: it carried the skeleton as its
        // baseline and the remaining wall-clock budget.
        let requests = std::fs::read_to_string(dir.join("requests.log")).unwrap();
        assert_eq!(
            requests.matches("\"cmd\":\"tree\"").count(),
            2,
            "exactly one launch traversal + one event wait: {requests}"
        );
        assert!(
            requests.contains("\"baseline_tree\""),
            "the wait must hand the skeleton back as the baseline: {requests}"
        );
        assert!(
            requests.contains("\"settle_timeout_ms\""),
            "the wait must be bounded by the wall clock: {requests}"
        );
        std::env::remove_var("ACOS_ART_BIN");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Content never arrives: the label is a placeholder, the frame is the
    /// skeleton and the settle verdict is rolled back to the last settled
    /// traversal (never the timed-out one).
    #[test]
    fn g3f1_missing_content_stays_a_labelled_placeholder() {
        let dir = g3f1_scratch("never");
        let _guard = art_bin_env_lock();
        let mut session = start_fake_first_frame(&dir, "never");
        std::env::set_var("ACOS_ART_FIRST_FRAME_MS", "60");
        let tree = session
            .ensure_launch_tree("strict", true)
            .expect("launch binds a placeholder");
        std::env::remove_var("ACOS_ART_FIRST_FRAME_MS");
        let report = session.first_frame_report().expect("report");
        assert!(report.placeholder, "an unmounted frame is a placeholder: {report:?}");
        // GATEFIX (F1): the wait is bounded by the content *window*, so a fake
        // that answers instantly is observed again (paced) until the window has
        // run out. Every observation after the launch's own traversal is a gated
        // change request, and the verdict rests on the whole window.
        assert!(report.traversals >= 2, "{report:?}");
        assert_eq!(report.event_waits, report.traversals - 1, "{report:?}");
        assert!(report.window_spent, "{report:?}");
        assert_eq!(report.window_ms, 60, "{report:?}");
        assert!(
            report.settled,
            "the rolled-back frame is the settled skeleton: {report:?}"
        );
        assert_eq!(report.bound.as_deref(), Some("strict-event"), "{report:?}");
        assert!(
            first_frame_is_bare_skeleton(&tree),
            "the placeholder is the skeleton itself, not invented content"
        );
        let settle = session.last_tree_settle().expect("settle block");
        assert_eq!(
            settle.get("settled").and_then(|v| v.as_bool()),
            Some(true),
            "a timed-out retry must roll the settle verdict back to the settled one: {settle}"
        );
        assert_eq!(
            settle.get("bound").and_then(|v| v.as_str()),
            Some("strict-event"),
            "{settle}"
        );
        // FB2c: the placeholder needed evidence beyond the tree — the frame
        // never moved (constant identity, identical pixels) — and the label is
        // what refuses a screenshot.
        assert!(!report.frames_advanced, "{report:?}");
        assert!(report.pixels_compared, "{report:?}");
        assert!(!report.pixels_changed, "{report:?}");
        assert!(session.placeholder_frame());
        std::env::remove_var("ACOS_ART_BIN");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// FB2c item 1: a text-free skeleton whose pixels **change** is a mounted,
    /// image-only screen (a custom `View`, a SurfaceView/GL surface, an
    /// image-only activity, Flutter without semantics). The node count alone used
    /// to call it a placeholder — a blocked verdict on both routes. It is a pass
    /// with a `placeholder_suspect` warning now.
    #[test]
    fn fb2c_a_skeleton_with_changing_pixels_passes_with_a_suspect_warning() {
        let dir = g3f1_scratch("skeleton-animating");
        let _guard = art_bin_env_lock();
        let mut session = start_fake_first_frame(&dir, "skeleton-animating");
        let apk = dir.join("fake.apk");
        let outcome = session
            .launch(&apk)
            .expect("a text-free mounted screen must not be a blocked launch");
        let report = session.first_frame_report().expect("report");
        assert!(
            !report.placeholder,
            "changing pixels are app frame: {report:?}"
        );
        assert!(
            report.suspect,
            "…but the skeleton is still a suspicion: {report:?}"
        );
        assert!(
            report.pixels_compared && report.pixels_changed,
            "{report:?}"
        );
        assert!(
            !session.placeholder_frame(),
            "a suspect frame is not a placeholder: no screenshot may be refused"
        );
        assert_eq!(
            outcome.detail["first_frame"]["placeholder_suspect"],
            serde_json::json!(true),
            "the launch reply carries the warning: {}",
            outcome.detail
        );
        assert_eq!(
            outcome.detail["first_frame"]["placeholder"],
            serde_json::json!(false),
            "{}",
            outcome.detail
        );
        std::env::remove_var("ACOS_ART_BIN");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// FB2c item 2: a first traversal that does not settle (an animating splash)
    /// used to answer `placeholder` with `waited_ms: 0` — the content budget never
    /// ran. The wait now runs first and the launch binds the content frame.
    #[test]
    fn fb2c_an_unsettled_first_traversal_still_spends_the_content_budget() {
        let dir = g3f1_scratch("unsettled-first");
        let _guard = art_bin_env_lock();
        let mut session = start_fake_first_frame(&dir, "unsettled-first");
        let tree = session
            .ensure_launch_tree("strict", true)
            .expect("the wait binds the content frame");
        let report = session.first_frame_report().expect("report");
        assert_eq!(report.traversals, 2, "the content wait ran: {report:?}");
        assert_eq!(report.event_waits, 1, "{report:?}");
        assert!(!report.placeholder && !report.suspect, "{report:?}");
        assert!(
            ui_tree_has_app_text(&tree.nodes),
            "the bound tree is the app's content, not the splash skeleton"
        );
        assert_eq!(report.budget_ms, first_frame_budget_ms(), "{report:?}");
        std::env::remove_var("ACOS_ART_BIN");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// GATEFIX (F1, the v0.3.0 release-blocker): on a quiet host a `launch` step
    /// that carried `settle: content` answered `PLACEHOLDER_CONTENT` (2 of 32
    /// matrix negatives passed on a busy host and failed here). The launch's own
    /// traversal is `fast`, and the mount wait inherited the fast rule's missing
    /// gate: eight samples of the guest's stale skeleton cost ~0 ms, so the
    /// window was declared a placeholder before the app ever ran.
    ///
    /// The rule (ADR-0032 amendment): a placeholder needs the skeleton to be
    /// stable over the whole content **window**, confirmed by the guest's change
    /// signal — not N traversals. This test mounts the app 400 ms after the
    /// launch frame: before the fix the launch bound the skeleton in <1 ms and
    /// reported `placeholder`; now the wait asks the guest for its next committed
    /// tree state, keeps observing until content arrives, and binds it.
    #[test]
    fn gatefix_a_slow_mount_is_never_called_a_placeholder() {
        let dir = g3f1_scratch("slow-mount");
        let _guard = art_bin_env_lock();
        std::env::set_var("ACOS_ART_FIRST_FRAME_MS", "3000");
        let mut session = start_fake_first_frame(&dir, "slow-mount");
        let started = Instant::now();
        let tree = session
            .ensure_launch_tree("fast", false)
            .expect("a slow mount binds the app's frame");
        let elapsed = started.elapsed();
        std::env::remove_var("ACOS_ART_FIRST_FRAME_MS");
        let report = session.first_frame_report().expect("report");
        assert!(
            ui_tree_has_app_text(&tree.nodes),
            "the app's content is the frame, not the skeleton: {report:?}"
        );
        assert!(
            !report.placeholder && !report.suspect,
            "content that mounted inside the window is never a placeholder: {report:?}"
        );
        assert!(report.event_waits >= 1, "the wait is a change request: {report:?}");
        assert!(
            elapsed >= Duration::from_millis(300),
            "the wait has to give the app time to draw (was {elapsed:?}): {report:?}"
        );
        std::env::remove_var("ACOS_ART_BIN");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The other half of the same rule: a launch that really never mounts is
    /// still a placeholder, but only after the whole window was observed — never
    /// from a burst of samples taken within a millisecond of the window mount.
    #[test]
    fn gatefix_a_real_placeholder_still_blocks_after_the_window() {
        let dir = g3f1_scratch("never-window");
        let _guard = art_bin_env_lock();
        std::env::set_var("ACOS_ART_FIRST_FRAME_MS", "150");
        let mut session = start_fake_first_frame(&dir, "never");
        let started = Instant::now();
        let tree = session
            .ensure_launch_tree("fast", false)
            .expect("the launch binds the window it observed");
        let elapsed = started.elapsed();
        std::env::remove_var("ACOS_ART_FIRST_FRAME_MS");
        let report = session.first_frame_report().expect("report");
        assert!(!ui_tree_has_app_text(&tree.nodes));
        assert!(report.placeholder, "a skeleton that never mounted is a placeholder: {report:?}");
        assert!(!report.suspect, "{report:?}");
        assert!(
            report.window_spent && report.window_ms >= 150,
            "the verdict rests on the whole window: {report:?}"
        );
        assert!(
            elapsed >= Duration::from_millis(100),
            "the verdict cannot come from a burst (was {elapsed:?}): {report:?}"
        );
        std::env::remove_var("ACOS_ART_BIN");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A wait whose guest stops answering cannot prove either outcome. It is the
    /// guest's error, never a `placeholder` claim about an app that had no
    /// window — the fail-closed half of the F1 rule.
    #[test]
    fn gatefix_a_wait_that_cannot_observe_the_window_is_not_a_placeholder() {
        let dir = g3f1_scratch("wait-dies");
        let _guard = art_bin_env_lock();
        std::env::set_var("ACOS_ART_FIRST_FRAME_MS", "3000");
        let mut session = start_fake_first_frame(&dir, "wait-dies");
        let error = session
            .ensure_launch_tree("fast", false)
            .expect_err("a guest that stops answering must not become a placeholder");
        std::env::remove_var("ACOS_ART_FIRST_FRAME_MS");
        let text = error.to_string();
        assert!(
            text.contains("INTERNAL") || text.contains("guest stopped answering"),
            "the guest's own failure is reported: {error}"
        );
        assert!(
            session.first_frame_report().is_none(),
            "no verdict may be published from a wait that could not observe"
        );
        std::env::remove_var("ACOS_ART_BIN");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// FB2c item 4: the placeholder label is not sticky. Once a later frame
    /// carries content the session answers from that frame, so a second launch or
    /// a screenshot is not refused forever.
    #[test]
    fn fb2c_a_placeholder_is_cleared_once_a_later_frame_has_content() {
        let dir = g3f1_scratch("sticky");
        let _guard = art_bin_env_lock();
        let mut session = start_fake_first_frame(&dir, "never");
        std::env::set_var("ACOS_ART_FIRST_FRAME_MS", "60");
        session
            .ensure_launch_tree("strict", true)
            .expect("the launch binds");
        std::env::remove_var("ACOS_ART_FIRST_FRAME_MS");
        assert!(session.placeholder_frame(), "the launch is a placeholder");
        // The app mounts after all (the fake reads its mode each request).
        std::fs::write(dir.join("mode.txt"), "content-first").unwrap();
        let tree = session
            .tree_with_settle("strict")
            .expect("a later traversal");
        assert!(
            ui_tree_has_app_text(&tree.nodes),
            "the frame has content now"
        );
        assert!(
            !session.placeholder_frame(),
            "the placeholder label must clear when content arrives"
        );
        assert!(
            session.first_frame_report().is_none(),
            "a cleared report is cleared, not silently rewritten"
        );
        // …and the screenshot path works again (no `NO_FRESH_FRAME` forever).
        // …a second launch is judged on the current frame, not refused, and its
        // own PNG is what a screenshot serves.
        session
            .ensure_launch_tree("strict", true)
            .expect("a relaunch after the app mounted is not blocked");
        session
            .request_tree_with("strict", true)
            .expect("a traversal with pixels");
        let tree = session.require_cached_tree().expect("bound tree");
        assert!(!session.placeholder_frame());
        let (path, seq, _md5, bytes) = session
            .screenshot_source()
            .expect("pixels of the content frame");
        assert!(path.is_file() && bytes > 0);
        assert_eq!(seq, tree.frame.frame_seq);
        std::env::remove_var("ACOS_ART_BIN");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// FB2c item 3: `reset` binds the relaunched guest's first frame through the
    /// same content wait and placeholder verdict a launch uses. A relaunch that
    /// never mounts is `PLACEHOLDER_CONTENT`, not a `first_frame` pass.
    #[test]
    fn fb2c_a_reset_that_never_mounts_is_blocked() {
        let dir = g3f1_scratch("reset-never");
        let _guard = art_bin_env_lock();
        let mut session = start_fake_first_frame(&dir, "never");
        std::env::set_var("ACOS_ART_FIRST_FRAME_MS", "60");
        let error = session
            .reset()
            .expect_err("a relaunch that never mounts is not a pass");
        std::env::remove_var("ACOS_ART_FIRST_FRAME_MS");
        match &error {
            BackendError::Error { code, detail } => {
                assert_eq!(*code, crate::engine::ErrorCode::PlaceholderContent);
                assert!(detail.contains("placeholder-content"), "{detail}");
                assert!(detail.contains("ACOS_ART_FIRST_FRAME_MS"), "{detail}");
            }
            other => panic!("expected PLACEHOLDER_CONTENT, got {other:?}"),
        }
        assert!(session.last_frame().is_none(), "no stale frame survives");
        assert!(!session.placeholder_frame());
        std::env::remove_var("ACOS_ART_BIN");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// FB2c item 5/6: the screenshot source is the frame the tree is bound to.
    /// A tree bound without pixels is refused instead of being paired with a
    /// newer capture.
    #[test]
    fn fb2c_a_screenshot_never_comes_from_a_newer_frame_than_the_tree() {
        let dir = g3f1_scratch("shot-frame");
        let _guard = art_bin_env_lock();
        let mut session = start_fake_first_frame(&dir, "late");
        session
            .ensure_launch_tree("strict", true)
            .expect("the launch binds");
        let (path, seq, _md5, bytes) = session.screenshot_source().expect("the bound PNG");
        assert!(path.is_file() && bytes > 0);
        assert_eq!(Some(seq), session.frame_info().map(|info| info.0));
        // A tree traversal without pixels has no artifact of its own.
        session
            .tree_with_settle("strict")
            .expect("a pixels:false traversal");
        let error = session
            .screenshot_source()
            .expect_err("a tree without its own pixels must not borrow a newer frame");
        assert!(
            error.to_string().contains("NO_FRESH_FRAME") || error.to_string().contains("frame"),
            "{error}"
        );
        // The screenshot path asks the *same traversal* for pixels, so the tree
        // and the PNG of one reply are one frame.
        let tree = session
            .tree_with_settle_pixels("strict", true)
            .expect("a traversal that captures");
        let (path, seq, _md5, bytes) = session.screenshot_source().expect("the same frame's PNG");
        assert!(path.is_file() && bytes > 0);
        assert_eq!(
            seq, tree.frame.frame_seq,
            "screenshot.frame_seq == tree.frame_seq"
        );
        std::env::remove_var("ACOS_ART_BIN");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A screen that never goes quiet (Gaten's welcome timeline) still commits
    /// draws: the wait ends on the app's next draw commit, the frame is the
    /// content frame, and the reply says `settled:false` instead of pretending a
    /// quiet screen.
    #[test]
    fn g3f1_draw_commit_binds_content_and_says_it_is_not_settled() {
        let dir = g3f1_scratch("change-draw");
        let _guard = art_bin_env_lock();
        let mut session = start_fake_first_frame(&dir, "late-change-draw");
        let tree = session
            .ensure_launch_tree("strict", true)
            .expect("launch binds");
        let report = session.first_frame_report().expect("report");
        assert_eq!(report.traversals, 2, "{report:?}");
        assert_eq!(report.event_waits, 1, "{report:?}");
        assert!(!report.placeholder, "content is content: {report:?}");
        assert!(
            !report.settled,
            "a draw-commit bind is fresh, not settled, and must say so: {report:?}"
        );
        assert_eq!(report.bound.as_deref(), Some("change-draw"), "{report:?}");
        assert!(ui_tree_has_app_text(&tree.nodes));
        // The frame is not assertion evidence: the settle block keeps
        // `settled:false`, so an assertion can never rest on it.
        let settle = session.last_tree_settle().expect("settle block");
        assert_eq!(
            settle.get("settled").and_then(|v| v.as_bool()),
            Some(false),
            "{settle}"
        );
        std::env::remove_var("ACOS_ART_BIN");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A changed but unsettled retry also rolls back: the wait can never turn a
    /// passing launch into a TIMEOUT, and never binds a frame it cannot prove.
    #[test]
    fn g3f1_unsettled_retry_rolls_back_to_the_settled_traversal() {
        let dir = g3f1_scratch("unsettled");
        let _guard = art_bin_env_lock();
        let mut session = start_fake_first_frame(&dir, "unsettled-change");
        let tree = session
            .ensure_launch_tree("strict", true)
            .expect("launch binds");
        let report = session.first_frame_report().expect("report");
        // FB2c: the retry *saw* app content (a newer frame with different
        // pixels), it just never settled. The launch is not a blocked
        // placeholder any more (the frame moved) — it keeps the settled skeleton
        // as its bound frame and carries the `placeholder_suspect` warning, so no
        // assertion can rest on a frame the launch could not prove.
        assert!(!report.placeholder, "{report:?}");
        assert!(report.suspect, "{report:?}");
        assert_eq!(report.traversals, 2, "{report:?}");
        assert!(
            first_frame_is_bare_skeleton(&tree),
            "an unsettled retry rolls the frame back to the settled skeleton"
        );
        let settle = session.last_tree_settle().expect("settle block");
        assert_eq!(
            settle.get("settled").and_then(|v| v.as_bool()),
            Some(true),
            "the rolled-back verdict is the settled one: {settle}"
        );
        std::env::remove_var("ACOS_ART_BIN");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An app whose first frame already carries content pays ZERO extra
    /// traversals: no baseline request is ever sent.
    #[test]
    fn g3f1_content_first_pays_no_extra_traversal() {
        let dir = g3f1_scratch("content-first");
        let _guard = art_bin_env_lock();
        let mut session = start_fake_first_frame(&dir, "content-first");
        let tree = session
            .ensure_launch_tree("strict", true)
            .expect("launch binds");
        let report = session.first_frame_report().expect("report");
        assert_eq!(report.traversals, 1, "{report:?}");
        assert_eq!(report.event_waits, 0, "{report:?}");
        // The single traversal's own wall time (a forked shell answers in ~1 ms);
        // no content wait ran, which `event_waits == 0` and the request count prove.
        assert!(report.waited_ms < 250, "{report:?}");
        assert!(!report.placeholder, "{report:?}");
        assert!(report.settled, "{report:?}");
        assert!(ui_tree_has_app_text(&tree.nodes));
        let requests = std::fs::read_to_string(dir.join("requests.log")).unwrap();
        assert_eq!(
            requests.matches("\"cmd\":\"tree\"").count(),
            1,
            "a content-first launch must not re-traverse: {requests}"
        );
        assert!(!requests.contains("\"baseline_tree\""), "{requests}");
        std::env::remove_var("ACOS_ART_BIN");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A guest that dies before its first frame keeps its evidence under the
    /// session's artifacts and is classified (G3f1/b).
    #[test]
    fn g3f1_prelaunch_guest_exit_keeps_evidence_and_is_classified() {
        use std::os::unix::fs::PermissionsExt;
        let dir = g3f1_scratch("prelaunch-exit");
        let script = dir.join("fake-acos-art.sh");
        let out = dir.join("art-frames");
        std::fs::create_dir_all(&out).unwrap();
        let mut body = String::new();
        body.push_str("#!/bin/sh\n");
        body.push_str("echo '{\"ok\":true,\"cmd\":\"ready\",\"package\":\"dev.test\",\"activity\":\"dev.test/.Main\",\"width\":1080,\"height\":2400,\"density\":420}'\n");
        body.push_str("while IFS= read -r line; do\n");
        body.push_str("  case \"$line\" in\n");
        body.push_str("    *'\"cmd\":\"tree\"'*)\n");
        // The reply comes first and the exit record only as the launcher tears
        // down: exactly the pre-fix race that lost the evidence.
        body.push_str("      echo '{\"ok\":false,\"cmd\":\"tree\",\"code\":\"GUEST_EXITED\",\"error\":\"the guest ended while binding the tree (status exit status: 1)\"}'\n");
        body.push_str("      sleep 0.2\n");
        body.push_str(&format!(
            "      printf '%s' '{{\"route\":\"launch\",\"outcome\":\"crash\",\"code\":\"GUEST_EXITED\",\"stage\":\"teardown-evidence\",\"guest_exit\":1,\"guest_signal\":null,\"native_crash\":null,\"native_crash_path\":null,\"logd_tail\":[\"I/ActivityManager: start\",\"E/AndroidRuntime: FATAL EXCEPTION: main\"],\"stdout_tail\":[\"ACOSLAUNCH TRACE\"],\"stderr_tail\":[\"boom\"]}}' > '{}'\n",
            out.join("session-exit.json").display()
        ));
        body.push_str("      exit 1\n");
        body.push_str("      ;;\n");
        body.push_str(
            "    *) echo '{\"ok\":false,\"code\":\"BAD_REQUEST\",\"error\":\"no\"}'; ;;\n",
        );
        body.push_str("  esac\n");
        body.push_str("done\n");
        std::fs::write(&script, body).unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
        let apk = dir.join("fake.apk");
        std::fs::write(&apk, b"not-a-real-apk").unwrap();
        let _guard = art_bin_env_lock();
        std::env::set_var("ACOS_ART_BIN", &script);
        let mut session = ArtSession::start(&apk, &dir).expect("fake session starts");
        let error = session
            .ensure_launch_tree("strict", false)
            .expect_err("a dead guest cannot bind a first frame");
        let evidence = session.keep_launch_failure_evidence(&error);
        let class = evidence["classification"]["class"]
            .as_str()
            .unwrap_or_default();
        assert_eq!(class, "guest-exited-before-first-frame", "{evidence}");
        assert_eq!(
            evidence["classification"]["guest_exit"].as_i64(),
            Some(1),
            "{evidence}"
        );
        assert_eq!(
            evidence["classification"]["crash_record"].as_bool(),
            Some(false),
            "{evidence}"
        );
        let logd = evidence["logd_tail"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert!(
            logd.iter().any(|line| line
                .as_str()
                .unwrap_or_default()
                .contains("FATAL EXCEPTION")),
            "the guest's last logd lines must be kept: {evidence}"
        );
        let kept = evidence["kept_path"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(evidence["kept"].as_bool().unwrap_or(false), "{evidence}");
        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&kept).unwrap()).unwrap();
        assert_eq!(
            doc["classification"]["class"].as_str(),
            Some("guest-exited-before-first-frame")
        );
        assert!(
            doc["evidence_files"]["session-exit.json"].is_string(),
            "the evidence file list names what was kept: {doc}"
        );
        std::env::remove_var("ACOS_ART_BIN");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
