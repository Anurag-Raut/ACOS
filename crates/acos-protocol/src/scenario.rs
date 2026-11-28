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
use crate::commands::{CaptureFormat, KeyName, Orientation, Point, ResetScope};
use crate::error::ErrorCode;
use crate::identity::{DisplaySpec, IsolationLevel, ResourceBudget};
use crate::launch::ExtraValue;
use crate::ui::{Bounds, Selector};

/// The only scenario format version this crate accepts.
pub const SCENARIO_VERSION: u32 = 1;

/// Which engine the scenario is written for.
///
/// **One vocabulary: the CLI/MCP one.** `art` (real ART, or recorded ART
/// evidence) and `rust-vm` (the frozen Rust VM) are the names the `--engine`
/// flag and the MCP `engine` argument use; the YAML field says the same thing
/// with the same words. A scenario written for the pre-v0.3 names still parses:
///
/// | legacy value | same meaning as |
/// |---|---|
/// | `native-preferred` | `rust-vm` (the native runtime was preferred) |
/// | `native-only` | `rust-vm` (pair it with `fallback: forbid` if no fallback is meant) |
/// | `reference-only` | `art` (recorded ART reference evidence) |
///
/// A legacy value is accepted and reported as a [`ScenarioWarning`] naming its
/// new name, so an old scenario keeps running while its author learns the
/// current spelling (feedback 2026-11-14: `INVALID_SCENARIO: unknown variant
/// 'art'`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Engine {
    /// Real ART (or recorded ART reference evidence) — the CLI's `--engine art`.
    #[default]
    #[serde(alias = "reference-only")]
    Art,
    /// The frozen Rust VM / native runtime — the CLI's `--engine rust-vm`.
    #[serde(alias = "native-preferred", alias = "native-only")]
    RustVm,
}

impl Engine {
    /// The canonical name, the same string `--engine` takes.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Engine::Art => "art",
            Engine::RustVm => "rust-vm",
        }
    }
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
///
/// Every scenario in the corpus declares one, and the declaration is enforced at
/// run time: `pass` is the only value that may produce a pass, and a scenario
/// that cannot (or should not) produce one says so here instead of leaving a
/// red row looking like a regression. `expectation_reason` is the one-line why
/// (`non_pass`/`unsupported`) and is required for those values.
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
    /// The engine cannot provide what the scenario asks, by design or by a known
    /// gap. The run must report `unsupported` (or the code `expect_error` pins);
    /// a pass is a harness failure, never a successful test.
    Unsupported,
}

/// Host capabilities a scenario may need before it can run at all (`needs_env`).
///
/// One registry, validated at load time: a typo'd capability name would
/// otherwise disable a scenario forever with no error, which is the same
/// silent-skip failure the corpus is meant to prevent.
pub const KNOWN_NEEDS_ENV: &[&str] = &["pasta", "metro"];

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
    /// Wait for a fixed number of milliseconds (a timeline step).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait: Option<WaitStep>,
    /// Tap a selector or point.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tap: Option<TapStep>,
    /// Swipe/scroll the screen with a real touch gesture.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub swipe: Option<SwipeStep>,
    /// Swipe until a selector is visible (a bounded search).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scroll_until: Option<ScrollUntilStep>,
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
    /// Assert the window stack order (bottom to top) and focus.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assert_windows: Option<WindowOrderAssertion>,
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
    /// ADR-0029 item 4: one service-module operation in the middle of a run.
    ///
    /// `op` is either the module-lifecycle word (`attach`/`detach`, with an
    /// optional `profile`/`config`) or a module's own control op
    /// (`sensors.set_value`, `net.telephony.sms`, `hw.bluetooth.scan`, ...) with
    /// `args`. This is the scenario spelling of the agent API
    /// (`acos_services` for attach/detach, `acos_service_ctl` for a control op),
    /// so a scripted module can be driven between assertions instead of only at
    /// launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_ctl: Option<ServiceCtlStep>,
    /// METRO3: one operation on a **host dev server** (Metro) the executor owns:
    /// `start`, `edit`, `wait_serves`, `fast_refresh`, `reload`, `stop`.
    ///
    /// The scenario spelling of a developer's loop (save a file, wait for the
    /// bundler, let Fast Refresh or a reload reach the app) between guest
    /// assertions. The server is started and stopped by the run; it is always
    /// stopped at the end of the scenario, even when a step failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dev_server: Option<DevServerStep>,
    /// METRO3: assert the UI tree's node count against a count recorded earlier
    /// in the same run (`record`/`not_grown_since`). A reload that leaves a stale
    /// second root doubles the tree; this is the named check for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assert_tree: Option<TreeSizeAssertion>,
    /// METRO4: assert the React Native JS error the app reported (redbox,
    /// LogBox, `ExceptionsManager`), or `none: true` that there is none (a
    /// reload cleared it). A bundle that could not be fetched is **not** a JS
    /// error: it fails this step as `BUNDLE_UNREACHABLE` unless the step asks
    /// for `bundle_unreachable: true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assert_js_error: Option<JsErrorAssertion>,
    /// ADR-0029 item 4/5: module ids this step needs attached (`feedback.vibrator`).
    ///
    /// A step whose module is `off` is `unsupported`, never `pass`: the executor
    /// refuses to run an assertion against behaviour the session does not have.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires_services: Vec<String>,
}

/// A borrowed view of the one populated step kind.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StepRef<'a> {
    /// See [`Step::launch`].
    Launch(&'a LaunchStep),
    /// See [`Step::wait_for`].
    WaitFor(&'a WaitForStep),
    /// See [`Step::wait`].
    Wait(&'a WaitStep),
    /// See [`Step::tap`].
    Tap(&'a TapStep),
    /// See [`Step::swipe`].
    Swipe(&'a SwipeStep),
    /// See [`Step::scroll_until`].
    ScrollUntil(&'a ScrollUntilStep),
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
    /// See [`Step::assert_windows`].
    AssertWindows(&'a WindowOrderAssertion),
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
    /// See [`Step::service_ctl`].
    ServiceCtl(&'a ServiceCtlStep),
    /// See [`Step::dev_server`].
    DevServer(&'a DevServerStep),
    /// See [`Step::assert_tree`].
    AssertTree(&'a TreeSizeAssertion),
    /// See [`Step::assert_js_error`].
    AssertJsError(&'a JsErrorAssertion),
}

impl StepRef<'_> {
    /// Stable step-kind string (used in artifact file names).
    pub const fn kind(&self) -> &'static str {
        match self {
            StepRef::Launch(_) => "launch",
            StepRef::WaitFor(_) => "wait_for",
            StepRef::Wait(_) => "wait",
            StepRef::Tap(_) => "tap",
            StepRef::Swipe(_) => "swipe",
            StepRef::ScrollUntil(_) => "scroll_until",
            StepRef::Type(_) => "type",
            StepRef::Key(_) => "key",
            StepRef::Back(_) => "back",
            StepRef::Rotate(_) => "rotate",
            StepRef::AssertText(_) => "assert_text",
            StepRef::AssertVisible(_) => "assert_visible",
            StepRef::AssertNotVisible(_) => "assert_not_visible",
            StepRef::AssertState(_) => "assert_state",
            StepRef::AssertWindows(_) => "assert_windows",
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
            StepRef::ServiceCtl(_) => "service_ctl",
            StepRef::DevServer(_) => "dev_server",
            StepRef::AssertTree(_) => "assert_tree",
            StepRef::AssertJsError(_) => "assert_js_error",
        }
    }
}

impl Step {
    /// Number of step keys present (must be 1 for a valid step).
    pub fn variant_count(&self) -> usize {
        [
            self.launch.is_some(),
            self.wait_for.is_some(),
            self.wait.is_some(),
            self.tap.is_some(),
            self.swipe.is_some(),
            self.scroll_until.is_some(),
            self.r#type.is_some(),
            self.key.is_some(),
            self.back.is_some(),
            self.rotate.is_some(),
            self.assert_text.is_some(),
            self.assert_visible.is_some(),
            self.assert_not_visible.is_some(),
            self.assert_state.is_some(),
            self.assert_windows.is_some(),
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
            self.service_ctl.is_some(),
            self.dev_server.is_some(),
            self.assert_tree.is_some(),
            self.assert_js_error.is_some(),
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
        if self.wait.is_some() {
            return self.wait.as_ref().map(StepRef::Wait);
        }
        if self.tap.is_some() {
            return self.tap.as_ref().map(StepRef::Tap);
        }
        if self.swipe.is_some() {
            return self.swipe.as_ref().map(StepRef::Swipe);
        }
        if self.scroll_until.is_some() {
            return self.scroll_until.as_ref().map(StepRef::ScrollUntil);
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
        if self.assert_windows.is_some() {
            return self.assert_windows.as_ref().map(StepRef::AssertWindows);
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
        if self.service_ctl.is_some() {
            return self.service_ctl.as_ref().map(StepRef::ServiceCtl);
        }
        if self.dev_server.is_some() {
            return self.dev_server.as_ref().map(StepRef::DevServer);
        }
        if self.assert_tree.is_some() {
            return self.assert_tree.as_ref().map(StepRef::AssertTree);
        }
        if self.assert_js_error.is_some() {
            return self.assert_js_error.as_ref().map(StepRef::AssertJsError);
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
    /// REL3H2: see [`TapStep::expect_handled`]. `- back: {}` keeps parsing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_handled: Option<bool>,
}

/// `launch`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
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
    /// Typed extras (`String`, `int`, `long`, `float`, `double`, `boolean`,
    /// `String[]`; see [`ExtraValue`] for the YAML spelling). A `parcelable`
    /// extra is a legal declaration that the ART launcher refuses by name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extras: BTreeMap<String, ExtraValue>,
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
    /// MPROC7: the launcher's idle kill, in seconds, for this session
    /// (`0` disables it). A lazily spawned secondary process that starts no
    /// activity and holds no started/bound service is killed after this long,
    /// and the next intent respawns it. Launch-time (the launcher owns the
    /// timer): a scenario that proves the idle kill sets this small, exactly as
    /// `acos-art launch --idle-kill-seconds` / `ACOSLAUNCH_IDLE_KILL_SECS`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_kill_seconds: Option<u64>,
}

/// `wait`: a fixed timeline delay.
///
/// It asserts nothing; it exists so a scenario can prove **time-based**
/// behaviour (the launcher's idle kill, a timeout) because that is what the
/// test is about. It is **never** a synchronisation primitive: waiting a fixed
/// number of milliseconds before an assertion races the app (too early on a
/// fast host, flaky on a loaded one). Use `wait_for` or an assertion's own
/// `timeout_ms` to wait for a state the app reaches. The step's observation
/// records `waited_ms`, so the verdict shows the delay actually elapsed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WaitStep {
    /// Milliseconds to wait (1..=600000).
    pub ms: u64,
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
    /// SE1: fail the step when the settled frame's pixels are unchanged;
    /// A11YPACE: `expect_change: false` says the tap's effect is transient (a
    /// hide/re-show, an asynchronous worker) and **no** change is claimed.
    ///
    /// The tap's own `state_change` assertion (the UI tree + screenshot rule a
    /// tap runs even when the scenario says nothing) needs a tree that differs
    /// between the pre- and post-action frames. That is wrong for a control
    /// whose effect is transient: Gaten's bubble hides for its lookup and
    /// re-shows a moment later, so two probe samples can land on the same tree
    /// and a sampling race would fail the step. An explicit `false` turns the
    /// assertion into the "input delivered, no change claimed" form (the same
    /// `input_delivery` assertion a `handled: false` tap gets) and is recorded
    /// as `input.expect_change: false` in the step and verdict, so it can never
    /// read as a silent pass of a missing effect: only `true` or the default
    /// ("auto") still require the change.
    ///
    /// `true` is strict-only: with `settle: fast`/`content` the pixel claim has
    /// no evidence, so validation rejects it. `false` is not a claim and may
    /// combine with any settle rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_change: Option<bool>,
    /// REL3H2: the scenario's own claim about the app's handling of this input.
    ///
    /// `true` fails the step with `INPUT_NOT_HANDLED` when the app window
    /// acknowledged every event `finishInputEvent(handled) = false` (the gesture
    /// was delivered and nothing consumed it). `false` says "this press is
    /// expected to be a no-op" and silences the `INPUT_UNHANDLED` warning that
    /// REL3H2 attaches to every unhandled input. Unset: the step passes as
    /// measured and carries the warning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_handled: Option<bool>,
}

/// The direction of a swipe gesture (the direction the **finger** travels).
///
/// `up` moves the finger toward the top of the screen, so the content scrolls
/// up and later rows come into view; this is the same word the CLI/MCP
/// `acos_swipe` takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SwipeDirection {
    /// Finger up: content scrolls up (later rows).
    Up,
    /// Finger down: content scrolls down (earlier rows).
    Down,
    /// Finger left: content scrolls left.
    Left,
    /// Finger right: content scrolls right.
    Right,
}

impl SwipeDirection {
    /// The two gesture endpoints, centred on the display.
    ///
    /// `distance` defaults to 60 % of the axis (the same rule as the CLI/MCP
    /// `acos_swipe`); the two points are symmetric around the display centre so
    /// the gesture never starts outside the screen for a normal distance.
    #[must_use]
    pub fn endpoints(self, distance: Option<u32>, width: u32, height: u32) -> ((f32, f32), (f32, f32)) {
        let (w, h) = (width as f32, height as f32);
        let (cx, cy) = (w / 2.0, h / 2.0);
        let axis = if matches!(self, SwipeDirection::Up | SwipeDirection::Down) {
            h * 0.6
        } else {
            w * 0.6
        };
        let distance = distance.map_or(axis, |d| d as f32);
        let half = distance / 2.0;
        match self {
            SwipeDirection::Up => ((cx, cy + half), (cx, cy - half)),
            SwipeDirection::Down => ((cx, cy - half), (cx, cy + half)),
            SwipeDirection::Left => ((cx + half, cy), (cx - half, cy)),
            SwipeDirection::Right => ((cx - half, cy), (cx + half, cy)),
        }
    }
}

/// `swipe`.
///
/// One real touch gesture: `DOWN`, interpolated `MOVE`s at the display refresh
/// cadence, `UP`, delivered through the same live input channel a tap uses. The
/// list's own physics decides whether it scrolls/flings; the engine only
/// publishes the finger stream. The step settles with the scenario's settle
/// rule (strict by default) and reports the app's `finishInputEvent(handled)`
/// answer like every other input step (REL3E).
///
/// Geometry: explicit `from`/`to` points, or `direction` (+ optional `distance`)
/// over the display. Exactly one form is accepted.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct SwipeStep {
    /// Gesture start (finger down), in display pixels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<Point>,
    /// Gesture end (finger up), in display pixels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<Point>,
    /// Gesture direction (alternative to `from`/`to`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<SwipeDirection>,
    /// Gesture length in pixels with `direction` (default 60 % of the axis).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distance: Option<u32>,
    /// Gesture duration in milliseconds (default 400; 120 with `fling`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// A quick gesture: default duration 120 ms (the framework's own velocity
    /// tracker decides the fling; this only changes the timing).
    #[serde(default, skip_serializing_if = "is_false")]
    pub fling: bool,
    /// P12b: strict/fast settle deadline for this swipe (milliseconds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settle_timeout_ms: Option<u64>,
    /// Settle rule for this input (`strict` default, `fast`/`content` opt-in).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settle: Option<StepSettle>,
    /// SE1: fail the step when the settled frame's pixels are unchanged.
    #[serde(default, skip_serializing_if = "is_false")]
    pub expect_change: bool,
    /// REL3H2: `true` fails the step with `INPUT_NOT_HANDLED` when nothing
    /// consumed the gesture; `false` silences the `INPUT_UNHANDLED` warning (see
    /// [`TapStep::expect_handled`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_handled: Option<bool>,
}

/// `scroll_until`.
///
/// Swipe repeatedly until `selector` is visible in a **settled** observation.
/// Every attempt is one real gesture followed by a fresh traversal (never a
/// cached tree), so a found node is always backed by a frame the framework was
/// showing after the gesture settled. The search is bounded by `max_attempts`;
/// a miss is a structured `ELEMENT_NOT_FOUND` naming the attempts and the last
/// tree's texts - never a pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ScrollUntilStep {
    /// The node to scroll into view.
    pub selector: Selector,
    /// Gesture start (finger down), in display pixels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<Point>,
    /// Gesture end (finger up), in display pixels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<Point>,
    /// Gesture direction (alternative to `from`/`to`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<SwipeDirection>,
    /// Gesture length in pixels with `direction` (default 60 % of the axis).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distance: Option<u32>,
    /// Gesture duration in milliseconds (default 400; 120 with `fling`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// A quick gesture: default duration 120 ms and 10 attempts.
    #[serde(default, skip_serializing_if = "is_false")]
    pub fling: bool,
    /// Swipes to try before failing (default 6; 10 with `fling`; max 50).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_attempts: Option<usize>,
    /// P12b: strict settle deadline for each attempt (milliseconds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settle_timeout_ms: Option<u64>,
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
    /// REL3H2: `true` fails the step with `INPUT_NOT_HANDLED` when nothing
    /// consumed the input; `false` silences the `INPUT_UNHANDLED` warning (see
    /// [`TapStep::expect_handled`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_handled: Option<bool>,
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
    /// REL3H2: `true` fails the step with `INPUT_NOT_HANDLED` when nothing
    /// consumed the key; `false` silences the `INPUT_UNHANDLED` warning (see
    /// [`TapStep::expect_handled`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_handled: Option<bool>,
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

/// `assert_windows`: the window stack in policy z order, bottom first.
///
/// This is the LAYER1 assertion: it reads the tree's `windows[]` block (the
/// host's AOSP layer policy, not insertion order), so an application overlay
/// that is drawn and tapped above an activity can only pass when the reported
/// stack actually says so.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WindowOrderAssertion {
    /// Expected window kinds, bottom to top (`application`,
    /// `application_overlay`, `sub_window`, …). The count must match exactly:
    /// a missing or extra window is a failure, never silently ignored.
    pub order: Vec<String>,
    /// Optional expected kind of the focused window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus: Option<String>,
    /// Optional expected `LayoutParams.TYPE_*` name of the focused window.
    /// This is the precise form: the activity is `TYPE_BASE_APPLICATION`, a
    /// dialog is `TYPE_APPLICATION`, and both have kind `application`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus_type: Option<String>,
    /// Optional expected `LayoutParams.TYPE_*` names, bottom to top. When
    /// present this must have the same length as `order`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub types: Option<Vec<String>>,
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

/// `service_ctl`: one service-module operation between assertions (ADR-0029
/// item 4).
///
/// `op` is `attach` (with `profile`/`config`), `detach`, or one of the module's
/// own control ops (`sensors.set_value`, `net.telephony.sms`, `hw.bluetooth.scan`,
/// ...). The step is the one scenario spelling of the agent API —
/// `acos_services {op: attach|detach}` for the lifecycle words and
/// `acos_service_ctl {module, op, args}` for a control op — so a scripted module
/// can be driven mid-run: set a sensor value, assert the view, play a shake,
/// detach and assert the honest empty answer. A control op reaches only a module
/// that is attached in this run; a step naming a detached module is a structured
/// step failure (never a silent no-op).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ServiceCtlStep {
    /// Module id (`sensors`, `feedback.vibrator`).
    pub module: String,
    /// `attach`, `detach`, or a module control op (`set_value`, `play`, ...).
    pub op: String,
    /// Profile for `attach` (`off`/`stub`/`scripted`/`real`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Module config for `attach`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<Value>,
    /// Control-op arguments (`{sensor: "accelerometer", values: [0,0,9.81]}`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Value>,
    /// What the reply must contain, as a JSON **subset**.
    ///
    /// Objects match key by key (an expected key the reply lacks is a
    /// failure), arrays match element by element with the same length, and
    /// every other value must be equal. Absent = the reply is evidence only.
    /// This is what makes a control op an *assertion*: `list` can require the
    /// posted notification's title/channel/actions, `tap` the fired sender's
    /// `start_result`, `dismiss` an empty `posted`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect: Option<Value>,
    /// Wait for `expect` instead of sampling it once (1..=120000 ms).
    ///
    /// For a state the app reaches *asynchronously* (a screenshot the app
    /// requests from a worker thread after a tap, a service that binds a moment
    /// after the setting is written): the read-only op is re-issued until the
    /// reply contains `expect`, and the step fails with the **last** reply when
    /// the deadline passes. Only legal with `expect` and the read-only op
    /// `status`: a polled op must be idempotent, and a wait with no expectation
    /// would be a sleep (`wait` is that step).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

/// One operation of the `dev_server` step family (METRO3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DevServerOp {
    /// Copy `project` into run scratch and start Metro there on `port`.
    Start,
    /// Edit one file of the **project copy** (`find`/`replace`, or `contents`).
    Edit,
    /// Wait until the bundle Metro serves contains `contains`.
    WaitServes,
    /// Fast Refresh: the edit reaches the running app over Metro's `/hot`
    /// websocket by itself (file watcher -> HMR update); this step waits until
    /// Metro has rebuilt and serves the edit, so the app-side effect can be
    /// asserted next. It sends **no** reload and says so in its evidence.
    FastRefresh,
    /// Full reload: `POST /reload` (the CLI's `r` key). Best-effort on RN's
    /// legacy architecture (see `docs/METRO_PLAN.md`): the steps after it
    /// decide the verdict; this step only reports delivery.
    Reload,
    /// Stop Metro and delete the project copy (also done at scenario end).
    Stop,
    /// METRO5: open the React Native dev menu without a shake: Metro's
    /// `/message` broadcast `devMenu` (the CLI's `d` key). The app's
    /// `DevSupportManager` shows the menu; the steps after it read the items.
    DevMenu,
}

/// `dev_server` (METRO3): one operation on the host dev server the run owns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DevServerStep {
    /// What to do.
    pub op: DevServerOp,
    /// `start`: source project directory (relative to the working directory).
    /// It is **copied**; the original is never edited or served.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// `start`: directory hard-linked into the copy as `node_modules`
    /// (default: the host's React Native cache; see `acos doctor`, check `metro`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_modules: Option<String>,
    /// `start`: Metro's TCP port on the host (1024..=65535).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// `start`: open the relay's **host-loopback opt-in** for `port` before the
    /// guest exists (what `adb reverse` is on a phone). Default `true`: a debug
    /// app fetches its bundle during `Application.onCreate`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expose_to_guest: Option<bool>,
    /// `edit`: file inside the project copy (relative, no `..`, not `node_modules`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// `edit`: text to find; it must occur, otherwise the step fails (no silent no-op edit).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub find: Option<String>,
    /// `edit`: replacement for every occurrence of `find`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replace: Option<String>,
    /// `edit`: replace the whole file instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contents: Option<String>,
    /// `wait_serves`: a string the served bundle must contain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contains: Option<String>,
    /// `wait_serves`: a string the served bundle must NOT contain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub absent: Option<String>,
    /// `start`/`wait_serves`/`fast_refresh`: how long to wait (1..=300000 ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// `start` (METRO5): point the debug app at this server — the launcher
    /// writes `debug_http_host = localhost:<port>` into the app's default
    /// SharedPreferences before the guest boots (React Native's own "Debug
    /// server host & port" setting). ACOS never emulates `10.0.2.2`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub debug_http_host: Option<bool>,
}

/// `assert_js_error` (METRO4): the React Native JS error on screen / in the log.
///
/// Every field is a constraint on the **most recent** JS error in scope; all
/// given constraints must hold. `none: true` instead requires that no JS error
/// was reported in scope (a fix + reload cleared the redbox).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct JsErrorAssertion {
    /// No JS error in scope (exclusive with every other constraint).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub none: Option<bool>,
    /// Exact error class (`TypeError`, `SyntaxError`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Substring of the error message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// A frame's file ends with this (`App.js`, `src/screens/Home.tsx`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// That frame's line (needs `file`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// Require the frames to be symbolicated (source positions).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbolicated: Option<bool>,
    /// Expect the bundle-fetch failure instead of a JS error (`BUNDLE_UNREACHABLE`
    /// is then the pass condition; exclusive with the JS-error fields).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle_unreachable: Option<bool>,
    /// Only consider log lines produced since the previous step (default:
    /// the whole run; a `none: true` after a reload usually wants `true`).
    #[serde(default)]
    pub since_previous_step: bool,
    /// Wait up to this long for the condition (1..=300000 ms); the last
    /// observation decides.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

/// `assert_tree` (METRO3): the UI tree's node count against an earlier record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TreeSizeAssertion {
    /// Remember the current node count under this label (the step passes when the tree is non-empty).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record: Option<String>,
    /// Pass only when the node count is `<=` the one recorded under this label
    /// (a reload that left a stale root doubles the tree and fails).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_grown_since: Option<String>,
    /// Event-driven wait for the condition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// Settle rule for the backing observation (see `assert_text`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settle: Option<StepSettle>,
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
    /// One line saying **why** the expectation is not `pass` (the known reason:
    /// an open defect, an engine gap, a security rule). Required for
    /// `non_pass`/`unsupported` and for `needs_env`; refused for `pass`, so an
    /// honest declaration and a green row cannot disagree silently.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expectation_reason: Option<String>,
    /// Host capabilities the scenario needs before it can run (see
    /// [`KNOWN_NEEDS_ENV`]). A runner must skip the scenario when one is absent
    /// and say so, instead of running it into a failure that is not its own.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub needs_env: Vec<String>,
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
    /// ADR-0029 item 4: the service modules this scenario attaches at launch,
    /// in order. Shape is the launch-request shape (`{id, profile, config}`);
    /// the profiles and ids are validated here so a typo is an invalid
    /// scenario, never a silently defaulted session.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<ServiceRequest>,
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
    /// MPROC1: extra APKs installed into the session beside `app`, each at its
    /// own uid and reachable through `PackageManager`/`createPackageContext`.
    ///
    /// Paths are workspace-relative or absolute, resolved like `app`. A
    /// scenario that names packages here is self-contained: the run installs
    /// exactly this set, or fails with a structured error naming the APK.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_apks: Option<Vec<String>>,
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
        let mut validation = scenario.validate();
        // The legacy `engine:` spellings are serde aliases, so the parsed value
        // no longer knows which spelling it came from; the raw YAML does. One
        // warning names the canonical word instead of failing the scenario.
        if let Some(warning) = legacy_engine_warning(yaml) {
            validation.warnings.push(warning);
        }
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

/// One `services:` entry (`{id, profile, config}`), the scenario spelling of a
/// launch request (ADR-0029 item 4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ServiceRequest {
    /// Module id (`feedback.vibrator`).
    pub id: String,
    /// Requested profile; `None` means the module's default (strongest
    /// implemented) profile, exactly like the MCP/CLI form.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Module configuration object (scripted state, launch config).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<Value>,
}

/// The profiles a service module implements (plus `off`).
pub const SERVICE_PROFILES: &[&str] = &["off", "stub", "scripted", "real"];

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

/// A `dev_server` project-relative path: inside the copy, never `node_modules`.
fn dev_server_relative_path_problem(file: &str) -> Option<&'static str> {
    let p = std::path::Path::new(file);
    if file.trim().is_empty() {
        return Some("file must not be empty");
    }
    if p.is_absolute() {
        return Some("file must be relative to the project copy");
    }
    let mut first = true;
    for component in p.components() {
        match component {
            std::path::Component::Normal(name) => {
                if first && name == "node_modules" {
                    return Some("node_modules is shared with the host cache and is not editable");
                }
                first = false;
            }
            std::path::Component::CurDir => {}
            _ => return Some("file must stay inside the project copy (no `..`)"),
        }
    }
    None
}

fn validate_dev_server(
    v: &mut ScenarioValidation,
    path: &str,
    dev: &DevServerStep,
    scenario: &Scenario,
    index: usize,
) {
    if !scenario.needs_env.iter().any(|name| name.trim() == "metro") {
        v.push(
            path,
            "a `dev_server` step needs `needs_env: [metro]` (the host must have node and Metro; the runner skips the file when it does not)",
        );
    }
    let stray = |v: &mut ScenarioValidation, field: &str, present: bool, ops: &str| {
        if present {
            v.push(
                format!("{path}.{field}"),
                format!("`{field}` only applies to {ops}"),
            );
        }
    };
    let is = |op: DevServerOp| dev.op == op;
    stray(v, "project", dev.project.is_some() && !is(DevServerOp::Start), "op `start`");
    stray(v, "node_modules", dev.node_modules.is_some() && !is(DevServerOp::Start), "op `start`");
    stray(v, "port", dev.port.is_some() && !is(DevServerOp::Start), "op `start`");
    stray(
        v,
        "expose_to_guest",
        dev.expose_to_guest.is_some() && !is(DevServerOp::Start),
        "op `start`",
    );
    stray(
        v,
        "debug_http_host",
        dev.debug_http_host.is_some() && !is(DevServerOp::Start),
        "op `start`",
    );
    stray(
        v,
        "file",
        dev.file.is_some() && !is(DevServerOp::Edit),
        "op `edit`",
    );
    stray(
        v,
        "find",
        dev.find.is_some() && !is(DevServerOp::Edit),
        "op `edit`",
    );
    stray(
        v,
        "replace",
        dev.replace.is_some() && !is(DevServerOp::Edit),
        "op `edit`",
    );
    stray(
        v,
        "contents",
        dev.contents.is_some() && !is(DevServerOp::Edit),
        "op `edit`",
    );
    stray(
        v,
        "contains",
        dev.contains.is_some() && !is(DevServerOp::WaitServes),
        "op `wait_serves`",
    );
    stray(
        v,
        "absent",
        dev.absent.is_some() && !is(DevServerOp::WaitServes),
        "op `wait_serves`",
    );
    if let Some(timeout) = dev.timeout_ms {
        if !(1..=300_000).contains(&timeout) {
            v.push(
                format!("{path}.timeout_ms"),
                "timeout_ms must be between 1 and 300000",
            );
        }
        if matches!(
            dev.op,
            DevServerOp::Edit | DevServerOp::Reload | DevServerOp::Stop | DevServerOp::DevMenu
        ) {
            v.push(
                format!("{path}.timeout_ms"),
                "`timeout_ms` only applies to `start`, `wait_serves` and `fast_refresh`",
            );
        }
    }
    // the server's life: `start` ... `stop`, one at a time
    let mut running = false;
    for earlier in &scenario.steps[..index] {
        if let Some(step) = &earlier.dev_server {
            match step.op {
                DevServerOp::Start => running = true,
                DevServerOp::Stop => running = false,
                _ => {}
            }
        }
    }
    match dev.op {
        DevServerOp::Start => {
            if running {
                v.push(path, "a dev server is already running; `stop` it first");
            }
            if dev.debug_http_host == Some(true)
                && scenario.steps[..index].iter().any(|step| step.launch.is_some())
            {
                v.push(
                    format!("{path}.debug_http_host"),
                    "`debug_http_host` is written before the guest boots: the `start` step must come before the first `launch`",
                );
            }
            match &dev.project {
                None => v.push(format!("{path}.project"), "`start` needs `project`"),
                Some(project) if project.trim().is_empty() => {
                    v.push(format!("{path}.project"), "project must not be empty")
                }
                _ => {}
            }
            match dev.port {
                None => v.push(format!("{path}.port"), "`start` needs `port`"),
                Some(port) if port < 1024 => {
                    v.push(format!("{path}.port"), "port must be 1024..=65535")
                }
                _ => {}
            }
        }
        op => {
            if !running {
                v.push(
                    path,
                    "no dev server is running at this step (an earlier `dev_server: {op: start}` is required)",
                );
            }
            if op == DevServerOp::Edit {
                match &dev.file {
                    None => v.push(format!("{path}.file"), "`edit` needs `file`"),
                    Some(file) => {
                        if let Some(problem) = dev_server_relative_path_problem(file) {
                            v.push(format!("{path}.file"), problem);
                        }
                    }
                }
                match (&dev.find, &dev.replace, &dev.contents) {
                    (Some(find), Some(_), None) if !find.is_empty() => {}
                    (None, None, Some(_)) => {}
                    _ => v.push(
                        path,
                        "`edit` needs either `find` + `replace` (find non-empty) or `contents`",
                    ),
                }
            }
            if op == DevServerOp::WaitServes && dev.contains.is_none() && dev.absent.is_none() {
                v.push(path, "`wait_serves` needs `contains` and/or `absent`");
            }
        }
    }
}

/// METRO4: `assert_js_error` — `none` and `bundle_unreachable` are each
/// exclusive with the JS-error constraints, and `line` names a frame of `file`.
fn validate_js_error(v: &mut ScenarioValidation, path: &str, a: &JsErrorAssertion) {
    let error_fields = a.name.is_some()
        || a.message.is_some()
        || a.file.is_some()
        || a.line.is_some()
        || a.symbolicated.is_some();
    if a.none == Some(false) {
        v.push(format!("{path}.none"), "`none: false` says nothing; omit it or assert the error's fields");
    }
    if a.bundle_unreachable == Some(false) {
        v.push(
            format!("{path}.bundle_unreachable"),
            "`bundle_unreachable: false` says nothing; omit it (a JS-error assertion already fails on an unreachable bundle)",
        );
    }
    if a.none == Some(true) && (error_fields || a.bundle_unreachable.is_some()) {
        v.push(path, "`none: true` cannot be combined with other constraints");
    }
    if a.bundle_unreachable == Some(true) && error_fields {
        v.push(
            path,
            "`bundle_unreachable: true` asserts that no bundle ran; it cannot be combined with JS-error fields",
        );
    }
    if a.none.is_none() && a.bundle_unreachable.is_none() && !error_fields {
        v.push(
            path,
            "assert_js_error needs `none: true`, `bundle_unreachable: true` or at least one of name/message/file/line/symbolicated",
        );
    }
    if a.line.is_some() && a.file.is_none() {
        v.push(format!("{path}.line"), "`line` needs `file` (the frame it belongs to)");
    }
    for (field, value) in [("name", &a.name), ("message", &a.message), ("file", &a.file)] {
        if value.as_deref().is_some_and(|text| text.trim().is_empty()) {
            v.push(format!("{path}.{field}"), format!("{field} must not be empty"));
        }
    }
    if let Some(timeout) = a.timeout_ms {
        if !(1..=300_000).contains(&timeout) {
            v.push(format!("{path}.timeout_ms"), "timeout_ms must be between 1 and 300000");
        }
    }
}

fn validate_timeout(validation: &mut ScenarioValidation, path: &str, timeout: Option<u64>) {
    if timeout == Some(0) {
        validation.push(path, "timeout_ms must be > 0");
    }
}

/// Validate a swipe's geometry: explicit `from`/`to` points **or** a
/// `direction` (+ optional `distance`), never both and never half a pair.
///
/// `step_name` is the YAML step key (`swipe` / `scroll_until`), so the error
/// path names the exact field an author has to fix.
fn validate_swipe_geometry(
    validation: &mut ScenarioValidation,
    path: &str,
    step_name: &str,
    from: Option<Point>,
    to: Option<Point>,
    direction: Option<SwipeDirection>,
    distance: Option<u32>,
    duration_ms: Option<u64>,
    settle_timeout_ms: Option<u64>,
) {
    match (from, to) {
        (Some(from), Some(to)) => {
            if from == to {
                validation.push(
                    format!("{path}.{step_name}.to"),
                    "a swipe's `from` and `to` points must differ (a zero-length gesture is not a swipe)",
                );
            }
            if direction.is_some() || distance.is_some() {
                validation.push(
                    format!("{path}.{step_name}"),
                    "accepts `from`/`to` points or `direction`/`distance`, not both",
                );
            }
        }
        (None, None) => {
            if direction.is_none() {
                validation.push(
                    format!("{path}.{step_name}"),
                    "needs `from` and `to` points, or a `direction` (up/down/left/right)",
                );
            }
        }
        _ => validation.push(
            format!("{path}.{step_name}"),
            "needs both `from` and `to` (one point is not a gesture)",
        ),
    }
    if distance == Some(0) {
        validation.push(
            format!("{path}.{step_name}.distance"),
            "distance must be > 0 pixels",
        );
    }
    if duration_ms == Some(0) {
        validation.push(
            format!("{path}.{step_name}.duration_ms"),
            "duration_ms must be > 0",
        );
    }
    if settle_timeout_ms == Some(0) {
        validation.push(
            format!("{path}.{step_name}.settle_timeout_ms"),
            "settle_timeout_ms must be > 0",
        );
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
    expect_change: Option<bool>,
    settle: Option<StepSettle>,
) {
    // SE2: `expect_change: true` is a pixel claim. A content-stable frame is
    // never pixel evidence (the pixels may still be animating), so the
    // combination is rejected at parse time exactly like fast.
    //
    // A11YPACE: only `true` is a claim. `expect_change: false` (a tap whose
    // effect is transient) asks for *no* evidence, so it may combine with any
    // settle rule; the tap then reports `input_delivery` with "no change
    // claimed" instead of running the tree/pixel comparison.
    if expect_change == Some(true) && matches!(settle, Some(StepSettle::Fast | StepSettle::Content)) {
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

/// The top-level `engine:` value a scenario document actually declares.
///
/// The parsed [`Scenario`] cannot answer this: `#[serde(default)]` gives `art`
/// to a document that says nothing, and the legacy names are serde aliases, so
/// an explicit `engine: native-only` is indistinguishable from an omitted one.
/// This deserializes the raw spelling through the same YAML parser the scenario
/// itself uses — not a text scan — so an inline comment (`engine: native-only #
/// pinned`), quotes, and a nested `observe: {engine: ...}` all read exactly as
/// serde reads them.
///
/// `None` means "the document has no top-level `engine:` key" (an omitted key,
/// not a default).
#[must_use]
pub fn declared_engine(yaml: &str) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct TopLevel {
        #[serde(default)]
        engine: Option<String>,
    }
    let top: TopLevel = serde_yaml_ng::from_str(yaml).ok()?;
    top.engine
}

/// The warning for a scenario whose declared `engine:` is a pre-v0.3 spelling.
///
/// The three legacy names are serde aliases of the canonical `art` / `rust-vm`
/// values, so a parsed [`Scenario`] cannot tell which spelling it came from;
/// [`declared_engine`] reads the source spelling and this names the current word
/// (feedback 2026-11-14: `INVALID_SCENARIO: unknown variant 'art'` from two
/// vocabularies for one concept).
fn legacy_engine_warning(yaml: &str) -> Option<ScenarioWarning> {
    let value = declared_engine(yaml)?;
    let canonical = match value.as_str() {
        "native-preferred" => "rust-vm",
        "native-only" => "rust-vm",
        "reference-only" => "art",
        _ => return None,
    };
    Some(ScenarioWarning {
        path: "engine".to_string(),
        message: format!(
            "`engine: {value}` is the legacy name for `engine: {canonical}`; the scenario \
             format and the `--engine` flag use one vocabulary: `art` (real ART / recorded ART \
             evidence) or `rust-vm` (the frozen Rust VM). The run is unaffected."
        ),
    })
}

/// The warning for a scenario that pins one engine while another one ran.
///
/// The scenario's `engine:` never selects the engine — the CLI `--engine` flag /
/// the MCP `engine` argument do, and they stay authoritative. But a document
/// written for `rust-vm` that silently ran on `art` is a fact the reader of the
/// verdict must see, so the mismatch is a structured warning (never a status
/// change, never a silent engine switch).
#[must_use]
pub fn engine_mismatch_warning(declared: &str, ran: &str) -> Option<ScenarioWarning> {
    if declared == ran {
        return None;
    }
    Some(ScenarioWarning {
        path: "engine".to_string(),
        message: format!(
            "the scenario declares `engine: {declared}` but this run used `{ran}`: the \
             scenario field records the engine the scenario was written for, it does not select \
             one (`--engine {ran}` / the MCP `engine` argument do). The verdict is about the \
             `{ran}` run; pass `--engine {declared}` to run the engine the scenario names."
        ),
    })
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
    // MPROC1: an extra APK list is explicit — no empty entry, no duplicate
    // path, and never the launched APK itself (the install would collide).
    if let Some(extra_apks) = &scenario.extra_apks {
        for (index, path) in extra_apks.iter().enumerate() {
            if path.trim().is_empty() {
                v.push(
                    &format!("$.extra_apks[{index}]"),
                    "an extra APK path cannot be empty",
                );
            } else if path.trim() == scenario.app.trim() {
                v.push(
                    &format!("$.extra_apks[{index}]"),
                    "an extra APK cannot be the scenario's own `app`",
                );
            } else if extra_apks[..index]
                .iter()
                .any(|earlier| earlier.trim() == path.trim())
            {
                v.push(
                    &format!("$.extra_apks[{index}]"),
                    "the same extra APK is listed twice",
                );
            }
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
    // The declaration must be honest on its own: a non-pass expectation says
    // why in one line, and a `needs_env` names a registered host capability (a
    // typo would otherwise skip the scenario forever with no error).
    if let Some(reason) = scenario.expectation_reason.as_deref() {
        if reason.trim().is_empty() {
            v.push(
                "$.expectation_reason",
                "expectation_reason must not be empty (it is the one-line why)",
            );
        }
        if scenario.expectation == Expectation::Pass && scenario.needs_env.is_empty() {
            v.push(
                "$.expectation_reason",
                "`expectation: pass` has no reason to give; a reason belongs to non_pass or \
                 unsupported (or to needs_env, where it says what the host must provide)",
            );
        }
    }
    if matches!(
        scenario.expectation,
        Expectation::NonPass | Expectation::Unsupported
    ) && scenario.expectation_reason.as_deref().is_none_or(|r| r.trim().is_empty())
    {
        v.push(
            "$.expectation_reason",
            format!(
                "`expectation: {}` must say why in one line (expectation_reason)",
                match scenario.expectation {
                    Expectation::NonPass => "non_pass",
                    Expectation::Unsupported => "unsupported",
                    _ => unreachable!(),
                }
            ),
        );
    }
    let mut seen_needs: BTreeMap<&str, usize> = BTreeMap::new();
    for (index, name) in scenario.needs_env.iter().enumerate() {
        let path = format!("$.needs_env[{index}]");
        let name_trimmed = name.trim();
        if name_trimmed.is_empty() {
            v.push(path, "needs_env entries must not be empty");
            continue;
        }
        if !KNOWN_NEEDS_ENV.contains(&name_trimmed) {
            v.push(
                path,
                format!(
                    "unknown host capability `{name_trimmed}`; known: {}",
                    KNOWN_NEEDS_ENV.join(", ")
                ),
            );
        }
        *seen_needs.entry(name_trimmed).or_default() += 1;
    }
    for (name, count) in seen_needs {
        if count > 1 {
            v.push(
                "$.needs_env",
                format!("`{name}` is listed {count} times; name each capability once"),
            );
        }
    }
    if !scenario.needs_env.is_empty()
        && scenario.expectation_reason.as_deref().is_none_or(|r| r.trim().is_empty())
    {
        v.push(
            "$.needs_env",
            "a scenario that needs a host capability says so in expectation_reason \
             (what is missing and why the run cannot be honest without it)",
        );
    }
    // SE2b: a pinned failure only makes sense for a negative scenario. A
    // `pass` scenario that pins an error code would be self-contradictory, and
    // an empty code could never be satisfied - both are refused here rather than
    // silently ignored at run time.
    if let Some(expected) = &scenario.expect_error {
        if expected.code.trim().is_empty() {
            v.push("$.expect_error.code", "expect_error.code must not be empty");
        }
        if expected
            .reason
            .as_deref()
            .is_some_and(|r| r.trim().is_empty())
        {
            v.push(
                "$.expect_error.reason",
                "expect_error.reason must not be empty",
            );
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
                // LAUNCHEXTRAS: `activity` names the component and
                // `intent_action` is the Intent's action; they are independent
                // fields now (a scenario launching `X/.A` with an explicit
                // action is exactly what a host-built launch Intent carries).
                if let Some(secs) = launch.idle_kill_seconds {
                    if secs > 86_400 {
                        v.push(
                            format!("{path}.idle_kill_seconds"),
                            "idle_kill_seconds must be <= 86400 (one day); 0 disables the idle kill",
                        );
                    }
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
            StepRef::Wait(wait) => {
                if wait.ms == 0 || wait.ms > 600_000 {
                    v.push(format!("{path}.ms"), "ms must be in 1..=600000");
                }
            }
            StepRef::Tap(tap) => {
                validate_action_like(&mut v, &format!("steps[{index}]"), tap, "tap");
                validate_expect_change(&mut v, &path, tap.expect_change, tap.settle);
            }
            StepRef::Swipe(swipe) => {
                validate_swipe_geometry(
                    &mut v,
                    &format!("steps[{index}]"),
                    "swipe",
                    swipe.from,
                    swipe.to,
                    swipe.direction,
                    swipe.distance,
                    swipe.duration_ms,
                    swipe.settle_timeout_ms,
                );
                validate_expect_change(&mut v, &path, Some(swipe.expect_change), swipe.settle);
            }
            StepRef::ScrollUntil(scroll) => {
                validate_swipe_geometry(
                    &mut v,
                    &format!("steps[{index}]"),
                    "scroll_until",
                    scroll.from,
                    scroll.to,
                    scroll.direction,
                    scroll.distance,
                    scroll.duration_ms,
                    scroll.settle_timeout_ms,
                );
                validate_selector(&mut v, &path, &scroll.selector, "selector");
                if let Some(max_attempts) = scroll.max_attempts {
                    if max_attempts == 0 || max_attempts > 50 {
                        v.push(
                            format!("{path}.max_attempts"),
                            "max_attempts must be in 1..=50",
                        );
                    }
                }
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
                validate_expect_change(&mut v, &path, Some(ty.expect_change), ty.settle);
            }
            StepRef::Key(key) => {
                validate_expect_change(&mut v, &path, Some(key.expect_change), key.settle)
            }
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
            StepRef::AssertWindows(assertion) => {
                if assertion.order.is_empty() {
                    v.push(&path, "assert_windows.order must list at least one window");
                }
                if let Some(types) = &assertion.types {
                    if types.len() != assertion.order.len() {
                        v.push(
                            &path,
                            "assert_windows.types must have the same length as order",
                        );
                    }
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
            StepRef::DevServer(dev) => validate_dev_server(&mut v, &path, dev, scenario, index),
            StepRef::AssertJsError(assertion) => validate_js_error(&mut v, &path, assertion),
            StepRef::AssertTree(tree) => {
                match (&tree.record, &tree.not_grown_since) {
                    (Some(_), Some(_)) | (None, None) => v.push(
                        &path,
                        "assert_tree needs exactly one of `record` and `not_grown_since`",
                    ),
                    (Some(label), None) if label.trim().is_empty() => {
                        v.push(format!("{path}.record"), "label must not be empty")
                    }
                    (None, Some(label)) => {
                        if label.trim().is_empty() {
                            v.push(format!("{path}.not_grown_since"), "label must not be empty");
                        } else if !scenario.steps[..index].iter().any(|step| {
                            step.assert_tree
                                .as_ref()
                                .is_some_and(|earlier| earlier.record.as_ref() == Some(label))
                        }) {
                            v.push(
                                format!("{path}.not_grown_since"),
                                format!("no earlier `assert_tree: {{record: {label}}}` step"),
                            );
                        }
                    }
                    _ => {}
                }
                validate_timeout(&mut v, &format!("{path}.timeout_ms"), tree.timeout_ms);
                validate_observation_settle(&mut v, &format!("{path}.settle"), tree.settle);
            }
            StepRef::ServiceCtl(ctl) => {
                if ctl.module.trim().is_empty() {
                    v.push(format!("{path}.module"), "module id must not be empty");
                }
                if ctl.op.trim().is_empty() {
                    v.push(format!("{path}.op"), "op must not be empty");
                }
                if let Some(profile) = &ctl.profile {
                    if !SERVICE_PROFILES.contains(&profile.as_str()) {
                        v.push(
                            format!("{path}.profile"),
                            format!(
                                "unknown profile {profile:?}; use one of {}",
                                SERVICE_PROFILES.join(", ")
                            ),
                        );
                    }
                    if ctl.op != "attach" {
                        v.push(
                            format!("{path}.profile"),
                            "`profile` only applies to op `attach`",
                        );
                    }
                }
                if let Some(config) = &ctl.config {
                    if !config.is_object() {
                        v.push(format!("{path}.config"), "config must be an object");
                    }
                    if ctl.op != "attach" {
                        v.push(
                            format!("{path}.config"),
                            "`config` only applies to op `attach`",
                        );
                    }
                }
                if let Some(args) = &ctl.args {
                    if !args.is_object() && !args.is_null() {
                        v.push(format!("{path}.args"), "args must be an object");
                    }
                }
                if let Some(timeout) = ctl.timeout_ms {
                    if !(1..=120_000).contains(&timeout) {
                        v.push(
                            format!("{path}.timeout_ms"),
                            "timeout_ms must be between 1 and 120000",
                        );
                    }
                    if ctl.expect.is_none() {
                        v.push(
                            format!("{path}.timeout_ms"),
                            "`timeout_ms` waits for `expect`; without one it is a sleep (use `wait`)",
                        );
                    }
                    if ctl.op != "status" {
                        v.push(
                            format!("{path}.timeout_ms"),
                            "`timeout_ms` re-issues the op, so it only applies to the read-only op `status`",
                        );
                    }
                }
                // A control op drives a module that is attached in *this* run:
                // the scenario's `services:` block attaches it at launch, or an
                // earlier `service_ctl {op: attach}` step does. A step that can
                // only ever be refused is a scenario error, not a runtime one —
                // but the step that *does* the attaching is never one.
                if ctl.op != "attach" && !ctl.module.trim().is_empty() {
                    let attached_at_launch = scenario
                        .services
                        .iter()
                        .any(|service| service.id == ctl.module);
                    let attached_earlier = scenario.steps[..index].iter().any(|step| {
                        step.service_ctl.as_ref().is_some_and(|earlier| {
                            earlier.op == "attach" && earlier.module == ctl.module
                        })
                    });
                    if !attached_at_launch && !attached_earlier {
                        v.push(
                            &path,
                            format!(
                                "module {:?} is not attached by this scenario (no `services:` entry and no earlier `service_ctl: {{op: attach}}`); a control step can only drive an attached module",
                                ctl.module
                            ),
                        );
                    }
                }
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

    // ADR-0029 item 4: a `services:` entry must name a module and a profile
    // this registry knows; the runtime would refuse it anyway, but a scenario
    // that can never run must fail at parse time with a stable path.
    for (index, service) in scenario.services.iter().enumerate() {
        let path = format!("$.services[{index}]");
        if service.id.trim().is_empty() {
            v.push(format!("{path}.id"), "a service module id cannot be empty");
        }
        if let Some(profile) = &service.profile {
            if !SERVICE_PROFILES.contains(&profile.as_str()) {
                v.push(
                    format!("{path}.profile"),
                    format!(
                        "unknown profile {profile:?}; use one of {}",
                        SERVICE_PROFILES.join(", ")
                    ),
                );
            }
        }
        if let Some(config) = &service.config {
            if !config.is_object() {
                v.push(
                    format!("{path}.config"),
                    format!("config must be an object, got {config}"),
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

    /// SE1: the flag on the action payloads. A tap's is tri-state
    /// (`None` = the implicit "auto" tree rule, `true` = a change claim,
    /// `false` = A11YPACE's "transient effect, no change claimed"); the other
    /// actions keep the bool (default `false`, no claim).
    #[test]
    fn expect_change_parses_as_a_bool_and_defaults_false() {
        let scenario = Scenario::from_yaml_str(EXPECT_CHANGE).expect("valid scenario");
        assert!(scenario.validate().is_ok(), "{:?}", scenario.validate());
        assert_eq!(scenario.steps[1].tap.as_ref().unwrap().expect_change, Some(true));
        assert!(scenario.steps[2].back.as_ref().unwrap().expect_change);

        let plain = Scenario::from_yaml_str(
            "version: 1\nname: x\napp: y\nisolation: process\ndisplay: {mode: offscreen, width: 8, height: 8, density_dpi: 420}\nsteps:\n  - tap: {x: 1, y: 2}\n  - back: {}\n",
        )
        .expect("valid scenario");
        assert!(plain.validate().is_ok(), "{:?}", plain.validate());
        assert_eq!(plain.steps[0].tap.as_ref().unwrap().expect_change, None);
        assert!(!plain.steps[1].back.as_ref().unwrap().expect_change);
        // An unset flag is not written back: the wire form is unchanged.
        assert!(!plain.to_yaml_string().unwrap().contains("expect_change"));
    }

    /// A11YPACE: `expect_change: false` on a tap is a first-class value: it
    /// says "the effect is transient, no change claimed", it survives a
    /// round-trip (so a replayed scenario keeps it), and it is *not* subject to
    /// the pixel-claim rule that rejects `true` with fast/content.
    #[test]
    fn a11ypace_tap_expect_change_false_is_an_explicit_no_change_claim() {
        for settle in ["", ", settle: fast", ", settle: content"] {
            let yaml = format!(
                "version: 1\nname: x\napp: y\nisolation: process\ndisplay: {{mode: offscreen, width: 8, height: 8, density_dpi: 420}}\nsteps:\n  - tap: {{x: 1, y: 2, expect_change: false{settle}}}\n"
            );
            let scenario = Scenario::from_yaml_str(&yaml).expect("valid scenario");
            assert!(
                scenario.validate().is_ok(),
                "expect_change: false{settle} is not a pixel claim: {:?}",
                scenario.validate()
            );
            assert_eq!(
                scenario.steps[0].tap.as_ref().unwrap().expect_change,
                Some(false)
            );
            assert!(scenario.to_yaml_string().unwrap().contains("expect_change: false"));
        }
        // The pixel claim keeps its strict-only rule.
        let yaml = "version: 1\nname: x\napp: y\nisolation: process\ndisplay: {mode: offscreen, width: 8, height: 8, density_dpi: 420}\nsteps:\n  - tap: {x: 1, y: 2, settle: fast, expect_change: true}\n";
        let scenario = Scenario::from_yaml_str_unvalidated(yaml).expect("parses");
        assert!(!scenario.validate().is_ok());
    }

    /// METRO3: `dev_server` and `assert_tree` have the rules a dev loop needs.
    /// METRO4/METRO5: `assert_js_error` constraints and the `dev_menu` /
    /// `debug_http_host` additions to `dev_server`.
    #[test]
    fn metro4_assert_js_error_and_metro5_dev_server_rules() {
        let head = "version: 1\nname: m\napp: a.apk\nisolation: process\ndisplay: {mode: offscreen, width: 1080, height: 2400, density_dpi: 420}\nneeds_env: [metro]\nexpectation_reason: host needs node and Metro\nsteps:\n";
        let check = |body: &str| -> Vec<String> {
            match Scenario::from_yaml_str(&format!("{head}{body}")) {
                Ok(_) => Vec::new(),
                Err(error) => error.errors.iter().map(|e| e.message.clone()).collect(),
            }
        };
        for ok in [
            "  - assert_js_error: {name: SyntaxError, file: App.js, line: 26}\n",
            "  - assert_js_error: {message: boom, symbolicated: true, timeout_ms: 5000}\n",
            "  - assert_js_error: {none: true, since_previous_step: true}\n",
            "  - assert_js_error: {bundle_unreachable: true}\n",
            "  - dev_server: {op: start, project: p, port: 8081, debug_http_host: true}\n  - dev_server: {op: dev_menu}\n",
        ] {
            assert!(check(ok).is_empty(), "{ok}: {:?}", check(ok));
        }
        for (bad, needle) in [
            ("  - assert_js_error: {}\n", "needs `none: true`"),
            ("  - assert_js_error: {none: true, name: TypeError}\n", "cannot be combined"),
            ("  - assert_js_error: {bundle_unreachable: true, message: x}\n", "cannot be combined"),
            ("  - assert_js_error: {line: 3}\n", "`line` needs `file`"),
            ("  - assert_js_error: {name: \"\"}\n", "must not be empty"),
            ("  - assert_js_error: {none: false}\n", "says nothing"),
            ("  - assert_js_error: {name: E, timeout_ms: 0}\n", "between 1 and 300000"),
            ("  - assert_js_error: {name: E, nope: 1}\n", "unknown field"),
            ("  - dev_server: {op: dev_menu}\n", "no dev server is running"),
            ("  - launch: {activity: a/.M}\n  - dev_server: {op: start, project: p, port: 8081, debug_http_host: true}\n", "before the first `launch`"),
            ("  - dev_server: {op: start, project: p, port: 8081}\n  - dev_server: {op: reload, debug_http_host: true}\n", "only applies to op `start`"),
            ("  - dev_server: {op: start, project: p, port: 8081}\n  - dev_server: {op: dev_menu, timeout_ms: 5}\n", "only applies to"),
        ] {
            let problems = check(bad);
            assert!(problems.iter().any(|m| m.contains(needle)), "{bad}: {problems:?}");
        }
    }

    #[test]
    fn metro3_dev_server_and_assert_tree_rules() {
        let head = "version: 1\nname: m\napp: a.apk\nisolation: process\ndisplay: {mode: offscreen, width: 1080, height: 2400, density_dpi: 420}\nneeds_env: [metro]\nexpectation_reason: host needs node and Metro\nsteps:\n";
        let check = |body: &str| -> Vec<String> {
            match Scenario::from_yaml_str(&format!("{head}{body}")) {
                Ok(_) => Vec::new(),
                Err(error) => error.errors.iter().map(|e| e.message.clone()).collect(),
            }
        };
        let start = "  - dev_server: {op: start, project: p, port: 8081}\n";
        let ok = format!(
            "{start}  - dev_server: {{op: edit, file: App.js, find: a, replace: b}}\n  - dev_server: {{op: wait_serves, contains: b, timeout_ms: 1000}}\n  - dev_server: {{op: fast_refresh}}\n  - dev_server: {{op: reload}}\n  - dev_server: {{op: stop}}\n"
        );
        assert!(check(&ok).is_empty(), "{:?}", check(&ok));
        // order: nothing before `start`
        assert!(check("  - dev_server: {op: reload}\n").iter().any(|m| m.contains("no dev server is running")));
        // the original tree and node_modules are never editable
        for bad in ["../x.js", "/etc/passwd", "node_modules/react/index.js"] {
            let body = format!("{start}  - dev_server: {{op: edit, file: {bad}, contents: x}}\n");
            assert!(!check(&body).is_empty(), "{bad} must be refused");
        }
        // an edit says what to change: both forms or neither is an error
        assert!(!check(&format!("{start}  - dev_server: {{op: edit, file: a.js}}\n")).is_empty());
        assert!(!check(&format!("{start}  - dev_server: {{op: edit, file: a.js, find: x, replace: y, contents: z}}\n")).is_empty());
        // a wait_serves with nothing to wait for would be a sleep
        assert!(!check(&format!("{start}  - dev_server: {{op: wait_serves}}\n")).is_empty());
        // `metro` must be declared
        let undeclared = format!("{head}{start}").replace("needs_env: [metro]\n", "");
        let scenario = Scenario::from_yaml_str_unvalidated(&undeclared).unwrap();
        assert!(scenario.validate().errors.iter().any(|e| e.message.contains("needs_env: [metro]")));
        // assert_tree: exactly one mode, and a recorded label to compare with
        assert!(!check("  - assert_tree: {}\n").is_empty());
        assert!(!check("  - assert_tree: {not_grown_since: never}\n").is_empty());
        assert!(check("  - assert_tree: {record: before}\n  - assert_tree: {not_grown_since: before}\n").is_empty());
        assert!(KNOWN_NEEDS_ENV.contains(&"metro"));
    }

    /// GATENE2E: `service_ctl.timeout_ms` is a wait for `expect`, so it needs one,
    /// and it re-issues the op, so only the read-only `status` may carry it.
    #[test]
    fn gatene2e_service_ctl_timeout_rules() {
        let head = "version: 1\nname: x\napp: y\nisolation: process\ndisplay: {mode: offscreen, width: 8, height: 8, density_dpi: 420}\nservices:\n  - id: accessibility\n    profile: real\nsteps:\n";
        let ok = format!("{head}  - service_ctl: {{module: accessibility, op: status, expect: {{service_bound: true}}, timeout_ms: 5000}}\n");
        let scenario = Scenario::from_yaml_str(&ok).expect("valid");
        assert_eq!(scenario.steps[0].service_ctl.as_ref().unwrap().timeout_ms, Some(5000));
        assert!(scenario.to_yaml_string().unwrap().contains("timeout_ms: 5000"));
        for (step, why) in [
            ("{module: accessibility, op: status, timeout_ms: 5000}", "no expect"),
            ("{module: accessibility, op: enable, expect: {a: 1}, timeout_ms: 5000}", "not status"),
            ("{module: accessibility, op: status, expect: {a: 1}, timeout_ms: 0}", "zero"),
            ("{module: accessibility, op: status, expect: {a: 1}, timeout_ms: 999999}", "too long"),
        ] {
            let yaml = format!("{head}  - service_ctl: {step}\n");
            let scenario = Scenario::from_yaml_str_unvalidated(&yaml).expect("parses");
            assert!(!scenario.validate().is_ok(), "{why} must be rejected");
        }
    }

    const EXPECT_HANDLED: &str = r#"
version: 1
name: expect-handled
app: fixtures/build/apps/counter.apk
isolation: process
display: {mode: offscreen, width: 1080, height: 2400, density_dpi: 420}
steps:
  - launch: {activity: .MainActivity}
  - tap: {selector: {id: title}, expect_handled: false}
  - tap: {selector: {id: inc}, expect_handled: true}
  - key: {key: back, expect_handled: false}
  - back: {expect_handled: true}
  - swipe: {direction: up, expect_handled: false}
"#;

    /// REL3H2: `expect_handled` is an optional tri-state on every input step -
    /// unset (the step passes as measured, with the warning), `true` (the claim
    /// that is enforced) and `false` (the annotation that silences the warning).
    #[test]
    fn expect_handled_parses_and_defaults_to_unset() {
        let scenario = Scenario::from_yaml_str(EXPECT_HANDLED).expect("valid scenario");
        assert!(scenario.validate().is_ok(), "{:?}", scenario.validate());
        assert_eq!(
            scenario.steps[1].tap.as_ref().unwrap().expect_handled,
            Some(false)
        );
        assert_eq!(
            scenario.steps[2].tap.as_ref().unwrap().expect_handled,
            Some(true)
        );
        assert_eq!(
            scenario.steps[3].key.as_ref().unwrap().expect_handled,
            Some(false)
        );
        assert_eq!(
            scenario.steps[4].back.as_ref().unwrap().expect_handled,
            Some(true)
        );
        assert_eq!(
            scenario.steps[5].swipe.as_ref().unwrap().expect_handled,
            Some(false)
        );

        let plain = Scenario::from_yaml_str(
            "version: 1\nname: x\napp: y\nisolation: process\ndisplay: {mode: offscreen, width: 8, height: 8, density_dpi: 420}\nsteps:\n  - tap: {x: 1, y: 2}\n  - back: {}\n",
        )
        .expect("valid scenario");
        assert_eq!(plain.steps[0].tap.as_ref().unwrap().expect_handled, None);
        assert_eq!(plain.steps[1].back.as_ref().unwrap().expect_handled, None);
        // Unset is not written back: the wire form of an existing scenario is
        // byte-identical (the same rule `expect_change: false` follows).
        assert!(!plain.to_yaml_string().unwrap().contains("expect_handled"));
    }

    /// REL3H2: a non-bool `expect_handled` is a parse error, never a silently
    /// ignored field that would turn the claim into a no-op.
    #[test]
    fn expect_handled_rejects_a_non_bool() {
        for value in ["yes", "1", "\"true\"", "[true]"] {
            let yaml = format!(
                "version: 1\nname: x\napp: y\nisolation: process\ndisplay: {{mode: offscreen, width: 8, height: 8, density_dpi: 420}}\nsteps:\n  - tap: {{x: 1, y: 2, expect_handled: {value}}}\n"
            );
            let error = Scenario::from_yaml_str(&yaml)
                .err()
                .unwrap_or_else(|| panic!("expect_handled: {value} must not parse"));
            assert!(error
                .errors
                .iter()
                .all(|e| e.code == ErrorCode::InvalidScenario));
        }
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
            assert!(error
                .errors
                .iter()
                .all(|e| e.code == ErrorCode::InvalidScenario));
        }
    }

    /// SE1: `expect_change` + `settle: fast` is an unverifiable pass, so it is
    /// rejected at parse time with a stable path - and never at run time after
    /// the action already happened.
    #[test]
    fn expect_change_with_fast_settle_is_invalid() {
        for (kind, body) in [
            (
                "tap",
                "{selector: {id: inc}, settle: fast, expect_change: true}",
            ),
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

    /// REL3S: `swipe` and `scroll_until` are first-class steps. Geometry is
    /// either two points or a direction (+ distance), and a scroll search never
    /// accepts a non-strict settle (a found node must come from a settled
    /// observation).
    #[test]
    fn swipe_and_scroll_until_parse_and_validate() {
        let yaml = r#"
version: 1
name: swipe
app: fixtures/build/apps/scroll-list.apk
isolation: process
display: {mode: offscreen, width: 1080, height: 2400, density_dpi: 420}
steps:
  - launch: {activity: .MainActivity}
  - swipe: {from: {x: 540, y: 1800}, to: {x: 540, y: 600}, duration_ms: 300, fling: true}
  - swipe: {direction: up, distance: 900}
  - scroll_until: {selector: {text: {exact: "Row 80"}}, direction: up, fling: true, max_attempts: 12}
  - scroll_until: {selector: {text: {exact: "Row 90"}}, from: {x: 540, y: 1900}, to: {x: 540, y: 500}}
"#;
        let scenario = Scenario::from_yaml_str(yaml).expect("parses");
        assert!(scenario.validate().is_ok(), "{:?}", scenario.validate());
        assert_eq!(scenario.steps[1].kind(), "swipe");
        assert_eq!(scenario.steps[3].kind(), "scroll_until");
        let swipe = scenario.steps[2].swipe.as_ref().expect("direction swipe");
        assert_eq!(swipe.direction, Some(SwipeDirection::Up));
        assert_eq!(swipe.distance, Some(900));
        let scroll = scenario.steps[3]
            .scroll_until
            .as_ref()
            .expect("scroll_until");
        assert_eq!(scroll.max_attempts, Some(12));
        // The default form still serializes the fields it was given only.
        let text = scenario.to_yaml_string().unwrap();
        assert!(text.contains("scroll_until"), "{text}");
    }

    /// REL3S: half a gesture, both geometry forms at once, a zero-length
    /// gesture and an unbounded search are all scenario bugs, reported with a
    /// stable path before anything runs.
    #[test]
    fn swipe_geometry_errors_have_paths() {
        for (body, expected_path) in [
            ("{from: {x: 1, y: 2}}", "steps[1].swipe"),
            (
                "{from: {x: 1, y: 2}, to: {x: 1, y: 900}, direction: up}",
                "steps[1].swipe",
            ),
            ("{from: {x: 5, y: 5}, to: {x: 5, y: 5}}", "steps[1].swipe.to"),
            ("{}", "steps[1].swipe"),
        ] {
            let yaml = format!(
                "version: 1\nname: x\napp: y\nisolation: process\ndisplay: {{mode: offscreen, width: 8, height: 8, density_dpi: 420}}\nsteps:\n  - launch: {{activity: .MainActivity}}\n  - swipe: {body}\n"
            );
            let scenario = Scenario::from_yaml_str_unvalidated(&yaml).expect("parses");
            let validation = scenario.validate();
            assert!(!validation.is_ok(), "swipe {body} must be rejected");
            assert!(
                validation.errors.iter().any(|e| e.path == expected_path),
                "swipe {body}: expected {expected_path}, got {:?}",
                validation.errors
            );
        }
        let yaml = "version: 1\nname: x\napp: y\nisolation: process\ndisplay: {mode: offscreen, width: 8, height: 8, density_dpi: 420}\nsteps:\n  - scroll_until: {selector: {text: {exact: \"x\"}}, direction: up, max_attempts: 0}\n";
        let scenario = Scenario::from_yaml_str_unvalidated(yaml).expect("parses");
        let validation = scenario.validate();
        assert!(validation
            .errors
            .iter()
            .any(|e| e.path == "steps[0].scroll_until.max_attempts"));
    }

    /// REL3S: `up` is the direction the finger travels. The gesture starts at
    /// the bottom half and ends in the top half, so the content scrolls up and
    /// later rows come into view (the same rule the CLI uses).
    #[test]
    fn swipe_direction_up_moves_the_finger_toward_the_top() {
        let ((x1, y1), (x2, y2)) = SwipeDirection::Up.endpoints(Some(1000), 1080, 2400);
        assert_eq!((x1, x2), (540.0, 540.0));
        assert_eq!((y1, y2), (1700.0, 700.0));
        let ((x1, _), (x2, _)) = SwipeDirection::Left.endpoints(None, 1000, 2000);
        assert!(x1 > x2, "left moves the finger toward x=0");
        let ((_, y1), (_, y2)) = SwipeDirection::Down.endpoints(Some(400), 1000, 2000);
        assert!(y1 < y2, "down moves the finger toward the bottom");
    }
}
