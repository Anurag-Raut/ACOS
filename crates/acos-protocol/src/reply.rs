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

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

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
        // The node's *state* flags. `focused`/`password` are the live-input
        // evidence (FB1: "the tree now says the field is focused", "password
        // typing is masked"), `scrollable`/`enabled`/`checked`/`selected` are
        // what a tap or a wait decides on, and the daemon already computed them.
        // Dropping them left the compact screen unable to answer any question
        // about input, and forced a full-tree read for a boolean. They are
        // copied only when the source node carries them, so a minimal tree
        // (a Flutter semantics node, an RN tree) is not filled with invented
        // defaults.
        for key in [
            "visible",
            "window",
            "package",
            "enabled",
            "focused",
            "password",
            "scrollable",
            "checked",
            "selected",
        ] {
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

/// One process-wide cache of the full-tree artifacts already on disk.
///
/// A live screen is observed far more often than it changes: the same tree is
/// written by a reset and by every observation that follows it, each under a new
/// frame sequence. Serializing and writing that unchanged tree again used to be
/// on the reply path of *every* tree-bearing verb, and the dirty writes landed
/// in the next fast tap's settle window (PERF3: M8 form fast +3.6 ms, while the
/// tap reply itself never carried a tree). Reusing an existing file for
/// byte-identical content keeps the artifact the reply names without touching
/// the filesystem again.
#[derive(Default)]
struct TreeFileCache {
    /// `(artifacts dir, content hash)` -> the artifact already holding it.
    by_content: HashMap<(PathBuf, u64), PathBuf>,
    /// `(artifacts dir, frame seq)` -> the content hash written there, so a
    /// different tree for a reused sequence never overwrites a named artifact.
    by_seq: HashMap<(PathBuf, u64), u64>,
    /// The canonical `tree/` directory per artifacts dir, resolved once.
    canonical_dirs: HashMap<PathBuf, PathBuf>,
}

fn tree_file_cache() -> &'static Mutex<TreeFileCache> {
    static CACHE: OnceLock<Mutex<TreeFileCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(TreeFileCache::default()))
}

fn content_hash(bytes: &[u8]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

/// Write the full tree as pretty JSON under `dir/tree/<seq>.json`.
///
/// Byte-identical content already written for this artifacts directory is not
/// written again: the existing absolute path is returned, so the reply still
/// names a complete tree file and every frame keeps its evidence. A different
/// tree for a sequence that was already used gets its own `tree/<seq>-<hash>.json`
/// name instead of replacing a file an earlier reply named.
///
/// # Errors
/// The directory or file could not be written.
pub fn write_full_tree(dir: &Path, sequence: u64, tree: &Value) -> io::Result<PathBuf> {
    let bytes = serde_json::to_vec_pretty(tree)?;
    let hash = content_hash(&bytes);
    let key_content = (dir.to_path_buf(), hash);
    let key_seq = (dir.to_path_buf(), sequence);

    // Fast path: this exact tree is already an artifact for this directory.
    {
        let cache = tree_file_cache().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(path) = cache.by_content.get(&key_content) {
            if path.is_file() {
                return Ok(path.clone());
            }
        }
    }

    let tree_dir = dir.join(TREE_DIR);
    std::fs::create_dir_all(&tree_dir)?;
    let canonical_dir = {
        let mut cache = tree_file_cache().lock().unwrap_or_else(|e| e.into_inner());
        cache
            .canonical_dirs
            .entry(dir.to_path_buf())
            .or_insert_with(|| std::fs::canonicalize(&tree_dir).unwrap_or_else(|_| tree_dir.clone()))
            .clone()
    };
    let reused_sequence = {
        let cache = tree_file_cache().lock().unwrap_or_else(|e| e.into_inner());
        cache
            .by_seq
            .get(&key_seq)
            .is_some_and(|previous| *previous != hash)
    };
    let file_name = if reused_sequence {
        format!("{sequence}-{hash:016x}.json")
    } else {
        format!("{sequence}.json")
    };
    let path = canonical_dir.join(file_name);

    // Write beside the target and rename, so a reader never sees a partial
    // artifact and a failed write cannot truncate one already named.
    static TMP_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let tmp = canonical_dir.join(format!(
        ".tree-{sequence}-{}-{}.tmp",
        std::process::id(),
        TMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    if let Err(error) = std::fs::write(&tmp, &bytes) {
        let _ = std::fs::remove_file(&tmp);
        return Err(error);
    }
    if let Err(error) = std::fs::rename(&tmp, &path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(error);
    }

    let mut cache = tree_file_cache().lock().unwrap_or_else(|e| e.into_inner());
    if cache.by_content.len() > 4096 {
        cache.by_content.clear();
        cache.by_seq.clear();
        cache.canonical_dirs.clear();
    }
    cache.by_content.insert(key_content, path.clone());
    cache.by_seq.insert(key_seq, hash);
    Ok(path)
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
    // 2. Append-only lists (`logs`, launch `events`): keep the newest half, say
    //    so, and name the full file when there is one. These are the unbounded
    //    arrays a reply carries; truncating them keeps the *diagnosis* (the
    //    launch ladder, the verdict) inline, which is what a caller reads first.
    //    `acos launch` on a blocked APK used to exceed the cap on its `events`
    //    alone, so the whole ladder was replaced by a pointer and the reply could
    //    not say which rung blocked.
    for key in ["logs", "events"] {
        loop {
            let Some(count) = value
                .pointer(&format!("/result/{key}"))
                .and_then(Value::as_array)
                .map(|nodes| nodes.len())
            else {
                break;
            };
            if count <= 1 {
                break;
            }
            let keep = count / 2;
            if let Some(list) = value
                .pointer_mut(&format!("/result/{key}"))
                .and_then(Value::as_array_mut)
            {
                // Newest half: both lists are append-ordered, so the tail is
                // the part that explains the failure.
                list.drain(..count - keep);
            }
            if let Some(result) = value.get_mut("result").and_then(Value::as_object_mut) {
                result.insert(format!("{key}_truncated"), Value::Bool(true));
                result.insert(format!("{key}_returned"), json!(keep));
            }
            if measure(value) <= max_bytes {
                return before - measure(value);
            }
        }
    }
    // 2b. A launch ladder: the answer is `stage`/`status`/`blocker.code` plus the
    //     first line of `blocker.message`. That message can be tens of KB (the VM
    //     appends its API dump) and the same blocker is repeated on every later
    //     rung, so a blocked `acos launch --json` measured 88 KB - 71 KB of it
    //     rung text. REL3C's cap then replaced the *whole* result with a pointer
    //     object that carried no ladder at all, so the reply could not say which
    //     rung blocked. Keep the ladder, bound the repeated text, say so.
    if value
        .pointer("/result/stages")
        .and_then(Value::as_array)
        .is_some_and(|stages| !stages.is_empty())
    {
        const LADDER_MESSAGE_MAX: usize = 1024;
        let mut bounded = false;
        if let Some(stages) = value
            .pointer_mut("/result/stages")
            .and_then(Value::as_array_mut)
        {
            for stage in stages.iter_mut() {
                let Some(blocker) = stage.get_mut("blocker").and_then(Value::as_object_mut) else {
                    continue;
                };
                let Some(Value::String(message)) = blocker.get("message") else {
                    continue;
                };
                if message.len() <= LADDER_MESSAGE_MAX {
                    continue;
                }
                let bytes = message.len();
                let first_line = message.lines().next().unwrap_or("").to_string();
                // The repeated bulk is dropped, not silently: the marker says
                // how many bytes were there, that they are not repeated, and
                // where the whole text lives.
                let bounded_message = format!(
                    "{first_line}\n… [ACOS: this blocker is {bytes} bytes; only the first line \
                     is kept here and the remaining {} bytes are not repeated — the full text \
                     is in the run's artifacts] …",
                    bytes.saturating_sub(first_line.len()),
                );
                blocker.insert("message".into(), Value::String(bounded_message));
                blocker.insert("message_bytes".into(), json!(bytes));
                blocker.insert("message_truncated".into(), Value::Bool(true));
                bounded = true;
            }
        }
        if bounded {
            // The rung `details` are the same map the envelope's own
            // `error.details` carries (`launch::blocker_details`), so dropping
            // them from every rung loses nothing and keeps the ladder inline.
            let mut details_dropped = false;
            if let Some(stages) = value
                .pointer_mut("/result/stages")
                .and_then(Value::as_array_mut)
            {
                for stage in stages.iter_mut() {
                    let Some(blocker) = stage.get_mut("blocker").and_then(Value::as_object_mut) else {
                        continue;
                    };
                    if blocker.remove("details").is_some() {
                        details_dropped = true;
                    }
                }
            }
            if let Some(result) = value.get_mut("result").and_then(Value::as_object_mut) {
                result.insert("stage_messages_truncated".into(), Value::Bool(true));
                if details_dropped {
                    result.insert("stage_details_dropped".into(), Value::Bool(true));
                }
            }
        }
    }
    // 3. Content before structure: a reply that overflows because of *what it
    //    says* (a stack dump in `error.message`, an assertion detail repeated on
    //    every step) must not lose *which rungs it reports*. Bound every long
    //    string (head + tail + marker) and replace a repeated block by a
    //    back-reference, halving the budget while the reply still does not fit.
    //    `reached_stage`, the rung names/statuses and the error code an agent
    //    acts on survive; only the prose shrinks.
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
    // 3b. A capabilities answer: `commands[]` is the answer an agent acts on
    //     (what this build can do), while `modules[]` is the ADR-0029 catalog
    //     with its own query paths (`acos_services {"op": "list"}`, `acos_help
    //     topic: "limits"`). The catalog is the bulk — dropping it (naming the
    //     pointer) keeps every command row, which step 4 would otherwise replace
    //     wholesale with `reply_capped`.
    if value.pointer("/result/commands").is_some()
        && value
            .pointer_mut("/result")
            .and_then(Value::as_object_mut)
            .is_some_and(|result| {
                let had = result.remove("modules");
                if let Some(modules) = had {
                    result.insert("modules_omitted".into(), Value::Bool(true));
                    result.insert(
                        "modules_note".into(),
                        Value::String(
                            "the module catalog was omitted to keep this reply under the cap; \
                             ask `acos_services {\"op\": \"list\"}` or `acos_help {\"topic\": \
                             \"limits\"}` for the same rows"
                                .to_string(),
                        ),
                    );
                    if let Some(count) = modules.as_array().map(Vec::len) {
                        result.insert("modules_total".into(), json!(count));
                    }
                    true
                } else {
                    false
                }
            })
        && measure(value) <= max_bytes
    {
        return before - measure(value);
    }
    // 4. Anything else: keep the envelope, the diagnosis and the pointer to the
    //    artifacts, and drop only the *bulk*. A silent cut is worse than an
    //    honest "read it from here", but a pointer that says nothing about the
    //    outcome is worse still: an agent that asked "did this pass, and if not
    //    where" must not have to open a file to find out.
    if let Some(result) = value.get_mut("result") {
        *result = capped_result_pointer(result, max_bytes, measure);
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

/// The `result` object that replaces an oversized one: the artifact pointers
/// plus a bounded *diagnosis* (outcome, failing step, provenance).
///
/// REL3C shipped paths only, so a capped `verify --json` answered "reply_capped"
/// and nothing about the run. The fields below are exactly the ones an agent
/// branches on, and they are bounded by construction: they are scalars and two
/// small maps taken out of the verdict, never the tree, the step list or the
/// log.
fn capped_result_pointer(
    result: &Value,
    max_bytes: usize,
    measure: fn(&Value) -> usize,
) -> Value {
    let mut replacement = Map::new();
    replacement.insert("reply_capped".into(), Value::Bool(true));
    replacement.insert("cap_bytes".into(), json!(max_bytes));
    replacement.insert(
        "note".into(),
        Value::String(
            "this result was larger than the reply cap; only the bulk (tree, steps, logs) \
             was dropped and the full evidence is in the artifacts paths named here"
                .to_string(),
        ),
    );

    // Pointers to the evidence that stays on disk.
    for key in [
        "tree_path",
        "verdict_path",
        "artifacts_dir",
        "session_id",
        "frame_seq",
        "tree_node_count",
    ] {
        if let Some(value) = result.get(key) {
            replacement.insert(key.to_string(), value.clone());
        }
    }

    // The verdict's own outcome. `verdict.status` is authoritative; the legacy
    // `verdict.verdict` alias is used only when it is absent.
    let verdict = result.get("verdict");
    let verdict_status = verdict
        .and_then(|verdict| verdict.pointer("/status"))
        .or_else(|| verdict.and_then(|verdict| verdict.pointer("/verdict")))
        .or_else(|| result.get("status"))
        .cloned();
    if let Some(status) = verdict_status {
        replacement.insert("verdict_status".into(), status);
    }

    // The failing step, said in one line: index, kind, message. `step` is the
    // scenario's step kind; `message` is the step error's message (a failing
    // step always carries one), falling back to the verdict's failure summary.
    //
    // Only a run that did not pass has a failing step. Since MERGEINTEG §5 a
    // step is one entry whose `status` is the final outcome and a retried
    // attempt lives in `attempts[]`, so a passing verdict's steps are all
    // `pass`; this gate keeps that true even for a verdict written by an older
    // build (a transient non-pass attempt must never become "the failing step"
    // of a passing run).
    let verdict_passed = replacement
        .get("verdict_status")
        .and_then(Value::as_str)
        .is_some_and(|status| status == "pass");
    let failure_step = verdict
        .and_then(|verdict| verdict.pointer("/failure/step_index"))
        .and_then(Value::as_u64);
    if let Some(step) = (!verdict_passed)
        .then(|| {
            verdict
                .and_then(|verdict| verdict.pointer("/steps"))
                .and_then(Value::as_array)
                .and_then(|steps| {
                    steps.iter().find(|step| {
                        step.get("status")
                            .and_then(Value::as_str)
                            .is_some_and(|status| status != "pass")
                    })
                })
        })
        .flatten()
    {
        let mut summary = Map::new();
        for (key, from) in [
            ("index", "/index"),
            ("kind", "/step"),
            ("status", "/status"),
        ] {
            if let Some(value) = step.pointer(from) {
                summary.insert(key.to_string(), value.clone());
            }
        }
        if let Some(index) = failure_step {
            summary.insert("failure_index".into(), json!(index));
        }
        if let Some(message) = step
            .pointer("/error/message")
            .or_else(|| verdict.and_then(|verdict| verdict.pointer("/failure/summary")))
        {
            summary.insert("message".into(), message.clone());
        }
        replacement.insert("failed_step".into(), Value::Object(summary));
    }

    // Provenance: which APK/scenario produced this (REL3B `result.provenance`)
    // and which evidence classes are host-observed vs guest-asserted
    // (`verdict.evidence_provenance`).
    if let Some(provenance) = result.get("provenance") {
        replacement.insert("provenance".into(), provenance.clone());
    }
    if let Some(provenance) = verdict.and_then(|verdict| verdict.pointer("/evidence_provenance")) {
        replacement.insert("evidence_provenance".into(), provenance.clone());
    }
    if let Some(failure) = verdict.and_then(|verdict| verdict.pointer("/failure")) {
        replacement.insert("failure".into(), failure.clone());
    }

    // The summary above is small, but it is not allowed to break the cap it
    // exists to respect: drop the optional halves from the outside in, keeping
    // the outcome and the failing step (the two answers the cap must never take
    // away), and finally fall back to bare pointers.
    let mut value = Value::Object(replacement);
    for key in ["failure", "evidence_provenance", "provenance"] {
        if measure(&value) <= max_bytes {
            break;
        }
        if let Some(object) = value.as_object_mut() {
            object.remove(key);
        }
    }
    value
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

    fn reply_test_dir(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/acos-reply-tests")
            .join(format!(
                "{name}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ))
    }

    /// PERF3: an unchanged screen is observed many times. The second write of
    /// the same content names the artifact already on disk instead of dirtying
    /// another file on the reply path.
    #[test]
    fn identical_tree_content_reuses_the_named_artifact() {
        let dir = reply_test_dir("tree-reuse");
        let tree = large_tree(3);
        let first = write_full_tree(&dir, 7, &tree).expect("first write");
        let second = write_full_tree(&dir, 8, &tree).expect("second write");
        assert_eq!(first, second, "byte-identical content is not rewritten");
        let written: Value = serde_json::from_slice(&std::fs::read(&second).unwrap()).unwrap();
        assert_eq!(written["nodes"].as_array().unwrap().len(), 3);

        // A removed artifact is not trusted from the cache: the next request
        // writes it again and still returns a readable path.
        std::fs::remove_file(&second).expect("remove artifact");
        let third = write_full_tree(&dir, 9, &tree).expect("rewrite");
        assert!(third.is_file(), "a missing artifact is written again");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A frame sequence can be reused by a relaunched guest with a different
    /// tree. The new content must not replace the file an earlier reply named.
    #[test]
    fn a_different_tree_for_a_used_sequence_gets_its_own_artifact() {
        let dir = reply_test_dir("tree-seq-reuse");
        let first = write_full_tree(&dir, 7, &large_tree(3)).expect("first write");
        let other = json!({"max_nodes": 2000, "truncated": false, "nodes": [
            node("a different screen", 0, false)
        ]});
        let second = write_full_tree(&dir, 7, &other).expect("second write");
        assert_ne!(first, second, "different content must not overwrite");
        let old: Value = serde_json::from_slice(&std::fs::read(&first).unwrap()).unwrap();
        assert_eq!(old["nodes"].as_array().unwrap().len(), 3);
        let new: Value = serde_json::from_slice(&std::fs::read(&second).unwrap()).unwrap();
        assert_eq!(new["nodes"][0]["text"], "a different screen");
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

    /// A blocked launch ladder must survive the cap: the reply has to be able to
    /// say which rung blocked. The blocker message is the bulk (the VM appends
    /// its API dump, and the same blocker repeats on every later rung), so only
    /// that text is bounded - the ladder, the statuses and the blocker code stay.
    #[test]
    fn a_capped_launch_ladder_keeps_every_rung_and_the_blocker() {
        let long = format!(
            "vm: Ldev/acos/fixtures/nativehello/NativeHelloApp;->nativeHello()Ljava/lang/String; \
             is not implemented\n{}",
            "  at frameworks/base/api-34\n".repeat(900)
        );
        assert!(long.len() > 12_000);
        let rung = |name: &str, status: &str, blocker: Value| {
            json!({"stage": name, "status": status, "ok": status == "ok", "ms": null,
                   "blocker": blocker})
        };
        let blocked = || {
            json!({"code": "UNSUPPORTED_JAVA_API", "message": long,
                   "details": {"api": "Ldev/acos/...;->nativeHello()", "hint": "re-run with --native"}})
        };
        let mut envelope = json!({
            "ok": false,
            "status": "unsupported",
            "error": {"code": "UNSUPPORTED_JAVA_API", "message": "the native call did not run"},
            "result": {
                "stages": [
                    rung("apk_parsed", "ok", Value::Null),
                    rung("bind_application", "ok", Value::Null),
                    rung("application_on_create", "blocked", blocked()),
                    rung("client_transaction", "not_reached", blocked()),
                    rung("activity_on_create", "not_reached", blocked()),
                    rung("first_frame", "not_reached", blocked()),
                ],
                "reached_stage": "bind_application",
                "classes_loaded": 1659,
            },
        });
        assert!(
            serialized_pretty_len(&envelope) > MAX_REPLY_BYTES,
            "the fixture must exceed the cap: {}",
            serialized_pretty_len(&envelope)
        );
        let reduced = enforce_cap_pretty(&mut envelope, MAX_REPLY_BYTES);
        assert!(
            serialized_pretty_len(&envelope) <= MAX_REPLY_BYTES,
            "{}",
            serialized_pretty_len(&envelope)
        );
        assert!(reduced > 0);
        // The ladder is intact: every rung, in order, with its status.
        let stages = envelope["result"]["stages"].as_array().expect("ladder");
        assert_eq!(stages.len(), 6);
        assert_eq!(
            stages.iter().map(|s| s["stage"].as_str().unwrap()).collect::<Vec<_>>(),
            ["apk_parsed", "bind_application", "application_on_create",
             "client_transaction", "activity_on_create", "first_frame"]
        );
        assert_eq!(envelope["result"]["reached_stage"], "bind_application");
        // The blocking rung still answers "which, and why": code + first line,
        // with the byte count and the truncation flag.
        let blocker = &stages[2]["blocker"];
        assert_eq!(blocker["code"], "UNSUPPORTED_JAVA_API");
        assert_eq!(blocker["message_truncated"], true);
        assert!(
            blocker["message_bytes"].as_u64().unwrap() > 12_000,
            "{blocker:#?}"
        );
        let message = blocker["message"].as_str().expect("message");
        assert!(message.contains("nativeHello()"), "{message}");
        assert!(!message.contains("frameworks/base/api-34"), "{message}");
        // The pointer fallback is not used: this reply is still a ladder.
        assert!(envelope["result"].get("reply_capped").is_none());
    }

    /// A fix-shaped envelope whose `result` is far over the cap but whose
    /// diagnosis is small: the REL3C integration requirement is that the capped
    /// object still answers "did it pass, which step failed and why, and what
    /// produced this", not only "read the file".
    #[test]
    fn a_capped_verify_reply_keeps_the_outcome_failing_step_and_provenance() {
        let step = |index: usize, kind: &str, status: &str| {
            json!({"index": index, "step": kind, "status": status, "duration_ms": 1})
        };
        let mut steps = vec![
            step(0, "launch", "pass"),
            step(1, "tap", "pass"),
            json!({
                "index": 2,
                "step": "assert_text",
                "status": "fail",
                "duration_ms": 3,
                "error": {"code": "assertion_failed", "message": "expected \"Todo\" in the tree"},
            }),
        ];
        // Bulk: the real cap-filler is the step list and the assertion detail.
        for index in 3..400 {
            steps.push(json!({
                "index": index,
                "step": "assert_text",
                "status": "pass",
                "duration_ms": 1,
                "assertions": [{"detail": "y".repeat(120)}],
            }));
        }
        let mut envelope = json!({
            "ok": false,
            "status": "fail",
            "error": {"code": "assertion_failed", "message": "step 2 failed"},
            "result": {
                "artifacts_dir": "out/run",
                "verdict_path": "out/run/verdict.json",
                "provenance": {
                    "engine": "art",
                    "fidelity": "art-reference",
                    "apk": {"sha256_12": "8170835e70f2"},
                    "scenario": {"sha256_12": "4ac610c4ce06"},
                    "warnings": []
                },
                "verdict": {
                    "status": "fail",
                    "evidence_provenance": {"frames": "host-observed", "logs": "guest-asserted"},
                    "failure": {"code": "assertion_failed", "step_index": 2, "summary": "assert_text failed"},
                    "steps": steps,
                },
            },
        });
        assert!(
            serialized_len(&envelope) > MAX_REPLY_BYTES,
            "the fixture must exceed the cap: {}",
            serialized_len(&envelope)
        );
        let reduced = enforce_cap(&mut envelope, MAX_REPLY_BYTES);
        assert!(reduced > 0);
        assert!(serialized_len(&envelope) <= MAX_REPLY_BYTES);
        // Envelope outcome survives untouched.
        assert_eq!(envelope["status"], "fail");
        assert_eq!(envelope["error"]["code"], "assertion_failed");
        // Verdict outcome.
        assert_eq!(envelope["result"]["reply_capped"], true);
        assert_eq!(envelope["result"]["verdict_status"], "fail");
        // Failing step summary: index, kind, message.
        assert_eq!(envelope["result"]["failed_step"]["index"], 2);
        assert_eq!(envelope["result"]["failed_step"]["kind"], "assert_text");
        assert_eq!(
            envelope["result"]["failed_step"]["message"],
            "expected \"Todo\" in the tree"
        );
        // Provenance: apk/scenario identity and host vs guest evidence.
        assert_eq!(
            envelope["result"]["provenance"]["apk"]["sha256_12"],
            "8170835e70f2"
        );
        assert_eq!(
            envelope["result"]["evidence_provenance"]["frames"],
            "host-observed"
        );
        // Only the bulk is gone: no step list, no tree, no log list inline.
        assert!(envelope["result"].get("steps").is_none());
        assert!(envelope["result"].get("verdict").is_none());
    }
}
