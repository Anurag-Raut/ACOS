//! The UI tree and selector model.
//!
//! Selector semantics are deliberately small and stable:
//!
//! * `id` matches the full Android resource id exactly. A bare local id without
//!   `/` also matches the suffix after the last `/` (`btn` matches
//!   `dev.acos.fixtures.helloviews:id/btn`).
//! * `text` / `content_desc` are exact, substring (`contains`) or Rust-regex
//!   (`regex`, unanchored `is_match`) matches. Matching is case-sensitive.
//! * `class` is an exact match.
//! * `bounds` is exact equality of integer screen bounds.
//! * All specified fields are ANDed.
//! * `index` selects the 0-based occurrence among the nodes matching the other
//!   fields, in stable document order (pre-order depth-first).
//! * A selector with no fields is invalid (`INVALID_ARGUMENT` / `INVALID_SCENARIO`).
//! * Document order is pre-order depth-first with children in stored order.

use regex::Regex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::frame::FrameMeta;

/// Integer screen bounds `[left, top, right, bottom]` in pixels.
///
/// Serialized as a four-element array. `right`/`bottom` are exclusive at the
/// Android/UI-automator pixel-grid level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct Bounds(pub [i32; 4]);

impl Bounds {
    /// Build from edges.
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Bounds([left, top, right, bottom])
    }

    /// Left edge.
    pub const fn left(self) -> i32 {
        self.0[0]
    }

    /// Top edge.
    pub const fn top(self) -> i32 {
        self.0[1]
    }

    /// Right edge.
    pub const fn right(self) -> i32 {
        self.0[2]
    }

    /// Bottom edge.
    pub const fn bottom(self) -> i32 {
        self.0[3]
    }

    /// Width in pixels (may be negative for invalid bounds).
    pub const fn width(self) -> i32 {
        self.0[2] - self.0[0]
    }

    /// Height in pixels (may be negative for invalid bounds).
    pub const fn height(self) -> i32 {
        self.0[3] - self.0[1]
    }

    /// True when the rectangle has non-negative width/height.
    pub const fn is_valid(self) -> bool {
        self.0[0] <= self.0[2] && self.0[1] <= self.0[3]
    }

    /// Center point.
    pub const fn center(self) -> (i32, i32) {
        (self.0[0] + self.width() / 2, self.0[1] + self.height() / 2)
    }
}

impl std::fmt::Display for Bounds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "[{},{}][{},{}]",
            self.0[0], self.0[1], self.0[2], self.0[3]
        )
    }
}

impl From<[i32; 4]> for Bounds {
    fn from(value: [i32; 4]) -> Self {
        Bounds(value)
    }
}

impl std::str::FromStr for Bounds {
    type Err = BoundsParseError;

    /// Parse the uiautomator form `[42,317][231,368]`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let inner = s
            .strip_prefix('[')
            .and_then(|s| s.strip_suffix(']'))
            .ok_or_else(|| BoundsParseError(s.to_string()))?;
        let (first, second) = inner
            .split_once("][")
            .ok_or_else(|| BoundsParseError(s.to_string()))?;
        let mut nums = Vec::with_capacity(4);
        for part in [first, second] {
            for n in part.split(',') {
                nums.push(
                    n.trim()
                        .parse::<i32>()
                        .map_err(|_| BoundsParseError(s.to_string()))?,
                );
            }
        }
        if nums.len() != 4 {
            return Err(BoundsParseError(s.to_string()));
        }
        Ok(Bounds([nums[0], nums[1], nums[2], nums[3]]))
    }
}

/// Bounds string parse failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid bounds (expected `[left,top][right,bottom]`): {0}")]
pub struct BoundsParseError(pub String);

/// A text matching rule: exactly one of `exact`, `contains`, `regex`.
///
/// A struct rather than an enum so the agent-facing YAML/JSON form is a plain
/// map (`text: {exact: "Greet"}`) and not a YAML `!tag`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TextMatch {
    /// Full string equality (no trimming or normalisation).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exact: Option<String>,
    /// Substring containment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contains: Option<String>,
    /// Unanchored Rust regex (`is_match`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regex: Option<String>,
}

impl TextMatch {
    /// Exact matcher.
    pub fn exact(s: impl Into<String>) -> Self {
        TextMatch {
            exact: Some(s.into()),
            ..TextMatch::default()
        }
    }

    /// Contains matcher.
    pub fn contains(s: impl Into<String>) -> Self {
        TextMatch {
            contains: Some(s.into()),
            ..TextMatch::default()
        }
    }

    /// Regex matcher.
    pub fn regex(s: impl Into<String>) -> Self {
        TextMatch {
            regex: Some(s.into()),
            ..TextMatch::default()
        }
    }

    /// The populated matcher kind, when exactly one is set.
    pub fn kind(&self) -> Option<&'static str> {
        match (&self.exact, &self.contains, &self.regex) {
            (Some(_), None, None) => Some("exact"),
            (None, Some(_), None) => Some("contains"),
            (None, None, Some(_)) => Some("regex"),
            _ => None,
        }
    }

    /// Evaluate against a node string. An invalid matcher matches nothing.
    pub fn matches(&self, haystack: &str) -> bool {
        match (&self.exact, &self.contains, &self.regex) {
            (Some(needle), None, None) => haystack == needle,
            (None, Some(needle), None) => haystack.contains(needle),
            (None, None, Some(pattern)) => Regex::new(pattern)
                .map(|re| re.is_match(haystack))
                .unwrap_or(false),
            _ => false,
        }
    }

    /// Validate the matcher, returning a human-readable problem if any.
    pub fn validate(&self) -> Option<String> {
        match (&self.exact, &self.contains, &self.regex) {
            (Some(s), None, None) | (None, Some(s), None) => {
                if s.is_empty() {
                    Some("text matcher must not be empty".to_string())
                } else {
                    None
                }
            }
            (None, None, Some(pattern)) => {
                if pattern.is_empty() {
                    return Some("regex must not be empty".to_string());
                }
                Regex::new(pattern)
                    .err()
                    .map(|e| format!("invalid regex: {e}"))
            }
            _ => Some("text matcher must set exactly one of exact/contains/regex".to_string()),
        }
    }

    /// Human-readable description for logs and diffs.
    pub fn describe(&self) -> String {
        match (&self.exact, &self.contains, &self.regex) {
            (Some(s), None, None) => format!("exactly {s:?}"),
            (None, Some(s), None) => format!("containing {s:?}"),
            (None, None, Some(s)) => format!("matching /{s}/"),
            _ => "invalid text matcher".to_string(),
        }
    }

    /// Compact selector-field form: `text=="Increment"` (exact),
    /// `text*="crement"` (contains), `text~/^Incr/` (regex).
    fn field_form(&self, field: &str) -> String {
        match (&self.exact, &self.contains, &self.regex) {
            (Some(s), None, None) => format!("{field}=={s:?}"),
            (None, Some(s), None) => format!("{field}*={s:?}"),
            (None, None, Some(s)) => format!("{field}~/{s}/"),
            _ => format!("{field}==<invalid matcher>"),
        }
    }
}

/// A node selector. See the module docs for the stable matching rules.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Selector {
    /// Android resource id (`pkg:id/name`), or a bare local id (`name`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Text matcher.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<TextMatch>,
    /// Hint matcher (`TextView.getHint()`, e.g. an EditText placeholder).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<TextMatch>,
    /// Content-description matcher.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_desc: Option<TextMatch>,
    /// Exact view class.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<String>,
    /// Exact screen bounds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounds: Option<Bounds>,
    /// Zero-based occurrence among otherwise matching nodes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<usize>,
    /// Also match nodes that are not visible to the user.
    ///
    /// Default `false`: a selector matches only nodes the user can see
    /// (`UiNode::visible`, which follows `View.isVisibleToUser()`: shown, and
    /// its clipped visible rect non-empty). A warm-mounted page parked off
    /// screen is present in the tree but is not a selector match, so
    /// `wait_for`/`assert_text`/`assert_visible`/`tap` can never silently act on
    /// invisible content. Set it to `true` only for a deliberate check of an
    /// off-screen/invisible node (`assert_not_visible`, `assert_state
    /// {visible: false}`, diagnostics).
    #[serde(default, skip_serializing_if = "is_false")]
    pub include_invisible: bool,
}

impl Selector {
    /// A selector by resource id.
    pub fn by_id(id: impl Into<String>) -> Self {
        Selector {
            id: Some(id.into()),
            ..Selector::default()
        }
    }

    /// A selector by exact text.
    pub fn by_text(text: impl Into<String>) -> Self {
        Selector {
            text: Some(TextMatch::exact(text)),
            ..Selector::default()
        }
    }

    /// A selector by exact class.
    pub fn by_class(class: impl Into<String>) -> Self {
        Selector {
            class: Some(class.into()),
            ..Selector::default()
        }
    }

    /// True when no matching field is present.
    ///
    /// `include_invisible` is a matching *policy*, not a field, so it never
    /// makes an otherwise empty selector valid.
    pub fn is_empty(&self) -> bool {
        self.id.is_none()
            && self.text.is_none()
            && self.hint.is_none()
            && self.content_desc.is_none()
            && self.class.is_none()
            && self.bounds.is_none()
            && self.index.is_none()
    }

    /// All semantic problems with this selector (empty for a valid selector).
    pub fn validation_errors(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if self.is_empty() {
            errors.push(
                "selector is empty; set at least one of id/text/hint/content_desc/class/bounds"
                    .to_string(),
            );
            return errors;
        }
        if let Some(id) = &self.id {
            if id.is_empty() {
                errors.push("selector.id must not be empty".to_string());
            }
        }
        if let Some(class) = &self.class {
            if class.is_empty() {
                errors.push("selector.class must not be empty".to_string());
            }
        }
        if let Some(text) = &self.text {
            if let Some(err) = text.validate() {
                errors.push(format!("selector.text: {err}"));
            }
        }
        if let Some(hint) = &self.hint {
            if let Some(err) = hint.validate() {
                errors.push(format!("selector.hint: {err}"));
            }
        }
        if let Some(desc) = &self.content_desc {
            if let Some(err) = desc.validate() {
                errors.push(format!("selector.content_desc: {err}"));
            }
        }
        if let Some(bounds) = self.bounds {
            if !bounds.is_valid() {
                errors.push(format!(
                    "selector.bounds must satisfy left<=right and top<=bottom: {bounds}"
                ));
            }
        }
        errors
    }

    /// True when a single node matches all specified fields (index excluded).
    pub fn matches_node(&self, node: &UiNode) -> bool {
        if let Some(id) = &self.id {
            let matches = node.resource_id == *id
                || node
                    .resource_id
                    .rsplit_once('/')
                    .map(|(_, local)| local == id)
                    .unwrap_or(false);
            if !matches {
                return false;
            }
        }
        if let Some(text) = &self.text {
            if !text.matches(&node.text) && !text.matches(&node.hint) {
                return false;
            }
        }
        if let Some(hint) = &self.hint {
            if !hint.matches(&node.hint) {
                return false;
            }
        }
        if let Some(desc) = &self.content_desc {
            if !desc.matches(&node.content_desc) {
                return false;
            }
        }
        if let Some(class) = &self.class {
            if node.class != *class {
                return false;
            }
        }
        if let Some(bounds) = self.bounds {
            if node.bounds != bounds {
                return false;
            }
        }
        true
    }

    /// Whether this selector accepts `node` at all: the structural match plus
    /// the user-visibility rule (a selector matches only user-visible nodes
    /// unless it explicitly opts into invisible ones).
    pub fn accepts_node(&self, node: &UiNode) -> bool {
        (self.include_invisible || node.visible) && self.matches_node(node)
    }

    /// The human form used in error messages and the stderr trailer.
    ///
    /// `text=="Nonexistent"`, `content_desc=="X"`, `id=="pkg:id/btn"`,
    /// `class==android.widget.TextView`, `bounds=[0,0][100,100]`,
    /// `index=1 include_invisible=true`. Fields are space-separated in a stable
    /// order, so an agent can copy one straight back into a YAML scenario.
    pub fn human(&self) -> String {
        let mut fields: Vec<String> = Vec::new();
        if let Some(id) = &self.id {
            fields.push(format!("id=={id:?}"));
        }
        if let Some(text) = &self.text {
            fields.push(text.field_form("text"));
        }
        if let Some(hint) = &self.hint {
            fields.push(hint.field_form("hint"));
        }
        if let Some(desc) = &self.content_desc {
            fields.push(desc.field_form("content_desc"));
        }
        if let Some(class) = &self.class {
            fields.push(format!("class=={class}"));
        }
        if let Some(bounds) = self.bounds {
            fields.push(format!("bounds={bounds}"));
        }
        if let Some(index) = self.index {
            fields.push(format!("index={index}"));
        }
        if self.include_invisible {
            fields.push("include_invisible=true".to_string());
        }
        if fields.is_empty() {
            return "<empty selector>".to_string();
        }
        fields.join(" ")
    }
}

impl std::fmt::Display for Selector {
    /// Never a Rust `Debug` dump: an error line an agent reads says
    /// `text=="Nonexistent"`, not `Selector { id: None, text: Some(..) }`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.human())
    }
}

const fn is_false(value: &bool) -> bool {
    !*value
}

/// One UI node.
///
/// `id` is a tree-local stable node identity (for example `n7`), not the Android
/// resource id; `resource_id` is the Android id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UiNode {
    /// Tree-local node id, stable for one tree snapshot.
    pub id: String,
    /// Fully qualified view class.
    pub class: String,
    /// Android resource id (empty when absent).
    #[serde(default)]
    pub resource_id: String,
    /// Visible text (empty when absent).
    #[serde(default)]
    pub text: String,
    /// `TextView.getHint()` (empty when absent); the placeholder an EditText
    /// shows while its text is empty.
    #[serde(default)]
    pub hint: String,
    /// Accessibility content description (empty when absent).
    #[serde(default)]
    pub content_desc: String,
    /// Screen bounds in pixels (the view's full rect).
    pub bounds: Bounds,
    /// The part of `bounds` the user can actually see: `bounds` clipped by every
    /// ancestor (scroll/pager clip) and by the window/display bounds, in screen
    /// pixels. `None` when the engine does not report it (older probes); use
    /// [`UiNode::visible_rect`] for the rect a tap may use.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible_bounds: Option<Bounds>,
    /// Whether the node is visible *to the user* (Android's `isVisibleToUser`:
    /// shown — it and all ancestors VISIBLE and attached — and its global
    /// visible rect, clipped by every ancestor and the window/display, is
    /// non-empty).
    pub visible: bool,
    /// Whether the node is enabled.
    pub enabled: bool,
    /// Whether the node has input focus.
    pub focused: bool,
    /// Whether the node accepts clicks.
    pub clickable: bool,
    /// Whether the node can scroll.
    pub scrollable: bool,
    /// Whether the node is checked (checkboxes/switches).
    pub checked: bool,
    /// Whether the node is selected (`View.isSelected()`).
    #[serde(default)]
    pub selected: bool,
    /// Whether this is a password field (`AccessibilityNodeInfo.isPassword()`,
    /// i.e. the view's `inputType` carries `TYPE_TEXT_VARIATION_PASSWORD`).
    ///
    /// The platform still reports the field's text (AOSP `TextView` sets the
    /// node text unconditionally and only *flags* the node), so this is what an
    /// input verb uses to decide to **mask** the typed text in its reply, logs
    /// and events. It never hides the tree.
    #[serde(default)]
    pub password: bool,
    /// Child nodes, in layout order.
    #[serde(default)]
    pub children: Vec<UiNode>,
    /// Owning package (empty when unknown).
    #[serde(default)]
    pub package: String,
    /// Window this node belongs to (empty when the engine reports one window
    /// or predates multi-window probes). `bounds` is local to this window;
    /// `bounds_on_screen` is display space.
    #[serde(default)]
    pub window: String,
    /// Stable id of the owning window inside this tree (`""` when the engine
    /// does not report one). Short on purpose: it is repeated on every node and
    /// REL3C caps reply size.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub window_id: String,
    /// Symbolic `LayoutParams.TYPE_*` name of the owning window (`""` when
    /// unknown). Never a made-up name: an undocumented type is `TYPE_<n>`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub window_type: String,
    /// Agent-facing window kind (`application`, `application_overlay`, …).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub window_kind: String,
    /// Z-order index of the owning window, `0` = bottom. `None` only when the
    /// node has no window tag (a probe that predates the window block).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_z: Option<i64>,
}

/// One window of a UI tree, in **policy z order** (bottom first).
///
/// This is the window-level half of the LAYER1 tree contract: every node names
/// its window, and this block names the window's type, kind, z and input state,
/// so an agent can tell an application overlay from the app without guessing
/// from the label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WindowInfo {
    /// Stable id inside this tree (short hex; `""` when the probe has none).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    /// The W36 `window` label the nodes carry (kept for compatibility).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    /// Symbolic `LayoutParams.TYPE_*` name.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub type_name: String,
    /// Agent-facing kind ([`acos_wm::layers::WindowKind::name`] vocabulary).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub kind: String,
    /// Z-order index, `0` = bottom, contiguous over this vector.
    pub z: i64,
    /// Window frame in display pixels, when the probe reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounds: Option<Bounds>,
    /// Whether this window holds input focus.
    #[serde(default)]
    pub focus: bool,
    /// Whether this window receives touches (`!FLAG_NOT_TOUCHABLE`); the value
    /// an agent uses to explain why a tap passed through an overlay.
    #[serde(default)]
    pub touchable: bool,
    /// The window asked for `FLAG_HARDWARE_ACCELERATED` but the guest has no
    /// window GPU renderer, so it was painted in software (`Canvas.
    /// isHardwareAccelerated()` is false). The verdict records a
    /// `hardware_renderer_fallback` note for it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub renderer_fallback: bool,
}

impl WindowInfo {
    /// Construct the minimal window record.
    pub fn new(z: i64) -> Self {
        WindowInfo {
            id: String::new(),
            label: String::new(),
            type_name: String::new(),
            kind: String::new(),
            z,
            bounds: None,
            focus: false,
            touchable: false,
            renderer_fallback: false,
        }
    }
}

impl UiNode {
    /// Construct a minimal visible/enabled node.
    pub fn new(id: impl Into<String>, class: impl Into<String>, bounds: Bounds) -> Self {
        UiNode {
            id: id.into(),
            class: class.into(),
            resource_id: String::new(),
            text: String::new(),
            hint: String::new(),
            content_desc: String::new(),
            bounds,
            visible_bounds: None,
            visible: true,
            enabled: true,
            focused: false,
            clickable: false,
            scrollable: false,
            checked: false,
            selected: false,
            password: false,
            children: Vec::new(),
            package: String::new(),
            window: String::new(),
            window_id: String::new(),
            window_type: String::new(),
            window_kind: String::new(),
            window_z: None,
        }
    }

    /// Builder: set the owning window label.
    pub fn with_window(mut self, window: impl Into<String>) -> Self {
        self.window = window.into();
        self
    }

    /// Builder: set Android id.
    pub fn with_resource_id(mut self, resource_id: impl Into<String>) -> Self {
        self.resource_id = resource_id.into();
        self
    }

    /// Builder: set text.
    pub fn with_text(mut self, text: impl Into<String>) -> Self {
        self.text = text.into();
        self
    }

    /// Builder: set clickable.
    pub fn clickable(mut self, clickable: bool) -> Self {
        self.clickable = clickable;
        self
    }

    /// Builder: set children.
    pub fn with_children(mut self, children: Vec<UiNode>) -> Self {
        self.children = children;
        self
    }

    /// Builder: set the clipped (user-visible) rect.
    pub fn with_visible_bounds(mut self, visible_bounds: Bounds) -> Self {
        self.visible_bounds = Some(visible_bounds);
        self
    }

    /// Builder: flag a password field.
    pub fn with_password(mut self, password: bool) -> Self {
        self.password = password;
        self
    }

    /// Builder: set the framework's focus flag.
    pub fn with_focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    /// The rect a tap on this node must use: the clipped visible rect when the
    /// engine reports one, otherwise the full bounds. Never a point outside it.
    pub fn visible_rect(&self) -> Bounds {
        self.visible_bounds.unwrap_or(self.bounds)
    }

    /// The screen point at the centre of the *visible* rect.
    pub fn tap_point(&self) -> (i32, i32) {
        self.visible_rect().center()
    }

    /// Depth-first walk including `self`.
    pub fn walk<'a>(&'a self, out: &mut Vec<&'a UiNode>) {
        out.push(self);
        for child in &self.children {
            child.walk(out);
        }
    }

    /// Display label used by `observe --text`.
    pub fn text_line(&self) -> String {
        format!(
            "{} | {} | {} | {}",
            self.class, self.resource_id, self.text, self.bounds
        )
    }
}

/// Where a pointer event at one screen point lands in a [`UiTree`].
///
/// This is the *tree geometry* answer to "what is under the tap", computed from
/// the same tree the tap's selectors were resolved against (the visible rect of
/// every node, deepest match first). It is deliberately separate from the
/// authoritative framework fact: whether the app's window **consumed** the
/// event is `handled` in the reply, which comes from the guest's
/// `InputEventReceiver.finishInputEvent(handled)` and is never derived from this
/// geometry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TapHit<'a> {
    /// The visible nodes whose rect contains the point, root first, deepest last.
    pub path: Vec<&'a UiNode>,
    /// The node that consumes a press at the point: the deepest node on the path
    /// that is `clickable`. `None` means nothing on that branch takes a click.
    pub consumer: Option<&'a UiNode>,
    /// When `consumer` is `None`, the clickable node nearest the point that is
    /// **not** on the path: the sibling or cousin an agent should tap instead
    /// (a switch's label next to the switch, a row's title next to its
    /// pressable). `None` when the tree shows no clickable node at all.
    pub suggestion: Option<&'a UiNode>,
}

impl TapHit<'_> {
    /// The deepest node at the point, when any node's rect contains it.
    #[must_use]
    pub fn node(&self) -> Option<&UiNode> {
        self.path.last().copied()
    }

    /// Squared centre distance from `(x, y)` to a node, the ranking used to pick
    /// a `suggestion`.
    fn distance(node: &UiNode, x: i32, y: i32) -> i64 {
        let (cx, cy) = node.visible_rect().center();
        let dx = i64::from(cx) - i64::from(x);
        let dy = i64::from(cy) - i64::from(y);
        dx * dx + dy * dy
    }
}

/// The child list that contains the node whose id is `id` (its siblings,
/// including itself), anywhere in the tree.
fn sibling_nodes<'a>(nodes: &'a [UiNode], id: &str) -> Option<&'a [UiNode]> {
    if nodes.iter().any(|node| node.id == id) {
        return Some(nodes);
    }
    nodes
        .iter()
        .find_map(|node| sibling_nodes(&node.children, id))
}

/// Append the visible nodes whose rect contains `(x, y)`, root first.
///
/// A hidden node hides its whole subtree, matching `visible`'s definition (a
/// node is visible only when it and every ancestor are shown).
fn hit_path<'a>(node: &'a UiNode, x: i32, y: i32, out: &mut Vec<&'a UiNode>) -> bool {
    if !node.visible {
        return false;
    }
    let rect = node.visible_rect();
    let inside = x >= rect.left() && x < rect.right() && y >= rect.top() && y < rect.bottom();
    if !inside {
        return false;
    }
    out.push(node);
    for child in &node.children {
        if hit_path(child, x, y, out) {
            break;
        }
    }
    true
}

/// One node as an input reply describes it: identity, its accessible name, its
/// geometry and the input flags an agent has to reason about.
///
/// `withhold_text` is the password rule: a focused password node's own text is
/// the secret the field holds, so it is reported as `null`, never echoed.
#[must_use]
pub fn node_input_json(node: &UiNode, withhold_text: bool) -> serde_json::Value {
    serde_json::json!({
        "id": node.id,
        "class": node.class,
        "resource_id": node.resource_id,
        "content_desc": node.content_desc,
        "text": if withhold_text { serde_json::Value::Null } else { serde_json::json!(node.text) },
        "bounds": node.bounds.to_string(),
        "visible_bounds": node.visible_bounds.map(|b| b.to_string()),
        "clickable": node.clickable,
        "scrollable": node.scrollable,
        "enabled": node.enabled,
        "focused": node.focused,
        "password": node.password,
        "window": node.window,
    })
}

/// The `target` block of a tap reply (REL3E).
///
/// It answers the two questions `state_changed: false` left open: **what the
/// selector resolved to** (`selector`, `node`) and **what the injected point
/// actually hit** (`point`, `hit`). `hit` is the tree-geometry answer computed
/// from the same tree the tap resolved against; whether the app consumed the
/// press is `handled` in the reply, which comes from the guest's
/// `finishInputEvent(handled)` and never from this geometry.
#[must_use]
pub fn tap_target_json(
    selector: Option<&serde_json::Value>,
    node: Option<&UiNode>,
    tree: Option<&UiTree>,
    x: i32,
    y: i32,
) -> serde_json::Value {
    let mut target = serde_json::Map::new();
    if let Some(selector) = selector {
        target.insert("selector".into(), selector.clone());
    }
    if let Some(node) = node {
        target.insert("node".into(), node_input_json(node, false));
    }
    target.insert("point".into(), serde_json::json!({"x": x, "y": y}));
    let mut hit_json = serde_json::Map::new();
    hit_json.insert("method".into(), serde_json::json!("tree-geometry"));
    match tree.map(|tree| tree.tap_hit(x, y)) {
        Some(hit) => {
            hit_json.insert(
                "node".into(),
                hit.node().map_or(serde_json::Value::Null, |node| {
                    node_input_json(node, false)
                }),
            );
            hit_json.insert(
                "clickable".into(),
                serde_json::json!(hit.node().is_some_and(|node| node.clickable)),
            );
            hit_json.insert(
                "consumer".into(),
                hit.consumer.map_or(serde_json::Value::Null, |node| {
                    node_input_json(node, false)
                }),
            );
            hit_json.insert(
                "suggest".into(),
                hit.suggestion.map_or(serde_json::Value::Null, |node| {
                    node_input_json(node, false)
                }),
            );
        }
        None => {
            // No tree: the point was never hit-tested. Say so rather than
            // pretending nothing is there.
            hit_json.insert("node".into(), serde_json::Value::Null);
            hit_json.insert("clickable".into(), serde_json::Value::Null);
            hit_json.insert("consumer".into(), serde_json::Value::Null);
            hit_json.insert("suggest".into(), serde_json::Value::Null);
            hit_json.insert(
                "note".into(),
                serde_json::json!("no bound tree to hit-test against"),
            );
        }
    }
    target.insert("hit".into(), serde_json::Value::Object(hit_json));
    serde_json::Value::Object(target)
}

/// The `name` an agent would use for a node: its text, else its content
/// description (the platform's accessible name), for a one-line hint.
fn hint_node_name(node: &serde_json::Value) -> String {
    for field in ["text", "content_desc"] {
        if let Some(value) = node.get(field).and_then(serde_json::Value::as_str) {
            if !value.trim().is_empty() {
                return format!("{value:?}");
            }
        }
    }
    String::from("(no name)")
}

/// REL3I3: which fact an input action's `handled` verdict rests on.
///
/// MERGE3I3 precedence (one rule for the whole class):
///
/// 1. a guest `handled: true` ack is the app's own proof and wins - some window
///    consumed the event;
/// 2. else the host's own record that the **delivered** window was removed
///    within the step is the next strongest fact: the press acted on that window
///    (a dismissed dialog), so the gesture is handled. This outranks a
///    `finishInputEvent(handled) = false` answer because that answer cannot exist
///    for a disposed `InputEventReceiver` and, when it does exist, describes a
///    window that is provably gone - the same gesture the removal consumed;
/// 3. else a complete set of acks is the guest's own answer (handled or not);
/// 4. else nothing was answered: `None`, never a guess.
///
/// `any_handled` is "at least one ack said `handled: true`", `acked` counts every
/// `FINISHED` ack (handled or not). The removal is token-scoped by the caller
/// (`was_removed_since`), so a *different* window's removal is never evidence.
#[must_use]
pub fn handled_source(
    published: usize,
    acked: usize,
    any_handled: bool,
    window_removed: bool,
) -> crate::HandledSource {
    use crate::HandledSource;
    if published == 0 {
        return HandledSource::None;
    }
    if any_handled {
        return HandledSource::Guest;
    }
    if window_removed {
        // Precedence over the all-unhandled answer below: the window this input
        // was delivered to is gone, so the host's removal is the proof.
        return HandledSource::HostWindowRemoved;
    }
    if acked >= published {
        return HandledSource::Guest;
    }
    HandledSource::None
}

/// REL3I3: the `handled` verdict an input action can report.
///
/// The same ladder as [`handled_source`], so the answer and the fact it rests on
/// can never disagree:
///
/// * a guest `handled: true` ack, or a complete handled ack set, stands as is;
/// * a guest `handled: false` (or no answer at all) is overridden to `true` only
///   when no ack claimed the event and the delivered window was removed - then
///   the host's removal is the answer;
/// * no removal leaves the guest's answer (or `None`), never a guess.
#[must_use]
pub fn handled_answer(
    handled: Option<bool>,
    published: usize,
    any_handled: bool,
    window_removed: bool,
) -> Option<bool> {
    if published == 0 {
        return handled;
    }
    if any_handled {
        return handled;
    }
    if window_removed {
        return Some(true);
    }
    handled
}

/// REL3E: the one-line reason a reply is not enough on its own.
///
/// It only ever explains the framework fact (`handled`) together with the
/// hit-test in `target`; it never invents a reason the evidence does not show.
/// `None` means the action needs no explanation (the app consumed it and the
/// screen changed).
#[must_use]
pub fn handling_hint(
    target: Option<&serde_json::Value>,
    handled: Option<bool>,
    state_changed: bool,
) -> Option<String> {
    let hit = target.and_then(|target| target.get("hit"));
    let consumer = hit
        .and_then(|hit| hit.get("consumer"))
        .filter(|value| !value.is_null());
    let suggest = hit
        .and_then(|hit| hit.get("suggest"))
        .filter(|value| !value.is_null());
    let hit_node = hit
        .and_then(|hit| hit.get("node"))
        .filter(|value| !value.is_null());
    match handled {
        None => Some(
            "the guest did not ack every event of this action (see handled_evidence.published \
             vs acked), so whether the app consumed it is unknown; nothing here claims it was \
             handled or that nothing happened"
                .to_string(),
        ),
        Some(false) => {
            if let Some(consumer) = consumer {
                return Some(format!(
                    "the app's window did NOT consume this input even though the point is on the \
                     clickable {} {} at {} (its own `finishInputEvent(handled)` answered false): \
                     the control is disabled or a parent intercepted the press, so no state change \
                     is expected",
                    consumer["class"].as_str().unwrap_or("view"),
                    hint_node_name(consumer),
                    consumer["bounds"].as_str().unwrap_or("?"),
                ));
            }
            let Some(hit_node) = hit_node else {
                return Some(
                    "nothing at this point consumed the input and the tree has no node there \
                     (the app's window answered `finishInputEvent(handled) = false`); nothing \
                     changed because nothing was tappable"
                        .to_string(),
                );
            };
            let mut hint = format!(
                "nothing tappable there: the node at this point is {} {} at {}, which is not \
                 clickable and neither is any ancestor, and the app's window answered \
                 `finishInputEvent(handled) = false` (no handler consumed the input)",
                hit_node["class"].as_str().unwrap_or("view"),
                hint_node_name(hit_node),
                hit_node["bounds"].as_str().unwrap_or("?"),
            );
            if let Some(suggest) = suggest {
                let (cx, cy) = suggest
                    .get("bounds")
                    .and_then(serde_json::Value::as_str)
                    .and_then(|bounds| bounds.parse::<Bounds>().ok())
                    .map(|bounds| bounds.center())
                    .unwrap_or((0, 0));
                hint.push_str(&format!(
                    "; tap the nearest clickable {} {} at {} (centre {cx},{cy}) instead",
                    suggest["class"].as_str().unwrap_or("view"),
                    hint_node_name(suggest),
                    suggest["bounds"].as_str().unwrap_or("?"),
                ));
            } else {
                hint.push_str(
                    "; this screen shows no clickable node, so scroll or open the control that \
                     owns the state first",
                );
            }
            Some(hint)
        }
        Some(true) if !state_changed => Some(
            "the app's window consumed the input (`finishInputEvent(handled) = true`) but the \
             settled frame shows no change: the handler may be asynchronous, or the control \
             already had that state. Assert on the state you expect instead of expecting a \
             change"
                .to_string(),
        ),
        Some(true) => None,
    }
}

/// A snapshot of the UI tree for one frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UiTree {
    /// The frame this tree describes.
    pub frame: FrameMeta,
    /// Package owning the root window.
    pub package: String,
    /// Root nodes in document/window order.
    pub nodes: Vec<UiNode>,
    /// True when any window of the tree was truncated by a node limit.
    #[serde(default)]
    pub truncated: bool,
    /// False when the text/semantics route was unavailable.
    #[serde(default = "default_true")]
    pub text_available: bool,
    /// The tree's windows in policy z order (bottom first). Empty on a probe
    /// that predates the window block; every node then still carries its
    /// `window` label.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub windows: Vec<WindowInfo>,
}

const fn default_true() -> bool {
    true
}

impl UiTree {
    /// Build a tree.
    pub fn new(frame: FrameMeta, package: impl Into<String>, nodes: Vec<UiNode>) -> Self {
        UiTree {
            frame,
            package: package.into(),
            nodes,
            truncated: false,
            text_available: true,
            windows: Vec::new(),
        }
    }

    /// All nodes in stable document order.
    pub fn all_nodes(&self) -> Vec<&UiNode> {
        let mut out = Vec::new();
        for node in &self.nodes {
            node.walk(&mut out);
        }
        out
    }

    /// Find all nodes matching a selector, applying `selector.index` last.
    ///
    /// An empty selector is invalid and matches nothing. Only user-visible nodes
    /// match unless the selector sets `include_invisible`.
    pub fn find_all(&self, selector: &Selector) -> Vec<&UiNode> {
        if selector.is_empty() {
            return Vec::new();
        }
        let mut matched: Vec<&UiNode> = self
            .all_nodes()
            .into_iter()
            .filter(|node| selector.accepts_node(node))
            .collect();
        if let Some(index) = selector.index {
            matched = matched.into_iter().nth(index).into_iter().collect();
        }
        matched
    }

    /// Find the first matching node. An empty selector matches nothing.
    /// Only user-visible nodes match unless the selector sets `include_invisible`.
    pub fn find_one(&self, selector: &Selector) -> Option<&UiNode> {
        if selector.is_empty() {
            return None;
        }
        let mut no_index = selector.clone();
        no_index.index = None;
        self.all_nodes()
            .into_iter()
            .find(|node| no_index.accepts_node(node))
    }

    /// Hit-test `(x, y)` against the tree's visible rects.
    ///
    /// The deepest chain wins; when two windows both contain the point the later
    /// root (the window drawn on top in the probe's document order) wins, which
    /// is the same order every other tree consumer reads. Only nodes that are
    /// `visible` to the user can be hit: an off-screen node never answers a tap,
    /// exactly as it never resolves as a tap target.
    #[must_use]
    pub fn tap_hit(&self, x: i32, y: i32) -> TapHit<'_> {
        let mut best: Vec<&UiNode> = Vec::new();
        for root in &self.nodes {
            let mut path = Vec::new();
            hit_path(root, x, y, &mut path);
            // `>=` keeps the later (top-most) window when both contain the point.
            if !path.is_empty() && path.len() >= best.len() {
                best = path;
            }
        }
        let consumer = best.iter().rev().find(|node| node.clickable).copied();
        let suggestion = if consumer.is_some() {
            None
        } else {
            // A **sibling** of the deepest node on the path wins over anything
            // further away: the switch beside a row's label is the control that
            // owns the state, even when a button in the next row is closer to
            // the finger. Only when no level of the path has a clickable
            // sibling does the nearest clickable node anywhere answer.
            best.iter()
                .rev()
                .find_map(|on_path| {
                    sibling_nodes(&self.nodes, &on_path.id).and_then(|siblings| {
                        siblings
                            .iter()
                            .filter(|node| node.id != on_path.id && node.visible && node.clickable)
                            .min_by_key(|node| TapHit::distance(node, x, y))
                    })
                })
                .or_else(|| {
                    self.all_nodes()
                        .into_iter()
                        .filter(|node| node.visible && node.clickable)
                        .filter(|node| !best.iter().any(|on_path| on_path.id == node.id))
                        .min_by_key(|node| TapHit::distance(node, x, y))
                })
        };
        TapHit {
            path: best,
            consumer,
            suggestion,
        }
    }

    /// The first node that matches the selector's *fields* but is not visible to
    /// the user. Turns a silent "not found" into an honest `ELEMENT_OFFSCREEN`
    /// failure that names the off-screen node and its bounds.
    pub fn find_invisible(&self, selector: &Selector) -> Option<&UiNode> {
        if selector.is_empty() {
            return None;
        }
        let mut probe = selector.clone();
        probe.include_invisible = true;
        probe.index = None;
        self.all_nodes()
            .into_iter()
            .find(|node| !node.visible && probe.matches_node(node))
    }

    /// Find the node a pointer action should target for `selector`.
    ///
    /// This is the *input* resolution rule, and it differs from [`UiTree::find_one`]
    /// (evidence resolution) in exactly one way, the one the platform makes: what
    /// a pointer reaches is decided by the node's **accessible name**, and the
    /// platform composes that name as the node's text, else its content
    /// description (`AccessibilityNodeInfo.getText()`, else
    /// `getContentDescription()` — the same name UiAutomator's `By.desc` and an
    /// `agent-device` `label` resolve to).
    ///
    /// 1. the ordinary field match among user-visible nodes ([`UiTree::find_one`]);
    /// 2. for a `text` selector, a user-visible node whose own text is empty and
    ///    whose content description matches — a React Native `Pressable` is a
    ///    text-less `ViewGroup` whose whole name is its accessibility label, so
    ///    `tap {"text": "Settings"}` must reach the pressable the way the device
    ///    does instead of stopping at a decorative `TextView` underneath it.
    ///
    /// Only user-visible nodes are ever returned: this never makes an off-screen
    /// node tappable, and a node that is not visible to the user resolves to
    /// nothing here (callers report `ELEMENT_OFFSCREEN`).
    pub fn find_input_target(&self, selector: &Selector) -> Option<&UiNode> {
        self.find_input_candidates(selector).into_iter().next()
    }

    /// Every node [`UiTree::find_input_candidates`] would consider a target for
    /// `selector`, so a caller can refuse an **ambiguous** selector instead of
    /// silently picking one.
    ///
    /// Resolution is two-staged and the stages are never mixed:
    ///
    /// 1. the ordinary field match among user-visible nodes ([`UiTree::find_all`]);
    /// 2. only when stage 1 is empty **and** the selector is a `text` selector:
    ///    user-visible nodes whose own text is empty and whose `content_desc`
    ///    matches (the RN accessible-name case).
    ///
    /// An exact field match therefore always outranks a content-description
    /// match: the drawn label of the active bottom tab wins over the text-less
    /// pressable that carries the same name. Within the stage that wins, every
    /// match is returned in document order, so a caller that gets more than one
    /// knows the selector does not name a single node.
    ///
    /// `selector.index`, when set, is honoured by stage 1 (it is an explicit
    /// pick) and makes the result that one node.
    pub fn find_input_candidates(&self, selector: &Selector) -> Vec<&UiNode> {
        let direct = self.find_all(selector);
        if !direct.is_empty() {
            return direct;
        }
        let Some(text) = selector.text.as_ref() else {
            return Vec::new();
        };
        // A caller that pinned an occurrence gets exactly that one; never a
        // second-stage list that ignores the pin.
        if selector.index.is_some() {
            return Vec::new();
        }
        // The other selector fields (id/content_desc/class/bounds) still have to
        // match; only the text field is answered by the content description.
        let mut probe = selector.clone();
        probe.text = None;
        probe.index = None;
        self.all_nodes()
            .into_iter()
            .filter(|node| {
                node.visible
                    && node.text.is_empty()
                    && probe.matches_node(node)
                    && text.matches(&node.content_desc)
            })
            .collect()
    }

    /// The canonical `observe --text` rendering, one line per node with text or
    /// a content description, in document order.
    pub fn text_lines(&self) -> Vec<String> {
        self.all_nodes()
            .into_iter()
            .filter(|node| !node.text.is_empty() || !node.content_desc.is_empty())
            .map(UiNode::text_line)
            .collect()
    }

    /// The text rendering as a single newline-joined string.
    pub fn text(&self) -> String {
        self.text_lines().join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame() -> FrameMeta {
        FrameMeta::new(
            1,
            "2026-11-10T13:51:14Z",
            "req_1",
            1080,
            2400,
            420,
            "abc",
            true,
        )
    }

    fn sample_tree() -> UiTree {
        let title = UiNode::new(
            "n1",
            "android.widget.TextView",
            Bounds::new(42, 317, 231, 368),
        )
        .with_resource_id("dev.acos.fixtures.helloviews:id/title")
        .with_text("Hello ACOS");
        let input = UiNode::new(
            "n2",
            "android.widget.EditText",
            Bounds::new(42, 368, 1038, 486),
        )
        .with_resource_id("dev.acos.fixtures.helloviews:id/input")
        .with_text("World");
        let btn = UiNode::new(
            "n3",
            "android.widget.Button",
            Bounds::new(42, 486, 273, 612),
        )
        .with_resource_id("dev.acos.fixtures.helloviews:id/btn")
        .with_text("Greet")
        .clickable(true);
        let result = UiNode::new(
            "n4",
            "android.widget.TextView",
            Bounds::new(42, 612, 392, 663),
        )
        .with_resource_id("dev.acos.fixtures.helloviews:id/result")
        .with_text("Hello, World! clicks: 1");
        UiTree::new(
            frame(),
            "dev.acos.fixtures.helloviews",
            vec![UiNode::new(
                "root",
                "android.widget.LinearLayout",
                Bounds::new(0, 275, 1080, 2337),
            )
            .with_children(vec![title, input, btn, result])],
        )
    }

    #[test]
    fn bounds_round_trip_and_parse() {
        let b = Bounds::new(42, 317, 231, 368);
        assert_eq!(b.to_string(), "[42,317][231,368]");
        assert_eq!("[42,317][231,368]".parse::<Bounds>().unwrap(), b);
        assert_eq!(b.width(), 189);
        assert_eq!(b.height(), 51);
        assert_eq!(b.center(), (136, 342));
        assert!(b.is_valid());
        assert!(!Bounds::new(10, 0, 5, 1).is_valid());
        assert!("nope".parse::<Bounds>().is_err());
    }

    #[test]
    fn selectors_are_stable() {
        let tree = sample_tree();
        assert_eq!(tree.find_all(&Selector::by_id("btn")).len(), 1);
        assert_eq!(
            tree.find_all(&Selector::by_id("dev.acos.fixtures.helloviews:id/btn"))
                .len(),
            1
        );
        assert_eq!(
            tree.find_all(&Selector {
                text: Some(TextMatch::contains("clicks: 1")),
                ..Selector::default()
            })
            .len(),
            1
        );
        assert_eq!(
            tree.find_all(&Selector {
                class: Some("android.widget.TextView".into()),
                index: Some(1),
                ..Selector::default()
            })
            .len(),
            1
        );
        assert_eq!(tree.find_one(&Selector::by_text("Greet")).unwrap().id, "n3");
        assert!(tree.find_all(&Selector::default()).is_empty());
        assert!(!Selector::default().validation_errors().is_empty());
    }

    #[test]
    fn selectors_skip_offscreen_nodes_and_tap_the_visible_rect() {
        let mut hidden = UiNode::new(
            "off",
            "android.widget.TextView",
            Bounds::new(1122, 317, 1239, 368),
        )
        .with_text("Hidden");
        hidden.visible = false;
        let shown = UiNode::new(
            "on",
            "android.widget.TextView",
            Bounds::new(42, 317, 196, 368),
        )
        .with_text("Page one")
        .with_visible_bounds(Bounds::new(52, 320, 186, 365));
        let frame = frame();
        let tree = UiTree::new(
            frame,
            "dev.acos.fixtures.offscreenpager",
            vec![hidden, shown],
        );

        // GR2 W4: present in the tree but not a selector match.
        assert!(tree.find_one(&Selector::by_text("Hidden")).is_none());
        assert!(tree.find_invisible(&Selector::by_text("Hidden")).is_some());
        let mut both = Selector::by_text("Hidden");
        both.include_invisible = true;
        assert_eq!(tree.find_all(&both).len(), 1);
        // "index" counts only the accepted (visible) nodes.
        assert_eq!(
            tree.find_all(&Selector {
                class: Some("android.widget.TextView".into()),
                index: Some(0),
                ..Selector::default()
            })
            .len(),
            1
        );
        // A tap uses the centre of the clipped visible rect, never the full rect.
        let node = tree.find_one(&Selector::by_text("Page one")).unwrap();
        assert_eq!(node.visible_rect(), Bounds::new(52, 320, 186, 365));
        assert_eq!(node.tap_point(), (119, 342));
        // Same rect, moved so the full-bounds centre is outside the visible part.
        let clipped = UiNode::new(
            "on",
            "android.widget.TextView",
            Bounds::new(42, 317, 196, 368),
        )
        .with_visible_bounds(Bounds::new(120, 317, 196, 368));
        assert_eq!(clipped.tap_point(), (158, 342));
        assert_ne!(clipped.tap_point(), clipped.bounds.center());
    }

    #[test]
    fn include_invisible_is_not_a_matching_field() {
        // A selector that only sets the policy is still empty/invalid.
        let policy_only = Selector {
            include_invisible: true,
            ..Selector::default()
        };
        assert!(policy_only.is_empty());
        assert!(!policy_only.validation_errors().is_empty());
    }

    /// The Gaten search field as the API-34 guest reports it: an RN
    /// `TextInput` whose `text` is its placeholder (AOSP takes the hint when the
    /// field is empty) and whose accessible *name* is the content description.
    fn rn_search_field(collapsed_text: bool) -> UiNode {
        UiNode::new("n27_", "Z4.g", Bounds::new(189, 366, 978, 482))
            .with_text(if collapsed_text {
                "猫, ねこ, neko..."
            } else {
                ""
            })
            .with_password(false)
    }

    fn with_content_desc(mut node: UiNode, desc: &str) -> UiNode {
        node.content_desc = desc.to_string();
        node
    }

    #[test]
    fn content_desc_is_an_explicit_selector_field() {
        // FB1: an agent must be able to name the field by the label it read,
        // without hand-computing bounds. `text` cannot: the node's text is the
        // placeholder, so the accessible-name stage never reaches it either.
        let tree = UiTree::new(
            frame(),
            "com.gaten",
            vec![with_content_desc(
                rn_search_field(true),
                "Search Japanese dictionary",
            )],
        );
        let desc = Selector {
            content_desc: Some(TextMatch::exact("Search Japanese dictionary")),
            ..Selector::default()
        };
        assert_eq!(tree.find_all(&desc).len(), 1);
        assert_eq!(tree.find_input_candidates(&desc).len(), 1);
        assert!(tree
            .find_input_target(&Selector::by_text("Search Japanese dictionary"))
            .is_none());
    }

    #[test]
    fn several_visible_matches_are_all_returned_as_candidates() {
        // Two user-visible buttons that share a label: the caller must be able
        // to see that the selector is ambiguous (and refuse), not get a pick.
        let first = UiNode::new("n1", "android.widget.Button", Bounds::new(0, 0, 100, 60))
            .with_text("Continue");
        let second = UiNode::new("n2", "android.widget.Button", Bounds::new(0, 60, 100, 120))
            .with_text("Continue");
        let hidden = UiNode::new("n3", "android.widget.Button", Bounds::new(0, 120, 100, 180))
            .with_text("Continue");
        let mut hidden = hidden;
        hidden.visible = false;
        let tree = UiTree::new(frame(), "dev.test", vec![first, second, hidden]);
        let selector = Selector::by_text("Continue");
        let candidates = tree.find_input_candidates(&selector);
        assert_eq!(
            candidates.len(),
            2,
            "only the user-visible matches count: {candidates:#?}"
        );
        assert_eq!(candidates[0].id, "n1");
        assert_eq!(candidates[1].id, "n2");
        // `index` is an explicit pick and collapses the list to one.
        let pinned = Selector {
            index: Some(1),
            ..Selector::by_text("Continue")
        };
        assert_eq!(tree.find_input_candidates(&pinned).len(), 1);
        assert_eq!(tree.find_input_candidates(&pinned)[0].id, "n2");
    }

    #[test]
    fn the_text_stage_wins_over_the_content_desc_stage() {
        // RNTAP: an exact text match outranks a content-description match, and
        // the stages are never mixed, so the candidates of one answer belong to
        // one field.
        let label = UiNode::new("n1", "X4.e", Bounds::new(362, 2214, 445, 2251)).with_text("Home");
        let pressable = with_content_desc(
            UiNode::new("n2", "b5.g", Bounds::new(280, 2151, 453, 2316)),
            "Home",
        );
        let tree = UiTree::new(frame(), "com.gaten", vec![label, pressable]);
        let candidates = tree.find_input_candidates(&Selector::by_text("Home"));
        assert_eq!(
            candidates.len(),
            1,
            "the drawn label is the only text match"
        );
        assert_eq!(candidates[0].class, "X4.e");
        // With no text match at all, the content description answers alone.
        let tree = UiTree::new(
            frame(),
            "com.gaten",
            vec![with_content_desc(
                UiNode::new("n2", "b5.g", Bounds::new(280, 2151, 453, 2316)),
                "Home",
            )],
        );
        let candidates = tree.find_input_candidates(&Selector::by_text("Home"));
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].class, "b5.g");
    }

    #[test]
    fn regex_and_validation() {
        assert!(TextMatch::regex("^Hello, .+! clicks: [0-9]+$").matches("Hello, World! clicks: 1"));
        assert!(!TextMatch::regex("^World").matches("Hello, World!"));
        assert!(TextMatch::regex("(").validate().is_some());
        assert!(TextMatch::exact("").validate().is_some());
    }

    #[test]
    fn selector_human_form_is_not_a_debug_dump() {
        // The exact line an agent reads when a selector misses.
        let by_text = Selector::by_text("Nonexistent");
        assert_eq!(by_text.human(), "text==\"Nonexistent\"");
        assert_eq!(format!("{by_text}"), "text==\"Nonexistent\"");
        assert!(!format!("{by_text}").contains("Selector {"));

        let mut desc = Selector::default();
        desc.content_desc = Some(TextMatch::exact("X"));
        assert_eq!(desc.human(), "content_desc==\"X\"");

        let mut by_id = Selector::by_id("dev.acos.fixtures.helloviews:id/btn");
        by_id.index = Some(1);
        assert_eq!(
            by_id.human(),
            "id==\"dev.acos.fixtures.helloviews:id/btn\" index=1"
        );

        // Contains/regex keep their distinct semantics visible.
        assert_eq!(
            Selector {
                text: Some(TextMatch::contains("crement")),
                ..Selector::default()
            }
            .human(),
            "text*=\"crement\""
        );
        assert_eq!(
            Selector {
                hint: Some(TextMatch::regex("^Incr")),
                ..Selector::default()
            }
            .human(),
            "hint~/^Incr/"
        );

        // Bounds + invisible policy are part of the description.
        let bounds = Selector {
            bounds: Some(Bounds::new(0, 0, 100, 100)),
            include_invisible: true,
            ..Selector::default()
        };
        assert_eq!(
            bounds.human(),
            "bounds=[0,0][100,100] include_invisible=true"
        );
        assert_eq!(Selector::default().human(), "<empty selector>");
    }

    /// REL3E: the hit-test answers "what is at this point", deepest node first,
    /// and ranks a clickable **sibling** above a nearer clickable node in
    /// another subtree - the switch beside a row's label is the control that
    /// owns the state even when a button below is closer to the finger.
    #[test]
    fn tap_hit_finds_the_deepest_node_and_prefers_a_clickable_sibling() {
        let label = UiNode::new("label", "android.widget.TextView", Bounds::new(40, 300, 660, 405))
            .with_text("Floating button");
        let switch = UiNode::new("switch", "android.widget.Switch", Bounds::new(670, 300, 1040, 405))
            .with_text("Floating button")
            .clickable(true);
        let row = UiNode::new("row", "android.widget.LinearLayout", Bounds::new(40, 300, 1040, 430))
            .with_children(vec![label, switch]);
        // A clickable button in the next row is geometrically closer to (350,350)
        // than the switch is, and must still lose to the sibling.
        let button = UiNode::new("inc", "android.widget.Button", Bounds::new(40, 430, 280, 560))
            .with_text("Increment")
            .clickable(true);
        let root = UiNode::new("root", "android.widget.LinearLayout", Bounds::new(0, 0, 1080, 2400))
            .with_children(vec![row, button]);
        let tree = UiTree::new(frame(), "dev.acos.fixtures.taptargets", vec![root]);

        let hit = tree.tap_hit(350, 350);
        assert_eq!(hit.path.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(), vec!["root", "row", "label"]);
        assert_eq!(hit.node().map(|n| n.id.as_str()), Some("label"));
        assert_eq!(hit.consumer, None, "neither the label nor its ancestors are clickable");
        assert_eq!(
            hit.suggestion.map(|n| n.id.as_str()),
            Some("switch"),
            "the sibling switch wins over the nearer button in another row"
        );

        // The switch itself is a consumer: no suggestion, and the hit node is it.
        let hit = tree.tap_hit(800, 350);
        assert_eq!(hit.node().map(|n| n.id.as_str()), Some("switch"));
        assert_eq!(hit.consumer.map(|n| n.id.as_str()), Some("switch"));
        assert_eq!(hit.suggestion, None);

        // A clickable ancestor is the consumer even when the leaf takes no click.
        let inner = UiNode::new("inner", "android.widget.TextView", Bounds::new(60, 320, 200, 380))
            .with_text("Row title");
        let pressable =
            UiNode::new("pressable", "android.view.ViewGroup", Bounds::new(40, 300, 660, 405))
                .with_children(vec![inner])
                .clickable(true);
        let root = UiNode::new("root", "android.widget.LinearLayout", Bounds::new(0, 0, 1080, 2400))
            .with_children(vec![pressable]);
        let tree = UiTree::new(frame(), "dev.acos.fixtures.taptargets", vec![root]);
        let hit = tree.tap_hit(100, 350);
        assert_eq!(hit.node().map(|n| n.id.as_str()), Some("inner"));
        assert_eq!(hit.consumer.map(|n| n.id.as_str()), Some("pressable"));

        // A point outside every rect hits nothing at all (the root is a real
        // node, so only a point outside the root is empty).
        let empty = tree.tap_hit(2000, 2000);
        assert!(empty.path.is_empty());
        assert_eq!(empty.node(), None);
        assert_eq!(empty.consumer, None);
    }

    /// MERGE3I3 precedence: the host's window-removal fact outranks an
    /// all-unhandled guest ack set, and never outranks a guest `handled: true`.
    /// The source and the answer always agree.
    #[test]
    fn a_host_window_removal_outranks_an_unhandled_ack() {
        use crate::HandledSource;
        // Every event acked `handled: false` and the delivered window removed:
        // the removal wins - the press acted on that window.
        assert_eq!(
            handled_source(2, 2, false, true),
            HandledSource::HostWindowRemoved
        );
        assert_eq!(handled_answer(Some(false), 2, false, true), Some(true));
        // No ack at all (a disposed InputEventReceiver): same answer.
        assert_eq!(
            handled_source(2, 0, false, true),
            HandledSource::HostWindowRemoved
        );
        assert_eq!(handled_answer(None, 2, false, true), Some(true));
        // A guest `handled: true` is the app's own proof and wins.
        assert_eq!(handled_source(2, 2, true, true), HandledSource::Guest);
        assert_eq!(handled_answer(Some(true), 2, true, true), Some(true));
        // Without a removal the unhandled acks stay the guest's own answer
        // (REL3H2's `INPUT_UNHANDLED` path, unchanged).
        assert_eq!(handled_source(2, 2, false, false), HandledSource::Guest);
        assert_eq!(handled_answer(Some(false), 2, false, false), Some(false));
        // No delivery and no removal: nothing to claim.
        assert_eq!(handled_source(0, 0, false, false), HandledSource::None);
        assert_eq!(handled_answer(None, 0, false, false), None);
        // A removal with nothing delivered is not evidence either.
        assert_eq!(handled_source(0, 0, false, true), HandledSource::None);
        assert_eq!(handled_answer(None, 0, false, true), None);
    }
}
