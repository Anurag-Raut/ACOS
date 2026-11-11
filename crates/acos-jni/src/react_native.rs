//! React Native APK support (W158-A): structural detection and the UIManager
//! operation stream → agent UI tree.
//!
//! React Native's legacy (Paper) renderer builds the Android `View` tree
//! entirely in JavaScript and ships every mutation to the host as a
//! `UIManagerModule` call (`createView` / `updateView` / `setChildren` /
//! `manageChildren`). ACOS captures that stream (see
//! `crates/acos-runtime/src/rn_session.rs` and the W157-A probe) instead of
//! reading a native view hierarchy, because no native view managers are
//! registered yet.
//!
//! This module is the pure, VM-free half:
//!
//! * [`detect`] recognises an RN APK from its zip layout (`lib/<abi>/
//!   libreactnative*.so` plus `assets/index.android.bundle`), never a manifest
//!   guess;
//! * [`build_tree`] replays the recorded ops into an [`RnNode`] tree with
//!   tag/class/testID/text, and text→touchable resolution for `tap --text`.
//!
//! Bounds are reported only when the op stream carries them. The current RN
//! bridge registers no view managers, so `UIImplementation` never lays the tree
//! out and `bounds` is absent — the honest state, not a zero rectangle.

use std::collections::HashMap;
use std::path::Path;

use serde_json::Value;

/// Reading or parsing an APK for React Native structure failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReactNativeError {
    /// The APK could not be read.
    Io(String),
    /// The APK is not a valid zip archive.
    Zip(String),
}

impl std::fmt::Display for ReactNativeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReactNativeError::Io(m) | ReactNativeError::Zip(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for ReactNativeError {}

/// A React Native APK as identified by its zip layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReactNativeApk {
    /// Every ABI that ships an RN/Hermes runtime (`libreactnative*.so`,
    /// `libhermes.so`).
    pub native_abis: Vec<String>,
    /// Whether the APK carries `assets/index.android.bundle`.
    pub has_bundle: bool,
}

impl ReactNativeApk {
    /// The ABI a host launch should use.
    #[must_use]
    pub fn host_abi(&self) -> Option<&str> {
        self.native_abis.first().map(String::as_str)
    }
}

fn is_rn_library(lib: &str) -> bool {
    lib.starts_with("libreactnative") || lib == "libhermes.so"
}

/// Identify a React Native APK from its zip entries.
///
/// Recognition is structural: an RN/Hermes runtime library **and**
/// `assets/index.android.bundle`. A plain Android APK is never misclassified.
pub fn detect(apk_path: &Path) -> Result<Option<ReactNativeApk>, ReactNativeError> {
    let bytes = std::fs::read(apk_path)
        .map_err(|e| ReactNativeError::Io(format!("read {}: {e}", apk_path.display())))?;
    detect_bytes(&bytes)
}

/// [`detect`] over an in-memory zip.
pub fn detect_bytes(apk: &[u8]) -> Result<Option<ReactNativeApk>, ReactNativeError> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(apk))
        .map_err(|e| ReactNativeError::Zip(format!("open apk: {e}")))?;
    let mut native_abis = Vec::new();
    let mut has_bundle = false;
    for i in 0..archive.len() {
        let Ok(entry) = archive.by_index(i) else {
            continue;
        };
        let name = entry.name();
        if let Some(rest) = name.strip_prefix("lib/") {
            let mut parts = rest.splitn(2, '/');
            if let (Some(abi), Some(lib)) = (parts.next(), parts.next()) {
                if is_rn_library(lib) {
                    native_abis.push(abi.to_string());
                }
            }
        } else if name == "assets/index.android.bundle" {
            has_bundle = true;
        }
    }
    native_abis.sort();
    native_abis.dedup();
    if native_abis.is_empty() || !has_bundle {
        return Ok(None);
    }
    Ok(Some(ReactNativeApk {
        native_abis,
        has_bundle,
    }))
}

/// One node of the agent UI tree rebuilt from the UIManager op stream.
#[derive(Debug, Clone, PartialEq)]
pub struct RnNode {
    /// React view tag.
    pub tag: i32,
    /// View-manager class name (`RCTView`, `RCTText`, `RCTRawText`, …).
    pub class: String,
    /// `testID` prop, when present.
    pub test_id: Option<String>,
    /// The node's text: a raw-text node's own string, or an `RCTText` node's
    /// concatenated descendant raw texts.
    pub text: Option<String>,
    /// Layout bounds `[x0, y0, x1, y1]`, when the op stream carries them. The
    /// current bridge registers no view managers, so this is normally absent.
    pub bounds: Option<[f32; 4]>,
    /// Whether the JS responder system treats this node as a touchable.
    pub clickable: bool,
    /// The root tag this node belongs to (`createView`'s `rootTag`).
    pub root_tag: Option<i32>,
    /// Child nodes, in `setChildren` order.
    pub children: Vec<RnNode>,
}

impl RnNode {
    /// Depth-first iterator over this subtree (self first).
    pub fn walk(&self) -> impl Iterator<Item = &RnNode> {
        let mut stack = vec![self];
        std::iter::from_fn(move || {
            let node = stack.pop()?;
            for child in node.children.iter().rev() {
                stack.push(child);
            }
            Some(node)
        })
    }

    /// The touchable tag for `text`: the first node whose text matches, or its
    /// nearest clickable ancestor. Exact match wins over substring.
    #[must_use]
    pub fn tag_for_text(&self, text: &str) -> Option<i32> {
        self.find_text(text, false).or_else(|| self.find_text(text, true))
    }

    fn find_text(&self, text: &str, substring: bool) -> Option<i32> {
        fn search(node: &RnNode, text: &str, substring: bool, ancestor: Option<i32>) -> Option<i32> {
            let here = if node.clickable { Some(node.tag) } else { ancestor };
            let matched = node.text.as_deref().is_some_and(|t| {
                if substring {
                    t.contains(text)
                } else {
                    t == text
                }
            });
            if matched {
                if node.clickable {
                    return Some(node.tag);
                }
                if let Some(tag) = ancestor {
                    return Some(tag);
                }
            }
            for child in &node.children {
                if let Some(tag) = search(child, text, substring, here) {
                    return Some(tag);
                }
            }
            None
        }
        search(self, text, substring, None)
    }

    /// The tag with `testID == id`, or its nearest clickable ancestor.
    #[must_use]
    pub fn tag_for_test_id(&self, id: &str) -> Option<i32> {
        fn search(node: &RnNode, id: &str, ancestor: Option<i32>) -> Option<i32> {
            let here = if node.clickable { Some(node.tag) } else { ancestor };
            if node.test_id.as_deref() == Some(id) {
                if node.clickable {
                    return Some(node.tag);
                }
                if let Some(tag) = ancestor {
                    return Some(tag);
                }
                return Some(node.tag);
            }
            for child in &node.children {
                if let Some(tag) = search(child, id, here) {
                    return Some(tag);
                }
            }
            None
        }
        search(self, id, None)
    }

    /// Serialise the subtree for the protocol.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut object = serde_json::Map::new();
        object.insert("tag".into(), Value::from(self.tag));
        object.insert("class".into(), Value::from(self.class.clone()));
        if let Some(id) = &self.test_id {
            object.insert("testID".into(), Value::from(id.clone()));
        }
        if let Some(text) = &self.text {
            object.insert("text".into(), Value::from(text.clone()));
        }
        if let Some(bounds) = self.bounds {
            object.insert(
                "bounds".into(),
                Value::Array(bounds.iter().map(|v| Value::from(*v)).collect()),
            );
        }
        object.insert("clickable".into(), Value::from(self.clickable));
        if !self.children.is_empty() {
            object.insert(
                "children".into(),
                Value::Array(self.children.iter().map(RnNode::to_json).collect()),
            );
        }
        Value::Object(object)
    }
}

/// The rebuilt tree plus the roots it was built from.
#[derive(Debug, Clone, PartialEq)]
pub struct RnTree {
    /// Top-level nodes (usually the `AppContainer` root tag).
    pub roots: Vec<RnNode>,
    /// Number of ops consumed.
    pub op_count: usize,
}

impl RnTree {
    /// Total node count across every root.
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.roots.iter().map(|r| r.walk().count()).sum()
    }

    /// Find the first root-subtree node matching a predicate (pre-order).
    #[must_use]
    pub fn find(&self, pred: impl Fn(&RnNode) -> bool) -> Option<&RnNode> {
        for root in &self.roots {
            if let Some(node) = root.walk().find(|n| pred(n)) {
                return Some(node);
            }
        }
        None
    }

    /// The touchable tag for `text` across every root.
    #[must_use]
    pub fn tag_for_text(&self, text: &str) -> Option<i32> {
        self.roots.iter().find_map(|r| r.tag_for_text(text))
    }

    /// The touchable tag for `testID` across every root.
    #[must_use]
    pub fn tag_for_test_id(&self, id: &str) -> Option<i32> {
        self.roots.iter().find_map(|r| r.tag_for_test_id(id))
    }

    /// Serialise the forest for the protocol.
    #[must_use]
    pub fn to_json(&self) -> Value {
        Value::Array(self.roots.iter().map(RnNode::to_json).collect())
    }
}

/// Read a React tag from a JSON number that may be integer- or float-encoded.
///
/// The RN driver serialises `ReadableArray` children through `ReadableArray ->
/// JSON`, which prints each tag as a double (`3.0`). The tree builder must
/// accept both spellings or every `setChildren` edge is silently dropped.
fn json_tag(value: &Value) -> Option<i32> {
    if let Some(tag) = value.as_i64() {
        return i32::try_from(tag).ok();
    }
    let float = value.as_f64()?;
    if float.fract() == 0.0 && (f64::from(i32::MIN)..=f64::from(i32::MAX)).contains(&float) {
        Some(float as i32)
    } else {
        None
    }
}

#[derive(Debug, Default)]
struct NodeMeta {
    class: String,
    props: serde_json::Map<String, Value>,
    root_tag: Option<i32>,
}

#[derive(Debug, Default)]
struct TreeBuilder {
    meta: HashMap<i32, NodeMeta>,
    children: HashMap<i32, Vec<i32>>,
}

impl TreeBuilder {
    fn apply(&mut self, op: &Value) {
        let Some(object) = op.as_object() else { return };
        let Some(kind) = object.get("op").and_then(Value::as_str) else {
            return;
        };
        match kind {
            "createView" | "updateView" => {
                let Some(tag) = object.get("tag").and_then(json_tag) else {
                    return;
                };
                let entry = self.meta.entry(tag).or_default();
                if let Some(class) = object.get("class").and_then(Value::as_str) {
                    entry.class = class.to_string();
                }
                if let Some(root) = object.get("rootTag").and_then(json_tag) {
                    entry.root_tag = Some(root);
                }
                if let Some(props) = object.get("props").and_then(Value::as_object) {
                    for (k, v) in props {
                        entry.props.insert(k.clone(), v.clone());
                    }
                }
            }
            "setChildren" | "manageChildren" => {
                let Some(tag) = object.get("tag").and_then(json_tag) else {
                    return;
                };
                let mut kids = Vec::new();
                for key in ["children", "add"] {
                    if let Some(array) = object.get(key).and_then(Value::as_array) {
                        for child in array {
                            if let Some(n) = json_tag(child) {
                                kids.push(n);
                            }
                        }
                    }
                }
                self.children.insert(tag, kids);
            }
            _ => {}
        }
    }

    fn build(self) -> RnTree {
        // Every tag that appears anywhere; a root is a tag that is never a
        // child. The ReactRootView's own tag has no `createView`, so it is
        // tracked from parents/rootTag as well.
        let mut all: Vec<i32> = self.meta.keys().copied().collect();
        for (parent, kids) in &self.children {
            if !all.contains(parent) {
                all.push(*parent);
            }
            for kid in kids {
                if !all.contains(kid) {
                    all.push(*kid);
                }
            }
        }
        let mut child_set: Vec<i32> = self.children.values().flatten().copied().collect();
        child_set.sort_unstable();
        child_set.dedup();
        let mut roots: Vec<i32> = all
            .iter()
            .copied()
            .filter(|tag| !child_set.contains(tag))
            .collect();
        roots.sort_unstable();
        let roots = roots
            .into_iter()
            .map(|tag| self.build_node(tag, 0))
            .collect();
        RnTree {
            roots,
            op_count: 0,
        }
    }

    fn build_node(&self, tag: i32, depth: usize) -> RnNode {
        let meta = self.meta.get(&tag);
        let class = meta.map(|m| m.class.clone()).unwrap_or_default();
        let props = meta.map(|m| &m.props);
        let root_tag = meta.and_then(|m| m.root_tag);
        let test_id = props
            .and_then(|p| p.get("testID"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let clickable = props.is_some_and(|p| {
            p.get("onStartShouldSetResponder").and_then(Value::as_bool) == Some(true)
                || p.get("onResponderRelease").and_then(Value::as_bool) == Some(true)
                || p.get("onClick").and_then(Value::as_bool) == Some(true)
                || p.get("accessibilityRole").and_then(Value::as_str) == Some("button")
        });
        let bounds = props
            .and_then(|p| p.get("bounds").or_else(|| p.get("frame")))
            .and_then(Value::as_array)
            .and_then(|a| {
                let mut b = [0.0f32; 4];
                for (i, v) in a.iter().take(4).enumerate() {
                    b[i] = v.as_f64()? as f32;
                }
                Some(b)
            });
        let mut children = Vec::new();
        if depth < 256 {
            if let Some(kids) = self.children.get(&tag) {
                for kid in kids {
                    children.push(self.build_node(*kid, depth + 1));
                }
            }
        }
        let raw_text = props
            .and_then(|p| p.get("text"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let text = if class == "RCTText" || class == "RCTVirtualText" {
            let mut parts: Vec<String> = Vec::new();
            for child in &children {
                if let Some(t) = &child.text {
                    parts.push(t.clone());
                }
            }
            if parts.is_empty() {
                raw_text.clone()
            } else {
                Some(parts.join(""))
            }
        } else {
            raw_text
        };
        RnNode {
            tag,
            class,
            test_id,
            text,
            bounds,
            clickable,
            root_tag,
            children,
        }
    }
}

/// Replay a recorded `ACOS_RN_OP` stream into an agent UI tree.
///
/// `ops` is the JSON array as emitted by the RN bridge probe; malformed entries
/// are skipped rather than failing the whole tree.
#[must_use]
pub fn build_tree(ops: &[Value]) -> RnTree {
    let mut builder = TreeBuilder::default();
    for op in ops {
        builder.apply(op);
    }
    let mut tree = builder.build();
    tree.op_count = ops.len();
    tree
}

/// Parse the bridge's `opsJson()` string and build the tree.
pub fn build_tree_from_json(json: &str) -> Result<RnTree, serde_json::Error> {
    let ops: Vec<Value> = serde_json::from_str(json)?;
    Ok(build_tree(&ops))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_requires_runtime_and_bundle() {
        // No runtime library.
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        zip.start_file("assets/index.android.bundle", zip::write::SimpleFileOptions::default())
            .unwrap();
        let bytes = zip.finish().unwrap().into_inner();
        assert!(detect_bytes(&bytes).unwrap().is_none());

        // Runtime but no bundle.
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        zip.start_file("lib/x86_64/libhermes.so", zip::write::SimpleFileOptions::default())
            .unwrap();
        let bytes = zip.finish().unwrap().into_inner();
        assert!(detect_bytes(&bytes).unwrap().is_none());
    }

    #[test]
    fn detect_recognises_react_native() {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        zip.start_file("lib/x86_64/libreactnative.so", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.start_file("lib/x86_64/libhermes.so", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.start_file("assets/index.android.bundle", zip::write::SimpleFileOptions::default())
            .unwrap();
        let bytes = zip.finish().unwrap().into_inner();
        let apk = detect_bytes(&bytes).unwrap().unwrap();
        assert_eq!(apk.native_abis, vec!["x86_64"]);
        assert!(apk.has_bundle);
    }

    #[test]
    fn tree_accepts_float_encoded_tags() {
        // The driver's `json(ReadableArray)` prints children as doubles.
        let ops: Vec<Value> = serde_json::from_str(
            r#"[
            {"op":"createView","tag":3,"class":"RCTRawText","rootTag":1,"props":{"text":"TAP ME"}},
            {"op":"createView","tag":17,"class":"RCTView","rootTag":1,"props":{"testID":"hello-button","onStartShouldSetResponder":true}},
            {"op":"setChildren","tag":17,"children":[3.0]},
            {"op":"setChildren","tag":1.0,"children":[17.0]}
        ]"#,
        )
        .unwrap();
        let tree = build_tree(&ops);
        assert_eq!(tree.roots.len(), 1);
        assert_eq!(tree.roots[0].tag, 1);
        assert_eq!(tree.roots[0].children[0].tag, 17);
        assert_eq!(tree.tag_for_text("TAP ME"), Some(17));
        assert_eq!(tree.tag_for_test_id("hello-button"), Some(17));
    }

    #[test]
    fn tree_replays_ops_and_resolves_text() {
        let ops: Vec<Value> = serde_json::from_str(
            r#"[
            {"op":"createView","tag":3,"class":"RCTRawText","rootTag":1,"props":{"text":"Hello ACOS"}},
            {"op":"createView","tag":5,"class":"RCTText","rootTag":1,"props":{"testID":"hello-text"}},
            {"op":"setChildren","tag":5,"children":[3]},
            {"op":"createView","tag":13,"class":"RCTRawText","rootTag":1,"props":{"text":"TAP ME"}},
            {"op":"createView","tag":15,"class":"RCTText","rootTag":1,"props":{}},
            {"op":"setChildren","tag":15,"children":[13]},
            {"op":"createView","tag":17,"class":"RCTView","rootTag":1,"props":{"testID":"hello-button","onStartShouldSetResponder":true}},
            {"op":"setChildren","tag":17,"children":[15]},
            {"op":"createView","tag":25,"class":"RCTView","rootTag":1,"props":{"testID":"hello-root"}},
            {"op":"setChildren","tag":25,"children":[5,17]},
            {"op":"setChildren","tag":1,"children":[25]}
        ]"#,
        )
        .unwrap();
        let tree = build_tree(&ops);
        let root = &tree.roots[0];
        assert_eq!(root.tag, 1);
        assert_eq!(root.children[0].tag, 25);
        assert_eq!(tree.find(|n| n.tag == 5).unwrap().text.as_deref(), Some("Hello ACOS"));
        // `tap --text TAP ME` resolves through the RCTText to the touchable.
        assert_eq!(tree.tag_for_text("TAP ME"), Some(17));
        assert_eq!(tree.tag_for_test_id("hello-button"), Some(17));
        assert_eq!(tree.tag_for_text("nope"), None);
        assert_eq!(tree.node_count(), 7);
    }
}
