//! The backend seam.
//!
//! `acos verify` never talks to the VM, the service host or the renderer
//! directly. It talks to a [`Backend`], which advertises what it can do
//! ([`Capabilities`]) and performs one operation at a time. The executor maps
//! each scenario step to the capability it needs; a step whose capability is
//! absent is reported as `status: unsupported` with the exact missing
//! capability — it is never executed against a guess and never counted as a
//! pass.
//!
//! This is the extension point for M2/M3: when the native runtime grows a UI
//! tree, screenshots or input, the corresponding method is implemented and the
//! capability is switched on. The scenario schema does not change.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use acos_protocol::commands::{KeyName, Orientation, ResetScope};
use acos_protocol::identity::{DisplaySpec, IsolationLevel};
use acos_protocol::scenario::{
    DensitySetup, LocaleSetup, NetworkSetup, PermissionSetup, ThemeSetup, TimeSetup,
};
use acos_protocol::{
    Backend as ProtocolBackend, ErrorCode, Fidelity, FrameMeta, LogLevel, ProtocolError, Selector,
    Status, UiTree,
};

/// One capability the executor can require from a backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Capability {
    /// Static APK/manifest facts via `acos-apk`.
    StaticManifest,
    /// Boot/launch the application process and reach the first attach point.
    Launch,
    /// A fresh UI tree (text, ids, bounds, classes, state flags).
    UiTree,
    /// A logcat-style log stream and crash/ANR records.
    Logs,
    /// Fresh encoded frames.
    Screenshot,
    /// tap/type/key/back/rotate.
    Input,
    /// Reset task/app/data state.
    Reset,
    /// set_network / set_permission / set_locale / set_density / set_theme / set_time.
    Environment,
    /// Crash/ANR reports distinct from ordinary logs.
    CrashReports,
}

impl Capability {
    /// The stable agent-facing capability name (`ui.tree`, `input`, ...).
    pub const fn as_str(self) -> &'static str {
        match self {
            Capability::StaticManifest => "static-manifest",
            Capability::Launch => "lifecycle",
            Capability::UiTree => "ui.tree",
            Capability::Logs => "logs",
            Capability::Screenshot => "screenshot",
            Capability::Input => "input",
            Capability::Reset => "reset",
            Capability::Environment => "environment",
            Capability::CrashReports => "crash-reports",
        }
    }
}

impl std::fmt::Display for Capability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<Capability> for String {
    fn from(capability: Capability) -> Self {
        capability.as_str().to_string()
    }
}

/// What a backend can do right now.
///
/// A `false` field means "not implemented yet"; the executor reports the
/// corresponding steps as `unsupported` instead of executing them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Capabilities {
    /// Static APK/manifest facts.
    pub static_manifest: bool,
    /// Application-process launch/boot.
    pub launch: bool,
    /// UI tree observation.
    pub ui_tree: bool,
    /// Log stream.
    pub logs: bool,
    /// Screenshots.
    pub screenshot: bool,
    /// Input actions.
    pub input: bool,
    /// State reset.
    pub reset: bool,
    /// Environment knobs.
    pub environment: bool,
    /// Crash/ANR reports.
    pub crash_reports: bool,
}

impl Capabilities {
    /// Whether one capability is available.
    pub const fn has(self, capability: Capability) -> bool {
        match capability {
            Capability::StaticManifest => self.static_manifest,
            Capability::Launch => self.launch,
            Capability::UiTree => self.ui_tree,
            Capability::Logs => self.logs,
            Capability::Screenshot => self.screenshot,
            Capability::Input => self.input,
            Capability::Reset => self.reset,
            Capability::Environment => self.environment,
            Capability::CrashReports => self.crash_reports,
        }
    }

    /// The capability names available, in stable order (for `capabilities_used`).
    pub fn names(self) -> Vec<&'static str> {
        [
            Capability::StaticManifest,
            Capability::Launch,
            Capability::UiTree,
            Capability::Logs,
            Capability::Screenshot,
            Capability::Input,
            Capability::Reset,
            Capability::Environment,
            Capability::CrashReports,
        ]
        .into_iter()
        .filter(|c| self.has(*c))
        .map(Capability::as_str)
        .collect()
    }
}

/// Static identity of an APK, resolved with `acos-apk` (never execution).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestInfo {
    /// APK path as used.
    pub path: String,
    /// SHA-256 of the APK bytes.
    pub sha256: String,
    /// Android package name.
    pub package: String,
    /// `versionCode`.
    pub version_code: u64,
    /// `versionName` (empty when absent).
    pub version_name: String,
    /// `minSdkVersion`.
    pub min_sdk: u32,
    /// `targetSdkVersion`.
    pub target_sdk: u32,
    /// Application label (resolved from resources when possible).
    pub label: Option<String>,
    /// Launcher activity declared in the manifest.
    pub launcher_activity: Option<String>,
    /// Every declared activity (`pkg.Activity`).
    pub activities: Vec<String>,
    /// ABI directory names present in the APK.
    pub native_abis: Vec<String>,
    /// Whether the application is debugged.
    pub debuggable: bool,
}

impl ManifestInfo {
    /// True when `component` (fully qualified or `pkg/.Activity`) resolves to a
    /// declared activity.
    pub fn declares_activity(&self, component: &str) -> bool {
        match resolve_component(&self.package, component) {
            Some(name) => self.activities.iter().any(|a| a == &name),
            None => false,
        }
    }
}

/// Resolve a launch component (`pkg/.Activity`, `.Activity`, `Activity`) against
/// a package to a fully qualified class name.
pub fn resolve_component(package: &str, component: &str) -> Option<String> {
    let component = component.trim();
    if component.is_empty() {
        return None;
    }
    if let Some((pkg_part, class_part)) = component.split_once('/') {
        if pkg_part != package {
            return None;
        }
        let class = class_part.trim_start_matches('.');
        if class.is_empty() {
            return None;
        }
        return Some(format!("{package}.{class}"));
    }
    if let Some(rest) = component.strip_prefix('.') {
        if rest.is_empty() {
            return None;
        }
        return Some(format!("{package}.{rest}"));
    }
    if component.contains('.') {
        return component.starts_with(package).then(|| component.to_string());
    }
    // A bare class name is relative to the package.
    Some(format!("{package}.{component}"))
}

/// One logcat-style log record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogRecord {
    /// Monotonic per-session sequence.
    pub seq: u64,
    /// Milliseconds since the Unix epoch.
    pub timestamp_ms: u64,
    /// Severity.
    pub level: LogLevel,
    /// Log tag.
    pub tag: String,
    /// Message body (no trailing newline).
    pub message: String,
}

/// A crash/ANR record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrashRecord {
    /// Exception class or signal.
    pub exception: String,
    /// Exception message.
    pub message: String,
    /// Stack frames.
    pub stack: Vec<String>,
    /// True when the crash killed the process.
    pub fatal: bool,
}

/// Which evidence channels changed between two observations of one session.
///
/// This implements the advisor rule for input: a tap may only be claimed to
/// have changed state when the change is visible in **both** the UI tree and the
/// screenshot. A tree that changed while the pixels did not (or the reverse) is
/// not proof that the tap took effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StateChange {
    /// The UI-tree content hash differs from the previous observation.
    pub tree_changed: bool,
    /// Both observations carry a screenshot and the bytes differ.
    pub screenshot_changed: bool,
}

impl StateChange {
    /// At least one evidence channel changed.
    pub const fn any(self) -> bool {
        self.tree_changed || self.screenshot_changed
    }

    /// Every evidence channel the backend claims to provide changed.
    pub const fn both(self) -> bool {
        self.tree_changed && self.screenshot_changed
    }
}

impl SessionObservation {
    /// Re-stamp the observation with the causal request and time. Backends
    /// assign the frame sequence; the executor assigns the cause so the frame is
    /// traceable to the step that produced it.
    pub fn with_cause(mut self, cause_id: &str, produced_at: &str) -> Self {
        self.frame.cause_id = cause_id.to_string();
        self.frame.produced_at = produced_at.to_string();
        self.tree.frame = self.frame.clone();
        self
    }

    /// The advisor's binding rule, checked structurally.
    ///
    /// A screenshot, its UI tree and its frame metadata must all describe the
    /// same completed traversal. Because the executor receives them in one
    /// [`SessionObservation`], a backend that reports a tree frame that differs
    /// from the observation frame has produced evidence that cannot be bound to
    /// one `frame_seq`; that is a backend defect, never a pass.
    ///
    /// Returns `None` when the observation is consistently bound, or a
    /// human-readable description of the mismatch.
    pub fn binding_violation(&self) -> Option<String> {
        if self.tree.frame.frame_seq != self.frame.frame_seq {
            return Some(format!(
                "tree frame_seq {} does not match observation frame_seq {}",
                self.tree.frame.frame_seq, self.frame.frame_seq
            ));
        }
        if self.tree.frame.content_hash != self.frame.content_hash {
            return Some(format!(
                "tree content_hash {:?} does not match observation content_hash {:?}",
                self.tree.frame.content_hash, self.frame.content_hash
            ));
        }
        None
    }

    /// Compare the visible state with a previous observation of the same
    /// session. A missing screenshot on either side is not evidence of change.
    pub fn changed_from(&self, previous: &SessionObservation) -> StateChange {
        let tree_changed = self.tree.frame.content_hash != previous.tree.frame.content_hash;
        let screenshot_changed = match (&self.screenshot_png, &previous.screenshot_png) {
            (Some(current), Some(previous)) => current != previous,
            _ => false,
        };
        StateChange {
            tree_changed,
            screenshot_changed,
        }
    }
}

/// The result of a `launch` step.
///
/// A launch may legitimately have no UI tree yet (M2 stops at the attach
/// handshake). The detail value is recorded in step evidence/notes.
#[derive(Debug, Clone)]
pub struct LaunchOutcome {
    /// Session id (synthetic for replay backends).
    pub session_id: String,
    /// Package that was launched.
    pub package: String,
    /// The first frame, when the backend can produce one.
    pub frame: Option<FrameMeta>,
    /// Encoded PNG bytes for the first frame, when available.
    pub screenshot_png: Option<Vec<u8>>,
    /// A crash observed during launch, if any.
    pub crashed: Option<CrashRecord>,
    /// Backend-specific launch detail (boot stages, attach binder, ...).
    pub detail: serde_json::Value,
}

/// One guest input acknowledgment (`FINISHED`/`TIMELINE`).
///
/// The host wrote the input message; the ack itself is guest-asserted. It is
/// recorded for diagnostics, never as independent host evidence.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InputAck {
    /// Input sequence number.
    pub seq: u64,
    /// Whether the guest claimed to handle the event.
    pub handled: bool,
    /// Guest-reported latency, when present.
    pub latency_us: Option<u64>,
}

/// Host-observed input delivery plus the guest's acknowledgment.
///
/// `delivered` is the host's record that it injected the gesture/key/text.
/// `visual_change` is the host's comparison of the frame the action produced
/// with the frame before it. A legitimate action with no visual effect (a tap on
/// a no-op area, a key a view consumes without redrawing) is delivered with
/// `visual_change == false`; it is **not** a stale frame and must not be rejected
/// as one. It also must not be reported as a state change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputDelivery {
    /// The host injected the input and the guest session accepted it.
    pub delivered: bool,
    /// The completed frame after the action differs from the one before it.
    pub visual_change: bool,
    /// Guest acknowledgments, in delivery order.
    pub acks: Vec<InputAck>,
}

/// The session state after an observation or an action.
#[derive(Debug, Clone)]
pub struct SessionObservation {
    /// Session id (synthetic for replay backends).
    pub session_id: String,
    /// Package owning the tree.
    pub package: String,
    /// The frame this observation describes.
    pub frame: FrameMeta,
    /// The UI tree for that frame.
    pub tree: UiTree,
    /// Encoded PNG bytes for the frame, when the backend can provide one.
    pub screenshot_png: Option<Vec<u8>>,
    /// A crash observed for this session, if any.
    pub crashed: Option<CrashRecord>,
    /// Host-observed delivery for the input action that produced this
    /// observation (`None` for launch/tree/capture observations).
    pub input: Option<InputDelivery>,
}

/// The parameters for a `launch` step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchRequest {
    /// APK to launch.
    pub apk: PathBuf,
    /// Explicit component, when the scenario named one.
    pub activity: Option<String>,
    /// Intent action, when the scenario named one.
    pub intent_action: Option<String>,
    /// Intent data URI.
    pub data: Option<String>,
    /// String extras.
    pub extras: BTreeMap<String, String>,
    /// Clear app data first.
    pub clear_state: bool,
    /// Requested isolation.
    pub isolation: IsolationLevel,
    /// Requested display.
    pub display: DisplaySpec,
    /// Cause id for the first frame.
    pub cause_id: String,
}

/// One input action. Mirrors the scenario step payloads (not the `action`
/// command): it is deliberately small until M3.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputAction {
    /// Tap a selector or point.
    Tap {
        /// Target selector.
        selector: Option<Selector>,
        /// Explicit x.
        x: Option<i32>,
        /// Explicit y.
        y: Option<i32>,
    },
    /// Type text.
    Type {
        /// Target selector (absent = current focus).
        selector: Option<Selector>,
        /// Text to enter.
        text: String,
        /// Clear the field first.
        clear: bool,
    },
    /// Press a symbolic key.
    Key(KeyName),
    /// Press Back.
    Back,
    /// Rotate the display.
    Rotate(Orientation),
}

impl InputAction {
    /// Stable kind string.
    pub const fn kind(&self) -> &'static str {
        match self {
            InputAction::Tap { .. } => "tap",
            InputAction::Type { .. } => "type",
            InputAction::Key(_) => "key",
            InputAction::Back => "back",
            InputAction::Rotate(_) => "rotate",
        }
    }
}

/// One environment change (`set_*` step).
#[derive(Debug, Clone, PartialEq)]
pub enum EnvironmentChange {
    /// Mock/force network responses.
    Network(NetworkSetup),
    /// Grant/deny/revoke a permission.
    Permission(PermissionSetup),
    /// Set the locale.
    Locale(LocaleSetup),
    /// Set display density.
    Density(DensitySetup),
    /// Set the theme.
    Theme(ThemeSetup),
    /// Set the runtime clock.
    Time(TimeSetup),
}

impl EnvironmentChange {
    /// Stable kind string.
    pub const fn kind(&self) -> &'static str {
        match self {
            EnvironmentChange::Network(_) => "set_network",
            EnvironmentChange::Permission(_) => "set_permission",
            EnvironmentChange::Locale(_) => "set_locale",
            EnvironmentChange::Density(_) => "set_density",
            EnvironmentChange::Theme(_) => "set_theme",
            EnvironmentChange::Time(_) => "set_time",
        }
    }
}

/// A backend operation failure.
#[derive(Debug, Clone, thiserror::Error)]
pub enum BackendError {
    /// The backend cannot perform this capability.
    #[error("unsupported capability `{capability}`: {detail}")]
    Unsupported {
        /// Missing capability.
        capability: String,
        /// Exact detail (backend/API/step).
        detail: String,
    },
    /// The app crashed.
    #[error("app crashed: {message}")]
    Crash {
        /// Exception class or signal.
        exception: String,
        /// Exception message.
        message: String,
        /// Stack frames.
        stack: Vec<String>,
        /// Whether the crash killed the process.
        fatal: bool,
    },
    /// An event-driven wait or wall budget elapsed.
    #[error("timeout: {detail}")]
    Timeout {
        /// Detail.
        detail: String,
    },
    /// The app ran and a check did not hold.
    #[error("app failure ({code}): {detail}")]
    Failed {
        /// Failure code (assertion-level).
        code: ErrorCode,
        /// Diff/detail.
        detail: String,
    },
    /// The engine or harness failed.
    #[error("backend error ({code}): {detail}")]
    Error {
        /// Error code.
        code: ErrorCode,
        /// Detail.
        detail: String,
    },
}

impl BackendError {
    /// Build an `Unsupported` failure for a named capability.
    pub fn unsupported(capability: impl Into<String>, detail: impl Into<String>) -> Self {
        BackendError::Unsupported {
            capability: capability.into(),
            detail: detail.into(),
        }
    }

    /// The protocol error code this maps to.
    pub const fn code(&self) -> ErrorCode {
        match self {
            BackendError::Unsupported { .. } => ErrorCode::UnsupportedBackend,
            BackendError::Crash { .. } => ErrorCode::AppCrashed,
            BackendError::Timeout { .. } => ErrorCode::Timeout,
            BackendError::Failed { code, .. } | BackendError::Error { code, .. } => *code,
        }
    }

    /// The status this maps to.
    pub const fn status(&self) -> Status {
        self.code().default_status()
    }

    /// Convert to a protocol error (with details where useful).
    pub fn to_protocol_error(&self) -> ProtocolError {
        match self {
            BackendError::Unsupported {
                capability,
                detail,
            } => ProtocolError::new(
                ErrorCode::UnsupportedBackend,
                format!("{detail} (missing capability `{capability}`)"),
            )
            .with_hint(format!(
                "This ACOS backend does not provide `{capability}` yet. Do not fake a pass; \
                 run the step on a backend that supports it or report it as unsupported."
            ))
            .with_details(serde_json::json!({"capability": capability, "detail": detail})),
            BackendError::Crash {
                exception,
                message,
                stack,
                fatal,
            } => ProtocolError::new(
                ErrorCode::AppCrashed,
                format!("{exception}: {message}"),
            )
            .with_details(serde_json::json!({
                "exception": exception,
                "stack": stack,
                "fatal": fatal,
            })),
            BackendError::Timeout { detail } => ProtocolError::new(ErrorCode::Timeout, detail),
            BackendError::Failed { code, detail } => ProtocolError::new(*code, detail.clone()),
            BackendError::Error { code, detail } => ProtocolError::new(*code, detail.clone()),
        }
    }
}

/// The execution backend behind the scenario runner.
///
/// Default implementations return `unsupported` so a backend only implements
/// the capabilities it advertises in [`Backend::capabilities`].
pub trait Backend {
    /// Stable backend id (`acos-native`, `art-reference-replay`).
    fn id(&self) -> &'static str;

    /// Protocol backend label recorded in the verdict/envelope.
    fn protocol_backend(&self) -> ProtocolBackend;

    /// How faithful this backend's results are.
    fn fidelity(&self) -> Fidelity;

    /// What this backend can do right now.
    fn capabilities(&self) -> Capabilities;

    /// Static APK identity. Required by every backend.
    fn manifest(&mut self, apk: &Path) -> Result<ManifestInfo, BackendError>;

    /// Launch the app and return the first observation.
    fn launch(&mut self, _request: &LaunchRequest) -> Result<LaunchOutcome, BackendError> {
        Err(BackendError::unsupported(
            Capability::Launch,
            "this backend cannot launch an application process",
        ))
    }

    /// Observe the current UI tree.
    fn observe(&mut self) -> Result<SessionObservation, BackendError> {
        Err(BackendError::unsupported(
            Capability::UiTree,
            "this backend cannot observe a UI tree",
        ))
    }

    /// Log records with `seq > since`.
    fn logs_since(&mut self, _since: u64) -> Result<Vec<LogRecord>, BackendError> {
        Err(BackendError::unsupported(
            Capability::Logs,
            "this backend cannot expose a log stream",
        ))
    }

    /// Deliver one input action.
    fn input(&mut self, action: &InputAction) -> Result<SessionObservation, BackendError> {
        Err(BackendError::unsupported(
            Capability::Input,
            format!("this backend cannot deliver `{}` input", action.kind()),
        ))
    }

    /// Reset session state.
    fn reset(&mut self, _scope: ResetScope) -> Result<(), BackendError> {
        Err(BackendError::unsupported(
            Capability::Reset,
            "this backend cannot reset session state",
        ))
    }

    /// Apply an environment change.
    fn configure(&mut self, change: &EnvironmentChange) -> Result<(), BackendError> {
        Err(BackendError::unsupported(
            Capability::Environment,
            format!("this backend cannot apply `{}`", change.kind()),
        ))
    }

    /// Capture the current frame.
    fn capture(&mut self) -> Result<SessionObservation, BackendError> {
        Err(BackendError::unsupported(
            Capability::Screenshot,
            "this backend cannot capture frames",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_component_handles_forms() {
        let pkg = "dev.acos.fixtures.helloviews";
        assert_eq!(
            resolve_component(pkg, ".MainActivity").as_deref(),
            Some("dev.acos.fixtures.helloviews.MainActivity")
        );
        assert_eq!(
            resolve_component(pkg, "dev.acos.fixtures.helloviews.MainActivity").as_deref(),
            Some("dev.acos.fixtures.helloviews.MainActivity")
        );
        assert_eq!(
            resolve_component(pkg, "Other").as_deref(),
            Some("dev.acos.fixtures.helloviews.Other")
        );
        assert_eq!(resolve_component(pkg, "com.other.Activity"), None);
        assert_eq!(resolve_component(pkg, ""), None);
    }

    #[test]
    fn capability_names_are_stable() {
        let caps = Capabilities {
            static_manifest: true,
            launch: true,
            ..Capabilities::default()
        };
        assert_eq!(caps.names(), vec!["static-manifest", "lifecycle"]);
    }

    #[test]
    fn unsupported_maps_to_unsupported_status() {
        let error = BackendError::unsupported("ui.tree", "no tree");
        assert_eq!(error.code(), ErrorCode::UnsupportedBackend);
        assert_eq!(error.status(), Status::Unsupported);
        let protocol = error.to_protocol_error();
        assert_eq!(
            protocol.details.as_ref().unwrap()["capability"],
            "ui.tree"
        );
    }
}
