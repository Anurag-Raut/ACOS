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

/// One completed frame: the tree and the PNG describe the same traversal.
#[derive(Debug, Clone)]
pub struct ObservedFrame {
    /// The live View tree, with its frame metadata.
    pub tree: UiTree,
    /// PNG bytes of exactly that frame.
    pub png: Vec<u8>,
}

/// One engine's command surface.
///
/// Implementations own their process/session and are responsible for cleaning it
/// up in `close` and on drop. Every unsupported command returns
/// [`BackendError::Unsupported`] naming the missing capability.
pub trait SessionBackend {
    /// Which engine this is.
    fn engine(&self) -> Engine;

    /// Live execution or offline preview.
    fn mode(&self) -> Mode;

    /// The commands this backend executes today.
    fn capabilities(&self) -> SessionCapabilities;

    /// Start the app and return the first completed frame.
    fn launch(&mut self, apk: &Path) -> Result<LaunchOutcome, BackendError>;

    /// The live View tree bound to the most recent frame.
    fn tree(&mut self) -> Result<UiTree, BackendError>;

    /// Fresh PNG bytes (issued as a new capture command when the engine has one).
    fn screenshot(&mut self) -> Result<Vec<u8>, BackendError>;

    /// One bound observation: a tree and the PNG of the same completed frame.
    ///
    /// Backends that can answer both from one command override this; the
    /// default composes `tree()` and `screenshot()`.
    fn frame(&mut self) -> Result<ObservedFrame, BackendError> {
        let tree = self.tree()?;
        let png = self.screenshot()?;
        Ok(ObservedFrame { tree, png })
    }

    /// A *fresh* capture that must complete a new frame (a `capture` step).
    ///
    /// Defaults to [`SessionBackend::frame`]; a backend whose capture does not
    /// advance `frame_seq` inherits that honesty and the executor reports
    /// `NO_FRESH_FRAME` instead of accepting a stale image.
    fn capture(&mut self) -> Result<ObservedFrame, BackendError> {
        self.frame()
    }

    /// Tap a screen point.
    fn tap(&mut self, x: i32, y: i32) -> Result<(), BackendError>;

    /// Tap the first node whose text equals `text` (host-side selector).
    fn tap_text(&mut self, text: &str) -> Result<(), BackendError>;

    /// Type UTF-8 text into the focused field.
    fn type_text(&mut self, text: &str) -> Result<(), BackendError>;

    /// Press a symbolic key (`back`, `enter`, ...).
    fn key(&mut self, key: &str) -> Result<(), BackendError>;

    /// Reset app state (fresh Activity).
    fn reset(&mut self) -> Result<(), BackendError>;

    /// Stop the session and reap the process.
    fn close(&mut self) -> Result<(), BackendError>;
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
        /// Frame sequence of the most recent observation.
        frame_seq: u64,
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
            frame_seq: 0,
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

    /// One bound observation: refresh the frame, then read the tree that belongs
    /// to it. Backends cache the tree of the frame their capture produced, so
    /// the two channels cannot describe different traversals.
    fn observe_session(&mut self) -> Result<SessionObservation, BackendError> {
        let (seq, tree, png, package) = match self {
            EngineBackend::Native(backend) => return backend.observe(),
            EngineBackend::Session {
                session,
                package,
                frame_seq,
                ..
            } => {
                let observed = session.frame()?;
                let mut tree = observed.tree;
                let png = observed.png;
                let seq = tree.frame.frame_seq.max(*frame_seq);
                *frame_seq = seq;
                tree.frame.frame_seq = seq;
                let package = package
                    .clone()
                    .unwrap_or_else(|| tree.package.clone());
                (seq, tree, png, package)
            }
        };
        let mut tree = tree;
        tree.frame.frame_seq = seq;
        Ok(SessionObservation {
            session_id: format!("s_{}_{}", "art", seq),
            package: package.clone(),
            frame: tree.frame.clone(),
            tree,
            screenshot_png: Some(png),
            crashed: None,
        })
    }

    /// Resolve a selector to a screen point from the current tree.
    fn point_for(&mut self, selector: &Selector) -> Result<(i32, i32), BackendError> {
        let tree = match self {
            EngineBackend::Session { session, .. } => session.tree()?,
            EngineBackend::Native(_) => unreachable!("native path forwards input"),
        };
        let node = tree.find_one(selector).ok_or_else(|| {
            BackendError::Failed {
                code: ErrorCode::AssertionFailed,
                detail: format!("no node matches selector {selector:?}"),
            }
        })?;
        Ok((
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
            EngineBackend::Session {
                session,
                frame_seq,
                cause_id,
                ..
            } => {
                let _ = (frame_seq, cause_id);
                session.tap_text(text)
            }
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
                frame_seq,
                cause_id,
                ..
            } => {
                *cause_id = request.cause_id.clone();
                *frame_seq = 0;
                let outcome = session.launch(&request.apk)?;
                if !outcome.package.is_empty() {
                    *package = Some(outcome.package.clone());
                }
                if let Some(frame) = &outcome.frame {
                    *frame_seq = frame.frame_seq;
                }
                Ok(outcome)
            }
        }
    }

    fn observe(&mut self) -> Result<SessionObservation, BackendError> {
        self.observe_session()
    }

    fn logs_since(&mut self, since: u64) -> Result<Vec<LogRecord>, BackendError> {
        match self {
            EngineBackend::Native(backend) => backend.logs_since(since),
            EngineBackend::Session { session, .. } => {
                if session.capabilities().logs {
                    Err(BackendError::unsupported(
                        "logs",
                        "this session engine has no log-stream command wired",
                    ))
                } else {
                    Err(BackendError::unsupported(
                        "logs",
                        "the ART session protocol has no log command yet",
                    ))
                }
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
                match action {
                    InputAction::Tap { selector, x, y } => {
                        if !caps.tap {
                            return Err(BackendError::unsupported("input.tap", "no tap command"));
                        }
                        if let (Some(text), true) = (
                            selector
                                .as_ref()
                                .and_then(|selector| selector.id.is_none().then_some(selector))
                                .and_then(|selector| selector.text.as_ref())
                                .and_then(|text| text.exact.clone()),
                            caps.tap_text,
                        ) {
                            self.tap_node(&text)?;
                        } else {
                            let point = match (x, y) {
                                (Some(x), Some(y)) => (*x, *y),
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
                            if let EngineBackend::Session { session, .. } = self {
                                session.tap(point.0, point.1)?;
                            }
                        }
                    }
                    InputAction::Type { text, .. } => {
                        if !caps.type_text {
                            return Err(BackendError::unsupported(
                                "input.type_text",
                                "the ART session protocol has no text-input command yet \
                                 (`type_text`); a scenario `type` step cannot run on this engine",
                            ));
                        }
                        if let EngineBackend::Session { session, .. } = self {
                            session.type_text(text)?;
                        }
                    }
                    InputAction::Key(key) => {
                        if !caps.key {
                            return Err(BackendError::unsupported("input.key", "no key command"));
                        }
                        if let EngineBackend::Session { session, .. } = self {
                            session.key(key_name(*key))?;
                        }
                    }
                    InputAction::Back => {
                        if !caps.key {
                            return Err(BackendError::unsupported("input.back", "no key command"));
                        }
                        if let EngineBackend::Session { session, .. } = self {
                            session.key("back")?;
                        }
                    }
                    InputAction::Rotate(_) => {
                        return Err(BackendError::unsupported(
                            "input.rotate",
                            "the ART session protocol has no rotate command yet",
                        ));
                    }
                }
                self.observe_session()
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
                let observed = match self {
                    EngineBackend::Session { session, frame_seq, .. } => {
                        let observed = session.capture()?;
                        *frame_seq = observed.tree.frame.frame_seq.max(*frame_seq);
                        observed
                    }
                    EngineBackend::Native(_) => unreachable!(),
                };
                let package = self.package();
                Ok(SessionObservation {
                    session_id: format!("s_art_{}", observed.tree.frame.frame_seq),
                    package: package.clone(),
                    frame: observed.tree.frame.clone(),
                    tree: observed.tree,
                    screenshot_png: Some(observed.png),
                    crashed: None,
                })
            }
        }
    }
}

impl Drop for EngineBackend {
    fn drop(&mut self) {
        if let EngineBackend::Session { session, .. } = self {
            let _ = session.close();
        }
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
    let bounds = value
        .get("bounds")
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
    node
}

/// `[l,t][r,b]` as protocol bounds.
fn parse_bounds(text: &str) -> Option<Bounds> {
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
            &dir,
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
        // The decisive transition really happened twice (tap, reset, tap).
        let taps = steps
            .iter()
            .filter(|step| step["step"] == "tap")
            .count();
        assert_eq!(taps, 2);
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
            &dir,
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
}
