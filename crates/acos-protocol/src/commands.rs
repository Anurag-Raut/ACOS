//! Typed requests and responses for every ACOS command.
//!
//! Requests are the `inputSchema` of the corresponding MCP tool; responses are
//! the typed payload carried in `Envelope.result`. A command name and its
//! request/response structs are always added together.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::artifacts::ArtifactValidation;
use crate::assertion::{Assertion, AssertionResult};
use crate::envelope::{Fidelity, Status};
use crate::error::ErrorCode;
use crate::events::Event;
use crate::frame::FrameMeta;
use crate::identity::{DisplaySpec, IsolationLevel, ResourceBudget};
use crate::ui::{Selector, UiTree};
use crate::verdict::{ResourceReport, Verdict};

const fn default_true_bool() -> bool {
    true
}

/// Every command in the protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum CommandName {
    /// Static APK inspection.
    Inspect,
    /// Execute a DEX `main` on the ACOS VM.
    RunDex,
    /// Launch an app and create a session.
    Run,
    /// Observe the UI tree/text/frame.
    Observe,
    /// Capture a fresh frame.
    Capture,
    /// Deliver an input action.
    Action,
    /// Evaluate an assertion.
    Assert,
    /// Read events since a cursor.
    Events,
    /// Reset session/app data.
    Reset,
    /// Collect memory/CPU/frame metrics.
    Profile,
    /// Destroy a session.
    Destroy,
    /// Run a scenario and produce artifacts.
    Verify,
    /// Report capability support for an APK.
    CapabilityReport,
}

impl CommandName {
    /// Stable wire string.
    pub const fn as_str(self) -> &'static str {
        match self {
            CommandName::Inspect => "inspect",
            CommandName::RunDex => "run-dex",
            CommandName::Run => "run",
            CommandName::Observe => "observe",
            CommandName::Capture => "capture",
            CommandName::Action => "action",
            CommandName::Assert => "assert",
            CommandName::Events => "events",
            CommandName::Reset => "reset",
            CommandName::Profile => "profile",
            CommandName::Destroy => "destroy",
            CommandName::Verify => "verify",
            CommandName::CapabilityReport => "capability-report",
        }
    }

    /// The MCP tool name (`acos_<command with underscores>`).
    pub const fn tool_name(self) -> &'static str {
        match self {
            CommandName::Inspect => "acos_inspect",
            CommandName::RunDex => "acos_run_dex",
            CommandName::Run => "acos_run",
            CommandName::Observe => "acos_observe",
            CommandName::Capture => "acos_capture",
            CommandName::Action => "acos_action",
            CommandName::Assert => "acos_assert",
            CommandName::Events => "acos_events",
            CommandName::Reset => "acos_reset",
            CommandName::Profile => "acos_profile",
            CommandName::Destroy => "acos_destroy",
            CommandName::Verify => "acos_verify",
            CommandName::CapabilityReport => "acos_capability_report",
        }
    }
}

/// Identity fields present on every request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RequestBase {
    /// Client-chosen request id, echoed in the envelope and used as the frame
    /// `cause_id` for actions that produce a frame.
    pub request_id: String,
    /// Session the request targets (absent for session-creating commands).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// The last frame sequence the client has seen, when relevant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_seq: Option<u64>,
    /// The cause of the last known frame, when relevant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause_id: Option<String>,
    /// Requested isolation level (for `run`/`verify`; reported everywhere).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolation: Option<IsolationLevel>,
    /// Requested display (for `run`; reported everywhere).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<DisplaySpec>,
    /// Hard resource budget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<ResourceBudget>,
    /// Capabilities the request needs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
}

impl RequestBase {
    /// A base with only a request id.
    pub fn new(request_id: impl Into<String>) -> Self {
        RequestBase {
            request_id: request_id.into(),
            session_id: None,
            frame_seq: None,
            cause_id: None,
            isolation: None,
            display: None,
            budget: None,
            capabilities: Vec::new(),
        }
    }

    /// Builder: target a session.
    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    /// Builder: request isolation.
    pub fn with_isolation(mut self, isolation: IsolationLevel) -> Self {
        self.isolation = Some(isolation);
        self
    }

    /// Builder: request a display.
    pub fn with_display(mut self, display: DisplaySpec) -> Self {
        self.display = Some(display);
        self
    }

    /// Builder: set a budget.
    pub fn with_budget(mut self, budget: ResourceBudget) -> Self {
        self.budget = Some(budget);
        self
    }
}

// ---------------------------------------------------------------------------
// inspect
// ---------------------------------------------------------------------------

/// `inspect`: static APK analysis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InspectRequest {
    /// Identity fields.
    pub base: RequestBase,
    /// APK path (workspace-relative or absolute).
    pub apk: String,
    /// Include native library details.
    #[serde(default)]
    pub native_libs: bool,
    /// Include the ABI map.
    #[serde(default)]
    pub abi_map: bool,
}

/// `<application>` summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ApplicationSummary {
    /// Application class name.
    #[serde(default)]
    pub name: Option<String>,
    /// Whether the app is debuggable.
    #[serde(default)]
    pub debuggable: Option<bool>,
    /// Whether backup is allowed.
    #[serde(default)]
    pub allow_backup: Option<bool>,
    /// Whether native libs are extracted.
    #[serde(default)]
    pub extract_native_libs: Option<bool>,
    /// Theme resource id.
    #[serde(default)]
    pub theme: Option<u32>,
    /// Custom process name.
    #[serde(default)]
    pub process: Option<String>,
}

/// A permission declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PermissionSummary {
    /// Permission name.
    pub name: String,
    /// Optional maximum SDK.
    #[serde(default)]
    pub max_sdk: Option<u32>,
}

/// A feature declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FeatureSummary {
    /// Feature name.
    pub name: String,
    /// Whether the feature is required.
    #[serde(default)]
    pub required: Option<bool>,
    /// Feature version.
    #[serde(default)]
    pub version: Option<u32>,
}

/// Manifest component kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ComponentKindSummary {
    /// Activity.
    Activity,
    /// `<activity-alias>` (a launcher entry that targets an activity).
    #[serde(rename = "activity_alias")]
    ActivityAlias,
    /// Service.
    Service,
    /// Broadcast receiver.
    Receiver,
    /// Content provider.
    Provider,
}

/// Intent data attributes.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct IntentDataSummary {
    /// URI scheme.
    #[serde(default)]
    pub scheme: Option<String>,
    /// URI host.
    #[serde(default)]
    pub host: Option<String>,
    /// URI port.
    #[serde(default)]
    pub port: Option<String>,
    /// Exact path.
    #[serde(default)]
    pub path: Option<String>,
    /// Path prefix.
    #[serde(default)]
    pub path_prefix: Option<String>,
    /// Path pattern.
    #[serde(default)]
    pub path_pattern: Option<String>,
    /// MIME type.
    #[serde(default)]
    pub mime_type: Option<String>,
}

/// Intent filter.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct IntentFilterSummary {
    /// Actions.
    #[serde(default)]
    pub actions: Vec<String>,
    /// Categories.
    #[serde(default)]
    pub categories: Vec<String>,
    /// Data rules.
    #[serde(default)]
    pub data: Vec<IntentDataSummary>,
}

/// One manifest component.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ComponentSummary {
    /// Component kind.
    pub kind: ComponentKindSummary,
    /// Fully qualified component name.
    pub name: String,
    /// Exported flag.
    #[serde(default)]
    pub exported: Option<bool>,
    /// Launch mode.
    #[serde(default)]
    pub launch_mode: Option<String>,
    /// Process override.
    #[serde(default)]
    pub process: Option<String>,
    /// Required permission.
    #[serde(default)]
    pub permission: Option<String>,
    /// Resolved label.
    #[serde(default)]
    pub label: Option<String>,
    /// Intent filters.
    #[serde(default)]
    pub intent_filters: Vec<IntentFilterSummary>,
    /// `<activity-alias android:targetActivity>`: the class the framework
    /// instantiates (absent for a plain activity).
    #[serde(default)]
    pub target_activity: Option<String>,
    /// `android:enabled`; absent means enabled.
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// Components grouped by kind.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct ComponentsSummary {
    /// Activities.
    #[serde(default)]
    pub activities: Vec<ComponentSummary>,
    /// Services.
    #[serde(default)]
    pub services: Vec<ComponentSummary>,
    /// Receivers.
    #[serde(default)]
    pub receivers: Vec<ComponentSummary>,
    /// Providers.
    #[serde(default)]
    pub providers: Vec<ComponentSummary>,
}

/// One DEX entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DexFileSummary {
    /// Entry name (`classes.dex`).
    pub name: String,
    /// Uncompressed size in bytes.
    pub size: u64,
}

/// One native library entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NativeLibSummary {
    /// ABI directory.
    pub abi: String,
    /// Entry name.
    pub name: String,
    /// Uncompressed size in bytes.
    pub size: u64,
}

/// One asset entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AssetSummary {
    /// Entry name.
    pub name: String,
    /// Uncompressed size in bytes.
    pub size: u64,
}

/// Signature-scheme detection (not cryptographic verification).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SigningSummary {
    /// v1 signature present.
    pub v1: bool,
    /// v2 signature present.
    pub v2: bool,
    /// v3 signature present.
    pub v3: bool,
    /// v3.1 signature present.
    pub v3_1: bool,
    /// Source stamp present.
    pub source_stamp: bool,
    /// Always states that signatures are not verified.
    pub note: String,
}

/// One resource type count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ResourceTypeSummary {
    /// Resource type name.
    pub name: String,
    /// Number of entries.
    pub entries: u64,
}

/// Resource-table summary.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct ResourceSummary {
    /// Whether `resources.arsc` is present.
    pub present: bool,
    /// Package count.
    pub packages: u64,
    /// Entry count.
    pub entries: u64,
    /// Type-spec count.
    pub type_specs: u64,
    /// Global string count.
    pub global_strings: u64,
    /// Per-type counts.
    #[serde(default)]
    pub types: Vec<ResourceTypeSummary>,
}

/// `inspect` result (a typed superset of the existing `acos inspect --json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InspectResult {
    /// APK path as given.
    pub apk_path: String,
    /// APK size in bytes.
    pub apk_size_bytes: u64,
    /// Number of zip entries.
    pub zip_entries: u64,
    /// Package name.
    pub package: String,
    /// Version code.
    #[serde(default)]
    pub version_code: Option<i64>,
    /// Version code major.
    #[serde(default)]
    pub version_code_major: Option<i32>,
    /// Version name.
    #[serde(default)]
    pub version_name: Option<String>,
    /// Minimum SDK.
    #[serde(default)]
    pub min_sdk: Option<u32>,
    /// Target SDK.
    #[serde(default)]
    pub target_sdk: Option<u32>,
    /// Maximum SDK.
    #[serde(default)]
    pub max_sdk: Option<u32>,
    /// Compile SDK.
    #[serde(default)]
    pub compile_sdk: Option<u32>,
    /// Resolved app label.
    #[serde(default)]
    pub label: Option<String>,
    /// Application attributes.
    pub application: ApplicationSummary,
    /// Launcher activity.
    #[serde(default)]
    pub launcher_activity: Option<String>,
    /// Declared permissions.
    #[serde(default)]
    pub permissions: Vec<PermissionSummary>,
    /// Declared features.
    #[serde(default)]
    pub uses_features: Vec<FeatureSummary>,
    /// Components.
    pub components: ComponentsSummary,
    /// DEX entries.
    #[serde(default)]
    pub dex_files: Vec<DexFileSummary>,
    /// Supported native ABIs.
    #[serde(default)]
    pub native_abis: Vec<String>,
    /// Native libraries.
    #[serde(default)]
    pub native_libs: Vec<NativeLibSummary>,
    /// Assets.
    #[serde(default)]
    pub assets: Vec<AssetSummary>,
    /// Signature detection.
    pub signing: SigningSummary,
    /// Resource summary.
    pub resources: ResourceSummary,
}

// ---------------------------------------------------------------------------
// run-dex
// ---------------------------------------------------------------------------

/// `run-dex`: execute a DEX `main`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RunDexRequest {
    /// Identity fields.
    pub base: RequestBase,
    /// `.dex`, `.apk`, or directory of `.dex` files.
    pub path: String,
    /// Fully qualified main class.
    #[serde(default = "default_main")]
    pub main: String,
    /// Program arguments for `main(String[])`.
    #[serde(default)]
    pub args: Vec<String>,
    /// Heap limit in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heap_max_bytes: Option<u64>,
    /// Run the GC-stress profile.
    #[serde(default)]
    pub gc_stress: bool,
}

fn default_main() -> String {
    "Main".to_string()
}

/// An uncaught Java exception.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UncaughtException {
    /// Exception class.
    pub class: String,
    /// Exception message.
    #[serde(default)]
    pub message: Option<String>,
    /// Stack frames.
    #[serde(default)]
    pub stack: Vec<String>,
}

/// `run-dex` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RunDexResult {
    /// Process exit code.
    pub exit_code: i32,
    /// Captured stdout.
    pub stdout: String,
    /// Captured stderr.
    pub stderr: String,
    /// Uncaught exception, when the program failed.
    #[serde(default)]
    pub uncaught_exception: Option<UncaughtException>,
}

// ---------------------------------------------------------------------------
// run
// ---------------------------------------------------------------------------

/// Data lifetime policy for a new session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum DataPolicy {
    /// Fresh app data, discarded on destroy.
    Ephemeral,
    /// Persist app data across sessions.
    Persistent,
    /// Fresh app data, kept until reset.
    Fresh,
}

/// `run`: launch an app and create a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RunRequest {
    /// Identity fields; `isolation` and `display` are honoured here.
    pub base: RequestBase,
    /// APK path.
    pub apk: String,
    /// Activity component (`pkg/.Activity` or `.Activity`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<String>,
    /// Intent action instead of an explicit activity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent_action: Option<String>,
    /// Intent data URI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    /// String extras.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extras: BTreeMap<String, String>,
    /// Data lifetime policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_policy: Option<DataPolicy>,
    /// Human label for the session (also used in artifacts).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// `run` result (the created session).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RunResponse {
    /// New session id.
    pub session_id: String,
    /// App package.
    pub package: String,
    /// Launched component.
    #[serde(default)]
    pub activity: Option<String>,
    /// Effective isolation.
    pub isolation: IsolationLevel,
    /// Effective display.
    pub display: DisplaySpec,
    /// Achieved fidelity.
    pub fidelity: Fidelity,
    /// First meaningful frame, when one was produced before the request returned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame: Option<FrameMeta>,
}

// ---------------------------------------------------------------------------
// observe
// ---------------------------------------------------------------------------

/// `observe`: UI tree, text and frame metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ObserveRequest {
    /// Identity fields.
    pub base: RequestBase,
    /// Include the structured UI tree.
    #[serde(default = "default_true_bool")]
    pub tree: bool,
    /// Include the newline-joined text rendering.
    #[serde(default)]
    pub text: bool,
    /// Include frame metadata.
    #[serde(default = "default_true_bool")]
    pub include_frame: bool,
    /// Maximum nodes to return (truncation is reported).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_nodes: Option<usize>,
}

/// `observe` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ObserveResponse {
    /// Session id.
    pub session_id: String,
    /// Package owning the foreground window.
    pub package: String,
    /// Frame metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame: Option<FrameMeta>,
    /// Structured tree (when requested).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree: Option<UiTree>,
    /// Text rendering (when requested).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

// ---------------------------------------------------------------------------
// capture
// ---------------------------------------------------------------------------

/// Capture encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum CaptureFormat {
    /// PNG.
    #[default]
    Png,
    /// Raw RGBA pixels.
    Raw,
}

/// `capture`: obtain a fresh frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CaptureRequest {
    /// Identity fields.
    pub base: RequestBase,
    /// Encoding.
    #[serde(default)]
    pub format: CaptureFormat,
    /// Require a frame produced after the request (default true).
    #[serde(default = "default_true_bool")]
    pub fresh: bool,
    /// Only accept a frame with `frame_seq > after_seq`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_seq: Option<u64>,
    /// Only accept a frame caused by this request/action id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_cause: Option<String>,
    /// Output path, relative to the artifacts directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
}

/// `capture` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CaptureResponse {
    /// The accepted frame.
    pub frame: FrameMeta,
    /// Encoding written.
    pub format: CaptureFormat,
    /// Output path, when written to disk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Encoded size in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

// ---------------------------------------------------------------------------
// action
// ---------------------------------------------------------------------------

/// A point in screen pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Point {
    /// X coordinate.
    pub x: i32,
    /// Y coordinate.
    pub y: i32,
}

/// Screen orientation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Orientation {
    /// Portrait.
    Portrait,
    /// Landscape.
    Landscape,
}

/// A symbolic key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum KeyName {
    /// Back.
    Back,
    /// Home.
    Home,
    /// Enter.
    Enter,
    /// Tab.
    Tab,
    /// Delete/backspace.
    Delete,
    /// Escape.
    Escape,
    /// Volume up.
    VolumeUp,
    /// Volume down.
    VolumeDown,
    /// Power.
    Power,
    /// Menu.
    Menu,
    /// D-pad up.
    DpadUp,
    /// D-pad down.
    DpadDown,
    /// D-pad left.
    DpadLeft,
    /// D-pad right.
    DpadRight,
}

/// One input action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    /// Tap a selector or a point.
    Tap {
        /// Target selector (mutually exclusive with x/y).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selector: Option<Selector>,
        /// Explicit x (mutually exclusive with selector).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        x: Option<i32>,
        /// Explicit y (mutually exclusive with selector).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        y: Option<i32>,
        /// Event-driven timeout.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
    },
    /// Long-press a selector or point.
    LongPress {
        /// Target selector.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selector: Option<Selector>,
        /// Explicit x.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        x: Option<i32>,
        /// Explicit y.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        y: Option<i32>,
        /// Hold duration.
        #[serde(default = "default_long_press_ms")]
        duration_ms: u64,
    },
    /// Swipe/drag between two points.
    Swipe {
        /// Start point.
        from: Point,
        /// End point.
        to: Point,
        /// Gesture duration.
        #[serde(default = "default_swipe_ms")]
        duration_ms: u64,
    },
    /// Type text, optionally into a selected node first.
    Type {
        /// Target node (absent = current focus).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selector: Option<Selector>,
        /// Text to enter.
        text: String,
        /// Clear the field first.
        #[serde(default)]
        clear: bool,
    },
    /// Press a symbolic key.
    Key {
        /// Key name.
        key: KeyName,
    },
    /// Press Back.
    Back,
    /// Rotate the display.
    Rotate {
        /// Requested orientation.
        orientation: Orientation,
    },
    /// Wait for a selector (event-driven, not a polling sleep).
    Wait {
        /// Selector to wait for.
        selector: Selector,
        /// Event-driven timeout.
        #[serde(default = "default_wait_ms")]
        timeout_ms: u64,
    },
}

const fn default_long_press_ms() -> u64 {
    600
}

const fn default_swipe_ms() -> u64 {
    300
}

const fn default_wait_ms() -> u64 {
    5_000
}

impl Action {
    /// Stable kind string.
    pub const fn kind(&self) -> &'static str {
        match self {
            Action::Tap { .. } => "tap",
            Action::LongPress { .. } => "long_press",
            Action::Swipe { .. } => "swipe",
            Action::Type { .. } => "type",
            Action::Key { .. } => "key",
            Action::Back => "back",
            Action::Rotate { .. } => "rotate",
            Action::Wait { .. } => "wait",
        }
    }

    /// Semantic problems with this action (empty when valid).
    pub fn validation_errors(&self) -> Vec<String> {
        let mut errors = Vec::new();
        match self {
            Action::Tap { selector, x, y, .. } => {
                if selector.is_none() && (x.is_none() || y.is_none()) {
                    errors.push("tap needs `selector` or both `x` and `y`".to_string());
                }
                if selector.is_some() && (x.is_some() || y.is_some()) {
                    errors.push("tap accepts `selector` or `x`/`y`, not both".to_string());
                }
                if let Some(sel) = selector {
                    errors.extend(
                        sel.validation_errors()
                            .into_iter()
                            .map(|e| format!("tap.selector: {e}")),
                    );
                }
            }
            Action::LongPress { selector, x, y, .. } => {
                if selector.is_none() && (x.is_none() || y.is_none()) {
                    errors.push("long_press needs `selector` or both `x` and `y`".to_string());
                }
                if let Some(sel) = selector {
                    errors.extend(
                        sel.validation_errors()
                            .into_iter()
                            .map(|e| format!("long_press.selector: {e}")),
                    );
                }
            }
            Action::Type {
                selector,
                text,
                clear,
            } => {
                if text.is_empty() && !clear {
                    errors.push("type needs non-empty `text` (or `clear: true`)".to_string());
                }
                if let Some(sel) = selector {
                    errors.extend(
                        sel.validation_errors()
                            .into_iter()
                            .map(|e| format!("type.selector: {e}")),
                    );
                }
            }
            Action::Wait {
                selector,
                timeout_ms,
            } => {
                errors.extend(
                    selector
                        .validation_errors()
                        .into_iter()
                        .map(|e| format!("wait.selector: {e}")),
                );
                if *timeout_ms == 0 {
                    errors.push("wait.timeout_ms must be > 0".to_string());
                }
            }
            Action::Swipe { duration_ms, .. } => {
                if *duration_ms == 0 {
                    errors.push("swipe.duration_ms must be > 0".to_string());
                }
            }
            Action::Key { .. } | Action::Back | Action::Rotate { .. } => {}
        }
        errors
    }
}

/// `action`: deliver one input action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ActionRequest {
    /// Identity fields.
    pub base: RequestBase,
    /// The action.
    pub action: Action,
}

/// `action` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ActionResponse {
    /// Session id.
    pub session_id: String,
    /// Action kind that ran.
    pub action: String,
    /// Whether the platform acknowledged delivery.
    pub accepted: bool,
    /// Whether the observable state changed.
    pub state_changed: bool,
    /// Event seq of the input acknowledgement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_ack_seq: Option<u64>,
    /// Fresh frame caused by this action, when produced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame: Option<FrameMeta>,
}

// ---------------------------------------------------------------------------
// assert
// ---------------------------------------------------------------------------

/// `assert`: evaluate an assertion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AssertRequest {
    /// Identity fields.
    pub base: RequestBase,
    /// The assertion.
    pub assertion: Assertion,
    /// Produce a fresh frame as part of the assertion evidence.
    #[serde(default)]
    pub fresh_frame: bool,
}

/// `assert` result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AssertResponse {
    /// Whether the assertion held.
    pub passed: bool,
    /// Structured result (diff, nodes, message).
    pub result: AssertionResult,
    /// Fresh frame, when requested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame: Option<FrameMeta>,
}

// ---------------------------------------------------------------------------
// events
// ---------------------------------------------------------------------------

/// `events`: read the session event stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EventsRequest {
    /// Identity fields.
    pub base: RequestBase,
    /// Return events with `seq > since_seq` (cursor from the previous call).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_seq: Option<u64>,
    /// Only return these kinds (empty = all).
    #[serde(default)]
    pub kinds: Vec<crate::events::EventKind>,
    /// Maximum number of events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    /// Event-driven wait for at least one matching event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

/// `events` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EventsResponse {
    /// Session id.
    pub session_id: String,
    /// Matching events in increasing `seq` order.
    pub events: Vec<Event>,
    /// Cursor for the next call (the last returned `seq`, or the input cursor).
    pub next_seq: u64,
    /// True when the result was truncated by `limit`.
    #[serde(default)]
    pub truncated: bool,
}

// ---------------------------------------------------------------------------
// reset / profile / destroy
// ---------------------------------------------------------------------------

/// What a reset clears.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ResetScope {
    /// Recreate the task only; app data is shared.
    Task,
    /// Clear app data and relaunch.
    App,
    /// Clear app data and caches.
    Data,
    /// Reset everything the session owns.
    All,
}

/// `reset`: reset session/app/data state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ResetRequest {
    /// Identity fields.
    pub base: RequestBase,
    /// What to reset.
    pub scope: ResetScope,
}

/// `reset` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ResetResponse {
    /// Session id.
    pub session_id: String,
    /// Scope applied.
    pub scope: ResetScope,
    /// Whether observable state was actually reset.
    pub state_reset: bool,
    /// Whether app data was cleared.
    pub data_cleared: bool,
    /// Effective isolation (shared-task resets cannot clear shared data).
    pub isolation: IsolationLevel,
}

/// `profile`: collect metrics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProfileRequest {
    /// Identity fields.
    pub base: RequestBase,
    /// Include memory metrics.
    #[serde(default = "default_true_bool")]
    pub memory: bool,
    /// Include CPU metrics.
    #[serde(default = "default_true_bool")]
    pub cpu: bool,
    /// Include frame metrics.
    #[serde(default = "default_true_bool")]
    pub frames: bool,
    /// Sampling window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

/// `profile` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProfileResponse {
    /// Session id.
    pub session_id: String,
    /// Measured resource report (metric definitions are in `docs/AGENT_API.md`).
    pub report: ResourceReport,
}

/// `destroy`: tear down a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DestroyRequest {
    /// Identity fields.
    pub base: RequestBase,
}

/// `destroy` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DestroyResponse {
    /// Session id.
    pub session_id: String,
    /// True when a session was destroyed (false when it was already gone).
    pub destroyed: bool,
    /// True when the response is idempotent for an already-absent session.
    #[serde(default)]
    pub already_absent: bool,
}

// ---------------------------------------------------------------------------
// verify
// ---------------------------------------------------------------------------

/// The execution engine `verify` runs on.
///
/// This is the **execution** engine (real ART or the frozen Rust VM). It is not
/// the scenario YAML `engine:` field, which is a legacy native/reference
/// preference (`native-preferred` | `native-only` | `reference-only`) that ART
/// verification does not consume.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum VerifyEngine {
    /// Real Google ART through an `acos-art` guest (the default).
    Art,
    /// The frozen Rust VM / native runtime (diagnostics; never silently chosen).
    RustVm,
}

impl Default for VerifyEngine {
    fn default() -> Self {
        VerifyEngine::Art
    }
}

impl VerifyEngine {
    /// The stable CLI string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            VerifyEngine::Art => "art",
            VerifyEngine::RustVm => "rust-vm",
        }
    }
}

/// `verify`: run an APK + scenario and produce artifacts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VerifyRequest {
    /// Identity fields.
    pub base: RequestBase,
    /// APK path (overrides the scenario `app` when set).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apk: Option<String>,
    /// Scenario path.
    pub scenario: String,
    /// Artifacts output directory.
    ///
    /// A run owns its directory: a non-empty directory written by another run is
    /// refused with `INVALID_ARGUMENT` naming the previous run id, unless
    /// [`VerifyRequest::overwrite`] is set.
    pub artifacts: String,
    /// Replace artifacts left by another run in `artifacts` (`--overwrite`).
    ///
    /// Off by default: two runs never interleave evidence in one directory, so a
    /// stale frame can never be mistaken for this run's.
    #[serde(default)]
    pub overwrite: bool,
    /// Optional art-reference directory to compare against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    /// Minimum fidelity the verdict must achieve.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_fidelity: Option<Fidelity>,
    /// Execution engine: `art` (default, real ART) or `rust-vm` (frozen).
    #[serde(default)]
    pub engine: VerifyEngine,
    /// Deterministic `NAME=VALUE` flags recorded in the reproducer
    /// (`repro.json`'s `flags` and the `--flag` lines of `repro.sh`).
    ///
    /// A flag is metadata, not a command-line option: it must be
    /// `NAME=VALUE` with a name matching `[A-Za-z][A-Za-z0-9_.-]*` and a
    /// non-empty value, so a value can never be read as another option. A
    /// malformed flag is `INVALID_ARGUMENT` (`details.index` names it).
    #[serde(default)]
    pub flags: Vec<String>,
}

/// `verify` result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct VerifyResponse {
    /// The complete verdict document (also written to `verdict.json`).
    pub verdict: Verdict,
    /// Path to `verdict.json`.
    pub verdict_path: String,
    /// Artifacts directory.
    pub artifacts_dir: String,
    /// Validation of the artifacts directory against its verdict.
    pub artifact_validation: ArtifactValidation,
}

// ---------------------------------------------------------------------------
// capability-report
// ---------------------------------------------------------------------------

/// Support state for one capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum CapabilityStatus {
    /// Implemented and exercised.
    Supported,
    /// Not implemented; the task cannot be claimed as a pass.
    Unsupported,
    /// Not yet determined.
    Unknown,
    /// Requires a real device.
    RequiresDevice,
    /// Requires Google Mobile Services.
    RequiresGms,
    /// Requires a specific ABI the engine lacks.
    RequiresAbi,
}

/// One capability check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CapabilityCheck {
    /// Capability name (`ui.text`, `input`, `jni`, `network.mock`, ...).
    pub capability: String,
    /// Support state.
    pub status: CapabilityStatus,
    /// Machine code when unsupported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<ErrorCode>,
    /// Human detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// What to do next.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

/// `capability-report`: what this engine can do for an APK/scenario.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CapabilityReportRequest {
    /// Identity fields.
    pub base: RequestBase,
    /// APK path.
    pub apk: String,
    /// Optional scenario to check against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scenario: Option<String>,
    /// Include native-library/symbol coverage.
    #[serde(default)]
    pub native_libs: bool,
}

/// `capability-report` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CapabilityReport {
    /// APK path.
    pub apk: String,
    /// Detected package.
    #[serde(default)]
    pub package: Option<String>,
    /// APK ABIs.
    #[serde(default)]
    pub abis: Vec<String>,
    /// Minimum SDK.
    #[serde(default)]
    pub min_sdk: Option<u32>,
    /// Target SDK.
    #[serde(default)]
    pub target_sdk: Option<u32>,
    /// One check per capability required by the APK/scenario.
    pub checks: Vec<CapabilityCheck>,
    /// True only when every required capability is supported.
    pub all_required_supported: bool,
    /// Fidelity this engine can offer for this input today.
    pub fidelity: Fidelity,
    /// Overall status (`pass` when supported; never a pass when any required
    /// capability is unsupported).
    pub status: Status,
}

// ---------------------------------------------------------------------------
// typed command union
// ---------------------------------------------------------------------------

/// A request for any command.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "command", content = "request", rename_all = "kebab-case")]
pub enum CommandRequest {
    /// `inspect`.
    Inspect(InspectRequest),
    /// `run-dex`.
    RunDex(RunDexRequest),
    /// `run`.
    Run(RunRequest),
    /// `observe`.
    Observe(ObserveRequest),
    /// `capture`.
    Capture(CaptureRequest),
    /// `action`.
    Action(ActionRequest),
    /// `assert`.
    Assert(AssertRequest),
    /// `events`.
    Events(EventsRequest),
    /// `reset`.
    Reset(ResetRequest),
    /// `profile`.
    Profile(ProfileRequest),
    /// `destroy`.
    Destroy(DestroyRequest),
    /// `verify`.
    Verify(VerifyRequest),
    /// `capability-report`.
    CapabilityReport(CapabilityReportRequest),
}

impl CommandRequest {
    /// The command name.
    pub const fn name(&self) -> CommandName {
        match self {
            CommandRequest::Inspect(_) => CommandName::Inspect,
            CommandRequest::RunDex(_) => CommandName::RunDex,
            CommandRequest::Run(_) => CommandName::Run,
            CommandRequest::Observe(_) => CommandName::Observe,
            CommandRequest::Capture(_) => CommandName::Capture,
            CommandRequest::Action(_) => CommandName::Action,
            CommandRequest::Assert(_) => CommandName::Assert,
            CommandRequest::Events(_) => CommandName::Events,
            CommandRequest::Reset(_) => CommandName::Reset,
            CommandRequest::Profile(_) => CommandName::Profile,
            CommandRequest::Destroy(_) => CommandName::Destroy,
            CommandRequest::Verify(_) => CommandName::Verify,
            CommandRequest::CapabilityReport(_) => CommandName::CapabilityReport,
        }
    }
}

/// A typed response payload for any command.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "command", content = "response", rename_all = "kebab-case")]
pub enum CommandResponse {
    /// `inspect`.
    Inspect(InspectResult),
    /// `run-dex`.
    RunDex(RunDexResult),
    /// `run`.
    Run(RunResponse),
    /// `observe`.
    Observe(ObserveResponse),
    /// `capture`.
    Capture(CaptureResponse),
    /// `action`.
    Action(ActionResponse),
    /// `assert`.
    Assert(AssertResponse),
    /// `events`.
    Events(EventsResponse),
    /// `reset`.
    Reset(ResetResponse),
    /// `profile`.
    Profile(ProfileResponse),
    /// `destroy`.
    Destroy(DestroyResponse),
    /// `verify`.
    Verify(VerifyResponse),
    /// `capability-report`.
    CapabilityReport(CapabilityReport),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_validation_catches_ambiguity() {
        let tap_both = Action::Tap {
            selector: Some(Selector::by_id("btn")),
            x: Some(10),
            y: Some(20),
            timeout_ms: None,
        };
        assert!(!tap_both.validation_errors().is_empty());
        let tap_none = Action::Tap {
            selector: None,
            x: None,
            y: None,
            timeout_ms: None,
        };
        assert!(!tap_none.validation_errors().is_empty());
        let tap_ok = Action::Tap {
            selector: Some(Selector::by_id("btn")),
            x: None,
            y: None,
            timeout_ms: None,
        };
        assert!(tap_ok.validation_errors().is_empty());
    }

    #[test]
    fn typed_union_round_trips() {
        let req = CommandRequest::Inspect(InspectRequest {
            base: RequestBase::new("req_1"),
            apk: "fixtures/build/apps/hello-views.apk".to_string(),
            native_libs: true,
            abi_map: false,
        });
        let value = serde_json::to_value(&req).unwrap();
        assert_eq!(value["command"], "inspect");
        assert_eq!(
            value["request"]["apk"],
            "fixtures/build/apps/hello-views.apk"
        );
        let back: CommandRequest = serde_json::from_value(value).unwrap();
        assert_eq!(back, req);
    }
}

/// Validate one deterministic reproducer flag (`NAME=VALUE`).
///
/// The syntax is deliberately strict: a flag is recorded in the reproducer and
/// echoed by `repro.sh`, so a value that started with `-` (for example
/// `--native`) could be read as a command-line option by whatever replays the
/// script. `acos verify --flag` and the MCP `acos_verify` tool share this
/// function so the CLI and the server cannot disagree.
///
/// Valid: `engine=art`, `variant.pre-fix=1`, `build_id=abc-123`.
/// Invalid: `--native` (no `=`, looks like an option), `engine=` (empty value),
/// `=art` (empty name), `engine=ar t` (whitespace), `1x=2` (name starts with a
/// digit).
pub fn validate_reproducer_flag(flag: &str) -> std::result::Result<(), String> {
    if flag.is_empty() {
        return Err("a flag must not be empty".to_string());
    }
    if flag.len() > 256 {
        return Err(format!(
            "a flag must be at most 256 bytes (this one is {})",
            flag.len()
        ));
    }
    let Some((name, value)) = flag.split_once('=') else {
        return Err(format!(
            "`{flag}` is not NAME=VALUE (a deterministic flag is metadata, not an acos option;              use for example `engine=art`)"
        ));
    };
    if name.is_empty() {
        return Err(format!("`{flag}` has an empty name (expected NAME=VALUE)"));
    }
    let mut chars = name.chars();
    let first = chars.next().expect("non-empty");
    if !first.is_ascii_alphabetic() {
        return Err(format!(
            "`{name}` must start with an ASCII letter (NAME=VALUE)"
        ));
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-') {
        return Err(format!(
            "`{name}` may only contain ASCII letters, digits, `_`, `.` and `-`"
        ));
    }
    if value.is_empty() {
        return Err(format!("`{flag}` has an empty value (expected NAME=VALUE)"));
    }
    if let Some(bad) = value
        .chars()
        .find(|c| c.is_whitespace() || c.is_control() || *c == '"' || *c == '\'')
    {
        return Err(format!(
            "the value of `{name}` contains {bad:?}; use a plain token (no whitespace or quotes)"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod flag_tests {
    use super::validate_reproducer_flag;

    #[test]
    fn name_value_flags_are_accepted() {
        for flag in [
            "engine=art",
            "variant.pre-fix=1",
            "build_id=abc-123",
            "a.b_c-d=value",
            "k=v=w",
        ] {
            assert!(validate_reproducer_flag(flag).is_ok(), "{flag}");
        }
    }

    #[test]
    fn option_like_and_malformed_flags_are_rejected() {
        for (flag, needle) in [
            ("--native", "NAME=VALUE"),
            ("-x", "NAME=VALUE"),
            ("engine", "NAME=VALUE"),
            ("engine=", "empty value"),
            ("=art", "empty name"),
            ("1x=2", "ASCII letter"),
            ("en gine=art", "letters"),
            ("engine=ar t", "whitespace"),
            ("engine=a\"b", "whitespace or quotes"),
            ("", "empty"),
        ] {
            let error = validate_reproducer_flag(flag).expect_err(flag);
            assert!(
                error.contains(needle),
                "flag {flag:?} produced {error:?}, expected {needle:?}"
            );
        }
        assert!(validate_reproducer_flag(&format!("k={}", "v".repeat(300))).is_err());
    }
}
