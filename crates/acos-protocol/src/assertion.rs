//! Assertion types shared by the `assert` command and scenario steps.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ui::{Bounds, Selector, TextMatch};

/// A rectangle excluded from screenshot comparison (clock, status bar, ...).
///
/// Bounds are **display pixels**, like every other rectangle in the scenario
/// format; the executor maps them onto the frame with the frame's
/// `display_origin`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RegionMask {
    /// Screen region to mask out.
    pub bounds: Bounds,
    /// Why this region is nondeterministic (documented, not decorative).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Screenshot regression assertion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ScreenshotAssertion {
    /// Baseline PNG path, relative to the scenario file or workspace root.
    pub baseline: String,
    /// Maximum allowed fraction of changed pixels in `0.0..=1.0`.
    pub tolerance: f64,
    /// Regions excluded from the comparison.
    #[serde(default)]
    pub region_masks: Vec<RegionMask>,
}

/// A host-side pixel/region assertion over one freshly captured frame.
///
/// **Coordinates are display pixels** (the same space as UI-tree bounds and
/// tap `x`/`y`), not PNG pixels. The executor converts them with the frame's
/// [`acos_protocol::FrameMeta::display_origin`]; a region the frame does not
/// cover (the status bar or navigation band above/below the ART app-area
/// capture) fails loudly instead of cropping a shifted region.
///
/// At least one criterion must be present; `text_ocr` is recognised but has no
/// implementation in this build (the executor returns `UNSUPPORTED_ASSERTION`,
/// never a pass). `hash` is the lowercase/uppercase hex SHA-256 of the cropped
/// region's tightly packed RGBA8 bytes (row-major). `equals_png` names a PNG
/// relative to the scenario file; it must be the same size as the region, or the
/// same size as the frame (in which case the same region is cropped from it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RegionAssertion {
    /// Region left edge in display pixels.
    pub x: i32,
    /// Region top edge in display pixels.
    pub y: i32,
    /// Region width in pixels (must be > 0).
    pub w: u32,
    /// Region height in pixels (must be > 0).
    pub h: u32,
    /// Exact RGBA reference image for the region.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub equals_png: Option<String>,
    /// Expected SHA-256 of the region's RGBA8 bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    /// Minimum number of distinct RGBA colours in the region.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_distinct_colors: Option<u32>,
    /// Maximum number of distinct RGBA colours in the region.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_distinct_colors: Option<u32>,
    /// Text expected to appear in the region (OCR is not implemented today).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_ocr: Option<String>,
}

/// Assert a node's text (and optionally its exact bounds).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TextAssertion {
    /// Which node to inspect.
    pub selector: Selector,
    /// Expected text matcher.
    pub text: TextMatch,
    /// Optional exact expected screen bounds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounds: Option<Bounds>,
    /// Event-driven timeout; the runtime waits for a matching frame/tree, never
    /// for a fixed sleep.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

/// Assert a node is visible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VisibilityAssertion {
    /// Which node to inspect.
    pub selector: Selector,
    /// Optional exact expected bounds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounds: Option<Bounds>,
    /// Event-driven timeout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

/// Assert node state flags.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StateAssertion {
    /// Which node to inspect.
    pub selector: Selector,
    /// Expected visibility.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible: Option<bool>,
    /// Expected enabled flag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Expected focus flag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused: Option<bool>,
    /// Expected clickable flag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clickable: Option<bool>,
    /// Expected scrollable flag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scrollable: Option<bool>,
    /// Expected checked flag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked: Option<bool>,
    /// Expected selected flag (`View.isSelected()`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected: Option<bool>,
    /// Event-driven timeout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

/// Assert a logcat-style line sequence.
///
/// Each pattern is a Rust regex. With `ordered: true` the patterns must match in
/// order (extra lines are allowed); with `ordered: false` each pattern must match
/// at least once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LogAssertion {
    /// Log tag to filter on (for example `AcosFixture`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    /// Regex patterns to match against log messages.
    pub patterns: Vec<String>,
    /// Require the patterns in order.
    #[serde(default = "default_true")]
    pub ordered: bool,
    /// Only consider log events produced since the previous scenario step.
    /// This is what makes lifecycle assertions step-scoped instead of matching
    /// an earlier cycle.
    #[serde(default)]
    pub since_previous_step: bool,
    /// Event-driven timeout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// Minimum number of matched lines required per pattern.
    #[serde(default = "default_one")]
    pub min_matches: usize,
}

const fn default_true() -> bool {
    true
}

const fn default_one() -> usize {
    1
}

/// Assert that no crash/ANR event occurred.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct NoCrashAssertion {
    /// Only consider events with `seq > since_seq`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_seq: Option<u64>,
}

/// Any assertion the `assert` command can evaluate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Assertion {
    /// Node text (and optional bounds).
    Text(TextAssertion),
    /// Node visibility.
    Visible(VisibilityAssertion),
    /// Node invisibility.
    NotVisible(VisibilityAssertion),
    /// Node state flags.
    State(StateAssertion),
    /// Log line sequence.
    Log(LogAssertion),
    /// No crash/ANR.
    NoCrash(NoCrashAssertion),
    /// Screenshot regression.
    Screenshot(ScreenshotAssertion),
}

impl Assertion {
    /// Stable kind string for artifacts.
    pub const fn kind(&self) -> &'static str {
        match self {
            Assertion::Text(_) => "text",
            Assertion::Visible(_) => "visible",
            Assertion::NotVisible(_) => "not_visible",
            Assertion::State(_) => "state",
            Assertion::Log(_) => "log",
            Assertion::NoCrash(_) => "no_crash",
            Assertion::Screenshot(_) => "screenshot",
        }
    }
}

/// Structured expected-vs-actual detail for a failed assertion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AssertionDiff {
    /// What the scenario/command expected.
    pub expected: Value,
    /// What the runtime observed.
    pub actual: Value,
    /// Optional pointer to the mismatching field (`nodes[1].text`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Human-readable explanation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// The result of one assertion, stored per step in the verdict.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AssertionResult {
    /// Assertion kind (`text`, `visible`, `log`, ...).
    pub kind: String,
    /// Whether the assertion held.
    pub passed: bool,
    /// Present when the assertion failed or produced an explainable result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<AssertionDiff>,
    /// Node ids that were inspected (empty for log/crash assertions).
    #[serde(default)]
    pub matched_node_ids: Vec<String>,
    /// Optional short explanation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}
