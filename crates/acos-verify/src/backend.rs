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

/// Resolve a launch component (`pkg/.Activity`, `pkg/Activity`, `.Activity`,
/// `Activity`) to a fully qualified class name.
///
/// The `pkg/class` forms follow Android's own rules, which are the rules a
/// scenario author copies from `adb shell am start`:
///
/// * `pkg/.Cls` → `pkg.Cls`: `ComponentName.unflattenFromString` expands a
///   leading `.` **against the package part of the string**, not against the
///   manifest package — `.cache/aosp/base/core/java/android/content/
///   ComponentName.java` (`if (cls.charAt(0) == '.') cls = pkg + cls;`). A
///   manifest package and a component's package prefix are allowed to differ
///   (`com.gaten` vs `com.gaten.app/.MainActivity`), so the old rule that
///   rejected a mismatched prefix rejected a valid Android component name.
/// * `pkg/Cls` with no `.` in `Cls` is relative: `PackageParser.buildClassName`
///   prefixes the component package (`if (name.indexOf('.') < 0) name =
///   pkg + '.' + name`), so it is `pkg.Cls`.
/// * `.Cls` and a bare `Cls` are relative to the manifest package (same
///   `buildClassName` rule).
///
/// A class name that already contains a `.` is fully qualified and is kept
/// verbatim, except that it must name the APK's own package (a typo from
/// another package is a scenario error, and the caller reports it as one).
pub fn resolve_component(package: &str, component: &str) -> Option<String> {
    let component = component.trim();
    if component.is_empty() {
        return None;
    }
    if let Some((pkg_part, class_part)) = component.split_once('/') {
        let pkg_part = pkg_part.trim();
        let class_part = class_part.trim();
        if pkg_part.is_empty() || class_part.is_empty() {
            return None;
        }
        // ComponentName.unflattenFromString: the leading dot is expanded with
        // the package written in the component string itself.
        if let Some(rest) = class_part.strip_prefix('.') {
            if rest.is_empty() {
                return None;
            }
            return Some(format!("{pkg_part}.{rest}"));
        }
        // PackageParser.buildClassName: a dotless class is relative.
        if !class_part.contains('.') {
            return Some(format!("{pkg_part}.{class_part}"));
        }
        return Some(class_part.to_string());
    }
    if let Some(rest) = component.strip_prefix('.') {
        if rest.is_empty() {
            return None;
        }
        return Some(format!("{package}.{rest}"));
    }
    if component.contains('.') {
        let own_package =
            component == package || component.starts_with(&format!("{package}."));
        return own_package.then(|| component.to_string());
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
    /// REL3E: the app window's own `finishInputEvent(handled)` answer for the
    /// whole gesture. `Some(false)` proves nothing consumed the event (a tap on
    /// a label with no handler), `Some(true)` proves the window consumed it,
    /// `None` means the guest did not ack every dispatched event: unknown, never
    /// a guess.
    pub handled: Option<bool>,
    /// REL3E: how many `InputMessage`s the host published for this gesture.
    /// Compared with `acks.len()` it says whether `handled` is a complete
    /// answer.
    pub published: usize,
    /// Guest acknowledgments, in delivery order.
    pub acks: Vec<InputAck>,
    /// REL3E: the action reply's `target` block, verbatim: the node the selector
    /// resolved to and the hit-test result at the injected point. `None` when the
    /// backend reports no target.
    pub target: Option<serde_json::Value>,
    /// The settle block the action's reply carried (P12). Includes
    /// `pixels_changed`; this is evidence, not a pass/fail rule.
    pub settle: Option<serde_json::Value>,
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
    /// SE2: how this observation was bound to the tree it carries. `None` when
    /// the backend reports no settle record. A `mode: "content"` record means
    /// the tree was proven content-stable (not pixel-stable): UI/text
    /// assertions may describe it, pixel assertions may not.
    pub settle: Option<serde_json::Value>,
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
    /// SE2: the settle rule the launch's first tree must satisfy (`strict`
    /// default; `content` proves tree content stability, never pixels).
    pub settle: acos_protocol::scenario::StepSettle,
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
    /// Swipe/scroll with a real touch gesture.
    ///
    /// The engine publishes `DOWN`, interpolated `MOVE`s and `UP` on the live
    /// input channel; the scroll container's own physics decides whether it
    /// moves/flings. Coordinates are display pixels (the same space as a tap).
    Swipe {
        /// Finger-down point (display pixels).
        from: (i32, i32),
        /// Finger-up point (display pixels).
        to: (i32, i32),
        /// Gesture duration in milliseconds.
        duration_ms: u64,
        /// A quick gesture (timing only; the app decides the physics).
        fling: bool,
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
            InputAction::Swipe { .. } => "swipe",
            InputAction::Type { .. } => "type",
            InputAction::Key(_) => "key",
            InputAction::Back => "back",
            InputAction::Rotate(_) => "rotate",
        }
    }
}

/// The result of a bounded `scroll_until` search.
///
/// `frame_seq` is the settled traversal the target was found in, so a caller
/// can bind an assertion to the very frame that proves the node was on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollUntilReport {
    /// Swipes it took (1 = the first gesture already revealed the node).
    pub attempts: usize,
    /// The settled traversal's frame sequence the node was found in.
    pub frame_seq: u64,
}

/// The texts (else content descriptions) a settled tree shows, in document
/// order, capped at 40.
///
/// Both `scroll_until` search loops report their last settled observation with
/// this list, so an agent can see *where* the list actually stopped without a
/// second call.
pub fn scroll_until_seen_texts(tree: &UiTree) -> Vec<String> {
    tree.all_nodes()
        .into_iter()
        .filter_map(|node| {
            (!node.text.is_empty())
                .then(|| node.text.clone())
                .or_else(|| (!node.content_desc.is_empty()).then(|| node.content_desc.clone()))
        })
        .take(40)
        .collect()
}

/// REL3H: the structured end of a `scroll_until` search.
///
/// A search whose last gesture moved nothing (the settled tree before and after
/// it is identical) has reached the end of the list: there is no content left to
/// reveal in this direction, so spending the remaining attempts would only make
/// the same gesture again. That is a different fact from "the selector names a
/// node that is never rendered" and from "the gesture is too short", so it has
/// its own `END_OF_LIST` code, the number of gestures it took and the last
/// settled tree's texts - and it is still a non-pass (the requested node was not
/// observed).
pub fn scroll_until_end_of_list(
    selector: &Selector,
    attempts: usize,
    max_attempts: usize,
    frame_seq: u64,
    last_tree_texts: &[String],
    unchanged_gestures: usize,
) -> BackendError {
    BackendError::Protocol {
        code: ErrorCode::EndOfList,
        detail: format!(
            "scroll_until: {attempts} swipe(s) used and the last {unchanged_gestures} consumed \
             gestures in a row left the settled tree unchanged, so {selector:?} was not revealed \
             by scrolling further; the last settled tree (frame {frame_seq}) does not contain it"
        ),
        // REL3H2: a softened hint. REL3H asserted "the list is at its end"; a
        // list that is still loading (or a scroller that re-lays-out without
        // moving) also leaves the tree unchanged for a gesture, so the evidence
        // is stated as what it is - two consumed gestures moved nothing - and the
        // reader is pointed at the last tree before concluding anything.
        hint: Some(
            "Two consumed gestures moved nothing, which is what the end of a list looks like \
             (and also what a list that is still loading looks like for one gesture). Read \
             `details.last_tree_texts` for what the last settled tree showed; if the node is on \
             the screen, scroll the other way (`direction: down`) or fix the selector, and if the \
             list may still be loading, retry. Raising `max_attempts` alone cannot help here."
                .to_string(),
        ),
        details: Some(serde_json::json!({
            "end_of_list": true,
            "gesture_consumed": true,
            "attempts": attempts,
            "max_attempts": max_attempts,
            "unchanged_gestures": unchanged_gestures,
            "last_frame_seq": frame_seq,
            "selector": selector.human(),
            "last_tree_texts": last_tree_texts,
        })),
    }
}

/// REL3H2: was this `scroll_until` gesture **not** consumed by the app?
///
/// The REL3H end-of-list rule inferred "the list ended" from "the settled tree
/// did not change". A swipe the app never consumed - the gesture missed the
/// scroller, or landed on a non-scrollable view - also changes nothing, but for
/// a different reason: nothing received it. Reporting `END_OF_LIST` there tells
/// an agent "there is no more content in this direction" when the truth is "your
/// gesture went nowhere", so the search refuses to infer the end of the list and
/// reports the miss with the delivery evidence instead.
///
/// Two facts prove it, both the app's own: the settle record says
/// `reason: "unhandled_input"` (REL3H: the window acked every event
/// `finishInputEvent(handled) = false`), or the delivery says
/// `handled: Some(false)`. `None` (no record, or an unknown ack) is *not* proof
/// of the opposite; it keeps the existing behaviour.
pub fn scroll_until_unconsumed_reason(
    settle: Option<&serde_json::Value>,
    delivery: Option<&InputDelivery>,
) -> Option<&'static str> {
    if settle
        .and_then(|settle| settle.get("reason"))
        .and_then(serde_json::Value::as_str)
        == Some(acos_protocol::SETTLE_UNHANDLED_INPUT_REASON)
    {
        return Some("unhandled_input");
    }
    match delivery.and_then(|delivery| delivery.handled) {
        Some(false) => Some("not_handled"),
        _ => None,
    }
}

/// REL3H2: the structured miss of a search whose gesture the app never consumed.
///
/// This is `ELEMENT_NOT_FOUND` (the selector was not observed in a settled tree
/// and the search cannot get further by scrolling), with a hint that names the
/// real cause: the gesture missed the scroller. It is never `END_OF_LIST`
/// (nothing was learned about the end of the list) and never a pass.
pub fn scroll_until_gesture_not_consumed(
    selector: &Selector,
    attempts: usize,
    max_attempts: usize,
    frame_seq: u64,
    last_tree_texts: &[String],
    reason: &str,
    settle: Option<serde_json::Value>,
    handled: Option<bool>,
) -> BackendError {
    BackendError::Protocol {
        code: ErrorCode::ElementNotFound,
        detail: format!(
            "scroll_until: the app window did not consume the swipe on attempt {attempts} \
             ({reason}: `finishInputEvent(handled) = {handled:?}`), so the gesture moved nothing \
             and {selector:?} could not be searched for further; the last settled tree (frame \
             {frame_seq}) does not contain it"
        ),
        hint: Some(
            "gesture not consumed (swipe missed the scroller?): the swipe was delivered but \
             nothing handled it - it started outside the scrollable area, the view under the \
             points does not scroll, or the list is already fully visible. Aim the gesture inside \
             the list's bounds (read them from the previous tree), or assert the node directly if \
             it is on screen."
                .to_string(),
        ),
        details: Some(serde_json::json!({
            "end_of_list": false,
            "gesture_consumed": false,
            "reason": reason,
            "handled": handled,
            "settle": settle,
            "attempts": attempts,
            "max_attempts": max_attempts,
            "last_frame_seq": frame_seq,
            "selector": selector.human(),
            "last_tree_texts": last_tree_texts,
        })),
    }
}

/// REL3H2: the refusal for a traversal that reported **no** settle record at all.
///
/// An engine that cannot say how the observation was bound (no `settle` block)
/// has not proven it settled, and a `scroll_until` hit may only come from a
/// settled observation. REL3S treated an absent record as "not a timeout" and
/// let it through; the ART engine's own search now fails closed instead, because
/// ART always reports a record - an absent one means the reply was truncated or
/// came from a guest that did not answer, and the search must not guess.
pub fn scroll_until_no_settle_record(
    selector: &Selector,
    attempts: usize,
    frame_seq: u64,
) -> BackendError {
    BackendError::Protocol {
        code: ErrorCode::Timeout,
        detail: format!(
            "scroll_until: attempt {attempts} returned a tree with no settle record, so nothing \
             proves the traversal settled; {selector:?} was not looked for"
        ),
        hint: Some(
            "A found node must come from a settled observation and this engine reports no settle \
             evidence for its traversals. Report this as an engine bug (the ART engine always \
             reports a `settle` block) rather than trusting the tree."
                .to_string(),
        ),
        details: Some(serde_json::json!({
            "reason": "no_settle_record",
            "attempts": attempts,
            "frame_seq": frame_seq,
            "selector": selector.human(),
        })),
    }
}

/// REL3H2: the movement rule of a `scroll_until` search.
///
/// One unchanged settled tree does not prove the end of a list: a list that is
/// still loading (an async feed appends rows a moment later) is unchanged for a
/// gesture too. Two **consumed** gestures that each left the tree unchanged do -
/// and only then is `END_OF_LIST` honest. The tracker is shared by both search
/// loops (the ART engine's own command and the default `Backend::scroll_until`),
/// so the two front doors cannot disagree about where a list ends.
#[derive(Debug, Clone)]
pub struct ScrollUntilMovement {
    before: UiTree,
    /// Consecutive consumed gestures that left the settled tree identical.
    unchanged: usize,
    /// The count that means "the list ended".
    needed: usize,
}

impl ScrollUntilMovement {
    /// Start from the settled tree **before** the first gesture, so the very
    /// first swipe (which may already be at the end) is measurable.
    #[must_use]
    pub fn new(before: UiTree) -> Self {
        ScrollUntilMovement {
            before,
            unchanged: 0,
            needed: 2,
        }
    }

    /// Record one consumed gesture's settled tree: `true` when this gesture and
    /// the previous `needed - 1` ones all left it unchanged (the list ended).
    pub fn observe(&mut self, tree: &UiTree) -> bool {
        if tree.nodes == self.before.nodes {
            self.unchanged += 1;
        } else {
            self.unchanged = 0;
            self.before = tree.clone();
        }
        self.unchanged >= self.needed
    }

    /// How many consecutive unchanged gestures have been seen.
    #[must_use]
    pub fn unchanged(&self) -> usize {
        self.unchanged
    }
}

/// REL3H: the refusal for a `scroll_until` traversal that did not settle.
///
/// A search may only report `found` from a settled observation, and a backend
/// that says its traversal did not settle (`settled: false`) cannot back that
/// claim: the node may be a mid-animation state the app is about to move. This
/// is a `TIMEOUT` (the harness could not complete the observation), never a
/// miss and never a pass. A backend that reports **no** settle record keeps the
/// REL3S behaviour (an absent record is not proof of the opposite - REL3A's
/// rule); only an explicit `settled: false` refuses.
pub fn scroll_until_not_settled(
    selector: &Selector,
    attempts: usize,
    frame_seq: u64,
    settle: &serde_json::Value,
) -> BackendError {
    let reason = settle
        .get("reason")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("the backend reported an unsettled observation");
    BackendError::Protocol {
        code: ErrorCode::Timeout,
        detail: format!(
            "scroll_until: attempt {attempts} produced no settled observation ({reason}); \
             {selector:?} was not looked for in a frame the app had stopped changing"
        ),
        hint: Some(
            "A found node must come from a settled observation. Raise `settle_timeout_ms` for a \
             slow screen, use `settle: content` on the surrounding assertions for a screen whose \
             pixels animate forever, or scroll with plain `swipe` steps and assert afterwards. \
             Nothing is reported as found."
                .to_string(),
        ),
        details: Some(serde_json::json!({
            "reason": "settle_timeout",
            "attempts": attempts,
            "frame_seq": frame_seq,
            "selector": selector.human(),
            "settle": settle,
        })),
    }
}

/// The structured miss of a `scroll_until` search that used every attempt
/// without finding the node and without reaching the end of the list.
pub fn scroll_until_not_found(
    selector: &Selector,
    attempts: usize,
    max_attempts: usize,
    frame_seq: u64,
    last_tree_texts: &[String],
) -> BackendError {
    BackendError::Protocol {
        code: ErrorCode::ElementNotFound,
        detail: format!(
            "scroll_until: {selector:?} was not visible after {attempts} swipe(s); the last \
             settled tree (frame {frame_seq}) does not contain it"
        ),
        hint: Some(
            "The gesture may be too short to scroll, the selector may name a node that is never \
             rendered, or the list may still have more content. Check the last tree nodes in \
             `details.last_tree_texts` before raising `max_attempts`."
                .to_string(),
        ),
        details: Some(serde_json::json!({
            "end_of_list": false,
            "attempts": attempts,
            "max_attempts": max_attempts,
            "last_frame_seq": frame_seq,
            "selector": selector.human(),
            "last_tree_texts": last_tree_texts,
        })),
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
    /// A structured protocol failure with evidence attached (W46).
    ///
    /// Used when the failure was classified from a session's own record
    /// (`session-exit.json`) or from an `acos-art` reply code: the code, the
    /// retryability and the facts must reach the verdict unchanged instead of
    /// being flattened into a detail string.
    #[error("backend error ({code}): {detail}")]
    Protocol {
        /// Error code.
        code: ErrorCode,
        /// Detail.
        detail: String,
        /// The producer's situation-specific hint, when it has one.
        hint: Option<String>,
        /// Structured facts (art reply, session-exit record, stage, ...).
        details: Option<serde_json::Value>,
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

    /// Build a backend error from a structured protocol failure.
    pub fn from_protocol(error: ProtocolError) -> Self {
        BackendError::Protocol {
            code: error.code,
            detail: error.message,
            hint: Some(error.hint),
            details: error.details,
        }
    }

    /// The protocol error code this maps to.
    pub const fn code(&self) -> ErrorCode {
        match self {
            BackendError::Unsupported { .. } => ErrorCode::UnsupportedBackend,
            BackendError::Crash { .. } => ErrorCode::AppCrashed,
            BackendError::Timeout { .. } => ErrorCode::Timeout,
            BackendError::Failed { code, .. }
            | BackendError::Error { code, .. }
            | BackendError::Protocol { code, .. } => *code,
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
            BackendError::Protocol {
                code,
                detail,
                hint,
                details,
            } => {
                let error = ProtocolError::new(*code, detail.clone());
                let error = match hint {
                    Some(hint) => error.with_hint(hint.clone()),
                    None => error,
                };
                match details {
                    Some(details) => error.with_details(details.clone()),
                    None => error,
                }
            }
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

    /// REL3E: the most recent action's delivery evidence (`handled`, acks and
    /// the resolved target), when this backend records one.
    ///
    /// The executor keeps it even when the step failed: a tap the app never
    /// consumed produces no new frame and times out its settle, and the agent
    /// still has to be told *that nothing consumed it*. `None` means the backend
    /// has no delivery record, and the step says `handled: null`, never `false`.
    fn last_input_delivery(&self) -> Option<InputDelivery> {
        None
    }

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

    /// Re-traverse the live UI and return the tree bound to a newly completed
    /// frame. This is the polling seam for `wait_for`: unlike [`Self::observe`],
    /// which answers from the frame the previous action completed, it asks the
    /// backend to look again. A backend that cannot re-traverse may keep the
    /// default (the current observation), but `wait_for` still honours its
    /// timeout instead of treating the first sample as final.
    fn refresh(&mut self) -> Result<SessionObservation, BackendError> {
        self.observe()
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

    /// The live display size in pixels, when the backend knows it.
    ///
    /// Used to resolve a direction-only gesture (`swipe: {direction: up}`) into
    /// two points. `None` means the backend cannot say, and such a step is a
    /// structured error rather than a guess.
    fn display_size(&self) -> Option<(u32, u32)> {
        None
    }

    /// Swipe repeatedly until `selector` is visible in a **settled** observation.
    ///
    /// Bounded by `max_attempts`; every attempt is one real gesture followed by
    /// a fresh observation (never a cached tree), so a "found" can only come
    /// from a frame the app itself produced after the gesture. The default loops
    /// over [`Self::input`]; an engine with its own search command (ART) overrides
    /// it and reports the same facts.
    ///
    /// REL3H: two gestures that each moved nothing (the settled tree before and
    /// after them is identical) have reached the end of the list, so the search
    /// stops there with a structured `END_OF_LIST` instead of spending the
    /// remaining attempts on gestures that cannot reveal anything. A search that
    /// used every attempt without reaching the end is `ELEMENT_NOT_FOUND`.
    ///
    /// REL3H2 tightens *when* that inference is allowed, because one unchanged
    /// tree is not proof of an end:
    ///
    /// * the gesture must have been **consumed** by the app. A swipe the window
    ///   acknowledged `finishInputEvent(handled) = false` (or a settle with
    ///   `reason: unhandled_input`) moved nothing because it missed the
    ///   scroller, and the search reports `ELEMENT_NOT_FOUND` with "gesture not
    ///   consumed" instead of claiming the list ended;
    /// * the tree must be unchanged across **two** consumed gestures, so a list
    ///   that is still loading (its rows arrive a moment later) is not mistaken
    ///   for an end.
    ///
    /// Both rules live in the shared [`ScrollUntilMovement`] /
    /// [`scroll_until_unconsumed_reason`], so the ART engine's own search applies
    /// exactly the same test.
    fn scroll_until(
        &mut self,
        selector: &Selector,
        from: (f32, f32),
        to: (f32, f32),
        duration_ms: u64,
        fling: bool,
        max_attempts: usize,
    ) -> Result<(ScrollUntilReport, SessionObservation), BackendError> {
        let attempts = max_attempts.max(1);
        // The baseline is the settled tree *before* the first gesture: the very
        // first swipe may already be at the end, and that has to be measurable.
        let mut movement = ScrollUntilMovement::new(self.refresh()?.tree);
        let mut last: Option<(u64, usize, Vec<String>)> = None;
        for attempt in 1..=attempts {
            let observation = self.input(&InputAction::Swipe {
                from: (from.0.round() as i32, from.1.round() as i32),
                to: (to.0.round() as i32, to.1.round() as i32),
                duration_ms,
                fling,
            })?;
            let seen = scroll_until_seen_texts(&observation.tree);
            let frame_seq = observation.frame.frame_seq;
            // REL3H: a `found` claim needs a settled observation. A backend that
            // says this traversal did not settle never backs one; a backend that
            // reports no record at all keeps the REL3S behaviour.
            if let Some(settle) = observation.settle.as_ref() {
                if settle.get("settled").and_then(serde_json::Value::as_bool) == Some(false) {
                    return Err(scroll_until_not_settled(selector, attempt, frame_seq, settle));
                }
            }
            let hit = observation
                .tree
                .find_one(selector)
                .is_some_and(|node| node.visible);
            last = Some((frame_seq, attempt, seen.clone()));
            if hit {
                return Ok((
                    ScrollUntilReport {
                        attempts: attempt,
                        frame_seq,
                    },
                    observation,
                ));
            }
            // REL3H2: a gesture nothing consumed teaches us nothing about the
            // end of the list - report the miss with the delivery evidence.
            if let Some(reason) = scroll_until_unconsumed_reason(
                observation.settle.as_ref(),
                observation.input.as_ref(),
            ) {
                return Err(scroll_until_gesture_not_consumed(
                    selector,
                    attempt,
                    attempts,
                    frame_seq,
                    &seen,
                    reason,
                    observation.settle.clone(),
                    observation.input.as_ref().and_then(|input| input.handled),
                ));
            }
            if movement.observe(&observation.tree) {
                return Err(scroll_until_end_of_list(
                    selector,
                    attempt,
                    attempts,
                    frame_seq,
                    &seen,
                    movement.unchanged(),
                ));
            }
        }
        let (frame_seq, attempt, seen) = last.unwrap_or((0, 0, Vec::new()));
        Err(scroll_until_not_found(
            selector, attempt, attempts, frame_seq, &seen,
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

    /// P12b: the per-step settle deadline (`settle_timeout_ms`). Backends that
    /// bind observations through a settle rule apply it to the next observation;
    /// the default is a no-op (a backend with no settle rule has nothing to
    /// bound). A timeout is still an observation, never a pass for an assertion.
    fn set_settle_timeout(&mut self, _timeout_ms: Option<u64>) {}

    /// The settle rule for the next input action (a scenario's `settle: fast`).
    /// Assertions never use this: they observe the action's own bound frame.
    fn set_input_settle(&mut self, _fast: bool) {}

    /// SE2: request the content-stable rule for the next input action (a
    /// scenario's `settle: content`). A backend without the rule keeps the
    /// default no-op and reports no content settle record, so a step that
    /// asked for it can never be mistaken for one that was bound by it.
    fn set_input_content_settle(&mut self, _content: bool) {}

    /// REL3A: the settle rule the next **observation** (`wait_for`, an
    /// `assert_*`, a `capture`) must bind its tree/frame with.
    ///
    /// The executor calls this before every observation it makes; a backend with
    /// a content-stable rule applies it to the next traversal. A backend without
    /// one keeps the default no-op and reports no content record, so a step that
    /// asked for content fails closed (`content_settle_unhonored`) instead of
    /// running under a different rule than it reports.
    fn set_observation_settle(&mut self, _mode: crate::settle::ObservationSettle) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use acos_protocol::{Bounds, UiNode};

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

    /// The shorthand Android's own tooling accepts: `ComponentName.
    /// unflattenFromString` (`pkg/.Cls`) and `PackageParser.buildClassName`
    /// (`pkg/Cls`, dotless class relative to the component's package). A
    /// component prefix that differs from the manifest package is legal and
    /// must resolve (Gaten: manifest package `com.gaten`, component package
    /// `com.gaten.app`).
    #[test]
    fn resolve_component_expands_android_component_shorthand() {
        // `pkg/.Cls` — ComponentName.unflattenFromString, package part wins.
        assert_eq!(
            resolve_component("com.gaten", "com.gaten.app/.MainActivity").as_deref(),
            Some("com.gaten.app.MainActivity")
        );
        // `pkg/Cls` — dotless class is relative (buildClassName).
        assert_eq!(
            resolve_component("com.gaten", "com.gaten.app/MainActivity").as_deref(),
            Some("com.gaten.app.MainActivity")
        );
        // `pkg/a.b.Cls` — fully qualified class after the slash.
        assert_eq!(
            resolve_component("com.gaten", "com.gaten.app/com.gaten.app.MainActivity").as_deref(),
            Some("com.gaten.app.MainActivity")
        );
        // `.Cls` and bare `Cls` — relative to the manifest package.
        assert_eq!(
            resolve_component("com.gaten", ".MainActivity").as_deref(),
            Some("com.gaten.MainActivity")
        );
        assert_eq!(
            resolve_component("com.gaten", "MainActivity").as_deref(),
            Some("com.gaten.MainActivity")
        );
        // Degenerate forms are not components.
        assert_eq!(resolve_component("com.gaten", "com.gaten.app/"), None);
        assert_eq!(resolve_component("com.gaten", "/.MainActivity"), None);
        assert_eq!(resolve_component("com.gaten", "com.gaten.app/."), None);
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

    // ---- REL3H: a scroll_until search stops at the end of the list ---------

    /// A list that either keeps moving (a row counter changes with every
    /// gesture) or is already at its end (the settled tree never changes).
    ///
    /// REL3H2 adds the two ways that "the tree did not change" is *not* an end:
    /// `handled: Some(false)` / settle `reason: unhandled_input` (the swipe
    /// missed the scroller) and a list that is unchanged for the first gesture
    /// and only then loads its rows (an async feed).
    struct ListBackend {
        frame: u64,
        at_end: bool,
        /// REL3H2: the swipe's own delivery record (`handled`), when the scripted
        /// guest would report one.
        handled: Option<bool>,
        /// REL3H2: the settle record the swipe's observation carries.
        settle: Option<serde_json::Value>,
        /// REL3H2: how many leading gestures leave the tree unchanged before the
        /// rows arrive (`0` = every gesture moves the list, or `at_end`).
        unchanged_gestures: usize,
    }

    impl ListBackend {
        fn observation(&mut self) -> SessionObservation {
            // REL3H2: an async list that loads after `unchanged_gestures`
            // gestures; every gesture before that leaves the tree identical.
            let row = if self.at_end {
                0
            } else if self.frame <= self.unchanged_gestures as u64 {
                0
            } else {
                self.frame
            };
            let mut list = UiNode::new("list", "android.widget.ListView", Bounds::new(0, 0, 1080, 2400))
                .clickable(false);
            list.scrollable = true;
            let first = UiNode::new(
                "row_0",
                "android.widget.TextView",
                Bounds::new(0, 0, 1080, 240),
            )
            .with_text(format!("Row {row}"));
            let last = UiNode::new(
                "row_end",
                "android.widget.TextView",
                Bounds::new(0, 2160, 1080, 2400),
            )
            .with_text("Row 99");
            list.children = vec![first, last];
            // REL3H2: the async feed's rows arrive *after* the first gesture;
            // until then the list is mounted but unchanged.
            if self.unchanged_gestures > 0 && self.frame > self.unchanged_gestures as u64 {
                list.children.push(
                    UiNode::new(
                        "loaded",
                        "android.widget.TextView",
                        Bounds::new(0, 1200, 1080, 1440),
                    )
                    .with_text("Loaded"),
                );
            }
            let delivery = self.handled.map(|handled| InputDelivery {
                delivered: true,
                visual_change: false,
                handled: Some(handled),
                published: 5,
                acks: (0..5)
                    .map(|seq| InputAck {
                        seq,
                        handled,
                        latency_us: Some(1_000),
                    })
                    .collect(),
                target: None,
                settle: self.settle.clone(),
            });
            SessionObservation {
                session_id: "s_rel3h".to_string(),
                package: "dev.acos.fixtures.scrolllist".to_string(),
                frame: FrameMeta::new(
                    self.frame,
                    "2026-11-16T00:00:00Z",
                    "req_rel3h",
                    1080,
                    2400,
                    420,
                    "h",
                    true,
                ),
                tree: UiTree::new(
                    FrameMeta::new(
                        self.frame,
                        "2026-11-16T00:00:00Z",
                        "req_rel3h",
                        1080,
                        2400,
                        420,
                        "h",
                        true,
                    ),
                    "dev.acos.fixtures.scrolllist",
                    vec![list],
                ),
                screenshot_png: None,
                crashed: None,
                input: delivery,
                settle: self.settle.clone(),
            }
        }
    }

    impl Backend for ListBackend {
        fn id(&self) -> &'static str {
            "rel3h-list"
        }

        fn protocol_backend(&self) -> ProtocolBackend {
            ProtocolBackend::AcosNative
        }

        fn fidelity(&self) -> Fidelity {
            Fidelity::ArtReference
        }

        fn capabilities(&self) -> Capabilities {
            Capabilities {
                launch: true,
                ui_tree: true,
                input: true,
                ..Capabilities::default()
            }
        }

        fn manifest(&mut self, apk: &Path) -> Result<ManifestInfo, BackendError> {
            Ok(ManifestInfo {
                path: apk.display().to_string(),
                sha256: "0".repeat(64),
                package: "dev.acos.fixtures.scrolllist".to_string(),
                version_code: 1,
                version_name: "1.0".to_string(),
                min_sdk: 26,
                target_sdk: 34,
                label: None,
                launcher_activity: Some(
                    "dev.acos.fixtures.scrolllist.MainActivity".to_string(),
                ),
                activities: vec!["dev.acos.fixtures.scrolllist.MainActivity".to_string()],
                native_abis: Vec::new(),
                debuggable: true,
            })
        }

        fn refresh(&mut self) -> Result<SessionObservation, BackendError> {
            Ok(self.observation())
        }

        fn input(&mut self, action: &InputAction) -> Result<SessionObservation, BackendError> {
            assert!(
                matches!(action, InputAction::Swipe { .. }),
                "a scroll_until search only publishes swipes"
            );
            self.frame += 1;
            Ok(self.observation())
        }
    }

    fn missing() -> Selector {
        Selector::by_text("Row 9999")
    }

    /// A gesture that moved nothing ends the search with `END_OF_LIST` - not
    /// with a full run of identical gestures and `ELEMENT_NOT_FOUND`.
    ///
    /// REL3H2: one unchanged gesture is not enough (a loading list is unchanged
    /// too), so the end is reported after the second consumed gesture that moved
    /// nothing.
    #[test]
    fn a_swipe_that_moves_nothing_ends_the_search() {
        let mut backend = ListBackend {
            frame: 7,
            at_end: true,
            handled: Some(true),
            settle: None,
            unchanged_gestures: 0,
        };
        let error = backend
            .scroll_until(&missing(), (540.0, 1800.0), (540.0, 600.0), 200, true, 6)
            .expect_err("a node that is not in a list at its end is never found");
        assert_eq!(error.code(), ErrorCode::EndOfList);
        assert_eq!(error.status(), Status::Fail, "END_OF_LIST is never a pass");
        let details = error.to_protocol_error().details.expect("details");
        assert_eq!(details["end_of_list"], serde_json::json!(true));
        assert_eq!(
            details["attempts"],
            serde_json::json!(2),
            "one unchanged gesture is not an end (REL3H2)"
        );
        assert_eq!(details["unchanged_gestures"], serde_json::json!(2));
        assert_eq!(details["gesture_consumed"], serde_json::json!(true));
        assert_eq!(details["max_attempts"], serde_json::json!(6));
        assert_eq!(details["selector"], serde_json::json!("text==\"Row 9999\""));
        let texts = details["last_tree_texts"].as_array().expect("texts");
        assert!(texts.iter().any(|text| text == "Row 99"), "{details}");
    }

    /// REL3H2 case 1: the swipe was delivered and the app window did not consume
    /// it (`handled: false`, the gesture missed the scroller). The tree is
    /// unchanged for the same reason an end would be, but the search must not
    /// claim the list ended: that would tell an agent "there is no more content"
    /// when the truth is "your gesture went nowhere".
    #[test]
    fn an_unconsumed_swipe_is_never_an_end_of_list() {
        let mut backend = ListBackend {
            frame: 3,
            at_end: true,
            handled: Some(false),
            settle: Some(serde_json::json!({
                "mode": "strict",
                "settled": true,
                "reason": "unhandled_input",
                "bound": "unhandled-input",
                "pixels_changed": false,
            })),
            unchanged_gestures: 0,
        };
        let error = backend
            .scroll_until(&missing(), (540.0, 1800.0), (540.0, 600.0), 200, true, 6)
            .expect_err("an unconsumed swipe never finds the node");
        assert_eq!(
            error.code(),
            ErrorCode::ElementNotFound,
            "an unconsumed gesture is a miss, never END_OF_LIST"
        );
        let protocol = error.to_protocol_error();
        let hint = protocol.hint.clone();
        assert!(
            hint.contains("gesture not consumed (swipe missed the scroller?)"),
            "the hint must name the real cause, got {hint:?}"
        );
        let details = protocol.details.expect("details");
        assert_eq!(details["end_of_list"], serde_json::json!(false));
        assert_eq!(details["gesture_consumed"], serde_json::json!(false));
        assert_eq!(details["handled"], serde_json::json!(false));
        assert_eq!(details["reason"], serde_json::json!("unhandled_input"));
        assert_eq!(
            details["attempts"],
            serde_json::json!(1),
            "there is no point repeating a gesture the app never consumed"
        );
        assert_eq!(details["max_attempts"], serde_json::json!(6));
    }

    /// REL3H2 case 1b: `handled: false` without the settle reason refuses just
    /// the same - the delivery record alone proves nothing consumed the gesture.
    #[test]
    fn a_not_handled_delivery_alone_refuses_the_end_of_list() {
        let mut backend = ListBackend {
            frame: 1,
            at_end: true,
            handled: Some(false),
            settle: None,
            unchanged_gestures: 0,
        };
        let error = backend
            .scroll_until(&missing(), (540.0, 1800.0), (540.0, 600.0), 200, true, 6)
            .expect_err("an unconsumed swipe never finds the node");
        assert_eq!(error.code(), ErrorCode::ElementNotFound);
        let details = error.to_protocol_error().details.expect("details");
        assert_eq!(details["reason"], serde_json::json!("not_handled"));
        assert_eq!(details["end_of_list"], serde_json::json!(false));
    }

    /// REL3H2 case 2: an async list that is unchanged for the first gesture and
    /// only then loads its rows. REL3H would have declared the end of the list
    /// after that first unchanged observation; the search must keep going and
    /// find the row that arrives.
    #[test]
    fn a_list_that_loads_after_the_first_unchanged_observation_is_not_an_end() {
        // Frames 1..=2 leave the tree unchanged (the feed has not arrived), then
        // the list moves and finally contains `Row 80`.
        let mut backend = ListBackend {
            frame: 0,
            at_end: false,
            handled: Some(true),
            settle: None,
            unchanged_gestures: 1,
        };
        let (report, observation) = backend
            .scroll_until(
                &Selector::by_text("Loaded"),
                (540.0, 1800.0),
                (540.0, 600.0),
                200,
                true,
                6,
            )
            .expect("the rows arrive after the feed loads");
        assert_eq!(
            report.attempts, 2,
            "the first unchanged gesture must not end the search: {report:?}"
        );
        assert_eq!(report.frame_seq, observation.frame.frame_seq);
        assert!(observation
            .tree
            .find_one(&Selector::by_text("Loaded"))
            .is_some());
    }

    /// REL3H2 case 3: a real end. Two consumed gestures moved nothing, so the
    /// search stops there - in fewer attempts than the budget.
    #[test]
    fn a_real_end_of_list_is_reported_after_two_consumed_gestures() {
        let mut backend = ListBackend {
            frame: 5,
            at_end: true,
            handled: Some(true),
            settle: Some(serde_json::json!({
                "mode": "strict",
                "settled": true,
                "bound": "strict-event",
                "pixels_changed": false,
            })),
            unchanged_gestures: 0,
        };
        let error = backend
            .scroll_until(&missing(), (540.0, 1800.0), (540.0, 600.0), 200, true, 12)
            .expect_err("the node is not in the list");
        assert_eq!(error.code(), ErrorCode::EndOfList);
        let details = error.to_protocol_error().details.expect("details");
        assert_eq!(details["attempts"], serde_json::json!(2));
        assert_eq!(details["unchanged_gestures"], serde_json::json!(2));
    }

    /// A list that keeps moving is not "at its end": all attempts are used and
    /// the miss stays `ELEMENT_NOT_FOUND` (never a false END_OF_LIST).
    #[test]
    fn a_moving_list_uses_every_attempt() {
        let mut backend = ListBackend {
            frame: 0,
            at_end: false,
            handled: Some(true),
            settle: None,
            unchanged_gestures: 0,
        };
        let error = backend
            .scroll_until(&missing(), (540.0, 1800.0), (540.0, 600.0), 200, true, 3)
            .expect_err("the row never exists");
        assert_eq!(error.code(), ErrorCode::ElementNotFound);
        let details = error.to_protocol_error().details.expect("details");
        assert_eq!(details["end_of_list"], serde_json::json!(false));
        assert_eq!(details["attempts"], serde_json::json!(3));
    }

    /// A node that *is* in the first settled observation is found without a
    /// second gesture, and the report names that frame.
    #[test]
    fn a_visible_node_is_found_in_the_first_settled_observation() {
        let mut backend = ListBackend {
            frame: 2,
            at_end: true,
            handled: Some(true),
            settle: None,
            unchanged_gestures: 0,
        };
        let (report, observation) = backend
            .scroll_until(
                &Selector::by_text("Row 99"),
                (540.0, 1800.0),
                (540.0, 600.0),
                200,
                true,
                6,
            )
            .expect("Row 99 is in the list");
        assert_eq!(report.attempts, 1);
        assert_eq!(report.frame_seq, observation.frame.frame_seq);
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
