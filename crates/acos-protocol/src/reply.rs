//! Agent-sized replies: one rule for the CLI's `--json` output and every MCP tool.
//!
//! Root cause C: a live UI tree is 12–111 KB. Inlining it costs an agent the
//! whole context window for one observation, and the old head/tail string bound
//! kept the *first* and *last* bytes of a flat text rendering — exactly the
//! parts of a screen that are not the content. This module replaces that with
//! one deterministic rule:
//!
//! * the **default** view is compact: every visible `text`/`content_desc` node,
//!   in document (reading) order, with bounds, clickable flag and ids;
//! * a compact list bigger than one page is **paged** with `cursor` /
//!   `next_cursor`, never cut in the middle;
//! * the **full** tree is always written to `tree/<frame_seq>.json` and the
//!   reply names its `tree_path`;
//! * a hard cap ([`MAX_REPLY_BYTES`]) applies to the serialized reply. Callers
//!   ask for the full tree explicitly; if it still does not fit, the compact
//!   view is returned with the path and a note, because a silent oversized
//!   reply is the failure this module exists to prevent.
//!
//! The rule is deliberately format-agnostic: ART result trees are objects with a
//! nested `nodes` array, Flutter semantics trees are a flat array, and an RN
//! tree may be any object with `children`. A tree node is treated as visible
//! unless it says `visible:false`.

use std::io;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

/// Bytes a diagnostic string keeps from each end when the reply has to fit.
///
/// The emergency path below bounds the *content* of a reply (one long string, a
/// repeated block of text) before it ever considers dropping the reply's
/// *structure*. 2 KiB is roughly a first screen of stack traces plus the marker
/// that says what was cut.
const CAP_STRING_BYTES: usize = 2 * 1024;

/// Floor for [`CAP_STRING_BYTES`]: below this a string stops being a diagnosis.
const CAP_STRING_FLOOR: usize = 256;

/// Hard ceiling for one serialized agent reply, in bytes.
///
/// The previous bounded-reply code in `acos-mcp` already kept one string under
/// 4 KB, but a tree is made of many small strings, so its cap never applied to
/// the reply as a whole. 20 KiB is below the 24 KiB ceiling that the F11
/// bounded-reply test asserts and leaves room for the envelope around the
/// compact screen (the compact view of a normal phone screen is ~1–3 KB).
pub const MAX_REPLY_BYTES: usize = 20 * 1024;

/// Default number of compact nodes per page.
pub const DEFAULT_PAGE_SIZE: usize = 40;

/// Path components the reply module owns.
pub const TREE_DIR: &str = "tree";

/// What the caller asked for when shaping a tree-bearing reply.
#[derive(Debug, Clone, Default)]
pub struct TreeShapeOptions {
    /// Zero-based compact-node cursor (ignored for a full tree).
    pub cursor: usize,
    /// Compact nodes per page; [`DEFAULT_PAGE_SIZE`] when zero.
    pub page_size: usize,
    /// Keep the full tree inline when it fits [`MAX_REPLY_BYTES`].
    pub full: bool,
    /// Directory that receives the full tree file.
    pub artifacts_dir: Option<PathBuf>,
    /// Session id, used to build a stable per-session directory when the caller
    /// did not choose one.
    pub session: Option<String>,
}

impl TreeShapeOptions {
    /// The page size that applies.
    #[must_use]
    pub fn page(&self) -> usize {
        if self.page_size == 0 {
            DEFAULT_PAGE_SIZE
        } else {
            self.page_size
        }
    }
}

/// The result of shaping one tree.
#[derive(Debug, Clone, PartialEq)]
pub struct ShapedTree {
    /// The value that belongs in the reply's `tree` slot.
    pub view: Value,
    /// Number of visible text/content-desc nodes in the full tree.
    pub total: usize,
    /// Number of nodes in this compact page (or in the full tree).
    pub returned: usize,
    /// Cursor for the next compact page, when one exists.
    pub next_cursor: Option<usize>,
    /// Absolute path of the full tree on disk, when one was written.
    pub tree_path: Option<String>,
}

/// Compact tree nodes in document (pre-order) order.
///
/// Only visible nodes whose `text` or `content_desc` is non-empty are returned:
/// a structural wrapper carries no answer to "what is on screen". Each node
/// keeps its tree `id` and Android `resource_id`, bounds, class, clickable and
/// window/package fields, its text and content description, and the input state
/// (`focused`, `password`) an agent needs to type into it.
#[must_use]
pub fn compact_nodes(tree: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    match tree {
        Value::Array(nodes) => {
            for node in nodes {
                walk_compact(node, &mut out);
            }
        }
        Value::Object(map) => {
            if let Some(Value::Array(nodes)) = map.get("nodes") {
                for node in nodes {
                    walk_compact(node, &mut out);
                }
            } else {
                walk_compact(tree, &mut out);
            }
        }
        _ => {}
    }
    out
}

fn walk_compact(node: &Value, out: &mut Vec<Value>) {
    let Some(map) = node.as_object() else {
        return;
    };
    let visible = map.get("visible").and_then(Value::as_bool).unwrap_or(true);
    let text = string_field(map, "text");
    let content_desc = map
        .get("content_desc")
        .or_else(|| map.get("content_description"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if visible && (!text.is_empty() || !content_desc.is_empty()) {
        let mut compact = Map::new();
        compact.insert("id".into(), node_id(map));
        compact.insert(
            "resource_id".into(),
            map.get("resource_id")
                .cloned()
                .unwrap_or_else(|| Value::String(String::new())),
        );
        compact.insert(
            "class".into(),
            map.get("class")
                .cloned()
                .unwrap_or_else(|| Value::String(String::new())),
        );
        compact.insert("text".into(), Value::String(text));
        compact.insert(
            "content_desc".into(),
            Value::String(content_desc.to_string()),
        );
        compact.insert(
            "bounds".into(),
            map.get("bounds")
                .cloned()
                .unwrap_or_else(|| Value::String(String::new())),
        );
        compact.insert(
            "clickable".into(),
            Value::Bool(
                map.get("clickable")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            ),
        );
        // Input state is what an agent needs to act on the node it found:
        // `focused` is how the typing path knows where text will land, and
        // `password` is how it masks the characters it types (REL3E). Both are
        // booleans, so the page stays small.
        for key in ["visible", "window", "package", "focused", "password"] {
            if let Some(value) = map.get(key) {
                compact.insert(key.to_string(), value.clone());
            }
        }
        out.push(Value::Object(compact));
    }
    if let Some(Value::Array(children)) = map.get("children") {
        for child in children {
            walk_compact(child, out);
        }
    }
}

fn string_field(map: &Map<String, Value>, key: &str) -> String {
    map.get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn node_id(map: &Map<String, Value>) -> Value {
    map.get("id")
        .cloned()
        .or_else(|| map.get("resource_id").cloned())
        .unwrap_or_else(|| Value::String(String::new()))
}

/// Render one compact page as plain text, in reading order.
///
/// The line format is the agent-facing one used by `observe --text`:
/// `class#resource_id "text" bounds clickable`. An empty text/content
/// description can never be the reason a line exists, so the first page is a
/// ~1–3 KB answer instead of a head/tail fragment of a 30 KB dump.
#[must_use]
pub fn compact_text(
    tree: &Value,
    cursor: usize,
    page_size: usize,
) -> (String, usize, Option<usize>) {
    let nodes = compact_nodes(tree);
    let total = nodes.len();
    let page = if page_size == 0 {
        DEFAULT_PAGE_SIZE
    } else {
        page_size
    };
    let start = cursor.min(total);
    let end = start.saturating_add(page).min(total);
    let mut text = String::new();
    for node in &nodes[start..end] {
        let class = node.get("class").and_then(Value::as_str).unwrap_or("");
        let resource_id = node
            .get("resource_id")
            .and_then(Value::as_str)
            .unwrap_or("");
        let text_value = node.get("text").and_then(Value::as_str).unwrap_or("");
        let content_desc = node
            .get("content_desc")
            .and_then(Value::as_str)
            .unwrap_or("");
        let bounds = node.get("bounds").and_then(Value::as_str).unwrap_or("");
        let clickable = node
            .get("clickable")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !class.is_empty() {
            text.push_str(class);
        }
        if !resource_id.is_empty() {
            text.push('#');
            text.push_str(resource_id);
        }
        if !text_value.is_empty() {
            text.push_str(&format!(" {text_value:?}"));
        } else if !content_desc.is_empty() {
            text.push_str(&format!(" desc={content_desc:?}"));
        }
        if !bounds.is_empty() {
            text.push(' ');
            text.push_str(bounds);
        }
        if clickable {
            text.push_str(" clickable");
        }
        text.push('\n');
    }
    let next_cursor = (end < total).then_some(end);
    (text, total, next_cursor)
}

/// Shape one full tree into either a compact page or the full value.
///
/// `tree` is the untruncated value; `opts.page()` is the compact page size.
/// The full tree is written to `opts.artifacts_dir/tree/<seq>.json` when a
/// directory is given, and the returned `tree_path` is absolute so an agent in
/// another repository can read it.
#[must_use]
pub fn shape_tree(tree: &Value, sequence: Option<u64>, opts: &TreeShapeOptions) -> ShapedTree {
    let nodes = compact_nodes(tree);
    let total = nodes.len();
    let tree_path = opts.artifacts_dir.as_ref().and_then(|dir| {
        let seq = sequence.unwrap_or(0);
        write_full_tree(dir, seq, tree)
            .ok()
            .map(|path| path.display().to_string())
    });
    if opts.full {
        return ShapedTree {
            view: tree.clone(),
            total,
            returned: total,
            next_cursor: None,
            tree_path,
        };
    }
    let page_size = opts.page();
    let start = opts.cursor.min(total);
    let end = start.saturating_add(page_size).min(total);
    let page: Vec<Value> = nodes[start..end].to_vec();
    let next_cursor = (end < total).then_some(end);
    let view = match tree {
        Value::Array(_) => Value::Array(page),
        Value::Object(map) => {
            let mut compact = map.clone();
            compact.insert("compact".into(), Value::Bool(true));
            compact.insert("node_count".into(), json!(total));
            compact.insert("returned".into(), json!(end - start));
            compact.insert("cursor".into(), json!(start));
            compact.insert(
                "next_cursor".into(),
                next_cursor.map_or(Value::Null, |cursor| json!(cursor)),
            );
            compact.insert("nodes".into(), Value::Array(page));
            compact.insert(
                "note".into(),
                Value::String(match &tree_path {
                    Some(path) => format!(
                        "compact view: visible text/content_desc nodes in reading order; full tree: {path}"
                    ),
                    None => "compact view: visible text/content_desc nodes in reading order".into(),
                }),
            );
            Value::Object(compact)
        }
        other => other.clone(),
    };
    ShapedTree {
        view,
        total,
        returned: end - start,
        next_cursor,
        tree_path,
    }
}

/// Write the full tree as pretty JSON under `dir/tree/<seq>.json`.
///
/// # Errors
/// The directory or file could not be written.
pub fn write_full_tree(dir: &Path, sequence: u64, tree: &Value) -> io::Result<PathBuf> {
    let tree_dir = dir.join(TREE_DIR);
    std::fs::create_dir_all(&tree_dir)?;
    let path = tree_dir.join(format!("{sequence}.json"));
    let bytes = serde_json::to_vec_pretty(tree)?;
    std::fs::write(&path, bytes)?;
    Ok(std::fs::canonicalize(&path).unwrap_or(path))
}

/// Replace `value["result"]["tree"]` with its compact form and name the file.
///
/// `sequence` names the full-tree artifact; when absent it is read from
/// `result.frame.frame_seq`, `result.frame_seq`, or `tree.frame.frame_seq`.
/// Returns `true` when a tree was present.
pub fn shape_result_tree(
    value: &mut Value,
    sequence: Option<u64>,
    opts: &TreeShapeOptions,
) -> bool {
    let Some(tree) = value.pointer("/result/tree").cloned() else {
        return false;
    };
    // The daemon (or an earlier layer) already shaped this tree; do not compact
    // a compact page again (that would turn page N into page 0 of page N).
    if value
        .pointer("/result/tree/compact")
        .and_then(Value::as_bool)
        == Some(true)
    {
        return false;
    }
    let seq = sequence
        .or_else(|| {
            value
                .pointer("/result/frame/frame_seq")
                .and_then(Value::as_u64)
        })
        .or_else(|| value.pointer("/result/frame_seq").and_then(Value::as_u64))
        .or_else(|| {
            value
                .pointer("/result/tree/frame/frame_seq")
                .and_then(Value::as_u64)
        });
    let shaped = shape_tree(&tree, seq, opts);
    if let Some(result) = value.get_mut("result").and_then(Value::as_object_mut) {
        result.insert("tree".into(), shaped.view);
        result.insert("tree_node_count".into(), json!(shaped.total));
        result.insert("tree_returned".into(), json!(shaped.returned));
        result.insert(
            "tree_next_cursor".into(),
            shaped
                .next_cursor
                .map_or(Value::Null, |cursor| json!(cursor)),
        );
        if let Some(path) = shaped.tree_path {
            result.insert("tree_path".into(), Value::String(path));
        }
    }
    true
}

/// Truncate reply arrays until the serialized value fits `max_bytes`.
///
/// This is the last line of defence, not the normal shaping path. It prefers
/// the tree's compact page and the log list, because those are the two unbounded
/// arrays a session reply carries. If even the reduced value does not fit, the
/// result is replaced by a small, honest pointer object: a reply never silently
/// exceeds the cap, and the full evidence stays on disk.
///
/// Returns the number of bytes by which the value was reduced (0 when it already
/// fit).
pub fn enforce_cap(value: &mut Value, max_bytes: usize) -> usize {
    enforce_cap_measured(value, max_bytes, serialized_len)
}

/// [`enforce_cap`] measured on the CLI's pretty-printed output.
///
/// The CLI prints with `to_string_pretty`; the same value is larger on the wire
/// than its compact JSON form, so a reply that fits the cap compactly can still
/// overflow the terminal/client. This variant measures what is printed.
pub fn enforce_cap_pretty(value: &mut Value, max_bytes: usize) -> usize {
    enforce_cap_measured(value, max_bytes, serialized_pretty_len)
}

fn enforce_cap_measured(
    value: &mut Value,
    max_bytes: usize,
    measure: fn(&Value) -> usize,
) -> usize {
    let before = measure(value);
    if before <= max_bytes {
        return 0;
    }
    // 1. Trees: keep halving the compact page (and keep the path).
    loop {
        let Some(nodes) = value
            .pointer("/result/tree/nodes")
            .and_then(Value::as_array)
            .map(|nodes| nodes.len())
        else {
            break;
        };
        if nodes <= 1 {
            break;
        }
        shrink_tree_page(value, nodes / 2);
        if measure(value) <= max_bytes {
            return before - measure(value);
        }
    }
    // 2. Logs: keep the newest half of the list, say so, and name the full file.
    loop {
        let Some(count) = value
            .pointer("/result/logs")
            .and_then(Value::as_array)
            .map(|nodes| nodes.len())
        else {
            break;
        };
        if count <= 1 {
            break;
        }
        let keep = count / 2;
        if let Some(logs) = value
            .pointer_mut("/result/logs")
            .and_then(Value::as_array_mut)
        {
            logs.truncate(keep);
        }
        if let Some(result) = value.get_mut("result").and_then(Value::as_object_mut) {
            result.insert("logs_truncated".into(), Value::Bool(true));
            result.insert("logs_returned".into(), json!(keep));
        }
        if measure(value) <= max_bytes {
            return before - measure(value);
        }
    }
    // 3. Content before structure: a reply that overflows because of *what it
    //    says* (a launch ladder repeats the same 13 KB blocker on every unproven
    //    rung; a stack dump sits in `error.message`) must not lose *which rungs
    //    it reports*. Bound every long string (head + tail + marker) and replace
    //    a repeated block by a back-reference, halving the budget while the
    //    reply still does not fit. `reached_stage`, the rung names/statuses and
    //    the error code an agent acts on survive; only the prose shrinks.
    //    Keeping the ladder is not cosmetic: an agent retries on `reached_stage`
    //    and reads `blocker.code`, and 41566095 (REL3C) replaced the whole
    //    result here, so a blocked launch answered `result: {reply_capped}` with
    //    no rungs at all (crates/acos-cli/tests/native_launch.rs).
    let mut string_cap = CAP_STRING_BYTES;
    loop {
        bound_long_strings(value, string_cap);
        if measure(value) <= max_bytes {
            return before - measure(value);
        }
        if string_cap <= CAP_STRING_FLOOR {
            break;
        }
        string_cap /= 2;
    }
    // 4. Anything else: keep the envelope and the diagnosis, replace the
    //    oversized result with a pointer to the artifacts. A silent cut is worse
    //    than an honest "read it from here".
    if let Some(result) = value.get_mut("result").and_then(Value::as_object_mut) {
        let keep: Vec<(&str, Value)> = [
            "tree_path",
            "verdict_path",
            "artifacts_dir",
            "session_id",
            "frame_seq",
            "tree_node_count",
        ]
        .into_iter()
        .filter_map(|key| result.get(key).cloned().map(|value| (key, value)))
        .collect();
        let mut replacement = Map::new();
        replacement.insert("reply_capped".into(), Value::Bool(true));
        replacement.insert("cap_bytes".into(), json!(max_bytes));
        replacement.insert(
            "note".into(),
            Value::String(
                "this result was larger than the reply cap; the full evidence is in the \
                 artifacts paths named here"
                    .to_string(),
            ),
        );
        for (key, value) in keep {
            replacement.insert(key.to_string(), value);
        }
        *result = replacement;
    }
    before - measure(value)
}

/// Bound every string longer than `cap` in `value`, and replace a repeat of an
/// already-bounded string with a one-line back-reference.
///
/// The whole point is that the value keeps its shape: callers walk
/// `result.stages[i].status` and `result.reached_stage` after the cap runs.
fn bound_long_strings(value: &mut Value, cap: usize) {
    let mut seen: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    bound_in_place(value, "result", cap, &mut seen);
}

fn bound_in_place(
    value: &mut Value,
    path: &str,
    cap: usize,
    seen: &mut std::collections::HashMap<String, String>,
) {
    match value {
        Value::String(text) => {
            if text.len() <= cap {
                return;
            }
            let original = text.to_string();
            let bytes = original.len();
            if let Some(first) = seen.get(&original) {
                // The same block of text twice costs an agent twice and says
                // nothing new; the first copy carries the whole text. The head
                // line travels with the back-reference, because that line is the
                // message an agent greps for (`... re-run with --native`), not
                // the stack dump behind it.
                let head = original.lines().next().unwrap_or_default();
                let head = &head[..floor_char_boundary(head, 512)];
                *text = format!(
                    "… [ACOS: identical to {first} ({bytes} bytes), not repeated; it begins: \
                     {head}] …"
                );
                return;
            }
            seen.insert(original.clone(), path.to_string());
            *text = bound_string_head_tail(&original, cap, path);
        }
        Value::Array(items) => {
            for (index, item) in items.iter_mut().enumerate() {
                bound_in_place(item, &format!("{path}[{index}]"), cap, seen);
            }
        }
        Value::Object(map) => {
            for (key, item) in map.iter_mut() {
                // Paths the cap does not shorten: they are the pointer to the
                // full evidence, and a cut path is useless.
                if matches!(
                    key.as_str(),
                    "tree_path" | "verdict_path" | "artifacts_dir" | "log_path"
                ) {
                    continue;
                }
                bound_in_place(item, &format!("{path}.{key}"), cap, seen);
            }
        }
        _ => {}
    }
}

/// Keep the head and the line-aligned tail of `text`, with an honest marker.
///
/// Head first (the message, the exception class, the first frames), tail last
/// (the deepest `caused by:`, the last frames): the two parts a reader uses.
fn bound_string_head_tail(text: &str, cap: usize, path: &str) -> String {
    let bytes = text.len();
    let head = floor_char_boundary(text, cap * 2 / 3);
    let tail_start = ceil_char_boundary(text, bytes - cap / 3).min(bytes);
    let (head, tail_start) = if tail_start <= head {
        (floor_char_boundary(text, cap / 2), ceil_char_boundary(text, bytes - cap / 2))
    } else {
        (head, tail_start)
    };
    if tail_start <= head || tail_start > bytes {
        return format!(
            "… [ACOS: {path} bounded for size — {bytes} bytes; read the run's artifacts] …"
        );
    }
    format!(
        "{}… [ACOS: {path} bounded for size — {bytes} bytes total, keeping the first {head} and \
         the last {} bytes; the full text is in the run's artifacts] …{}",
        &text[..head],
        bytes - tail_start,
        &text[tail_start..],
    )
}

fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    index = index.min(text.len());
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn ceil_char_boundary(text: &str, mut index: usize) -> usize {
    index = index.min(text.len());
    while index < text.len() && !text.is_char_boundary(index) {
        index += 1;
    }
    index
}

fn shrink_tree_page(value: &mut Value, page: usize) {
    let Some(tree) = value.get_mut("result").and_then(Value::as_object_mut) else {
        return;
    };
    let Some(tree_value) = tree.get_mut("tree").and_then(Value::as_object_mut) else {
        return;
    };
    let cursor = tree_value
        .get("cursor")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let total = tree_value
        .get("node_count")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    if let Some(nodes) = tree_value.get_mut("nodes").and_then(Value::as_array_mut) {
        nodes.truncate(page.max(1));
        let returned = nodes.len();
        tree_value.insert("returned".into(), json!(returned));
        let next = cursor.saturating_add(returned);
        tree_value.insert(
            "next_cursor".into(),
            if next < total {
                json!(next)
            } else {
                Value::Null
            },
        );
    }
}

fn serialized_len(value: &Value) -> usize {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len())
        .unwrap_or(0)
}

fn serialized_pretty_len(value: &Value) -> usize {
    serde_json::to_string_pretty(value)
        .map(|text| text.len())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(text: &str, top: i32, clickable: bool) -> Value {
        json!({
            "id": format!("n{top}"),
            "class": "android.widget.TextView",
            "resource_id": format!("app:id/row{top}"),
            "text": text,
            "content_desc": "",
            "bounds": format!("[0,{top}][1080,{}]", top + 48),
            "visible": true,
            "clickable": clickable,
            "children": [],
        })
    }

    fn large_tree(rows: usize) -> Value {
        let nodes: Vec<Value> = (0..rows)
            .map(|i| {
                node(
                    &format!("Settings row {i} — the content in the middle"),
                    i as i32 * 50,
                    i % 2 == 0,
                )
            })
            .collect();
        json!({"max_nodes": 2000, "truncated": false, "nodes": nodes})
    }

    #[test]
    fn compact_view_keeps_the_middle_text_and_the_ids() {
        let tree = large_tree(80);
        let opts = TreeShapeOptions::default();
        let shaped = shape_tree(&tree, Some(9), &opts);
        assert_eq!(shaped.total, 80);
        assert_eq!(shaped.returned, DEFAULT_PAGE_SIZE);
        assert_eq!(shaped.next_cursor, Some(DEFAULT_PAGE_SIZE));
        let nodes = shaped.view["nodes"].as_array().unwrap();
        // Reading order is preserved: the page is the first page, not head+tail.
        assert_eq!(
            nodes[0]["text"],
            "Settings row 0 — the content in the middle"
        );
        assert_eq!(
            nodes[DEFAULT_PAGE_SIZE - 1]["text"],
            "Settings row 39 — the content in the middle"
        );
        assert!(nodes[0]["bounds"].as_str().unwrap().starts_with("[0,0]"));
        assert_eq!(nodes[0]["clickable"], true);
        assert_eq!(nodes[0]["id"], "n0");
        assert_eq!(nodes[0]["resource_id"], "app:id/row0");
    }

    #[test]
    fn the_second_page_reaches_the_middle_and_later_rows() {
        let tree = large_tree(80);
        let opts = TreeShapeOptions {
            cursor: 40,
            ..TreeShapeOptions::default()
        };
        let shaped = shape_tree(&tree, None, &opts);
        let nodes = shaped.view["nodes"].as_array().unwrap();
        assert_eq!(
            nodes[0]["text"],
            "Settings row 40 — the content in the middle"
        );
        assert_eq!(
            nodes[39]["text"],
            "Settings row 79 — the content in the middle"
        );
        assert_eq!(shaped.next_cursor, None);
    }

    #[test]
    fn the_full_tree_is_written_once_and_named() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/acos-reply-tests")
            .join(format!(
                "tree-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ));
        let tree = large_tree(3);
        let opts = TreeShapeOptions {
            artifacts_dir: Some(dir.clone()),
            ..TreeShapeOptions::default()
        };
        let shaped = shape_tree(&tree, Some(193), &opts);
        let path = shaped.tree_path.expect("path");
        let written: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(written["nodes"].as_array().unwrap().len(), 3);
        assert_eq!(
            written["nodes"][1]["text"],
            "Settings row 1 — the content in the middle"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn enforce_cap_shrinks_a_compact_page_instead_of_cutting_the_middle() {
        let tree = large_tree(400);
        let opts = TreeShapeOptions::default();
        let mut envelope = json!({"result": {"tree": shape_tree(&tree, None, &opts).view}});
        let reduced = enforce_cap(&mut envelope, 8 * 1024);
        assert!(reduced > 0, "the fixture must exceed the cap");
        assert!(
            serialized_len(&envelope) <= 8 * 1024,
            "{}",
            serialized_len(&envelope)
        );
        let nodes = envelope
            .pointer("/result/tree/nodes")
            .unwrap()
            .as_array()
            .unwrap();
        assert!(!nodes.is_empty());
        // Still reading order, and the cursor says where to continue.
        assert!(nodes[0]["text"]
            .as_str()
            .unwrap()
            .contains("Settings row 0"));
        assert!(envelope
            .pointer("/result/tree/next_cursor")
            .unwrap()
            .as_u64()
            .is_some());
    }

    #[test]
    fn an_enormous_non_tree_result_is_replaced_by_a_pointer_not_silently_cut() {
        // Genuinely unshrinkable: many *small* strings, so bounding a string
        // cannot help and the result can only be replaced by a pointer.
        let junk: Vec<Value> = (0..600)
            .map(|i| json!(format!("junk row {i} {}", "y".repeat(60))))
            .collect();
        let mut envelope = json!({"ok": false, "error": {"message": "boom"},
                                  "result": {"tree_path": "out/tree/1.json", "junk": junk}});
        enforce_cap(&mut envelope, 4096);
        assert_eq!(envelope["result"]["reply_capped"], true);
        assert_eq!(envelope["result"]["tree_path"], "out/tree/1.json");
        assert!(serialized_len(&envelope) <= 4096);
    }

    #[test]
    fn an_oversized_string_is_bounded_in_place_before_the_result_is_replaced() {
        // One long string is content, not structure: bound it (head + tail +
        // marker) and keep the rest of the result, instead of throwing the
        // result away.
        let huge = "x".repeat(40_000);
        let mut envelope = json!({"ok": false, "error": {"message": "boom"},
                                  "result": {"tree_path": "out/tree/1.json", "junk": huge}});
        enforce_cap(&mut envelope, 4096);
        assert!(envelope["result"]["reply_capped"].is_null());
        assert_eq!(envelope["result"]["tree_path"], "out/tree/1.json");
        let junk = envelope["result"]["junk"].as_str().unwrap();
        assert!(junk.contains("bounded for size"), "{junk}");
        assert!(junk.contains("40000 bytes"), "{junk}");
        assert!(serialized_len(&envelope) <= 4096);
    }

    #[test]
    fn a_blocked_launch_keeps_its_ladder_and_shortens_the_repeated_blocker() {
        // The launch ladder names every rung and repeats one blocker text on
        // each unproven one (a real `cos launch` reply: 5 x 13 KB). The cap
        // must keep `reached_stage`, every rung's status and the blocker code:
        // that is what an agent acts on. REL3C replaced the whole result here.
        let blocker =
            format!("vm: nativeHello() is not implemented\n{}", "frame 1\n".repeat(1600));
        let stages: Vec<Value> = [
            ("apk_parsed", "ok"),
            ("process_booted", "ok"),
            ("bind_application", "ok"),
            ("application_on_create", "blocked"),
            ("client_transaction", "not_reached"),
            ("activity_on_create", "not_reached"),
            ("on_resume", "not_reached"),
            ("first_frame", "not_reached"),
        ]
        .into_iter()
        .map(|(stage, status)| {
            json!({
                "stage": stage,
                "ok": status == "ok",
                "status": status,
                "blocker": if status == "ok" { Value::Null } else { json!({
                    "code": "UNSUPPORTED_JAVA_API",
                    "message": blocker,
                    "details": {"stage": stage},
                }) },
            })
        })
        .collect();
        let mut envelope = json!({
            "ok": false,
            "status": "unsupported",
            "error": {"code": "UNSUPPORTED_JAVA_API", "message": blocker},
            "result": {
                "reached_stage": "bind_application",
                "stages": stages,
                "package": "dev.acos.fixtures.nativehello",
                "events": ["bind_application=ok", "application_on_create=blocked"],
            },
        });
        assert!(serialized_len(&envelope) > MAX_REPLY_BYTES);
        enforce_cap(&mut envelope, MAX_REPLY_BYTES);
        assert!(serialized_len(&envelope) <= MAX_REPLY_BYTES);
        assert!(envelope["result"]["reply_capped"].is_null());
        assert_eq!(envelope["result"]["reached_stage"], "bind_application");
        let stages = envelope["result"]["stages"].as_array().unwrap();
        assert_eq!(stages.len(), 8);
        assert_eq!(stages[3]["stage"], "application_on_create");
        assert_eq!(stages[3]["status"], "blocked");
        assert_eq!(stages[3]["blocker"]["code"], "UNSUPPORTED_JAVA_API");
        // The whole text appears once (the envelope's top-level error is walked
        // first), bounded head-and-tail with a marker; every later copy is a
        // back-reference that still carries the actionable first line, so an
        // agent reading any rung finds the `--native` hint.
        let message = envelope["error"]["message"].as_str().unwrap();
        assert!(message.contains("nativeHello() is not implemented"), "{message}");
        assert!(message.contains("bounded for size"), "{message}");
        assert!(message.contains("frame 1"), "the tail survives: {message}");
        for rung in [3usize, 4, 7] {
            let repeat = stages[rung]["blocker"]["message"].as_str().unwrap();
            assert!(repeat.contains("not repeated"), "{repeat}");
            assert!(
                repeat.contains("nativeHello() is not implemented"),
                "{repeat}"
            );
        }
        // The top-level error keeps its code.
        assert_eq!(envelope["error"]["code"], "UNSUPPORTED_JAVA_API");
    }
}
