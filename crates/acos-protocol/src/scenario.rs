//! The ACOS scenario format (YAML).
//!
//! `wait_for` and every assertion are **event-driven**: the runtime waits for a
//! matching frame/tree/log event. The scenario format has no `sleep` and the
//! runtime must not implement `wait_for` as a polling loop with fixed delays.
//!
//! Validation returns *all* problems with a stable JSON path, not just the first
//! one. Parse errors are reported as `INVALID_SCENARIO` at path `$`.

use std::collections::BTreeMap;
use std::path::Path;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::assertion::{
    LogAssertion, NoCrashAssertion, RegionAssertion, ScreenshotAssertion, StateAssertion,
    TextAssertion, VisibilityAssertion,
};
use crate::commands::{CaptureFormat, KeyName, Orientation, ResetScope};
use crate::error::ErrorCode;
use crate::identity::{DisplaySpec, IsolationLevel, ResourceBudget};
use crate::ui::{Bounds, Selector};

/// The only scenario format version this crate accepts.
pub const SCENARIO_VERSION: u32 = 1;

/// Which engine should run the scenario.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Engine {
    /// Use the native runtime when it can, otherwise fall back per policy.
    #[default]
    NativePreferred,
    /// Only the native runtime; never fall back.
    NativeOnly,
    /// Only the reference backend.
    ReferenceOnly,
}

/// What to do when the preferred engine cannot run a step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum FallbackPolicy {
    /// Report the native result and escalate to the reference backend when allowed.
    #[default]
    ReportAndEscalate,
    /// Report the native result only; never silently switch engines.
    ReportOnly,
    /// Forbid fallback; an unsupported capability stays unsupported.
    Forbid,
}

/// What the scenario expects. Used to make negative tests explicit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Expectation {
    /// The scenario must produce `status: pass`.
    #[default]
    Pass,
    /// The scenario must produce `status: fail` (the app ran; a check did not hold).
    Fail,
    /// The scenario must produce any non-pass status (crash/unsupported/... count).
    NonPass,
}

/// A scenario step.
///
/// On the wire a step is a one-key map: `- launch: {activity: .MainActivity}`.
/// The struct has one optional field per step kind; validation enforces that
/// **exactly one** is present. (A Rust enum would map to YAML `!tag` syntax with
/// `serde_yaml_ng`, which is not the agent-facing format.)
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Step {
    /// Launch the app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch: Option<LaunchStep>,
    /// Wait for a selector (event-driven).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_for: Option<WaitForStep>,
    /// Tap a selector or point.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tap: Option<TapStep>,
    /// Type text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#type: Option<TypeStep>,
    /// Press a key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<KeyStep>,
    /// Press Back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub back: Option<BackStep>,
    /// Rotate the display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotate: Option<RotateStep>,
    /// Assert node text/bounds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assert_text: Option<TextAssertion>,
    /// Assert node visibility.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assert_visible: Option<VisibilityAssertion>,
    /// Assert node invisibility.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assert_not_visible: Option<VisibilityAssertion>,
    /// Assert node state flags.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assert_state: Option<StateAssertion>,
    /// Assert a log sequence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assert_log: Option<LogAssertion>,
    /// Assert no crash/ANR.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assert_no_crash: Option<NoCrashAssertion>,
    /// Assert a screenshot against a baseline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assert_screenshot: Option<ScreenshotAssertion>,
    /// Assert a rectangular region of a fresh host-observed frame.
    ///
    /// The region is in **display pixels** (tree bounds, tap points and this
    /// region share one space); the executor converts it to frame pixels with
    /// the frame's `display_origin`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assert_region: Option<RegionAssertion>,
    /// Capture a frame into the artifacts directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture: Option<CaptureStep>,
    /// Reset session/app/data state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset: Option<ResetStep>,
    /// Install deterministic network responses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set_network: Option<NetworkSetup>,
    /// Set a runtime permission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set_permission: Option<PermissionSetup>,
    /// Set the locale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set_locale: Option<LocaleSetup>,
    /// Set display density.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set_density: Option<DensitySetup>,
    /// Set light/dark theme.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set_theme: Option<ThemeSetup>,
    /// Set the runtime clock.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set_time: Option<TimeSetup>,
    /// Invoke an explicitly injected test hook. Optional and only safe for
    /// debuggable/source-owned builds; never for arbitrary third-party APKs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_activity_method: Option<CallActivityMethodStep>,
}

/// A borrowed view of the one populated step kind.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StepRef<'a> {
    /// See [`Step::launch`].
    Launch(&'a LaunchStep),
    /// See [`Step::wait_for`].
    WaitFor(&'a WaitForStep),
    /// See [`Step::tap`].
    Tap(&'a TapStep),
    /// See [`Step::r#type`].
    Type(&'a TypeStep),
    /// See [`Step::key`].
    Key(&'a KeyStep),
    /// See [`Step::back`].
    Back(&'a BackStep),
    /// See [`Step::rotate`].
    Rotate(&'a RotateStep),
    /// See [`Step::assert_text`].
    AssertText(&'a TextAssertion),
    /// See [`Step::assert_visible`].
    AssertVisible(&'a VisibilityAssertion),
    /// See [`Step::assert_not_visible`].
    AssertNotVisible(&'a VisibilityAssertion),
    /// See [`Step::assert_state`].
    AssertState(&'a StateAssertion),
    /// See [`Step::assert_log`].
    AssertLog(&'a LogAssertion),
    /// See [`Step::assert_no_crash`].
    AssertNoCrash(&'a NoCrashAssertion),
    /// See [`Step::assert_screenshot`].
    AssertScreenshot(&'a ScreenshotAssertion),
    /// See [`Step::assert_region`].
    AssertRegion(&'a RegionAssertion),
    /// See [`Step::capture`].
    Capture(&'a CaptureStep),
    /// See [`Step::reset`].
    Reset(&'a ResetStep),
    /// See [`Step::set_network`].
    SetNetwork(&'a NetworkSetup),
    /// See [`Step::set_permission`].
    SetPermission(&'a PermissionSetup),
    /// See [`Step::set_locale`].
    SetLocale(&'a LocaleSetup),
    /// See [`Step::set_density`].
    SetDensity(&'a DensitySetup),
    /// See [`Step::set_theme`].
    SetTheme(&'a ThemeSetup),
    /// See [`Step::set_time`].
    SetTime(&'a TimeSetup),
    /// See [`Step::call_activity_method`].
    CallActivityMethod(&'a CallActivityMethodStep),
}

impl StepRef<'_> {
    /// Stable step-kind string (used in artifact file names).
    pub const fn kind(&self) -> &'static str {
        match self {
            StepRef::Launch(_) => "launch",
            StepRef::WaitFor(_) => "wait_for",
            StepRef::Tap(_) => "tap",
            StepRef::Type(_) => "type",
            StepRef::Key(_) => "key",
            StepRef::Back(_) => "back",
            StepRef::Rotate(_) => "rotate",
            StepRef::AssertText(_) => "assert_text",
            StepRef::AssertVisible(_) => "assert_visible",
            StepRef::AssertNotVisible(_) => "assert_not_visible",
            StepRef::AssertState(_) => "assert_state",
            StepRef::AssertLog(_) => "assert_log",
            StepRef::AssertNoCrash(_) => "assert_no_crash",
            StepRef::AssertScreenshot(_) => "assert_screenshot",
            StepRef::AssertRegion(_) => "assert_region",
            StepRef::Capture(_) => "capture",
            StepRef::Reset(_) => "reset",
            StepRef::SetNetwork(_) => "set_network",
            StepRef::SetPermission(_) => "set_permission",
            StepRef::SetLocale(_) => "set_locale",
            StepRef::SetDensity(_) => "set_density",
            StepRef::SetTheme(_) => "set_theme",
            StepRef::SetTime(_) => "set_time",
            StepRef::CallActivityMethod(_) => "call_activity_method",
        }
    }
}

impl Step {
    /// Number of step keys present (must be 1 for a valid step).
    pub fn variant_count(&self) -> usize {
        [
            self.launch.is_some(),
            self.wait_for.is_some(),
            self.tap.is_some(),
            self.r#type.is_some(),
            self.key.is_some(),
            self.back.is_some(),
            self.rotate.is_some(),
            self.assert_text.is_some(),
            self.assert_visible.is_some(),
            self.assert_not_visible.is_some(),
            self.assert_state.is_some(),
            self.assert_log.is_some(),
            self.assert_no_crash.is_some(),
            self.assert_screenshot.is_some(),
            self.assert_region.is_some(),
            self.capture.is_some(),
            self.reset.is_some(),
            self.set_network.is_some(),
            self.set_permission.is_some(),
            self.set_locale.is_some(),
            self.set_density.is_some(),
            self.set_theme.is_some(),
            self.set_time.is_some(),
            self.call_activity_method.is_some(),
        ]
        .into_iter()
        .filter(|present| *present)
        .count()
    }

    /// The populated step kind, when exactly one is present.
    pub fn as_step(&self) -> Option<StepRef<'_>> {
        if self.launch.is_some() {
            return self.launch.as_ref().map(StepRef::Launch);
        }
        if self.wait_for.is_some() {
            return self.wait_for.as_ref().map(StepRef::WaitFor);
        }
        if self.tap.is_some() {
            return self.tap.as_ref().map(StepRef::Tap);
        }
        if self.r#type.is_some() {
            return self.r#type.as_ref().map(StepRef::Type);
        }
        if self.key.is_some() {
            return self.key.as_ref().map(StepRef::Key);
        }
        if self.back.is_some() {
            return self.back.as_ref().map(StepRef::Back);
        }
        if self.rotate.is_some() {
            return self.rotate.as_ref().map(StepRef::Rotate);
        }
        if self.assert_text.is_some() {
            return self.assert_text.as_ref().map(StepRef::AssertText);
        }
        if self.assert_visible.is_some() {
            return self.assert_visible.as_ref().map(StepRef::AssertVisible);
        }
        if self.assert_not_visible.is_some() {
            return self
                .assert_not_visible
                .as_ref()
                .map(StepRef::AssertNotVisible);
        }
        if self.assert_state.is_some() {
            return self.assert_state.as_ref().map(StepRef::AssertState);
        }
        if self.assert_log.is_some() {
            return self.assert_log.as_ref().map(StepRef::AssertLog);
        }
        if self.assert_no_crash.is_some() {
            return self.assert_no_crash.as_ref().map(StepRef::AssertNoCrash);
        }
        if self.assert_screenshot.is_some() {
            return self
                .assert_screenshot
                .as_ref()
                .map(StepRef::AssertScreenshot);
        }
        if self.assert_region.is_some() {
            return self.assert_region.as_ref().map(StepRef::AssertRegion);
        }
        if self.capture.is_some() {
            return self.capture.as_ref().map(StepRef::Capture);
        }
        if self.reset.is_some() {
            return self.reset.as_ref().map(StepRef::Reset);
        }
        if self.set_network.is_some() {
            return self.set_network.as_ref().map(StepRef::SetNetwork);
        }
        if self.set_permission.is_some() {
            return self.set_permission.as_ref().map(StepRef::SetPermission);
        }
        if self.set_locale.is_some() {
            return self.set_locale.as_ref().map(StepRef::SetLocale);
        }
        if self.set_density.is_some() {
            return self.set_density.as_ref().map(StepRef::SetDensity);
        }
        if self.set_theme.is_some() {
            return self.set_theme.as_ref().map(StepRef::SetTheme);
        }
        if self.set_time.is_some() {
            return self.set_time.as_ref().map(StepRef::SetTime);
        }
        if self.call_activity_method.is_some() {
            return self
                .call_activity_method
                .as_ref()
                .map(StepRef::CallActivityMethod);
        }
        None
    }

    /// Stable step-kind string (used in artifact file names).
    pub fn kind(&self) -> &'static str {
        self.as_step().map(|step| step.kind()).unwrap_or("empty")
    }

    /// Convenience: a `back` step.
    pub fn back() -> Self {
        Step {
            back: Some(BackStep::default()),
            ..Step::default()
        }
    }

    /// Convenience: a `launch` step.
    pub fn launch(step: LaunchStep) -> Self {
        Step {
            launch: Some(step),
            ..Step::default()
        }
    }
}

/// The `back` payload (`- back: {}`).
///
/// It has no target: Back goes to the focused window. SE1's opt-in
/// `expect_change` is the only field, so `- back: {}` keeps parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct BackStep {
    /// SE1: fail the step when the settled frame's pixels are unchanged.
    #[serde(default, skip_serializing_if = "is_false")]
    pub expect_change: bool,
}

/// `launch`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct LaunchStep {
    /// Explicit component (`pkg/.Activity` or `.Activity`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<String>,
    /// Intent action.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent_action: Option<String>,
    /// Intent data URI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    /// String extras.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extras: BTreeMap<String, String>,
    /// Clear app data before launching.
    #[serde(default)]
    pub clear_state: bool,
    /// P12b: strict/fast settle deadline for the first tree (milliseconds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settle_timeout_ms: Option<u64>,
    /// SE2: settle rule for the launch's first tree (`strict` default,
    /// `content` for a screen whose tree is stable while its pixels animate).
    /// A launch's frame is never pixel evidence under `content`, so a later
    /// pixel assertion bound to it fails closed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settle: Option<StepSettle>,
}

/// `wait_for`.
///
/// REL3A: `wait_for` accepts a settle rule like every other observation step.
/// Its default (no `settle`) is *auto*: strict first, then the content-stable
/// rule when strict never settles but the tree is stable (the always-animating
/// RN/Gaten screen). A `wait_for` that passed on a content-settled tree says so
/// in its `settle` record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WaitForStep {
    /// Selector to wait for.
    pub selector: Selector,
    /// Event-driven timeout.
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    /// Require the matched node to be visible.
    #[serde(default = "default_true")]
    pub require_visible: bool,
    /// Settle rule for the observation this wait binds (REL3A). Absent = auto
    /// (strict, then content); `content` = content-stable only; `strict` =
    /// event settle only. `fast` is rejected: it is navigation-only and can
    /// never back a wait or an assertion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settle: Option<StepSettle>,
    /// P12b: settle deadline for each observation attempt (milliseconds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settle_timeout_ms: Option<u64>,
}

/// The settle rule an input step asks for. `strict` is the default and
/// evidence for every assertion; `fast` is the navigation rule for a screen
/// that never settles (a JS auto-advance timer, an endless animation) and
/// never backs a pass; `content` (SE2, ADR-0026 decision 3) is the
/// content-stable rule for a screen that animates forever in pixels: it is
/// settled when the UI tree content signature is unchanged for a bounded
/// window. Content settle backs UI/text assertions only; a pixel/frame
/// assertion bound to it fails closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum StepSettle {
    /// Strict event settle (default).
    #[default]
    Strict,
    /// Fast navigation settle (never assertion evidence).
    Fast,
    /// Content-stable settle: the UI tree content signature is unchanged for
    /// the bounded window even if pixels keep animating (SE2). Backs UI/text
    /// assertions; never a pixel/frame assertion.
    Content,
}

/// `tap`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TapStep {
    /// Target selector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<Selector>,
    /// Explicit x.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<i32>,
    /// Explicit y.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<i32>,
    /// Event-driven timeout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// P12b: strict/fast settle deadline for this tap (milliseconds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settle_timeout_ms: Option<u64>,
    /// Settle rule for this input (`strict` default, `fast` navigation).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settle: Option<StepSettle>,
    /// SE1: fail the step when the settled frame's pixels are unchanged.
    ///
    /// Opt-in and strict-only: `expect_change: true` with `settle: fast` is
    /// rejected at scenario-validation time, because a fast frame may be
    /// mid-animation and can never back a claim.
    #[serde(default, skip_serializing_if = "is_false")]
    pub expect_change: bool,
}

/// `type`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TypeStep {
    /// Target selector (absent = current focus).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<Selector>,
    /// Text to enter.
    pub text: String,
    /// Clear the field first.
    #[serde(default)]
    pub clear: bool,
    /// P12b: strict/fast settle deadline for this type action (milliseconds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settle_timeout_ms: Option<u64>,
    /// Settle rule for this input (`strict` default, `fast` navigation).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settle: Option<StepSettle>,
    /// SE1: fail the step when the settled frame's pixels are unchanged.
    #[serde(default, skip_serializing_if = "is_false")]
    pub expect_change: bool,
}

/// `key`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct KeyStep {
    /// Symbolic key.
    pub key: KeyName,
    /// P12b: strict/fast settle deadline for this key action (milliseconds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settle_timeout_ms: Option<u64>,
    /// Settle rule for this input (`strict` default, `fast` navigation).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settle: Option<StepSettle>,
    /// SE1: fail the step when the settled frame's pixels are unchanged.
    #[serde(default, skip_serializing_if = "is_false")]
    pub expect_change: bool,
}

/// `rotate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RotateStep {
    /// Requested orientation.
    pub orientation: Orientation,
}

/// `capture`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CaptureStep {
    /// Artifact name (`frames/<seq>.png` is always written; this names the step).
    pub name: String,
    /// Encoding.
    #[serde(default)]
    pub format: CaptureFormat,
    /// Require a fresh frame.
    #[serde(default = "default_true")]
    pub fresh: bool,
}

/// `reset`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ResetStep {
    /// Reset scope.
    pub scope: ResetScope,
    /// P12b: strict/fast settle deadline for the relaunched tree (milliseconds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settle_timeout_ms: Option<u64>,
}

/// One mocked HTTP response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NetworkRule {
    /// HTTP method (defaults to any).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    /// URL or URL pattern.
    pub url: String,
    /// Response status.
    pub status: u16,
    /// Response headers.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    /// Response body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Artificial delay before responding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delay_ms: Option<u64>,
    /// Match at most this many times (absent = unlimited).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub times: Option<u32>,
}

/// `set_network`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NetworkSetup {
    /// Force offline/online state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offline: Option<bool>,
    /// Mocked responses.
    #[serde(default)]
    pub rules: Vec<NetworkRule>,
}

/// Permission state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum PermissionState {
    /// Granted.
    Grant,
    /// Denied.
    Deny,
    /// Revoked.
    Revoke,
    /// Reset to the manifest/default state.
    Reset,
}

/// `set_permission`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PermissionSetup {
    /// Fully qualified permission name.
    pub permission: String,
    /// State to apply.
    pub state: PermissionState,
}

/// `set_locale`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LocaleSetup {
    /// BCP-47-ish locale (`en-US`, `pt_BR`).
    pub locale: String,
}

/// `set_density`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DensitySetup {
    /// Density in dpi.
    pub density_dpi: u32,
}

/// Theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    /// Light.
    Light,
    /// Dark.
    Dark,
    /// Follow the system.
    System,
}

/// `set_theme`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ThemeSetup {
    /// Theme to apply.
    pub theme: Theme,
}

/// `set_time`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TimeSetup {
    /// RFC 3339 UTC timestamp.
    pub timestamp: String,
    /// Optional IANA timezone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
}

/// `call_activity_method` (optional; debug/source-owned builds only).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CallActivityMethodStep {
    /// Activity component (defaults to the launched activity).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<String>,
    /// Method name.
    pub method: String,
    /// JSON arguments.
    #[serde(default)]
    pub args: Vec<Value>,
}

/// Artifact options.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OutputOptions {
    /// Write the event trace (`events.jsonl`).
    #[serde(default = "default_true")]
    pub traces: bool,
    /// Write UI trees (`tree/<seq>.json`).
    #[serde(default = "default_true")]
    pub ui_tree: bool,
    /// Write screenshots (`frames/<seq>.png`).
    #[serde(default = "default_true")]
    pub screenshots: bool,
    /// Write `resources.json`.
    #[serde(default = "default_true")]
    pub resource_metrics: bool,
    /// Write logs (`logs/`).
    #[serde(default = "default_true")]
    pub logs: bool,
}

impl Default for OutputOptions {
    fn default() -> Self {
        OutputOptions {
            traces: true,
            ui_tree: true,
            screenshots: true,
            resource_metrics: true,
            logs: true,
        }
    }
}

const fn default_true() -> bool {
    true
}

/// `skip_serializing_if` for the opt-in boolean flags (false is the default).
const fn is_false(value: &bool) -> bool {
    !*value
}

const fn default_timeout_ms() -> u64 {
    5_000
}

/// A negative scenario's pinned failure (see [`Scenario::expect_error`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExpectedError {
    /// Error code the run must report (`ASSERTION_FAILED`, `TIMEOUT`, ...).
    pub code: String,
    /// Optional `details.reason` the failing step's error must carry (for
    /// example `content_settle_pixel` or `content-not-stable`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// A complete scenario.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Scenario {
    /// Format version; must be [`SCENARIO_VERSION`].
    pub version: u32,
    /// Human/agent-readable name (also used in the verdict).
    pub name: String,
    /// Optional description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// APK path (workspace-relative or absolute).
    pub app: String,
    /// Engine preference.
    #[serde(default)]
    pub engine: Engine,
    /// Fallback policy.
    #[serde(default)]
    pub fallback: FallbackPolicy,
    /// Android API level required, when the scenario pins one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_level: Option<u32>,
    /// Isolation level.
    pub isolation: IsolationLevel,
    /// Display spec.
    pub display: DisplaySpec,
    /// Capabilities the scenario needs.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Resource budget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budgets: Option<ResourceBudget>,
    /// Expected outcome.
    #[serde(default)]
    pub expectation: Expectation,
    /// Ordered steps.
    pub steps: Vec<Step>,
    /// Artifact options.
    #[serde(default)]
    pub output: OutputOptions,
    /// P12: per-session framework animation scales (`on`/`off`, default `on`).
    ///
    /// `off` sets Android's three animation scales to 0 for functional checks;
    /// it is never for animation/transition testing. Launch-time only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub animations: Option<String>,
    /// SE2b: the exact failure a **negative** scenario must produce.
    ///
    /// `expectation: fail` only says "the run did not pass"; it accepts ANY
    /// failing step, so a negative control can go red (or green) for the wrong
    /// reason and still look like proof. `expect_error` pins the error code and,
    /// optionally, the `details.reason` of the failure. A run that fails with
    /// another code/reason is a harness error, never a successful negative.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_error: Option<ExpectedError>,
    /// W7b: per-session app-window inset model (`carve`/`report`).
    ///
    /// Selects the model for this scenario exactly as `ACOS_WM_INSETS` does,
    /// and takes precedence over the environment. A scenario that pins the
    /// model is self-contained: it cannot silently change meaning when the
    /// build default or the caller's environment changes. Launch-time only;
    /// the Rust VM has no window insets and ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub insets: Option<String>,
}

impl Scenario {
    /// Parse YAML and validate it.
    pub fn from_yaml_str(yaml: &str) -> Result<Scenario, ScenarioValidation> {
        let scenario: Scenario = serde_yaml_ng::from_str(yaml).map_err(|e| {
            ScenarioValidation::from_error(ScenarioError::invalid(
                "$",
                format!("YAML/format parse failed: {e}"),
            ))
        })?;
        let validation = scenario.validate();
        if validation.is_ok() {
            Ok(scenario)
        } else {
            Err(validation)
        }
    }

    /// Parse YAML without semantic validation (parse errors still fail).
    pub fn from_yaml_str_unvalidated(yaml: &str) -> Result<Scenario, ScenarioValidation> {
        serde_yaml_ng::from_str(yaml).map_err(|e| {
            ScenarioValidation::from_error(ScenarioError::invalid(
                "$",
                format!("YAML/format parse failed: {e}"),
            ))
        })
    }

    /// Load, parse and validate a scenario file.
    pub fn load(path: &Path) -> Result<Scenario, ScenarioValidation> {
        let text = std::fs::read_to_string(path).map_err(|e| {
            ScenarioValidation::from_error(ScenarioError::invalid(
                "$",
                format!("cannot read scenario {}: {e}", path.display()),
            ))
        })?;
        Scenario::from_yaml_str(&text)
    }

    /// Load, parse and validate without stopping at the first semantic error.
    pub fn load_unvalidated(path: &Path) -> Result<Scenario, ScenarioValidation> {
        let text = std::fs::read_to_string(path).map_err(|e| {
            ScenarioValidation::from_error(ScenarioError::invalid(
                "$",
                format!("cannot read scenario {}: {e}", path.display()),
            ))
        })?;
        Scenario::from_yaml_str_unvalidated(&text)
    }

    /// Validate every field and every step, returning all problems.
    pub fn validate(&self) -> ScenarioValidation {
        validate_scenario(self)
    }

    /// Serialize back to YAML (stable key order is not guaranteed by serde).
    pub fn to_yaml_string(&self) -> Result<String, serde_yaml_ng::Error> {
        serde_yaml_ng::to_string(self)
    }
}

/// One validation error with a stable path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ScenarioError {
    /// Always `INVALID_SCENARIO` today; kept for forward-compatible tooling.
    pub code: ErrorCode,
    /// JSON-ish path (`steps[3].assert_log.patterns[1]`).
    pub path: String,
    /// What is wrong.
    pub message: String,
    /// What to do next.
    pub hint: String,
}

impl ScenarioError {
    /// Build an `INVALID_SCENARIO` error.
    pub fn invalid(path: impl Into<String>, message: impl Into<String>) -> Self {
        ScenarioError {
            code: ErrorCode::InvalidScenario,
            path: path.into(),
            message: message.into(),
            hint:
                "Fix the scenario field at `path`; do not weaken runtime validation to accept it."
                    .to_string(),
        }
    }
}

/// A non-fatal scenario issue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ScenarioWarning {
    /// Path of the surprising field.
    pub path: String,
    /// What the agent should know.
    pub message: String,
}

/// Result of validating a scenario.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct ScenarioValidation {
    /// Fatal problems. Any error makes the scenario invalid.
    pub errors: Vec<ScenarioError>,
    /// Non-fatal issues.
    pub warnings: Vec<ScenarioWarning>,
}

impl ScenarioValidation {
    /// Empty validation.
    pub fn ok() -> Self {
        ScenarioValidation::default()
    }

    /// Build from one error.
    pub fn from_error(error: ScenarioError) -> Self {
        ScenarioValidation {
            errors: vec![error],
            warnings: Vec::new(),
        }
    }

    /// True when there are no errors.
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }

    fn push(&mut self, path: impl Into<String>, message: impl Into<String>) {
        self.errors.push(ScenarioError::invalid(path, message));
    }

    fn warn(&mut self, path: impl Into<String>, message: impl Into<String>) {
        self.warnings.push(ScenarioWarning {
            path: path.into(),
            message: message.into(),
        });
    }
}

fn validate_selector(
    validation: &mut ScenarioValidation,
    path: &str,
    selector: &Selector,
    prefix: &str,
) {
    for problem in selector.validation_errors() {
        validation.push(format!("{path}.{prefix}"), problem);
    }
}

fn validate_bounds(validation: &mut ScenarioValidation, path: &str, bounds: Bounds) {
    if !bounds.is_valid() {
        validation.push(
            path,
            format!("bounds must satisfy left<=right and top<=bottom: {bounds}"),
        );
    }
}

fn validate_timeout(validation: &mut ScenarioValidation, path: &str, timeout: Option<u64>) {
    if timeout == Some(0) {
        validation.push(path, "timeout_ms must be > 0");
    }
}

/// REL3A: `settle` on a wait or an assertion is `strict`, `content` or absent.
///
/// `fast` is a navigation rule and is never assertion/wait evidence (ADR-0026
/// decision 5), so it is rejected at parse time with a stable path instead of
/// producing a frame an assertion may not describe. Absent means *auto*: strict
/// first, then the content-stable rule when strict never settles.
fn validate_observation_settle(
    validation: &mut ScenarioValidation,
    path: &str,
    settle: Option<StepSettle>,
) {
    if matches!(settle, Some(StepSettle::Fast)) {
        validation.push(
            path,
            "`settle: fast` is navigation-only and can never back a wait or an assertion: use \
             `settle: strict` (event settle), `settle: content` (content-stable, for a screen that \
             animates forever in pixels), or omit it (auto: strict, then content when strict never \
             settles)",
        );
    }
}

fn validate_regex(validation: &mut ScenarioValidation, path: &str, pattern: &str) {
    if let Err(e) = regex::Regex::new(pattern) {
        validation.push(path, format!("invalid regex: {e}"));
    }
}

/// SE1: `expect_change` is strict-only.
///
/// A `settle: fast` frame may be mid-animation and is never verification
/// evidence (ADR-0026 decision 5), so `expect_change: true` on a fast input
/// would be an unverifiable pass. Reject it at parse time with a stable path
/// instead of discovering it after the action ran.
fn validate_expect_change(
    validation: &mut ScenarioValidation,
    path: &str,
    expect_change: bool,
    settle: Option<StepSettle>,
) {
    // SE2: `expect_change` is a pixel claim. A content-stable frame is never
    // pixel evidence (the pixels may still be animating), so the combination
    // is rejected at parse time exactly like fast.
    if expect_change && matches!(settle, Some(StepSettle::Fast | StepSettle::Content)) {
        let mode = if matches!(settle, Some(StepSettle::Content)) {
            "content"
        } else {
            "fast"
        };
        validation.push(
            format!("{path}.expect_change"),
            format!(
                "`expect_change: true` cannot be combined with `settle: {mode}`: that rule is \
                 not pixel evidence (the frame may still be animating), so the claim could never \
                 be verified. Drop `settle` (strict is the default) or use `settle: strict`; an \
                 unverifiable pass is never accepted."
            ),
        );
    }
}

fn validate_action_like(
    validation: &mut ScenarioValidation,
    path: &str,
    step: &TapStep,
    step_name: &str,
) {
    if step.selector.is_none() && (step.x.is_none() || step.y.is_none()) {
        validation.push(
            format!("{path}.{step_name}"),
            "needs `selector` or both `x` and `y`",
        );
    }
    if step.selector.is_some() && (step.x.is_some() || step.y.is_some()) {
        validation.push(
            format!("{path}.{step_name}"),
            "accepts `selector` or `x`/`y`, not both",
        );
    }
    if let Some(selector) = &step.selector {
        validate_selector(validation, path, selector, &format!("{step_name}.selector"));
    }
    validate_timeout(
        validation,
        &format!("{path}.{step_name}.timeout_ms"),
        step.timeout_ms,
    );
}

/// Validate a scenario and return every problem.
pub fn validate_scenario(scenario: &Scenario) -> ScenarioValidation {
    let mut v = ScenarioValidation::ok();

    if scenario.version != SCENARIO_VERSION {
        v.push(
            "$.version",
            format!(
                "unsupported scenario version {} (expected {})",
                scenario.version, SCENARIO_VERSION
            ),
        );
    }
    if scenario.name.trim().is_empty() {
        v.push("$.name", "name must not be empty");
    }
    if scenario.app.trim().is_empty() {
        v.push("$.app", "app must name an APK path");
    }
    if let Some(api) = scenario.api_level {
        if !(1..=100).contains(&api) {
            v.push("$.api_level", "api_level must be in 1..=100");
        }
    }
    if let Some(animations) = &scenario.animations {
        if !animations.eq_ignore_ascii_case("on") && !animations.eq_ignore_ascii_case("off") {
            v.push("$.animations", "animations must be `on` or `off`");
        }
    }
    if let Some(insets) = &scenario.insets {
        if !insets.eq_ignore_ascii_case("carve") && !insets.eq_ignore_ascii_case("report") {
            v.push("$.insets", "insets must be `carve` or `report`");
        }
    }
    match scenario.display {
        DisplaySpec::None => {
            v.warn(
                "$.display",
                "headless scenario: capture/observe/UI assertions will return NO_DISPLAY",
            );
        }
        DisplaySpec::Offscreen {
            width,
            height,
            density_dpi,
        } => {
            if width == 0 || height == 0 {
                v.push("$.display", "offscreen width and height must be > 0");
            }
            if !(72..=1000).contains(&density_dpi) {
                v.push("$.display.density_dpi", "density must be in 72..=1000");
            }
        }
    }
    if let Some(budget) = scenario.budgets {
        if budget.memory_mib == Some(0) {
            v.push("$.budgets.memory_mib", "memory_mib must be > 0");
        }
        if budget.wall_ms == Some(0) {
            v.push("$.budgets.wall_ms", "wall_ms must be > 0");
        }
        if budget.cpu_ms == Some(0) {
            v.push("$.budgets.cpu_ms", "cpu_ms must be > 0");
        }
    }
    if scenario.steps.is_empty() {
        v.push("$.steps", "scenario must contain at least one step");
    }
    // SE2b: a pinned failure only makes sense for a negative scenario. A
    // `pass` scenario that pins an error code would be self-contradictory, and
    // an empty code could never be satisfied - both are refused here rather than
    // silently ignored at run time.
    if let Some(expected) = &scenario.expect_error {
        if expected.code.trim().is_empty() {
            v.push("$.expect_error.code", "expect_error.code must not be empty");
        }
        if expected.reason.as_deref().is_some_and(|r| r.trim().is_empty()) {
            v.push("$.expect_error.reason", "expect_error.reason must not be empty");
        }
        if scenario.expectation == Expectation::Pass {
            v.push(
                "$.expect_error",
                "expect_error pins a failure; the scenario must declare `expectation: fail` \
                 (or non_pass)",
            );
        }
    }

    let mut capture_names: BTreeMap<&str, usize> = BTreeMap::new();
    for (index, step) in scenario.steps.iter().enumerate() {
        let count = step.variant_count();
        if count == 0 {
            v.push(
                format!("steps[{index}]"),
                "step must contain exactly one step key (launch/wait_for/tap/...)",
            );
            continue;
        }
        if count > 1 {
            v.push(
                format!("steps[{index}]"),
                format!("step contains {count} step keys; exactly one is allowed"),
            );
            continue;
        }
        let path = format!("steps[{index}].{}", step.kind());
        match step.as_step().expect("variant_count == 1") {
            StepRef::Launch(launch) => {
                if launch.activity.as_deref().unwrap_or("").is_empty()
                    && launch.intent_action.as_deref().unwrap_or("").is_empty()
                {
                    v.push(
                        &path,
                        "launch needs `activity` or `intent_action` (both may be omitted only if the APK has a single launcher activity)",
                    );
                }
                if launch.activity.is_some() && launch.intent_action.is_some() {
                    v.push(
                        &path,
                        "launch accepts `activity` or `intent_action`, not both",
                    );
                }
            }
            StepRef::WaitFor(wait) => {
                validate_selector(&mut v, &path, &wait.selector, "selector");
                if wait.timeout_ms == 0 {
                    v.push(format!("{path}.timeout_ms"), "timeout_ms must be > 0");
                }
                validate_observation_settle(&mut v, &format!("{path}.settle"), wait.settle);
                validate_timeout(
                    &mut v,
                    &format!("{path}.settle_timeout_ms"),
                    wait.settle_timeout_ms,
                );
            }
            StepRef::Tap(tap) => {
                validate_action_like(&mut v, &format!("steps[{index}]"), tap, "tap");
                validate_expect_change(&mut v, &path, tap.expect_change, tap.settle);
            }
            StepRef::Type(ty) => {
                if ty.selector.is_none() {
                    v.warn(
                        &path,
                        "no selector: text goes to whatever currently has focus; this is fragile",
                    );
                }
                if let Some(selector) = &ty.selector {
                    validate_selector(&mut v, &path, selector, "selector");
                }
                if ty.text.is_empty() && !ty.clear {
                    v.push(&path, "type needs non-empty `text` (or `clear: true`)");
                }
                validate_expect_change(&mut v, &path, ty.expect_change, ty.settle);
            }
            StepRef::Key(key) => validate_expect_change(&mut v, &path, key.expect_change, key.settle),
            StepRef::Back(_) | StepRef::Rotate(_) => {}
            StepRef::AssertText(assertion) => {
                validate_selector(&mut v, &path, &assertion.selector, "selector");
                if let Some(problem) = assertion.text.validate() {
                    v.push(format!("{path}.text"), problem);
                }
                if let Some(bounds) = assertion.bounds {
                    validate_bounds(&mut v, &format!("{path}.bounds"), bounds);
                }
                validate_timeout(&mut v, &format!("{path}.timeout_ms"), assertion.timeout_ms);
                validate_observation_settle(
                    &mut v,
                    &format!("{path}.settle"),
                    assertion.settle,
                );
            }
            StepRef::AssertVisible(assertion) | StepRef::AssertNotVisible(assertion) => {
                validate_selector(&mut v, &path, &assertion.selector, "selector");
                if let Some(bounds) = assertion.bounds {
                    validate_bounds(&mut v, &format!("{path}.bounds"), bounds);
                }
                validate_timeout(&mut v, &format!("{path}.timeout_ms"), assertion.timeout_ms);
                validate_observation_settle(
                    &mut v,
                    &format!("{path}.settle"),
                    assertion.settle,
                );
            }
            StepRef::AssertState(assertion) => {
                validate_selector(&mut v, &path, &assertion.selector, "selector");
                if assertion.visible.is_none()
                    && assertion.enabled.is_none()
                    && assertion.focused.is_none()
                    && assertion.clickable.is_none()
                    && assertion.scrollable.is_none()
                    && assertion.checked.is_none()
                {
                    v.push(
                        &path,
                        "assert_state needs at least one expected flag (visible/enabled/focused/clickable/scrollable/checked)",
                    );
                }
                validate_timeout(&mut v, &format!("{path}.timeout_ms"), assertion.timeout_ms);
                validate_observation_settle(
                    &mut v,
                    &format!("{path}.settle"),
                    assertion.settle,
                );
            }
            StepRef::AssertLog(assertion) => {
                if assertion.patterns.is_empty() {
                    v.push(
                        format!("{path}.patterns"),
                        "assert_log needs at least one pattern",
                    );
                }
                if let Some(tag) = &assertion.tag {
                    if tag.is_empty() {
                        v.push(format!("{path}.tag"), "tag must not be empty when present");
                    }
                }
                for (i, pattern) in assertion.patterns.iter().enumerate() {
                    if pattern.is_empty() {
                        v.push(format!("{path}.patterns[{i}]"), "pattern must not be empty");
                    } else {
                        validate_regex(&mut v, &format!("{path}.patterns[{i}]"), pattern);
                    }
                }
                if assertion.min_matches == 0 {
                    v.push(format!("{path}.min_matches"), "min_matches must be >= 1");
                }
                validate_timeout(&mut v, &format!("{path}.timeout_ms"), assertion.timeout_ms);
            }
            StepRef::AssertNoCrash(_) => {}
            StepRef::AssertScreenshot(assertion) => {
                if assertion.baseline.trim().is_empty() {
                    v.push(
                        format!("{path}.baseline"),
                        "baseline path must not be empty",
                    );
                }
                if !assertion.tolerance.is_finite() || !(0.0..=1.0).contains(&assertion.tolerance) {
                    v.push(
                        format!("{path}.tolerance"),
                        "tolerance must be a finite fraction in 0.0..=1.0",
                    );
                }
                for (i, mask) in assertion.region_masks.iter().enumerate() {
                    validate_bounds(
                        &mut v,
                        &format!("{path}.region_masks[{i}].bounds"),
                        mask.bounds,
                    );
                }
            }
            StepRef::AssertRegion(assertion) => {
                if assertion.w == 0 || assertion.h == 0 {
                    v.push(&path, "assert_region needs non-zero w and h");
                }
                if assertion.x < 0 || assertion.y < 0 {
                    v.push(&path, "assert_region x/y must be >= 0");
                }
                // x/y are display pixels: the region must fit the display the
                // scenario declares. A region outside it can never be checked and
                // is a scenario bug, not an app bug.
                if let DisplaySpec::Offscreen { width, height, .. } = scenario.display {
                    if assertion.x.saturating_add(assertion.w as i32) > width as i32
                        || assertion.y.saturating_add(assertion.h as i32) > height as i32
                    {
                        v.push(
                            &path,
                            format!(
                                "assert_region is in display pixels (origin [0,0], display {width}x{height}): \
                                 region [{},{},{},{}] does not fit the display",
                                assertion.x, assertion.y, assertion.w, assertion.h
                            ),
                        );
                    }
                }
                let has_criterion = assertion.equals_png.is_some()
                    || assertion.hash.is_some()
                    || assertion.min_distinct_colors.is_some()
                    || assertion.max_distinct_colors.is_some()
                    || assertion.text_ocr.is_some();
                if !has_criterion {
                    v.push(
                        &path,
                        "assert_region needs at least one criterion (equals_png/hash/min_distinct_colors/max_distinct_colors/text_ocr)",
                    );
                }
                if let Some(hash) = &assertion.hash {
                    if hash.len() != 64 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
                        v.push(
                            format!("{path}.hash"),
                            "hash must be 64 hex characters (SHA-256)",
                        );
                    }
                }
                if let Some(png) = &assertion.equals_png {
                    if png.trim().is_empty() {
                        v.push(
                            format!("{path}.equals_png"),
                            "equals_png path must not be empty",
                        );
                    }
                }
                if let (Some(min), Some(max)) =
                    (assertion.min_distinct_colors, assertion.max_distinct_colors)
                {
                    if min > max {
                        v.push(
                            &path,
                            "min_distinct_colors must not exceed max_distinct_colors",
                        );
                    }
                }
            }
            StepRef::Capture(capture) => {
                if capture.name.trim().is_empty() {
                    v.push(format!("{path}.name"), "capture name must not be empty");
                }
                if let Some(previous) = capture_names.insert(capture.name.as_str(), index) {
                    v.warn(
                        format!("{path}.name"),
                        format!(
                            "capture name {:?} is also used by step {previous}; artifacts stay unique by frame seq",
                            capture.name
                        ),
                    );
                }
            }
            StepRef::Reset(_) => {}
            StepRef::SetNetwork(network) => {
                if network.offline.is_none() && network.rules.is_empty() {
                    v.push(&path, "set_network needs `offline` or at least one rule");
                }
                for (i, rule) in network.rules.iter().enumerate() {
                    let rule_path = format!("{path}.rules[{i}]");
                    if rule.url.trim().is_empty() {
                        v.push(format!("{rule_path}.url"), "url must not be empty");
                    }
                    if !(100..=599).contains(&rule.status) {
                        v.push(
                            format!("{rule_path}.status"),
                            "status must be a valid HTTP status in 100..=599",
                        );
                    }
                    if let Some(method) = &rule.method {
                        if method.trim().is_empty() {
                            v.push(format!("{rule_path}.method"), "method must not be empty");
                        }
                    }
                    if rule.times == Some(0) {
                        v.push(format!("{rule_path}.times"), "times must be >= 1");
                    }
                }
            }
            StepRef::SetPermission(permission) => {
                if permission.permission.trim().is_empty() {
                    v.push(format!("{path}.permission"), "permission must not be empty");
                } else if !permission.permission.contains('.') {
                    v.push(
                        format!("{path}.permission"),
                        "permission must be a fully qualified name (e.g. android.permission.CAMERA)",
                    );
                }
            }
            StepRef::SetLocale(locale) => {
                let value = &locale.locale;
                let valid = value.len() >= 2
                    && value.len() <= 20
                    && (value.contains('-') || value.contains('_'))
                    && value
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
                if !valid {
                    v.push(
                        format!("{path}.locale"),
                        "locale must look like `en-US` or `pt_BR`",
                    );
                }
            }
            StepRef::SetDensity(density) => {
                if !(72..=1000).contains(&density.density_dpi) {
                    v.push(
                        format!("{path}.density_dpi"),
                        "density must be in 72..=1000",
                    );
                }
            }
            StepRef::SetTheme(_) => {}
            StepRef::SetTime(time) => {
                if time.timestamp.len() < 20 || !time.timestamp.contains('T') {
                    v.push(
                        format!("{path}.timestamp"),
                        "timestamp must be an RFC 3339 UTC value (e.g. 2026-11-10T10:00:00Z)",
                    );
                }
            }
            StepRef::CallActivityMethod(call) => {
                if call.method.trim().is_empty() {
                    v.push(format!("{path}.method"), "method must not be empty");
                }
                v.warn(
                    &path,
                    "optional step: only valid for debuggable/source-owned builds with an injected test hook",
                );
            }
        }
    }

    if !scenario.output.screenshots {
        for (index, step) in scenario.steps.iter().enumerate() {
            if step.assert_screenshot.is_some() || step.capture.is_some() {
                v.warn(
                    format!("steps[{index}]"),
                    "step needs screenshots but output.screenshots is false",
                );
            }
        }
    }

    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{Action, Point};

    const MINIMAL: &str = r#"
version: 1
name: minimal
app: fixtures/build/apps/hello-views.apk
isolation: sandbox
display: {mode: offscreen, width: 1080, height: 2400, density_dpi: 420}
steps:
  - launch: {activity: .MainActivity}
  - assert_text:
      selector: {id: result}
      text: {exact: "clicks: 0"}
  - back: {}
"#;

    #[test]
    fn parses_and_validates() {
        let scenario = Scenario::from_yaml_str(MINIMAL).unwrap();
        assert_eq!(scenario.name, "minimal");
        assert_eq!(scenario.steps.len(), 3);
        assert_eq!(scenario.steps[2].kind(), "back");
        assert_eq!(scenario.expectation, Expectation::Pass);
        assert!(scenario.output.screenshots);
        assert!(scenario.validate().is_ok());
    }

    #[test]
    fn missing_version_is_a_parse_error() {
        let err = Scenario::from_yaml_str(
            "name: x\napp: y\nisolation: sandbox\ndisplay: {mode: none}\nsteps:\n  - back: {}\n",
        )
        .unwrap_err();
        assert!(!err.is_ok());
    }

    #[test]
    fn semantic_errors_have_paths() {
        let yaml = r#"
version: 2
name: ""
app: ""
isolation: sandbox
display: {mode: offscreen, width: 0, height: 0, density_dpi: 10}
steps: []
"#;
        let scenario = Scenario::from_yaml_str_unvalidated(yaml).unwrap();
        let result = scenario.validate();
        let paths: Vec<&str> = result.errors.iter().map(|e| e.path.as_str()).collect();
        assert!(paths.contains(&"$.version"));
        assert!(paths.contains(&"$.name"));
        assert!(paths.contains(&"$.app"));
        assert!(paths.contains(&"$.display"));
        assert!(paths.contains(&"$.steps"));
        assert!(result
            .errors
            .iter()
            .all(|e| e.code == ErrorCode::InvalidScenario));
    }

    #[test]
    fn action_alias_is_a_string() {
        let action = Action::Key { key: KeyName::Back };
        assert_eq!(action.kind(), "key");
        let _ = Point { x: 1, y: 2 };
    }

    const EXPECT_CHANGE: &str = r#"
version: 1
name: expect-change
app: fixtures/build/apps/counter.apk
isolation: process
display: {mode: offscreen, width: 1080, height: 2400, density_dpi: 420}
steps:
  - launch: {activity: .MainActivity}
  - tap: {selector: {id: inc}, expect_change: true}
  - back: {expect_change: true}
"#;

    /// SE1: the flag is a bool on the action payloads and defaults to false
    /// (so every existing scenario keeps the P12 rule).
    #[test]
    fn expect_change_parses_as_a_bool_and_defaults_false() {
        let scenario = Scenario::from_yaml_str(EXPECT_CHANGE).expect("valid scenario");
        assert!(scenario.validate().is_ok(), "{:?}", scenario.validate());
        assert!(scenario.steps[1].tap.as_ref().unwrap().expect_change);
        assert!(scenario.steps[2].back.as_ref().unwrap().expect_change);

        let plain = Scenario::from_yaml_str(
            "version: 1\nname: x\napp: y\nisolation: process\ndisplay: {mode: offscreen, width: 8, height: 8, density_dpi: 420}\nsteps:\n  - tap: {x: 1, y: 2}\n  - back: {}\n",
        )
        .expect("valid scenario");
        assert!(plain.validate().is_ok(), "{:?}", plain.validate());
        assert!(!plain.steps[0].tap.as_ref().unwrap().expect_change);
        assert!(!plain.steps[1].back.as_ref().unwrap().expect_change);
        // A false flag is not written back: the wire form is unchanged.
        assert!(!plain.to_yaml_string().unwrap().contains("expect_change"));
    }

    /// SE1: a non-bool is a parse error, never a silently ignored field.
    #[test]
    fn expect_change_rejects_a_non_bool() {
        for value in ["yes", "1", "\"true\"", "[true]"] {
            let yaml = format!(
                "version: 1\nname: x\napp: y\nisolation: process\ndisplay: {{mode: offscreen, width: 8, height: 8, density_dpi: 420}}\nsteps:\n  - tap: {{x: 1, y: 2, expect_change: {value}}}\n"
            );
            let error = Scenario::from_yaml_str(&yaml)
                .err()
                .unwrap_or_else(|| panic!("expect_change: {value} must not parse"));
            assert!(error.errors.iter().all(|e| e.code == ErrorCode::InvalidScenario));
        }
    }

    /// SE1: `expect_change` + `settle: fast` is an unverifiable pass, so it is
    /// rejected at parse time with a stable path - and never at run time after
    /// the action already happened.
    #[test]
    fn expect_change_with_fast_settle_is_invalid() {
        for (kind, body) in [
            ("tap", "{selector: {id: inc}, settle: fast, expect_change: true}"),
            ("type", "{text: hi, settle: fast, expect_change: true}"),
            ("key", "{key: back, settle: fast, expect_change: true}"),
        ] {
            let yaml = format!(
                "version: 1\nname: x\napp: y\nisolation: process\ndisplay: {{mode: offscreen, width: 8, height: 8, density_dpi: 420}}\nsteps:\n  - launch: {{activity: .MainActivity}}\n  - {kind}: {body}\n"
            );
            let scenario = Scenario::from_yaml_str_unvalidated(&yaml).expect("parses");
            let validation = scenario.validate();
            assert!(!validation.is_ok(), "{kind}: must be rejected");
            let expected_path = format!("steps[1].{kind}.expect_change");
            assert!(
                validation.errors.iter().any(|e| e.path == expected_path),
                "{kind}: expected {expected_path}, got {:?}",
                validation.errors
            );
            assert!(validation
                .errors
                .iter()
                .all(|e| e.code == ErrorCode::InvalidScenario));
        }
    }
}
