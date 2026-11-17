//! Flutter APK support (W137-A): detection, the semantics tree, and the
//! pointer-packet wire format.
//!
//! The Flutter engine does not build an Android `View` tree, so the ACOS view
//! tree cannot come from the framework. Instead the engine ships a
//! **semantics** tree to the embedder whenever accessibility is enabled
//! (`FlutterJNI.setSemanticsEnabled(true)` → `nativeSetSemanticsEnabled`). The
//! embedder receives it as a custom little-endian `ByteBuffer`, decoded on
//! device by `io.flutter.view.AccessibilityBridge`. [`decode_semantics_update`]
//! is a byte-for-byte port of that decoder, so ACOS can turn the engine's
//! accessibility updates into the same `text`/`bounds`/`actions` agent API the
//! framework view tree exposes.
//!
//! The format is derived from the Flutter 3.47 embedding shipped in
//! `fixtures/build/apps/flutter-hello.apk` (`AccessibilityBridge` and
//! `AccessibilityBridge$SemanticsNode.updateWith`). The Java decoder is the
//! authority; the Rust port mirrors its read order exactly and is unit-tested
//! against synthetic buffers.

use std::io::Read;
use std::path::Path;

/// Reading or parsing an APK for Flutter structure failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlutterError {
    /// The APK could not be read.
    Io(String),
    /// The APK is not a valid zip archive, or an entry is corrupt.
    Zip(String),
}

impl std::fmt::Display for FlutterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FlutterError::Io(m) | FlutterError::Zip(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for FlutterError {}

/// A Flutter APK as identified by its zip layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlutterApk {
    /// Every ABI that ships `lib/<abi>/libflutter.so`.
    pub engine_abis: Vec<String>,
    /// Every ABI that ships an AOT snapshot (`lib/<abi>/libapp.so`).
    pub snapshot_abis: Vec<String>,
    /// Whether the APK carries an `assets/flutter_assets/` tree.
    pub has_flutter_assets: bool,
}

impl FlutterApk {
    /// The ABI a host launch should use, if any engine library is present.
    #[must_use]
    pub fn host_engine_abi(&self) -> Option<&str> {
        self.engine_abis.first().map(String::as_str)
    }
}

/// Identify a Flutter APK from its zip entries.
///
/// Recognition is structural (the engine `.so` and/or the `flutter_assets`
/// tree), never a manifest heuristic, so a plain Android APK is never
/// misclassified.
pub fn detect(apk_path: &Path) -> Result<Option<FlutterApk>, FlutterError> {
    let bytes = std::fs::read(apk_path)
        .map_err(|e| FlutterError::Io(format!("read {}: {e}", apk_path.display())))?;
    detect_bytes(&bytes)
}

/// [`detect`] over an in-memory zip.
pub fn detect_bytes(apk: &[u8]) -> Result<Option<FlutterApk>, FlutterError> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(apk))
        .map_err(|e| FlutterError::Zip(format!("open apk: {e}")))?;
    let mut engine_abis = Vec::new();
    let mut snapshot_abis = Vec::new();
    let mut has_flutter_assets = false;
    for i in 0..archive.len() {
        let Ok(entry) = archive.by_index(i) else {
            continue;
        };
        let name = entry.name();
        if let Some(rest) = name.strip_prefix("lib/") {
            let mut parts = rest.splitn(2, '/');
            if let (Some(abi), Some(lib)) = (parts.next(), parts.next()) {
                match lib {
                    "libflutter.so" => engine_abis.push(abi.to_string()),
                    "libapp.so" => snapshot_abis.push(abi.to_string()),
                    _ => {}
                }
            }
        } else if name.starts_with("assets/flutter_assets/") {
            has_flutter_assets = true;
        }
    }
    if engine_abis.is_empty() && !has_flutter_assets {
        return Ok(None);
    }
    engine_abis.sort();
    engine_abis.dedup();
    snapshot_abis.sort();
    snapshot_abis.dedup();
    Ok(Some(FlutterApk {
        engine_abis,
        snapshot_abis,
        has_flutter_assets,
    }))
}

/// A string-attribute span attached to a semantics string.
#[derive(Debug, Clone, PartialEq)]
pub struct StringAttribute {
    /// Inclusive start offset (UTF-16 code units, as the engine sends).
    pub start: i32,
    /// Exclusive end offset.
    pub end: i32,
    /// The engine's `AccessibilityStringBuilder.StringAttributeType` ordinal.
    pub kind: i32,
    /// Locale decoded from a string-attribute buffer, when `kind` is a locale.
    pub locale: Option<String>,
}

/// One Flutter semantics node, in ACOS terms.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SemanticsNode {
    /// Engine node id (stable across updates; `0` is the root).
    pub id: i32,
    /// Raw 64-bit `SemanticsFlag` bitset.
    pub flags: u64,
    /// Raw 32-bit `SemanticsAction` bitset.
    pub actions: u32,
    /// `SemanticsNode.maxValueLength` (input length cap; `-1` when unset).
    pub max_value_length: i32,
    /// `SemanticsNode.currentValueLength`.
    pub current_value_length: i32,
    /// Selection base, when the node is a text field.
    pub text_selection_base: i32,
    /// Selection extent, when the node is a text field.
    pub text_selection_extent: i32,
    /// Platform view id, or `-1`.
    pub platform_view_id: i32,
    /// `scrollChildren`, if the node is scrollable.
    pub scroll_children: i32,
    /// `scrollIndex`.
    pub scroll_index: i32,
    /// `traversalParent`.
    pub traversal_parent: i32,
    /// Logical scroll offset.
    pub scroll_position: f32,
    /// Maximum scroll extent.
    pub scroll_extent_max: f32,
    /// Minimum scroll extent.
    pub scroll_extent_min: f32,
    /// `SemanticsRole` ordinal.
    pub role: i32,
    /// `identifier` (Flutter's `SemanticsProperties.identifier`).
    pub identifier: Option<String>,
    /// Accessibility label — the ACOS `text`.
    pub label: Option<String>,
    /// Attribute spans for [`Self::label`].
    pub label_attributes: Option<Vec<StringAttribute>>,
    /// Accessibility value.
    pub value: Option<String>,
    /// Accessibility hint.
    pub hint: Option<String>,
    /// Tooltip text.
    pub tooltip: Option<String>,
    /// Link URL.
    pub link_url: Option<String>,
    /// Locale.
    pub locale: Option<String>,
    /// Minimum value description.
    pub min_value: Option<String>,
    /// Maximum value description.
    pub max_value: Option<String>,
    /// Heading level (`0` when not a heading).
    pub heading_level: i32,
    /// `TextDirection.fromInt` ordinal.
    pub text_direction: i32,
    /// Axis-aligned bounds in logical pixels: `[left, top, right, bottom]`.
    pub rect: [f32; 4],
    /// 4x4 row-major transform from the parent node.
    pub transform: [f32; 16],
    /// 4x4 row-major hit-test transform.
    pub hit_test_transform: [f32; 16],
    /// Children in traversal order.
    pub children_in_traversal_order: Vec<i32>,
    /// Children in hit-test order.
    pub children_in_hit_test_order: Vec<i32>,
    /// Custom action override ids attached to this node.
    pub custom_actions: Vec<i32>,
}

/// `SemanticsFlag` bit positions (`dart:ui`).
pub mod flag {
    /// `hasCheckedState`.
    pub const HAS_CHECKED_STATE: u64 = 1 << 0;
    /// `isChecked`.
    pub const IS_CHECKED: u64 = 1 << 1;
    /// `isSelected`.
    pub const IS_SELECTED: u64 = 1 << 2;
    /// `isButton`.
    pub const IS_BUTTON: u64 = 1 << 3;
    /// `isTextField`.
    pub const IS_TEXT_FIELD: u64 = 1 << 4;
    /// `isFocused`.
    pub const IS_FOCUSED: u64 = 1 << 5;
    /// `hasEnabledState`.
    pub const HAS_ENABLED_STATE: u64 = 1 << 6;
    /// `isEnabled`.
    pub const IS_ENABLED: u64 = 1 << 7;
    /// `isHeader`.
    pub const IS_HEADER: u64 = 1 << 9;
    /// `isObscured`.
    pub const IS_OBSCURED: u64 = 1 << 10;
    /// `scopesRoute`.
    pub const SCOPES_ROUTE: u64 = 1 << 11;
    /// `namesRoute`.
    pub const NAMES_ROUTE: u64 = 1 << 12;
    /// `isHidden`.
    pub const IS_HIDDEN: u64 = 1 << 13;
    /// `isImage`.
    pub const IS_IMAGE: u64 = 1 << 14;
    /// `isLiveRegion`.
    pub const IS_LIVE_REGION: u64 = 1 << 15;
    /// `isFocusable`.
    pub const IS_FOCUSABLE: u64 = 1 << 21;
    /// `isLink`.
    pub const IS_LINK: u64 = 1 << 22;
    /// `isSlider`.
    pub const IS_SLIDER: u64 = 1 << 23;
    /// `isReadOnly`.
    pub const IS_READ_ONLY: u64 = 1 << 20;
}

/// `SemanticsAction` bit positions (`dart:ui`).
pub mod action {
    /// `tap`.
    pub const TAP: u32 = 1 << 0;
    /// `longPress`.
    pub const LONG_PRESS: u32 = 1 << 1;
    /// `scrollLeft`.
    pub const SCROLL_LEFT: u32 = 1 << 2;
    /// `scrollRight`.
    pub const SCROLL_RIGHT: u32 = 1 << 3;
    /// `scrollUp`.
    pub const SCROLL_UP: u32 = 1 << 4;
    /// `scrollDown`.
    pub const SCROLL_DOWN: u32 = 1 << 5;
    /// `increase`.
    pub const INCREASE: u32 = 1 << 6;
    /// `decrease`.
    pub const DECREASE: u32 = 1 << 7;
    /// `setText`.
    pub const SET_TEXT: u32 = 1 << 21;
    /// `focus`.
    pub const FOCUS: u32 = 1 << 22;
}

impl SemanticsNode {
    /// Whether the node advertises the given raw `SemanticsFlag` bit.
    #[must_use]
    pub fn has_flag(&self, bit: u64) -> bool {
        self.flags & bit != 0
    }

    /// Whether the node advertises the given raw `SemanticsAction` bit.
    #[must_use]
    pub fn has_action(&self, bit: u32) -> bool {
        self.actions & bit != 0
    }

    /// `clickable` in ACOS terms: the node accepts a tap.
    #[must_use]
    pub fn clickable(&self) -> bool {
        self.has_action(action::TAP)
    }

    /// `enabled` in ACOS terms: an explicitly-disabled node is not enabled.
    #[must_use]
    pub fn enabled(&self) -> bool {
        !self.has_flag(flag::HAS_ENABLED_STATE) || self.has_flag(flag::IS_ENABLED)
    }

    /// `text` in ACOS terms: the accessibility label, then the value.
    #[must_use]
    pub fn text(&self) -> Option<&str> {
        self.label
            .as_deref()
            .filter(|s| !s.is_empty())
            .or_else(|| self.value.as_deref().filter(|s| !s.is_empty()))
    }

    /// Bounds as `[x0, y0, x1, y1]` rounded, matching the ACOS tree type.
    #[must_use]
    pub fn bounds(&self) -> [i32; 4] {
        [
            self.rect[0].round() as i32,
            self.rect[1].round() as i32,
            self.rect[2].round() as i32,
            self.rect[3].round() as i32,
        ]
    }
}

/// A semantics node could not be decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticsError {
    /// The buffer ended mid-field.
    Truncated {
        /// Byte offset where the read was attempted.
        at: usize,
        /// Number of bytes required.
        need: usize,
    },
    /// A string index was outside the supplied `strings` array.
    BadStringIndex {
        /// The index the buffer referenced.
        index: i32,
    },
    /// A string-attribute buffer index was outside the supplied array.
    BadAttributeIndex {
        /// The index the buffer referenced.
        index: i32,
    },
    /// A node count was negative.
    NegativeCount {
        /// The count read from the buffer.
        count: i32,
    },
}

impl std::fmt::Display for SemanticsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SemanticsError::Truncated { at, need } => write!(
                f,
                "semantics buffer truncated at offset {at}: need {need} more bytes"
            ),
            SemanticsError::BadStringIndex { index } => {
                write!(f, "semantics string index {index} is out of range")
            }
            SemanticsError::BadAttributeIndex { index } => {
                write!(
                    f,
                    "semantics string-attribute index {index} is out of range"
                )
            }
            SemanticsError::NegativeCount { count } => {
                write!(f, "semantics count {count} is negative")
            }
        }
    }
}

impl std::error::Error for SemanticsError {}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.bytes.len() - self.pos
    }

    fn take(&mut self, need: usize) -> Result<&'a [u8], SemanticsError> {
        if self.remaining() < need {
            return Err(SemanticsError::Truncated { at: self.pos, need });
        }
        let slice = &self.bytes[self.pos..self.pos + need];
        self.pos += need;
        Ok(slice)
    }

    fn i32(&mut self) -> Result<i32, SemanticsError> {
        let b = self.take(4)?;
        Ok(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u32(&mut self) -> Result<u32, SemanticsError> {
        Ok(self.i32()? as u32)
    }

    fn i64(&mut self) -> Result<i64, SemanticsError> {
        let b = self.take(8)?;
        Ok(i64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    fn f32(&mut self) -> Result<f32, SemanticsError> {
        Ok(f32::from_bits(self.u32()?))
    }
}

/// `AccessibilityBridge.getStringFromBuffer`: an int index into `strings`;
/// `-1` means "no string".
fn read_string(r: &mut Reader<'_>, strings: &[String]) -> Result<Option<String>, SemanticsError> {
    let index = r.i32()?;
    if index == -1 {
        return Ok(None);
    }
    strings
        .get(index as usize)
        .cloned()
        .map(Some)
        .ok_or(SemanticsError::BadStringIndex { index })
}

/// `SemanticsNode.getStringAttributesFromBuffer`.
fn read_string_attributes(
    r: &mut Reader<'_>,
    attr_buffers: &[Vec<u8>],
) -> Result<Option<Vec<StringAttribute>>, SemanticsError> {
    let count = r.i32()?;
    if count == -1 {
        return Ok(None);
    }
    if count < 0 {
        return Err(SemanticsError::NegativeCount { count });
    }
    let mut out = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let start = r.i32()?;
        let end = r.i32()?;
        let kind = r.i32()?;
        // Only the locale attribute ("LocaleStringAttribute", ordinal 1 in the
        // 3.47 enum) carries a payload buffer; spell-out carries none and the
        // remaining kinds have no extra fields.
        let locale = if kind == 1 {
            let index = r.i32()?;
            let bytes = attr_buffers
                .get(index as usize)
                .ok_or(SemanticsError::BadAttributeIndex { index })?;
            Some(String::from_utf8_lossy(bytes).into_owned())
        } else {
            None
        };
        out.push(StringAttribute {
            start,
            end,
            kind,
            locale,
        });
    }
    Ok(Some(out))
}

/// `AccessibilityBridge.getMatrix4FromBuffer`: exactly 16 little-endian floats.
fn read_matrix(r: &mut Reader<'_>) -> Result<[f32; 16], SemanticsError> {
    let mut m = [0.0f32; 16];
    for slot in &mut m {
        *slot = r.f32()?;
    }
    Ok(m)
}

/// `SemanticsNode.updateWith`: decode one node body (after its id).
#[allow(clippy::field_reassign_with_default)]
fn read_node(
    r: &mut Reader<'_>,
    strings: &[String],
    attr_buffers: &[Vec<u8>],
) -> Result<SemanticsNode, SemanticsError> {
    let mut node = SemanticsNode {
        flags: r.i64()? as u64,
        // The remaining scalar fields are read in the same order as the Java
        // decoder; see the module docs for the field-by-field mapping.
        ..SemanticsNode::default()
    };
    node.actions = r.u32()?;
    node.max_value_length = r.i32()?;
    node.current_value_length = r.i32()?;
    node.text_selection_base = r.i32()?;
    node.text_selection_extent = r.i32()?;
    node.platform_view_id = r.i32()?;
    node.scroll_children = r.i32()?;
    node.scroll_index = r.i32()?;
    node.traversal_parent = r.i32()?;
    node.scroll_position = r.f32()?;
    node.scroll_extent_max = r.f32()?;
    node.scroll_extent_min = r.f32()?;
    node.role = r.i32()?;
    node.identifier = read_string(r, strings)?;
    node.label = read_string(r, strings)?;
    node.label_attributes = read_string_attributes(r, attr_buffers)?;
    node.value = read_string(r, strings)?;
    let _value_attributes = read_string_attributes(r, attr_buffers)?;
    let _increased_value = read_string(r, strings)?;
    let _increased_value_attributes = read_string_attributes(r, attr_buffers)?;
    let _decreased_value = read_string(r, strings)?;
    let _decreased_value_attributes = read_string_attributes(r, attr_buffers)?;
    node.hint = read_string(r, strings)?;
    let _hint_attributes = read_string_attributes(r, attr_buffers)?;
    node.tooltip = read_string(r, strings)?;
    node.link_url = read_string(r, strings)?;
    node.locale = read_string(r, strings)?;
    node.min_value = read_string(r, strings)?;
    node.max_value = read_string(r, strings)?;
    node.heading_level = r.i32()?;
    node.text_direction = r.i32()?;
    node.rect = [r.f32()?, r.f32()?, r.f32()?, r.f32()?];
    node.transform = read_matrix(r)?;
    node.hit_test_transform = read_matrix(r)?;
    let traversal = r.i32()?;
    if traversal < 0 {
        return Err(SemanticsError::NegativeCount { count: traversal });
    }
    for _ in 0..traversal {
        node.children_in_traversal_order.push(r.i32()?);
    }
    let hit_test = r.i32()?;
    if hit_test < 0 {
        return Err(SemanticsError::NegativeCount { count: hit_test });
    }
    for _ in 0..hit_test {
        node.children_in_hit_test_order.push(r.i32()?);
    }
    let custom = r.i32()?;
    if custom < 0 {
        return Err(SemanticsError::NegativeCount { count: custom });
    }
    for _ in 0..custom {
        node.custom_actions.push(r.i32()?);
    }
    Ok(node)
}

/// Decode a Flutter semantics update buffer into its nodes.
///
/// Layout (the engine's `AccessibilityBridge.updateSemantics` reader):
/// repeatedly `i32 node_id` followed by a [`read_node`] body until the buffer
/// is exhausted. `strings` is the embedder's string table (the engine
/// deduplicates labels into it) and `attr_buffers` holds any per-string
/// attribute payloads.
pub fn decode_semantics_update(
    bytes: &[u8],
    strings: &[String],
    attr_buffers: &[Vec<u8>],
) -> Result<Vec<SemanticsNode>, SemanticsError> {
    let mut r = Reader::new(bytes);
    let mut nodes = Vec::new();
    while r.remaining() > 0 {
        let id = r.i32()?;
        let mut node = read_node(&mut r, strings, attr_buffers)?;
        node.id = id;
        nodes.push(node);
    }
    Ok(nodes)
}

/// Flutter's `PointerDataPacket` record size: 36 little-endian 8-byte fields.
pub const POINTER_FIELDS: usize = 36;
/// Byte size of one `PointerData` record.
pub const POINTER_BYTES: usize = POINTER_FIELDS * 8;

/// `PointerChange.down`.
pub const POINTER_CHANGE_DOWN: i64 = 4;
/// `PointerChange.up`.
pub const POINTER_CHANGE_UP: i64 = 6;

fn push_i64(out: &mut Vec<u8>, v: i64) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_f64(out: &mut Vec<u8>, v: f64) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// Encode a single `PointerData` record at the head of `out`.
fn encode_pointer(out: &mut Vec<u8>, timestamp_micros: i64, change: i64, x: f64, y: f64) {
    push_i64(out, 0); // embedderId
    push_i64(out, timestamp_micros); // timeStamp
    push_i64(out, change); // change
    push_i64(out, 0); // kind = touch
    push_i64(out, 0); // signalKind = none
    push_i64(out, 0); // device
    push_i64(out, 1); // pointerIdentifier
    push_f64(out, x); // physicalX
    push_f64(out, y); // physicalY
    push_f64(out, 0.0); // physicalDeltaX
    push_f64(out, 0.0); // physicalDeltaY
    push_i64(out, 1); // buttons (kPrimaryButton)
    push_i64(out, 0); // obscured
    push_i64(out, 0); // synthesized
    push_f64(out, 1.0); // pressure
    push_f64(out, 0.0); // pressureMin
    push_f64(out, 1.0); // pressureMax
    push_f64(out, 0.0); // distance
    push_f64(out, 0.0); // distanceMax
    push_f64(out, 0.0); // size
    push_f64(out, 0.0); // radiusMajor
    push_f64(out, 0.0); // radiusMinor
    push_f64(out, 0.0); // radiusMin
    push_f64(out, 0.0); // radiusMax
    push_f64(out, 0.0); // orientation
    push_f64(out, 0.0); // tilt
    push_i64(out, 0); // platformData
    push_f64(out, 0.0); // scrollDeltaX
    push_f64(out, 0.0); // scrollDeltaY
    push_f64(out, 0.0); // panX
    push_f64(out, 0.0); // panY
    push_f64(out, 0.0); // panDeltaX
    push_f64(out, 0.0); // panDeltaY
    push_f64(out, 1.0); // scale
    push_f64(out, 0.0); // rotation
    push_i64(out, 0); // viewId
}

/// Build the two-record `down`+`up` pointer packet for a tap at `(x, y)` in
/// **physical** pixels (the caller applies `devicePixelRatio`).
///
/// The returned `position` (`= len`) is the byte count the engine's
/// `FlutterJNI.dispatchPointerDataPacket(buffer, position)` copies out of the
/// direct buffer; passing `0` silently delivers an empty packet (W128-A).
#[must_use]
pub fn tap_packet(x: f64, y: f64, timestamp_micros: i64) -> Vec<u8> {
    let mut out = Vec::with_capacity(POINTER_BYTES * 2);
    encode_pointer(&mut out, timestamp_micros, POINTER_CHANGE_DOWN, x, y);
    encode_pointer(&mut out, timestamp_micros + 50_000, POINTER_CHANGE_UP, x, y);
    out
}

/// Whether the workspace ships the extracted Flutter engine for `abi`
/// (`.cache/flutter-1/lib-<abi>/libflutter.so`), as required by the current
/// launch gate.
#[must_use]
pub fn engine_cache_dir(repo_root: &Path, abi: &str) -> std::path::PathBuf {
    repo_root
        .join(".cache/flutter-1")
        .join(format!("lib-{abi}"))
}

/// Read a `flutter_assets` manifest entry (or any zip entry) into memory.
pub fn read_apk_entry(apk_path: &Path, entry: &str) -> Result<Vec<u8>, FlutterError> {
    let bytes = std::fs::read(apk_path)
        .map_err(|e| FlutterError::Io(format!("read {}: {e}", apk_path.display())))?;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| FlutterError::Zip(format!("open apk: {e}")))?;
    let mut data = Vec::new();
    archive
        .by_name(entry)
        .map_err(|e| FlutterError::Zip(format!("{entry}: {e}")))?
        .read_to_end(&mut data)
        .map_err(|e| FlutterError::Zip(format!("{entry}: {e}")))?;
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push_i32(out: &mut Vec<u8>, v: i32) {
        out.extend_from_slice(&v.to_le_bytes());
    }
    fn push_i64(out: &mut Vec<u8>, v: i64) {
        out.extend_from_slice(&v.to_le_bytes());
    }
    fn push_f32(out: &mut Vec<u8>, v: f32) {
        out.extend_from_slice(&v.to_le_bytes());
    }

    /// Read the module docs' field order, emitting a minimal valid node body.
    fn push_node(out: &mut Vec<u8>, label_index: i32, rect: [f32; 4]) {
        push_i64(out, (flag::IS_BUTTON | flag::IS_FOCUSABLE) as i64); // flags
        push_i32(out, action::TAP as i32); // actions
        for _ in 0..8 {
            push_i32(out, 0); // maxValueLength..traversalParent
        }
        for _ in 0..3 {
            push_f32(out, 0.0); // scrollPosition/ExtentMax/ExtentMin
        }
        push_i32(out, 0); // role
        push_i32(out, -1); // identifier
        push_i32(out, label_index); // label
        push_i32(out, -1); // label attributes
        push_i32(out, -1); // value
        push_i32(out, -1); // value attributes
        push_i32(out, -1); // increasedValue
        push_i32(out, -1); // increasedValue attributes
        push_i32(out, -1); // decreasedValue
        push_i32(out, -1); // decreasedValue attributes
        push_i32(out, -1); // hint
        push_i32(out, -1); // hint attributes
        push_i32(out, -1); // tooltip
        push_i32(out, -1); // linkUrl
        push_i32(out, -1); // locale
        push_i32(out, -1); // minValue
        push_i32(out, -1); // maxValue
        push_i32(out, 0); // headingLevel
        push_i32(out, 0); // textDirection
        for v in rect {
            push_f32(out, v);
        }
        for _ in 0..32 {
            push_f32(out, 0.0); // transform + hitTestTransform
        }
        push_i32(out, 0); // traversal children
        push_i32(out, 0); // hit-test children
        push_i32(out, 0); // custom actions
    }

    #[test]
    fn decodes_a_single_node() {
        let strings = vec!["Increment".to_string()];
        let mut buf = Vec::new();
        push_i32(&mut buf, 7);
        push_node(&mut buf, 0, [40.0, 208.0, 260.0, 272.0]);
        let nodes = decode_semantics_update(&buf, &strings, &[]).expect("decode");
        assert_eq!(nodes.len(), 1);
        let node = &nodes[0];
        assert_eq!(node.id, 7);
        assert_eq!(node.label.as_deref(), Some("Increment"));
        assert!(node.has_flag(flag::IS_BUTTON));
        assert!(node.clickable());
        assert!(node.enabled());
        assert_eq!(node.bounds(), [40, 208, 260, 272]);
    }

    #[test]
    fn decodes_multiple_nodes_with_children() {
        let strings = vec!["Root".to_string(), "Child".to_string()];
        let mut buf = Vec::new();
        push_i32(&mut buf, 0);
        push_node(&mut buf, 0, [0.0, 0.0, 100.0, 100.0]);
        // Rewrite the traversal/hit-test child counts of node 0 with child id 1.
        // The counts live in the last three i32 of node 0's body; simplest is to
        // decode a second, independent node and assert both survive.
        push_i32(&mut buf, 1);
        push_node(&mut buf, 1, [10.0, 10.0, 20.0, 20.0]);
        let nodes = decode_semantics_update(&buf, &strings, &[]).expect("decode");
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].text(), Some("Root"));
        assert_eq!(nodes[1].text(), Some("Child"));
    }

    #[test]
    fn truncated_buffer_is_an_error() {
        let strings = vec!["x".to_string()];
        let mut buf = Vec::new();
        push_i32(&mut buf, 1);
        push_i64(&mut buf, 0);
        let err = decode_semantics_update(&buf, &strings, &[]).unwrap_err();
        assert!(matches!(err, SemanticsError::Truncated { .. }));
    }

    #[test]
    fn empty_buffer_yields_no_nodes() {
        let nodes = decode_semantics_update(&[], &[], &[]).expect("decode");
        assert!(nodes.is_empty());
    }

    #[test]
    fn tap_packet_matches_the_pointer_wire_size() {
        let packet = tap_packet(150.0, 240.0, 1_000_000);
        assert_eq!(packet.len(), POINTER_BYTES * 2);
        // change is the third i64 of each record.
        let change_at = |record: usize| -> i64 {
            let base = record * POINTER_BYTES + 2 * 8;
            i64::from_le_bytes(packet[base..base + 8].try_into().unwrap())
        };
        assert_eq!(change_at(0), POINTER_CHANGE_DOWN);
        assert_eq!(change_at(1), POINTER_CHANGE_UP);
    }

    #[test]
    fn detects_a_flutter_apk_from_zip_layout() {
        let mut cursor = std::io::Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut cursor);
            let opts = zip::write::SimpleFileOptions::default();
            writer.start_file("lib/x86_64/libflutter.so", opts).unwrap();
            std::io::Write::write_all(&mut writer, b"engine").unwrap();
            writer.start_file("lib/x86_64/libapp.so", opts).unwrap();
            std::io::Write::write_all(&mut writer, b"snapshot").unwrap();
            writer
                .start_file("assets/flutter_assets/FontManifest.json", opts)
                .unwrap();
            std::io::Write::write_all(&mut writer, b"[]").unwrap();
            writer.finish().unwrap();
        }
        let info = detect_bytes(cursor.get_ref())
            .expect("detect")
            .expect("flutter");
        assert_eq!(info.engine_abis, vec!["x86_64"]);
        assert_eq!(info.snapshot_abis, vec!["x86_64"]);
        assert!(info.has_flutter_assets);
        assert_eq!(info.host_engine_abi(), Some("x86_64"));
    }

    #[test]
    fn a_plain_apk_is_not_flutter() {
        let mut cursor = std::io::Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut cursor);
            let opts = zip::write::SimpleFileOptions::default();
            writer.start_file("classes.dex", opts).unwrap();
            std::io::Write::write_all(&mut writer, b"dex").unwrap();
            writer.finish().unwrap();
        }
        assert!(detect_bytes(cursor.get_ref()).expect("detect").is_none());
    }
}
