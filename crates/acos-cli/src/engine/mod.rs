//! Engine selection and the session-backend seam.
//!
//! `acos verify --engine {rust-vm,art}` never guesses which engine ran. Each
//! engine is a [`SessionBackend`]: it says which commands it supports
//! ([`SessionCapabilities`]) and performs one command at a time. A scenario
//! step whose command is missing is reported as `unsupported` with that
//! command's name — never silently routed to a different engine and never
//! counted as a pass.
//!
//! Two rules are enforced here rather than left to callers:
//!
//! * the engine is explicit and appears in the verdict (`engine`, `mode`);
//! * a backend that only renders offline is [`Mode::Preview`], and a preview
//!   pass is downgraded to `unsupported` — only a live frame from a launched
//!   Activity can pass (`docs/OWNER_DIRECTION_2026-11-11.md` §5).
//!
//! [`EngineBackend`] adapts a [`SessionBackend`] to the verifier's
//! [`acos_verify::Backend`]. The Rust VM path forwards to the existing
//! `NativeBackend` unchanged, so default behaviour does not move.

use std::path::Path;

use acos_protocol::commands::{KeyName, ResetScope};
use acos_protocol::ui::{Bounds, UiNode, UiTree};
use acos_protocol::{
    Backend as ProtocolBackend, ErrorCode, Fidelity, FrameMeta, Selector,
};
use acos_verify::backend::{Capabilities, LaunchRequest};
use acos_verify::{
    Backend, BackendError, EnvironmentChange, InputAction, LaunchOutcome, LogRecord,
    ManifestInfo, SessionObservation,
};

pub mod art;
#[cfg(test)]
pub mod fake;
pub mod rust_vm;

/// The explicit execution engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    /// The in-process Rust VM / native runtime (`acos-native`).
    RustVm,
    /// Real Google ART driven through `acos-art session`.
    Art,
}

impl Engine {
    /// The stable CLI/verdict string.
    pub const fn as_str(self) -> &'static str {
        match self {
            Engine::RustVm => "rust-vm",
            Engine::Art => "art",
        }
    }

    /// Parse the `--engine` value. No aliases: an unknown value is an error.
    pub fn parse(value: &str) -> Option<Engine> {
        match value {
            "rust-vm" => Some(Engine::RustVm),
            "art" => Some(Engine::Art),
            _ => None,
        }
    }

    /// The protocol backend label recorded in the envelope.
    pub const fn protocol_backend(self) -> ProtocolBackend {
        match self {
            // The envelope has one runtime label; the precise engine is carried
            // in the verdict's `engine` field and this backend's `id`.
            Engine::RustVm | Engine::Art => ProtocolBackend::AcosNative,
        }
    }
}

/// Whether the evidence came from a launched Activity or from offline rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// A real app process/Activity produced the frame.
    Live,
    /// Offline render (P3c-style chrome preview). Never a real verification.
    Preview,
}

impl Mode {
    /// The stable verdict string.
    pub const fn as_str(self) -> &'static str {
        match self {
            Mode::Live => "live",
            Mode::Preview => "preview",
        }
    }
}

/// The commands a [`SessionBackend`] can actually execute today.
///
/// A `false` field is reported by name when a scenario needs it; the executor's
/// protocol [`Capabilities`] are derived from these, so a capability cannot be
/// advertised to the executor without a command behind it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SessionCapabilities {
    /// `launch(apk)` starts an app session.
    pub launch: bool,
    /// `tree()` returns the live View tree.
    pub tree: bool,
    /// `screenshot()` returns fresh PNG bytes.
    pub screenshot: bool,
    /// `tap(x, y)`.
    pub tap: bool,
    /// `tap_text(text)`.
    pub tap_text: bool,
    /// `type_text(text)`.
    pub type_text: bool,
    /// `key(name)`.
    pub key: bool,
    /// `reset()`.
    pub reset: bool,
    /// `close()`.
    pub close: bool,
    /// A log stream.
    pub logs: bool,
    /// Environment changes (`set_*`).
    pub environment: bool,
    /// Crash/ANR reports distinct from logs.
    pub crash_reports: bool,
}

impl SessionCapabilities {
    /// Every capability as a stable command name, in report order.
    pub fn names(self) -> Vec<&'static str> {
        self.all()
            .into_iter()
            .filter(|(name, supported)| {
                let _ = name;
                *supported
            })
            .map(|(name, _)| name)
            .collect()
    }

    /// The commands this backend does **not** support, by name.
    pub fn missing(self) -> Vec<&'static str> {
        self.all()
            .into_iter()
            .filter(|(_, supported)| !supported)
            .map(|(name, _)| name)
            .collect()
    }

    fn all(self) -> Vec<(&'static str, bool)> {
        vec![
            ("launch", self.launch),
            ("tree", self.tree),
            ("screenshot", self.screenshot),
            ("tap", self.tap),
            ("tap_text", self.tap_text),
            ("type_text", self.type_text),
            ("key", self.key),
            ("reset", self.reset),
            ("close", self.close),
            ("logs", self.logs),
            ("environment", self.environment),
            ("crash_reports", self.crash_reports),
        ]
    }

    /// The capability set the scenario executor sees.
    pub fn to_protocol(self) -> Capabilities {
        Capabilities {
            static_manifest: true,
            launch: self.launch,
            ui_tree: self.tree,
            logs: self.logs,
            screenshot: self.screenshot,
            input: self.tap || self.key || self.type_text,
            reset: self.reset,
            environment: self.environment,
            crash_reports: self.crash_reports,
        }
    }
}

/// The engine-level code for "there is no live frame right now".
///
/// It is never a pass: it means the session could not hand over a completed live
/// frame (no frame yet, the capture reply carried no pixels, or the engine has
/// no capture command). The protocol code is `NO_FRESH_FRAME`, and this string is
/// the first thing in the detail so an agent sees the exact cause.
pub const LIVE_FRAME_UNAVAILABLE: &str = "LIVE_FRAME_UNAVAILABLE";
/// The engine-level code for "the frame handed back is the one we already had".
///
/// A fresh capture or an input action must complete a *new* frame; a reply whose
/// `frame_seq` did not advance is rejected instead of being reported as fresh
/// evidence (protocol code `NO_FRESH_FRAME`).
pub const STALE_FRAME: &str = "STALE_FRAME";

/// Build the honest "no live frame" error (never a pass, never a preview).
pub fn live_frame_unavailable(detail: impl Into<String>) -> BackendError {
    BackendError::Error {
        code: ErrorCode::NoFreshFrame,
        detail: format!("{LIVE_FRAME_UNAVAILABLE}: {}", detail.into()),
    }
}

/// Build the honest "that frame is not new" error.
pub fn stale_frame(previous: u64, current: u64, context: &str) -> BackendError {
    BackendError::Error {
        code: ErrorCode::NoFreshFrame,
        detail: format!(
            "{STALE_FRAME}: {context} completed frame {current}, which is not newer than the \
             previous completed frame {previous}; refusing to hand a reused frame back as fresh \
             evidence"
        ),
    }
}

/// One completed frame and the evidence that it is new.
///
/// `frame_seq` is the engine's own monotonic sequence number, `captured_at_ms`
/// is the host time at which the pixels were complete (the PNG was read), and
/// `previous_seq` is the sequence the engine had before this capture. A capture
/// is fresh exactly when `frame_seq > previous_seq`; a backend that cannot prove
/// that must fail with [`STALE_FRAME`] rather than return the PNG it cached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameEvidence {
    /// Monotonic frame sequence assigned by the engine.
    pub frame_seq: u64,
    /// The previous sequence the engine had (0 when none).
    pub previous_seq: u64,
    /// Host time the pixels were complete, milliseconds since the Unix epoch.
    pub captured_at_ms: u64,
    /// The same instant as an RFC 3339 UTC string.
    pub captured_at: String,
    /// Encoded-pixel hash as the engine reported it (the ART guest's MD5).
    pub md5: String,
    /// PNG bytes of exactly this frame.
    pub png: Vec<u8>,
}

impl FrameEvidence {
    /// The stale-frame rule: this capture completed a new frame.
    pub const fn is_fresh(&self) -> bool {
        self.frame_seq > self.previous_seq
    }

    /// Stable label for logs/evidence (`frame 7 after 6 at <time>`).
    pub fn label(&self) -> String {
        format!(
            "frame {} after {} at {}",
            self.frame_seq, self.previous_seq, self.captured_at
        )
    }
}

/// One completed frame: the tree and the PNG describe the same traversal.
#[derive(Debug, Clone)]
pub struct ObservedFrame {
    /// The live View tree, with its frame metadata.
    pub tree: UiTree,
    /// The pixels of that traversal, with freshness evidence.
    pub frame: FrameEvidence,
}

impl ObservedFrame {
    /// PNG bytes of this frame.
    pub fn png(&self) -> &[u8] {
        &self.frame.png
    }

    /// The frame sequence of this observation.
    pub fn frame_seq(&self) -> u64 {
        self.frame.frame_seq
    }
}

/// Whether closing a session left its process tree clean.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeardownReport {
    /// The process group the session owned, when it had one.
    pub process_group: Option<i32>,
    /// Processes still alive in that group after teardown (empty is clean).
    pub survivors: Vec<u32>,
    /// False when this backend cannot verify its process tree.
    pub verified: bool,
}

impl TeardownReport {
    /// A backend that closed but cannot inspect its process tree.
    pub const fn unverified() -> Self {
        TeardownReport {
            process_group: None,
            survivors: Vec::new(),
            verified: false,
        }
    }

    /// True only when the tree was actually inspected and found empty.
    pub fn is_clean(&self) -> bool {
        self.verified && self.survivors.is_empty()
    }
}

/// One engine's command surface.
///
/// Implementations own their process/session and are responsible for cleaning it
/// up in `close` and on drop. Every unsupported command returns
/// [`BackendError::Unsupported`] naming the missing capability. Three rules are
/// part of the contract, not caller convention:
///
/// * `screenshot`/`capture` return a **fresh** frame: a new `frame_seq` strictly
///   greater than the previous one, otherwise [`STALE_FRAME`];
/// * an input action must leave behind its own completed frame, otherwise the
///   action is reported as [`STALE_FRAME`] instead of being claimed as taken;
/// * `close` reaps the whole process tree and `teardown` says whether it is clean;
///   a session that cannot prove it must not report clean.
pub trait SessionBackend {
    /// Which engine this is.
    fn engine(&self) -> Engine;

    /// Live execution or offline preview.
    fn mode(&self) -> Mode;

    /// The commands this backend executes today.
    fn capabilities(&self) -> SessionCapabilities;

    /// Start the app and return the first completed frame.
    fn launch(&mut self, apk: &Path) -> Result<LaunchOutcome, BackendError>;

    /// Traverse now and return the live tree bound to the new frame.
    fn tree(&mut self) -> Result<UiTree, BackendError>;

    /// A **fresh** capture: a new capture command, and evidence that the frame
    /// sequence advanced. The default has no capture command of its own and says so.
    ///
    /// The seam's tests drive this directly; `acos verify` reaches a fresh frame
    /// through `capture`, which returns the same evidence bundled with the tree.
    #[allow(dead_code)]
    fn screenshot(&mut self) -> Result<FrameEvidence, BackendError> {
        Err(BackendError::unsupported(
            "screenshot",
            "this session engine has no fresh-capture command",
        ))
    }

    /// One bound observation of a new traversal: the tree and the PNG of the
    /// same completed frame.
    fn frame(&mut self) -> Result<ObservedFrame, BackendError>;

    /// The bound frame the most recent command completed, if any.
    ///
    /// This is the live tree/pixels pair of the last completed traversal — it is
    /// returned without issuing a command, so a pure observation never invents a
    /// newer frame than the one an action produced.
    fn last_frame(&self) -> Option<ObservedFrame>;

    /// Host-observed delivery for the most recent input action, if the engine
    /// records it. `None` means the engine has no delivery record.
    fn last_input_delivery(&self) -> Option<acos_verify::backend::InputDelivery> {
        None
    }

    /// The highest completed frame sequence this session has seen (0 when none).
    ///
    /// A tree-less reply (a pure screenshot) can advance the sequence past the
    /// bound tree, so freshness checks use this, not [`Self::last_frame`].
    fn last_frame_seq(&self) -> u64 {
        self.last_frame().map(|frame| frame.frame_seq()).unwrap_or(0)
    }

    /// A *fresh* capture that must complete a new frame (a `capture` step).
    ///
    /// Implementations must fail with [`STALE_FRAME`] when the new traversal
    /// does not advance the frame sequence.
    fn capture(&mut self) -> Result<ObservedFrame, BackendError> {
        let previous = self.last_frame_seq();
        let observed = self.frame()?;
        if observed.frame_seq() <= previous {
            return Err(stale_frame(previous, observed.frame_seq(), "capture"));
        }
        Ok(observed)
    }

    /// Tap a screen point.
    /// Mark the start of one input action, before any traversal the action
    /// needs to resolve its target.
    ///
    /// A selector tap resolves its point from a *fresh* traversal; that
    /// traversal is not the action's frame, and it must not become the baseline
    /// an engine compares the post-action frame against. The executor compares
    /// the action's frame with the observation it handed back at the previous
    /// step, so an engine that measures "did this input change anything" must
    /// snapshot the same frame here.
    fn begin_input(&mut self) {}

    fn tap(&mut self, x: i32, y: i32) -> Result<(), BackendError>;

    /// Tap a screen point inside the window labelled `window`.
    ///
    /// A multi-window tree reports `bounds_on_screen` (display space) but each
    /// window's input channel takes window-local coordinates, so the backend
    /// must subtract that window's origin, not one global offset. Backends that
    /// only have one coordinate space keep [`Self::tap`].
    fn tap_in_window(&mut self, _window: &str, x: i32, y: i32) -> Result<(), BackendError> {
        self.tap(x, y)
    }

    /// Tap the first node whose text equals `text` (host-side selector).
    fn tap_text(&mut self, text: &str) -> Result<(), BackendError>;

    /// Type UTF-8 text into the focused field.
    fn type_text(&mut self, text: &str) -> Result<(), BackendError>;

    /// The first fatal crash signature this session has observed, if any.
    ///
    /// Guest-asserted on ART (from the guest log); an untrusted run must not
    /// expose it. `None` means no signature was seen, never "proven no crash".
    fn last_crash(&self) -> Option<acos_verify::backend::CrashRecord> {
        None
    }

    /// Log records newer than `since` (monotonic per-session sequence).
    ///
    /// A backend that reports `logs: true` must implement this; the default is
    /// the honest "no log command" answer. Provenance is the backend's to
    /// declare (the ART guest log is guest-asserted), and an untrusted run must
    /// refuse it.
    fn logs_since(&mut self, _since: u64) -> Result<Vec<LogRecord>, BackendError> {
        Err(BackendError::unsupported(
            "logs",
            "this session engine has no log-stream command",
        ))
    }

    /// Press a symbolic key (`back`, `enter`, ...).
    fn key(&mut self, key: &str) -> Result<(), BackendError>;

    /// Reset app state (fresh Activity).
    fn reset(&mut self) -> Result<(), BackendError>;

    /// Stop the session and reap the process tree.
    fn close(&mut self) -> Result<(), BackendError>;

    /// Close and report whether the whole process tree is gone.
    ///
    /// The default cannot inspect the tree, so it honestly reports
    /// `verified: false`; [`EngineBackend`] never turns that into "clean".
    fn teardown(&mut self) -> Result<TeardownReport, BackendError> {
        self.close()?;
        Ok(TeardownReport::unverified())
    }
}

/// The verifier backend over one [`SessionBackend`].
#[allow(clippy::large_enum_variant)]
pub enum EngineBackend {
    /// The existing Rust VM route, forwarded verbatim.
    Native(Box<dyn Backend>),
    /// A session engine driven command by command.
    Session {
        /// The engine.
        session: Box<dyn SessionBackend>,
        /// Static manifest cache (never execution).
        manifest: Option<ManifestInfo>,
        /// Package of the launched app.
        package: Option<String>,
        /// Cause id recorded on synthesized frame metadata.
        cause_id: String,
    },
}

impl EngineBackend {
    /// Wrap the existing Rust VM backend; every call forwards unchanged.
    pub fn native(backend: Box<dyn Backend>) -> Self {
        EngineBackend::Native(backend)
    }

    /// Wrap a session engine.
    pub fn session(session: Box<dyn SessionBackend>) -> Self {
        EngineBackend::Session {
            session,
            manifest: None,
            package: None,
            cause_id: "req_engine".to_string(),
        }
    }

    /// The engine label, or `None` for the native forwarding path.
    pub fn engine(&self) -> Option<Engine> {
        match self {
            EngineBackend::Native(_) => Some(Engine::RustVm),
            EngineBackend::Session { session, .. } => Some(session.engine()),
        }
    }

    /// Live or preview, or `None` for the native forwarding path.
    pub fn mode(&self) -> Option<Mode> {
        match self {
            EngineBackend::Native(_) => Some(Mode::Live),
            EngineBackend::Session { session, .. } => Some(session.mode()),
        }
    }

    /// The session command surface (empty for the native forwarding path).
    pub fn session_capabilities(&self) -> Option<SessionCapabilities> {
        match self {
            EngineBackend::Native(_) => None,
            EngineBackend::Session { session, .. } => Some(session.capabilities()),
        }
    }

    fn package(&self) -> String {
        match self {
            EngineBackend::Native(_) => String::new(),
            EngineBackend::Session {
                manifest, package, ..
            } => package.clone().or_else(|| {
                manifest
                    .as_ref()
                    .map(|manifest| manifest.package.clone())
            }).unwrap_or_default(),
        }
    }

    /// The frame sequence the session currently holds (0 when it has none).
    fn current_frame_seq(&self) -> u64 {
        match self {
            EngineBackend::Native(_) => 0,
            EngineBackend::Session { session, .. } => session.last_frame_seq(),
        }
    }

    /// Bind one completed frame into a session observation for the executor.
    ///
    /// The tree and the pixels must be one traversal: a backend that hands over
    /// a tree whose `frame_seq` differs from the capture's is a backend defect
    /// and is reported, never patched over.
    fn bind_observation(
        &self,
        observed: ObservedFrame,
    ) -> Result<SessionObservation, BackendError> {
        let seq = observed.frame_seq();
        let evidence = observed.frame.clone();
        let png = observed.png().to_vec();
        let ObservedFrame { tree, .. } = observed;
        if tree.frame.frame_seq != seq {
            return Err(BackendError::Error {
                code: ErrorCode::Internal,
                detail: format!(
                    "unbound observation: tree frame_seq {} but capture frame_seq {}; a tree and \
                     its pixels must come from one completed traversal",
                    tree.frame.frame_seq, seq
                ),
            });
        }
        let mut tree = tree;
        // The engine's own capture time and cause are the evidence identity.
        tree.frame.produced_at = evidence.captured_at.clone();
        if !evidence.md5.is_empty() {
            tree.frame.content_hash = evidence.md5.clone();
        }
        let package = self.package();
        let package = if package.is_empty() {
            tree.package.clone()
        } else {
            package
        };
        let crashed = match self {
            EngineBackend::Session { session, .. } => session.last_crash(),
            EngineBackend::Native(_) => None,
        };
        Ok(SessionObservation {
            session_id: format!("s_art_{seq}"),
            package,
            frame: tree.frame.clone(),
            tree,
            screenshot_png: Some(png),
            crashed,
            input: None,
        })
    }

    /// The cached live observation. No command is issued: the executor's
    /// frame-binding rule requires a pure assertion to describe the frame the
    /// preceding action produced, not a newer traversal.
    fn observe_session(&mut self) -> Result<SessionObservation, BackendError> {
        match self {
            EngineBackend::Native(backend) => backend.observe(),
            EngineBackend::Session { session, .. } => {
                let observed = session.last_frame().ok_or_else(|| {
                    live_frame_unavailable(
                        "this session has not completed a frame yet; launch or capture first",
                    )
                })?;
                self.bind_observation(observed)
            }
        }
    }

    /// The completed frame an action produced, proving it advanced the sequence.
    fn action_result(
        &mut self,
        previous: u64,
        context: &str,
    ) -> Result<SessionObservation, BackendError> {
        let observed = match self {
            EngineBackend::Native(_) => unreachable!("native input forwards"),
            EngineBackend::Session { session, .. } => session.last_frame().ok_or_else(|| {
                live_frame_unavailable(format!(
                    "{context} completed no live frame; the session did not hand one back"
                ))
            })?,
        };
        if observed.frame_seq() <= previous || !observed.frame.is_fresh() {
            return Err(stale_frame(previous, observed.frame_seq(), context));
        }
        let mut observation = self.bind_observation(observed)?;
        // The delivery record is host-observed: the host injected the input and
        // the guest session accepted it. Guest acks inside it stay guest-asserted.
        observation.input = match self {
            EngineBackend::Native(_) => None,
            EngineBackend::Session { session, .. } => session.last_input_delivery(),
        };
        Ok(observation)
    }

    /// Close the session and report whether its process tree is clean.
    pub fn teardown(&mut self) -> Result<TeardownReport, BackendError> {
        match self {
            EngineBackend::Native(_) => Ok(TeardownReport::unverified()),
            EngineBackend::Session { session, .. } => session.teardown(),
        }
    }

    /// Resolve a selector to a screen point and the window it belongs to, from
    /// the current tree.
    fn point_for(&mut self, selector: &Selector) -> Result<(String, i32, i32), BackendError> {
        let tree = match self {
            EngineBackend::Session { session, .. } => session.tree()?,
            EngineBackend::Native(_) => unreachable!("native path forwards input"),
        };
        let node = tree.find_one(selector).ok_or_else(|| {
            BackendError::Failed {
                code: if tree.truncated {
                    ErrorCode::TreeTruncated
                } else {
                    ErrorCode::AssertionFailed
                },
                detail: if tree.truncated {
                    format!(
                        "no node matches selector {selector:?}, but the tree was truncated at the node budget"
                    )
                } else {
                    format!("no node matches selector {selector:?}")
                },
            }
        })?;
        Ok((
            node.window.clone(),
            (node.bounds.left() + node.bounds.right()) / 2,
            (node.bounds.top() + node.bounds.bottom()) / 2,
        ))
    }

    fn tap_node(&mut self, text: &str) -> Result<(), BackendError> {
        match self {
            EngineBackend::Native(_) => Err(BackendError::unsupported(
                "input.tap_text",
                "the native forwarding path takes selectors, not text",
            )),
            EngineBackend::Session { session, .. } => session.tap_text(text),
        }
    }
}

impl Backend for EngineBackend {
    fn id(&self) -> &'static str {
        match self {
            EngineBackend::Native(backend) => backend.id(),
            EngineBackend::Session { session, .. } => match session.engine() {
                Engine::Art => "acos-art-session",
                Engine::RustVm => "acos-native-session",
            },
        }
    }

    fn protocol_backend(&self) -> ProtocolBackend {
        match self {
            EngineBackend::Native(backend) => backend.protocol_backend(),
            EngineBackend::Session { session, .. } => session.engine().protocol_backend(),
        }
    }

    fn fidelity(&self) -> Fidelity {
        match self {
            EngineBackend::Native(backend) => backend.fidelity(),
            EngineBackend::Session { session, .. } => match session.mode() {
                Mode::Live => Fidelity::ArtReference,
                Mode::Preview => Fidelity::Simulated,
            },
        }
    }

    fn capabilities(&self) -> Capabilities {
        match self {
            EngineBackend::Native(backend) => backend.capabilities(),
            EngineBackend::Session { session, .. } => session.capabilities().to_protocol(),
        }
    }

    fn manifest(&mut self, apk: &Path) -> Result<ManifestInfo, BackendError> {
        match self {
            EngineBackend::Native(backend) => backend.manifest(apk),
            EngineBackend::Session {
                manifest, package, ..
            } => {
                if manifest.is_none() {
                    let info = acos_verify::manifest::load_manifest(apk)?;
                    *package = Some(info.package.clone());
                    *manifest = Some(info);
                }
                Ok(manifest.clone().expect("manifest was just loaded"))
            }
        }
    }

    fn launch(&mut self, request: &LaunchRequest) -> Result<LaunchOutcome, BackendError> {
        match self {
            EngineBackend::Native(backend) => backend.launch(request),
            EngineBackend::Session {
                session,
                package,
                cause_id,
                ..
            } => {
                *cause_id = request.cause_id.clone();
                let outcome = session.launch(&request.apk)?;
                if !outcome.package.is_empty() {
                    *package = Some(outcome.package.clone());
                }
                Ok(outcome)
            }
        }
    }

    fn observe(&mut self) -> Result<SessionObservation, BackendError> {
        self.observe_session()
    }

    fn refresh(&mut self) -> Result<SessionObservation, BackendError> {
        match self {
            EngineBackend::Native(backend) => backend.refresh(),
            EngineBackend::Session { session, .. } => {
                // `frame()` issues the live `tree` command and returns the tree
                // and PNG of that completed traversal; a pure `observe()` would
                // only replay the previous frame and can never start waiting.
                let observed = session.frame()?;
                self.bind_observation(observed)
            }
        }
    }

    fn logs_since(&mut self, since: u64) -> Result<Vec<LogRecord>, BackendError> {
        match self {
            EngineBackend::Native(backend) => backend.logs_since(since),
            EngineBackend::Session { session, .. } => {
                if !session.capabilities().logs {
                    return Err(BackendError::unsupported(
                        "logs",
                        "this session engine has no log-stream command",
                    ));
                }
                session.logs_since(since)
            }
        }
    }

    fn input(&mut self, action: &InputAction) -> Result<SessionObservation, BackendError> {
        match self {
            EngineBackend::Native(backend) => backend.input(action),
            EngineBackend::Session { .. } => {
                let caps = match self {
                    EngineBackend::Session { session, .. } => session.capabilities(),
                    EngineBackend::Native(_) => unreachable!(),
                };
                // The action starts here: snapshot the frame the executor will
                // compare against, before any target-resolution traversal.
                if let EngineBackend::Session { session, .. } = self {
                    session.begin_input();
                }
                // Resolve the target first: a selector traversal is not the
                // action's frame and must not count as the action's progress.
                enum Target {
                    Text(String),
                    Point(String, i32, i32),
                }
                match action {
                    InputAction::Tap { selector, x, y } => {
                        if !caps.tap {
                            return Err(BackendError::unsupported("input.tap", "no tap command"));
                        }
                        let target = if let (Some(text), true) = (
                            selector
                                .as_ref()
                                .and_then(|selector| selector.id.is_none().then_some(selector))
                                .and_then(|selector| selector.text.as_ref())
                                .and_then(|text| text.exact.clone()),
                            caps.tap_text,
                        ) {
                            Target::Text(text)
                        } else {
                            let point = match (x, y) {
                                // An explicit screen point carries no window
                                // label: the backend falls back to the first
                                // window's measured origin.
                                (Some(x), Some(y)) => (String::new(), *x, *y),
                                _ => {
                                    let selector = selector.as_ref().ok_or_else(|| {
                                        BackendError::Unsupported {
                                            capability: "input.tap".into(),
                                            detail: "tap needs a selector or explicit x/y".into(),
                                        }
                                    })?;
                                    self.point_for(selector)?
                                }
                            };
                            Target::Point(point.0, point.1, point.2)
                        };
                        // The action starts here; `previous` is the frame before it.
                        let previous = self.current_frame_seq();
                        match target {
                            Target::Text(text) => self.tap_node(&text)?,
                            Target::Point(window, x, y) => {
                                if let EngineBackend::Session { session, .. } = self {
                                    session.tap_in_window(&window, x, y)?;
                                }
                            }
                        }
                        self.action_result(previous, action.kind())
                    }
                    InputAction::Type {
                        selector,
                        text,
                        clear,
                    } => {
                        if !caps.type_text {
                            return Err(BackendError::unsupported(
                                "input.type_text",
                                "the ART session protocol has no text-input command yet \
                                 (`type_text`); a scenario `type` step cannot run on this engine",
                            ));
                        }
                        let previous = self.current_frame_seq();
                        if let EngineBackend::Session { session, .. } = self {
                            if *clear {
                                // `clear: true` is part of the protocol, so the
                                // engine owes the scenario a cleared field. The
                                // ART session has no clear command; erase the
                                // field's current text with real DEL key events
                                // through the same input channel (host-injected,
                                // then one type_text for the replacement text).
                                let length = match selector {
                                    Some(selector) => session
                                        .tree()?
                                        .find_one(selector)
                                        .map(|node| node.text.chars().count())
                                        .unwrap_or(0),
                                    None => 0,
                                };
                                // A field can hold more characters than its
                                // on-screen text if the probe truncates; a small
                                // margin avoids a leftover tail.
                                for _ in 0..length {
                                    session.key("delete")?;
                                }
                            }
                            session.type_text(text)?;
                        }
                        self.action_result(previous, action.kind())
                    }
                    InputAction::Key(key) => {
                        if !caps.key {
                            return Err(BackendError::unsupported("input.key", "no key command"));
                        }
                        let previous = self.current_frame_seq();
                        if let EngineBackend::Session { session, .. } = self {
                            session.key(key_name(*key))?;
                        }
                        self.action_result(previous, action.kind())
                    }
                    InputAction::Back => {
                        if !caps.key {
                            return Err(BackendError::unsupported("input.back", "no key command"));
                        }
                        let previous = self.current_frame_seq();
                        if let EngineBackend::Session { session, .. } = self {
                            session.key("back")?;
                        }
                        self.action_result(previous, action.kind())
                    }
                    InputAction::Rotate(_) => Err(BackendError::unsupported(
                        "input.rotate",
                        "the ART session protocol has no rotate command yet",
                    )),
                }
            }
        }
    }

    fn reset(&mut self, _scope: ResetScope) -> Result<(), BackendError> {
        match self {
            EngineBackend::Native(backend) => backend.reset(_scope),
            EngineBackend::Session { session, .. } => {
                if !session.capabilities().reset {
                    return Err(BackendError::unsupported("reset", "no reset command"));
                }
                session.reset()
            }
        }
    }

    fn configure(&mut self, change: &EnvironmentChange) -> Result<(), BackendError> {
        match self {
            EngineBackend::Native(backend) => backend.configure(change),
            EngineBackend::Session { .. } => Err(BackendError::unsupported(
                "environment",
                format!(
                    "the ART session protocol has no `{}` command yet",
                    change.kind()
                ),
            )),
        }
    }

    fn capture(&mut self) -> Result<SessionObservation, BackendError> {
        match self {
            EngineBackend::Native(backend) => backend.capture(),
            EngineBackend::Session { .. } => {
                let caps = match self {
                    EngineBackend::Session { session, .. } => session.capabilities(),
                    EngineBackend::Native(_) => unreachable!(),
                };
                if !caps.screenshot {
                    return Err(BackendError::unsupported(
                        "screenshot",
                        "no screenshot command",
                    ));
                }
                let previous = self.current_frame_seq();
                let observed = match self {
                    EngineBackend::Session { session, .. } => session.capture()?,
                    EngineBackend::Native(_) => unreachable!(),
                };
                if observed.frame_seq() <= previous || !observed.frame.is_fresh() {
                    return Err(stale_frame(previous, observed.frame_seq(), "capture"));
                }
                self.bind_observation(observed)
            }
        }
    }
}

impl Drop for EngineBackend {
    fn drop(&mut self) {
        // The teardown guarantee: close the session and confirm the process
        // tree is gone. A failure here cannot be returned from `drop`; the
        // engine has already refused to report clean.
        let _ = self.teardown();
    }
}

/// Symbolic key name for a protocol [`KeyName`].
pub const fn key_name(key: KeyName) -> &'static str {
    match key {
        KeyName::Back => "back",
        KeyName::Home => "home",
        KeyName::Enter => "enter",
        KeyName::Tab => "tab",
        KeyName::Delete => "delete",
        KeyName::Escape => "escape",
        KeyName::VolumeUp => "volume_up",
        KeyName::VolumeDown => "volume_down",
        KeyName::Power => "power",
        KeyName::Menu => "menu",
        KeyName::DpadUp => "dpad_up",
        KeyName::DpadDown => "dpad_down",
        KeyName::DpadLeft => "dpad_left",
        KeyName::DpadRight => "dpad_right",
    }
}

/// Android keycode for a symbolic key name (the ART protocol takes integers).
pub fn key_code(key: &str) -> i32 {
    match key {
        "back" => 4,
        "home" => 3,
        "enter" => 66,
        "tab" => 61,
        "delete" => 67,
        "escape" => 111,
        "volume_up" => 24,
        "volume_down" => 25,
        "power" => 26,
        "menu" => 82,
        "dpad_up" => 19,
        "dpad_down" => 20,
        "dpad_left" => 21,
        "dpad_right" => 22,
        _ => -1,
    }
}

/// Build a protocol [`UiTree`] from the ART session's JSON View tree.
///
/// The ART tree is the guest's own hierarchy (`class`, `id`, `text`, `bounds`,
/// `clickable`, `visibility`, children). Nothing is added or inferred beyond
/// what the guest reported; an id is copied verbatim into `resource_id`.
pub fn ui_tree_from_json(
    package: &str,
    frame: FrameMeta,
    value: &serde_json::Value,
) -> Result<UiTree, BackendError> {
    let nodes = value
        .get("nodes")
        .and_then(|nodes| nodes.as_array())
        .ok_or_else(|| BackendError::Unsupported {
            capability: "ui.tree".into(),
            detail: "the ART session reply has no `nodes` array".into(),
        })?;
    let mut out = Vec::with_capacity(nodes.len());
    for node in nodes {
        out.push(ui_node_from_json(node, 0));
    }
    let mut tree = UiTree::new(frame, package.to_string(), out);
    tree.truncated = value
        .get("truncated")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    Ok(tree)
}

fn ui_node_from_json(value: &serde_json::Value, depth: usize) -> UiNode {
    let id = format!("n{depth}_{}", value.get("id").and_then(|v| v.as_str()).unwrap_or(""));
    let class = value
        .get("class")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    // Prefer display-space bounds: W17 measured every failing bounds case as a
    // constant +128 px y delta (the status-bar inset) because the probe's
    // `bounds` is window-local while the reference scenarios are screen-space.
    // `bounds_on_screen` is the same node's stock `getLocationOnScreen()`; a
    // probe that predates the field falls back to the window-local `bounds`.
    let bounds = value
        .get("bounds_on_screen")
        .or_else(|| value.get("bounds"))
        .and_then(|v| v.as_str())
        .and_then(parse_bounds)
        .unwrap_or_else(|| Bounds::new(0, 0, 0, 0));
    let visibility = value
        .get("visibility")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let resource_id = value
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let mut node = UiNode::new(id, class, bounds)
        .with_resource_id(resource_id)
        .with_text(value.get("text").and_then(|v| v.as_str()).unwrap_or(""));
    node.hint = value
        .get("hint")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    node.content_desc = value
        .get("content_description")
        .or_else(|| value.get("content_desc"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    node.checked = value
        .get("checked")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    node.selected = value
        .get("selected")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    node.scrollable = value
        .get("scrollable")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    node.clickable = value
        .get("clickable")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    node.visible = visibility == 0;
    node.enabled = value
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    node.focused = value
        .get("focused")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    node.package = value
        .get("package")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if let Some(children) = value.get("children").and_then(|v| v.as_array()) {
        for child in children {
            node.children.push(ui_node_from_json(child, depth + 1));
        }
    }
    node.window = value
        .get("window")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    node
}

/// `[l,t][r,b]` as protocol bounds.
pub(crate) fn parse_bounds(text: &str) -> Option<Bounds> {
    let cleaned = text
        .trim()
        .trim_start_matches('[')
        .replace("][", ",")
        .trim_end_matches(']')
        .replace(['[', ']'], "");
    let parts: Vec<i32> = cleaned
        .split(',')
        .filter_map(|part| part.trim().parse().ok())
        .collect();
    (parts.len() == 4).then(|| Bounds::new(parts[0], parts[1], parts[2], parts[3]))
}

/// Downgrade a serialized `pass` verdict that came from preview output.
///
/// Returns `true` when the verdict was changed. This is the JSON half of
/// [`enforce_mode`]: the CLI writes the annotated verdict, so both the typed
/// verdict and the evidence file carry the same decision.
pub fn preview_downgrade(verdict: &mut serde_json::Value) -> bool {
    if verdict.get("verdict").and_then(|v| v.as_str()) != Some("pass") {
        return false;
    }
    let error = serde_json::json!({
        "code": "UNSUPPORTED_BACKEND",
        "message": "preview output (mode: preview) is not a real verification",
        "details": {"capability": "mode.live", "mode": "preview"},
    });
    if let Some(object) = verdict.as_object_mut() {
        object.insert("verdict".to_string(), serde_json::json!("unsupported"));
        object.insert(
            "failure".to_string(),
            serde_json::json!({
                "code": "UNSUPPORTED_BACKEND",
                "summary": "preview output never counts as a pass",
                "error": error,
            }),
        );
        object.insert("mode".to_string(), serde_json::json!("preview"));
        let note = "mode: preview — offline-rendered output never counts as a passing real \
                    verification; run the same scenario on a live engine";
        let notes = object
            .entry("notes".to_string())
            .or_insert_with(|| serde_json::json!([]));
        if let Some(notes) = notes.as_array_mut() {
            notes.push(serde_json::json!(note));
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use acos_protocol::ui::Bounds;

    #[test]
    fn engine_parse_is_exact() {
        assert_eq!(Engine::parse("rust-vm"), Some(Engine::RustVm));
        assert_eq!(Engine::parse("art"), Some(Engine::Art));
        assert_eq!(Engine::parse("ART"), None);
        assert_eq!(Engine::parse("native"), None);
    }

    #[test]
    fn missing_capabilities_are_named() {
        let caps = SessionCapabilities {
            launch: true,
            tree: true,
            screenshot: true,
            tap: true,
            reset: true,
            close: true,
            ..SessionCapabilities::default()
        };
        assert_eq!(
            caps.missing(),
            vec!["tap_text", "type_text", "key", "logs", "environment", "crash_reports"]
        );
        assert!(!caps.to_protocol().environment);
        assert!(caps.to_protocol().input);
    }

    #[test]
    fn art_bounds_parse() {
        let bounds = parse_bounds("[42,317][63,368]").unwrap();
        assert_eq!(bounds, Bounds::new(42, 317, 63, 368));
    }

    #[test]
    fn ui_tree_keeps_guest_facts() {
        let value = serde_json::json!({
            "nodes": [{
                "class": "android.widget.TextView",
                "id": "dev.acos.fixtures.counter:id/count",
                "text": "0",
                "bounds": "[42,317][63,368]",
                "clickable": false,
                "visibility": 0,
                "children": [{
                    "class": "android.widget.Button",
                    "id": "dev.acos.fixtures.counter:id/inc",
                    "text": "Increment",
                    "bounds": "[42,368][273,494]",
                    "clickable": true,
                    "visibility": 0
                }]
            }]
        });
        let frame = FrameMeta::new(1, "2026-11-11T00:00:00Z", "req", 1080, 2400, 420, "h", true);
        let tree = ui_tree_from_json("dev.acos.fixtures.counter", frame, &value).unwrap();
        let count = tree
            .find_one(&Selector {
                id: Some("count".into()),
                ..Selector::default()
            })
            .expect("count node");
        assert_eq!(count.text, "0");
        assert_eq!(count.bounds, Bounds::new(42, 317, 63, 368));
        assert!(count.visible);
        let inc = tree
            .find_one(&Selector::by_text("Increment"))
            .expect("inc node");
        assert!(inc.clickable);
    }

    #[test]
    fn ui_tree_prefers_screen_bounds_and_carries_hint_and_state() {
        let value = serde_json::json!({
            "nodes": [{
                "class": "android.widget.EditText",
                "id": "dev.acos.fixtures.form:id/name",
                "text": "",
                "hint": "Name",
                "content_description": "player-name",
                "bounds": "[42,189][1038,307]",
                "bounds_on_screen": "[42,317][1038,435]",
                "checked": true,
                "selected": true,
                "visibility": 0
            }]
        });
        let frame = FrameMeta::new(1, "2026-11-11T00:00:00Z", "req", 1080, 2400, 420, "h", true);
        let tree = ui_tree_from_json("dev.acos.fixtures.form", frame, &value).unwrap();
        let node = tree
            .find_one(&Selector {
                hint: Some(acos_protocol::TextMatch::exact("Name")),
                ..Selector::default()
            })
            .expect("hint selector finds the EditText");
        assert_eq!(node.hint, "Name");
        assert_eq!(node.content_desc, "player-name");
        assert_eq!(node.bounds, Bounds::new(42, 317, 1038, 435));
        assert!(node.checked);
        assert!(node.selected);
        // Selector text also matches a hint, and content-desc selectors work.
        assert!(tree
            .find_one(&Selector::by_text("Name"))
            .is_some());
        assert!(tree
            .find_one(&Selector {
                content_desc: Some(acos_protocol::TextMatch::exact("player-name")),
                ..Selector::default()
            })
            .is_some());
    }

    #[test]
    fn key_names_map_to_android_keycodes() {
        assert_eq!(key_code(key_name(KeyName::Back)), 4);
        assert_eq!(key_code(key_name(KeyName::Enter)), 66);
        assert_eq!(key_code("nope"), -1);
    }
}

#[cfg(test)]
mod engine_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use acos_protocol::commands::VerifyRequest;
    use acos_protocol::{RequestBase, ResetScope, Status};
    use acos_verify::backend::{Capability, LaunchRequest};
    use acos_verify::{Backend, ManifestInfo};

    use crate::engine::fake::FakeSession;

    fn repo() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf()
    }

    fn artifacts(name: &str) -> std::path::PathBuf {
        let dir = repo().join(".cache/engine-tests").join(format!(
            "{}-{}-{}",
            name,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn request(scenario: &str, artifacts: &std::path::Path) -> VerifyRequest {
        VerifyRequest {
            base: RequestBase::new("req_engine_test"),
            apk: Some(repo().join("fixtures/build/apps/counter.apk").display().to_string()),
            scenario: repo().join(scenario).display().to_string(),
            artifacts: artifacts.display().to_string(),
            reference: None,
            required_fidelity: None,
            flags: Vec::new(),
        }
    }

    fn args(scenario: &str, artifacts: &std::path::Path, engine: &str, json: bool) -> crate::verify::VerifyArgs {
        crate::verify::VerifyArgs {
            scenario: Some(repo().join(scenario)),
            scenario_flag: None,
            apk: Some(repo().join("fixtures/build/apps/counter.apk")),
            artifacts: Some(artifacts.to_path_buf()),
            reference: None,
            required_fidelity: None,
            flags: Vec::new(),
            native: false,
            engine: engine.to_string(),
            untrusted: false,
            json,
        }
    }

    /// A `Backend` that records any call: engine=art must never reach it.
    #[derive(Default)]
    struct VmProbe {
        calls: Arc<AtomicUsize>,
    }

    impl Backend for VmProbe {
        fn id(&self) -> &'static str {
            "rust-vm-probe"
        }
        fn protocol_backend(&self) -> acos_protocol::Backend {
            acos_protocol::Backend::AcosNative
        }
        fn fidelity(&self) -> acos_protocol::Fidelity {
            acos_protocol::Fidelity::NativeRuntime
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities {
                static_manifest: true,
                launch: true,
                ui_tree: true,
                screenshot: true,
                input: true,
                reset: true,
                ..Capabilities::default()
            }
        }
        fn manifest(&mut self, _apk: &Path) -> Result<ManifestInfo, BackendError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(BackendError::unsupported("probe", "the Rust VM must not run"))
        }
        fn launch(&mut self, _r: &LaunchRequest) -> Result<LaunchOutcome, BackendError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(BackendError::unsupported("probe", "the Rust VM must not run"))
        }
        fn observe(&mut self) -> Result<SessionObservation, BackendError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(BackendError::unsupported("probe", "the Rust VM must not run"))
        }
        fn reset(&mut self, _scope: ResetScope) -> Result<(), BackendError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(BackendError::unsupported("probe", "the Rust VM must not run"))
        }
        fn capture(&mut self) -> Result<SessionObservation, BackendError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(BackendError::unsupported("probe", "the Rust VM must not run"))
        }
    }

    #[test]
    fn counter_scenario_passes_through_the_art_backend_path_live() {
        let dir = artifacts("live");
        let fake = FakeSession::live();
        let calls = fake.calls();
        let mut backend = EngineBackend::session(Box::new(fake));
        assert_eq!(backend.engine(), Some(Engine::Art));
        assert_eq!(backend.mode(), Some(Mode::Live));

        let req = request("scenarios/v2-art-counter.yaml", &dir);
        let mut envelope = acos_verify::verify(&req, &mut backend);
        assert!(
            envelope.status().is_pass(),
            "expected a live pass, got {:?}: {:?}",
            envelope.status(),
            envelope.error()
        );
        crate::verify::annotate(
            &mut envelope,
            &args("scenarios/v2-art-counter.yaml", &dir, "art", true),
            Some(Engine::Art),
            Mode::Live,
            backend.session_capabilities().as_ref(),
            crate::trust::Trust::Trusted,
            &dir,
            None,
        );
        let verdict = envelope.result().unwrap().get("verdict").unwrap();
        assert_eq!(verdict["engine"], "art");
        assert_eq!(verdict["mode"], "live");
        assert_eq!(verdict["verdict"], "pass");
        assert_eq!(verdict["unsupported_steps"].as_array().unwrap().len(), 0);
        let steps = verdict["steps"].as_array().unwrap();
        assert!(
            steps.iter().all(|step| step["status"] == "pass"),
            "every step must pass: {steps:?}"
        );
        // The decisive transition really happened three times: Increment,
        // Reset (the app's own control; the real launch engine has no reset
        // command), then Increment again.
        let taps = steps
            .iter()
            .filter(|step| step["step"] == "tap")
            .count();
        assert_eq!(taps, 3);
        let calls = calls.lock().unwrap();
        assert!(
            calls.iter().any(|call| call.starts_with("tap(")),
            "the ART session path must receive a real tap command: {calls:?}"
        );
        assert!(
            !calls.iter().any(|call| call == "type_text"),
            "the counter scenario must not need text input: {calls:?}"
        );
        assert!(dir.join("verdict.json").is_file());
        assert!(dir.join("engine.json").is_file());
    }

    #[test]
    fn preview_backend_never_yields_a_pass() {
        let dir = artifacts("preview");
        let mut backend = EngineBackend::session(Box::new(FakeSession::preview()));
        assert_eq!(backend.mode(), Some(Mode::Preview));
        let req = request("scenarios/v2-art-counter.yaml", &dir);
        let mut envelope = acos_verify::verify(&req, &mut backend);
        // The fake executes every step, but preview can never be a pass.
        assert!(envelope.status().is_pass());
        crate::verify::annotate(
            &mut envelope,
            &args("scenarios/v2-art-counter.yaml", &dir, "rust-vm", true),
            Some(Engine::Art),
            Mode::Preview,
            backend.session_capabilities().as_ref(),
            crate::trust::Trust::Trusted,
            &dir,
            None,
        );
        assert!(!envelope.status().is_pass());
        assert_eq!(envelope.status(), Status::Unsupported);
        let verdict = envelope.result().unwrap().get("verdict").unwrap();
        assert_eq!(verdict["verdict"], "unsupported");
        assert_eq!(verdict["mode"], "preview");
        assert_eq!(verdict["failure"]["error"]["details"]["capability"], "mode.live");
        // The written evidence carries the same decision.
        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("verdict.json")).unwrap()).unwrap();
        assert_eq!(written["verdict"], "unsupported");
        assert_eq!(written["mode"], "preview");
    }

    #[test]
    fn unsupported_step_names_the_missing_capability_and_never_falls_back() {
        let dir = artifacts("unsupported");
        let probe = VmProbe::default();
        let probe_calls = Arc::clone(&probe.calls);
        let mut backend = EngineBackend::session(Box::new(FakeSession::live()));
        // A Rust VM backend exists in this test on purpose: the ART path must
        // never touch it, not even when a step is unsupported.
        let _native_route = EngineBackend::native(Box::new(probe));
        let req = request("scenarios/v2-art-type-unsupported.yaml", &dir);
        let envelope = acos_verify::verify(&req, &mut backend);
        assert_eq!(probe_calls.load(Ordering::SeqCst), 0, "engine=art invoked the Rust VM");
        assert!(!envelope.status().is_pass());
        assert_eq!(envelope.status(), Status::Unsupported);
        let verdict = envelope.result().unwrap().get("verdict").unwrap();
        let unsupported = verdict["steps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|step| step["status"] == "unsupported")
            .expect("a step must be reported unsupported");
        assert_eq!(unsupported["step"], "type");
        assert_eq!(
            unsupported["error"]["details"]["capability"],
            "input.type_text"
        );
        assert_eq!(
            unsupported["error"]["code"],
            "UNSUPPORTED_BACKEND"
        );
    }

    #[test]
    fn art_session_capabilities_report_the_protocol_gaps() {
        let backend = EngineBackend::session(Box::new(FakeSession::live()));
        let caps = backend.session_capabilities().unwrap();
        for command in ["launch", "tree", "screenshot", "tap", "tap_text", "key", "reset", "close"] {
            assert!(caps.names().contains(&command), "{command} must be supported");
        }
        for missing in ["type_text", "logs", "environment", "crash_reports"] {
            assert!(
                caps.missing().contains(&missing),
                "{missing} must be reported missing"
            );
        }
        // `Capability` is the executor's name for the same gap.
        assert_eq!(Capability::UiTree.as_str(), "ui.tree");
    }

    fn launch_request(artifacts: &std::path::Path) -> LaunchRequest {
        LaunchRequest {
            apk: artifacts.join("counter.apk"),
            activity: Some("dev.acos.fixtures.counter/.MainActivity".to_string()),
            intent_action: None,
            data: None,
            extras: Default::default(),
            clear_state: true,
            isolation: acos_protocol::identity::IsolationLevel::Process,
            display: acos_protocol::identity::DisplaySpec::offscreen(1080, 2400, 420),
            cause_id: "req_engine_test:launch:0".to_string(),
        }
    }

    #[test]
    fn fresh_screenshot_carries_a_new_sequence_and_a_capture_time() {
        let dir = artifacts("fresh-shot");
        let mut backend = EngineBackend::session(Box::new(FakeSession::live()));
        backend
            .launch(&launch_request(&dir))
            .expect("fake launch");
        // `SessionBackend::screenshot` is a fresh capture, not the cached PNG.
        let shot = |engine: &mut EngineBackend| {
            match engine {
                EngineBackend::Session { session, .. } => session.screenshot(),
                EngineBackend::Native(_) => unreachable!(),
            }
        };
        let first = shot(&mut backend).expect("first fresh screenshot");
        let second = shot(&mut backend).expect("second fresh screenshot");
        assert!(first.is_fresh(), "{}", first.label());
        assert!(second.is_fresh(), "{}", second.label());
        assert!(
            second.frame_seq > first.frame_seq,
            "a fresh capture must complete a newer frame ({} -> {})",
            first.frame_seq,
            second.frame_seq
        );
        assert_eq!(second.previous_seq, first.frame_seq);
        assert!(second.captured_at_ms >= first.captured_at_ms);
        assert!(
            second.captured_at.contains('T') && second.captured_at.ends_with('Z'),
            "capture time must be an RFC 3339 UTC instant: {}",
            second.captured_at
        );
        assert!(!second.png.is_empty());
    }

    #[test]
    fn stale_tap_is_rejected_as_no_fresh_frame() {
        let dir = artifacts("stale-tap");
        // Both selector styles: a text selector is host-side, an id selector
        // resolves through a traversal first. That traversal is *not* the
        // action's frame and must not mask a tap that completes nothing.
        for selector in [
            Selector::by_text("Increment"),
            Selector {
                id: Some("inc".to_string()),
                ..Selector::default()
            },
        ] {
            let mut backend = EngineBackend::session(Box::new(FakeSession::stale_tap()));
            backend
                .launch(&launch_request(&dir))
                .expect("fake launch");
            let error = backend
                .input(&InputAction::Tap {
                    selector: Some(selector.clone()),
                    x: None,
                    y: None,
                })
                .expect_err("a tap that completes no new frame must not be accepted");
            assert_eq!(error.code(), ErrorCode::NoFreshFrame);
            assert!(
                error.to_string().contains(STALE_FRAME),
                "the error must name the stale frame ({selector:?}): {error}"
            );
            assert!(!error.status().is_pass());
        }
    }

    #[test]
    fn stale_capture_is_rejected_as_no_fresh_frame() {
        let dir = artifacts("stale-capture");
        let mut backend = EngineBackend::session(Box::new(FakeSession::stale_capture()));
        backend
            .launch(&launch_request(&dir))
            .expect("fake launch");
        let error = backend
            .capture()
            .expect_err("a capture that reuses the previous frame must fail");
        assert_eq!(error.code(), ErrorCode::NoFreshFrame);
        assert!(error.to_string().contains(STALE_FRAME), "{error}");
    }

    #[test]
    fn no_live_frame_is_an_honest_error_never_a_pass() {
        let dir = artifacts("no-live-frame");
        let mut backend = EngineBackend::session(Box::new(FakeSession::no_live_frame()));
        // Launch can legitimately succeed without a first frame.
        backend
            .launch(&launch_request(&dir))
            .expect("fake launch without a frame");
        let observe = backend.observe().expect_err("observe must not invent a frame");
        assert_eq!(observe.code(), ErrorCode::NoFreshFrame);
        assert!(
            observe.to_string().contains(LIVE_FRAME_UNAVAILABLE),
            "{observe}"
        );
        let capture = backend.capture().expect_err("capture must not invent a frame");
        assert_eq!(capture.code(), ErrorCode::NoFreshFrame);
        assert!(
            capture.to_string().contains(LIVE_FRAME_UNAVAILABLE),
            "{capture}"
        );
        assert!(!observe.status().is_pass() && !capture.status().is_pass());
    }

    #[test]
    fn the_whole_scenario_fails_honestly_when_no_live_frame_arrives() {
        let dir = artifacts("no-live-frame-scenario");
        let mut backend = EngineBackend::session(Box::new(FakeSession::no_live_frame()));
        let req = request("scenarios/v2-art-counter.yaml", &dir);
        let envelope = acos_verify::verify(&req, &mut backend);
        assert!(!envelope.status().is_pass(), "must not pass: {:?}", envelope.status());
        let error = envelope.error().expect("a structured failure");
        assert_eq!(error.code, ErrorCode::NoFreshFrame);
        assert!(
            error.message.contains(LIVE_FRAME_UNAVAILABLE),
            "{}",
            error.message
        );
    }

    #[test]
    fn a_pure_observation_does_not_invent_a_newer_frame() {
        let dir = artifacts("observe-no-advance");
        let fake = FakeSession::live();
        let calls = fake.calls();
        let mut backend = EngineBackend::session(Box::new(fake));
        let launched = backend.launch(&launch_request(&dir)).expect("fake launch");
        let launch_seq = launched.frame.as_ref().unwrap().frame_seq;
        let first = backend.observe().expect("observe after launch");
        let second = backend.observe().expect("second observe");
        assert_eq!(first.frame.frame_seq, launch_seq);
        assert_eq!(second.frame.frame_seq, launch_seq);
        assert_eq!(first.tree.frame.frame_seq, first.frame.frame_seq);
        assert_eq!(second.tree.frame.frame_seq, second.frame.frame_seq);
        assert!(first.binding_violation().is_none());
        assert!(second.binding_violation().is_none());
        // No new traversal was issued by a pure observation.
        let calls = calls.lock().unwrap();
        assert!(
            !calls.iter().any(|call| call == "frame" || call == "tree"),
            "observe must not traverse: {calls:?}"
        );
    }

    #[test]
    fn reset_step_binds_the_post_reset_frame_for_the_next_assertion() {
        // W22: the reset step leaves a new completed frame (fresh process,
        // initial UI). Without the executor's post-reset binding, the next pure
        // `assert_text` fails with "observation is frame N but the bound
        // completed frame is N-1" even though the reset really happened.
        let dir = artifacts("reset-binding");
        let mut backend = EngineBackend::session(Box::new(FakeSession::live()));
        let req = request("scenarios/w22-counter-reset.yaml", &dir);
        let envelope = acos_verify::verify(&req, &mut backend);
        assert!(
            envelope.status().is_pass(),
            "expected a live pass, got {:?}: {:?}",
            envelope.status(),
            envelope.error()
        );
        let verdict = envelope.result().unwrap().get("verdict").unwrap();
        let reset = verdict["steps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|step| step["step"] == "reset")
            .expect("the scenario has a reset step");
        assert_eq!(reset["status"], "pass");
        let after: Vec<&serde_json::Value> = verdict["steps"]
            .as_array()
            .unwrap()
            .iter()
            .skip_while(|step| step["step"] != "reset")
            .collect();
        assert!(
            after.iter().skip(1).all(|step| step["status"] == "pass"),
            "every step after the reset must be bound to the post-reset frame: {after:?}"
        );
    }

    #[test]
    fn type_with_clear_erases_the_field_with_keys_before_typing() {
        // `clear: true` is protocol, not a hint. The ART session has no clear
        // command, so the engine must erase the field with real DEL key events
        // through the same input channel before typing the replacement.
        let dir = artifacts("clear");
        let fake = FakeSession::live_with_type();
        let calls = fake.calls();
        let mut backend = EngineBackend::session(Box::new(fake));
        backend.launch(&launch_request(&dir)).expect("fake launch");
        let selector = Selector {
            id: Some("count".to_string()),
            ..Selector::default()
        };
        // The fake's count node reads "0": exactly one character to erase.
        backend
            .input(&InputAction::Type {
                selector: Some(selector.clone()),
                text: "7".to_string(),
                clear: true,
            })
            .expect("type with clear");
        {
            let calls = calls.lock().unwrap();
            let deletes: Vec<usize> = calls
                .iter()
                .enumerate()
                .filter(|(_, call)| *call == "key(delete)")
                .map(|(index, _)| index)
                .collect();
            let typed = calls
                .iter()
                .position(|call| call == "type_text(7)")
                .expect("the replacement text is typed");
            assert_eq!(deletes.len(), 1, "one DEL per existing character: {calls:?}");
            assert!(deletes[0] < typed, "clear must precede typing: {calls:?}");
        }
        // Negative control: without clear, no DEL is sent.
        backend
            .input(&InputAction::Type {
                selector: Some(selector),
                text: "8".to_string(),
                clear: false,
            })
            .expect("type without clear");
        let calls = calls.lock().unwrap();
        assert_eq!(
            calls.iter().filter(|call| *call == "key(delete)").count(),
            1,
            "clear: false must not delete anything: {calls:?}"
        );
        assert!(calls.iter().any(|call| call == "type_text(8)"));
    }

    #[test]
    fn teardown_says_whether_the_process_tree_is_clean() {
        let dir = artifacts("teardown");
        let mut backend = EngineBackend::session(Box::new(FakeSession::live()));
        backend.launch(&launch_request(&dir)).expect("fake launch");
        let report = backend.teardown().expect("teardown");
        assert!(report.verified, "the fake verifies its (empty) tree");
        assert!(report.is_clean());
        // Closing again is harmless.
        assert!(backend.teardown().expect("second teardown").is_clean());
    }
}
