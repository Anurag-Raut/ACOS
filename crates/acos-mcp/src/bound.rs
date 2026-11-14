//! Bound every text an MCP reply carries.
//!
//! A single failing `acos_verify` on a React Native APK produced a ~14k-token
//! reply: the CLI's `error.message` embeds the tail of the guest's Java stack
//! (200+ frames) **and** the tail of the native stack (400+ lines), and the
//! same string is repeated three times — top-level `error`, the failing
//! `result.steps[i].error`, and `result.verdict.failure.error`. An agent pays
//! for all of it and reads none of it: the first frames and the last frames are
//! what identify a crash, and the full text is on disk in the run's artifacts.
//!
//! This module caps that. For every string longer than the cap:
//!
//! * the **head** is kept (the message, the exception class, the first frames);
//! * the **tail** is kept line-aligned, so the deepest `caused by:` and the
//!   last native frames survive;
//! * the middle is replaced by one marker naming the original size, the cap and
//!   the artifact path that still holds the whole text;
//! * a second, identical copy of a text that was already bounded is replaced by
//!   a one-line back-reference, so the full text appears **once** per reply.
//!
//! The CLI's `--json` output is unchanged: this is an MCP-reply concern (an
//! agent reading `verdict.json` from disk gets everything).

use std::collections::HashMap;

use serde_json::Value;

/// Default cap, in bytes, for one string inside an MCP reply.
pub const DEFAULT_MAX_TEXT_BYTES: usize = 4000;

/// Where the bounded region starts/ends, in bytes. The head is one third of the
/// budget, the tail two thirds: a stack trace's useful part is its end.
fn split(bytes: usize, cap: usize) -> (usize, usize) {
    let head = cap / 3;
    let tail = cap.saturating_sub(head);
    (head.min(bytes), tail.min(bytes))
}

/// One bounded string, with the numbers the marker reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bounded {
    /// The replacement text.
    pub text: String,
    /// Original byte length.
    pub original_bytes: usize,
    /// Bytes kept from the start.
    pub head_bytes: usize,
    /// Bytes kept from the end.
    pub tail_bytes: usize,
    /// `true` when this was a repeat of an already-bounded string.
    pub duplicate: bool,
}

/// Bound one string.
///
/// `where_` names the JSON path the string was found at (used by the marker),
/// `artifacts` names where the full text lives when the caller knows.
#[must_use]
pub fn bound_string(text: &str, cap: usize, where_: &str, artifacts: Option<&str>) -> Bounded {
    let original_bytes = text.len();
    if cap == 0 || original_bytes <= cap {
        return Bounded {
            text: text.to_string(),
            original_bytes,
            head_bytes: original_bytes,
            tail_bytes: 0,
            duplicate: false,
        };
    }
    let (mut head, mut tail) = split(original_bytes, cap);
    // Keep boundaries on line breaks where possible so a frame is never cut in
    // half; fall back to a UTF-8 character boundary.
    head = floor_char_boundary(text, head);
    tail = ceil_char_boundary(text, original_bytes.saturating_sub(tail));
    if let Some(idx) = text[..head].rfind('\n') {
        head = idx + 1;
    }
    if let Some(idx) = text[tail..].find('\n') {
        tail += idx + 1;
    }
    if tail <= head {
        // Degenerate (one enormous line): keep the plain byte split.
        let (h, t) = split(original_bytes, cap);
        head = floor_char_boundary(text, h);
        tail = ceil_char_boundary(text, original_bytes.saturating_sub(t));
    }
    let head_text = &text[..head];
    let tail_text = &text[tail..];
    let where_full = match artifacts {
        Some(path) => format!("full text: {path} (and every steps/*.json of this run)"),
        None => "full text: the run's artifacts directory, or re-run the CLI with --json".to_string(),
    };
    let where_ = if where_.is_empty() { "text" } else { where_ };
    let marker = format!(
        "\n… [ACOS MCP: {where_} bounded for size — {original_bytes} bytes total, showing the \
         first {head} and last {} bytes; {where_full}] …\n",
        original_bytes - tail,
    );
    Bounded {
        text: format!("{head_text}{marker}{tail_text}"),
        original_bytes,
        head_bytes: head,
        tail_bytes: original_bytes - tail,
        duplicate: false,
    }
}

fn floor_char_boundary(text: &str, mut idx: usize) -> usize {
    idx = idx.min(text.len());
    while idx > 0 && !text.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

fn ceil_char_boundary(text: &str, mut idx: usize) -> usize {
    idx = idx.min(text.len());
    while idx < text.len() && !text.is_char_boundary(idx) {
        idx += 1;
    }
    idx
}

/// The artifact path a verify envelope names, for the truncation marker.
fn artifact_hint(root: &Value) -> Option<String> {
    for pointer in [
        "/result/verdict_path",
        "/result/artifacts_dir",
        "/verdict_path",
        "/artifacts_dir",
    ] {
        if let Some(path) = root.pointer(pointer).and_then(Value::as_str) {
            if !path.is_empty() {
                return Some(path.to_string());
            }
        }
    }
    None
}

/// Bound an MCP tool-result envelope (or any JSON value).
///
/// Every string longer than `cap` is shortened, and the full text appears at
/// most once per reply. Returns the value that was written plus the number of
/// strings that were bounded (`stats`), which the tests assert on.
#[must_use]
pub fn bound_value(value: &Value, cap: usize) -> (Value, Bounds) {
    let artifacts = artifact_hint(value);
    let mut state = State {
        cap,
        artifacts,
        seen: HashMap::new(),
        bounds: Bounds::default(),
    };
    let out = walk(value, "", &mut state);
    (out, state.bounds)
}

/// What a reply's bounding did, reported under `_meta.acos_mcp.bounded`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Bounds {
    /// Strings that were shortened.
    pub truncated: usize,
    /// Strings replaced by a back-reference to an earlier identical one.
    pub duplicates: usize,
    /// Bytes of text removed from the reply in total.
    pub saved_bytes: usize,
    /// The cap that was applied.
    pub cap_bytes: usize,
}

impl Bounds {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.truncated == 0 && self.duplicates == 0
    }

    #[must_use]
    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "truncated_strings": self.truncated,
            "duplicate_strings": self.duplicates,
            "saved_bytes": self.saved_bytes,
            "cap_bytes": self.cap_bytes,
        })
    }
}

struct State {
    cap: usize,
    artifacts: Option<String>,
    /// Original string -> JSON path of its first (bounded) copy.
    seen: HashMap<String, String>,
    bounds: Bounds,
}

/// Paths that are documentation, not diagnosis: `acos_help` deliberately
/// returns a whole guide section (or, with `topic: "all"`, the whole guide)
/// when an agent asks for it. Bounding that would break the one request whose
/// purpose is the text, so those keys are exempt.
const EXEMPT_PATHS: &[&str] = &["/result/guide", "/result/markdown"];

fn walk(value: &Value, path: &str, state: &mut State) -> Value {
    match value {
        Value::String(text) => {
            if EXEMPT_PATHS.contains(&path)
                || state.cap == 0
                || text.len() <= state.cap
            {
                return Value::String(text.clone());
            }
            if let Some(first) = state.seen.get(text).cloned() {
                state.bounds.duplicates += 1;
                state.bounds.saved_bytes += text.len();
                return Value::String(format!(
                    "[ACOS MCP: identical {} -byte text already shown at `{first}`; not repeated]",
                    text.len()
                ));
            }
            let bounded = bound_string(text, state.cap, path, state.artifacts.as_deref());
            state.seen.insert(text.clone(), path.to_string());
            state.bounds.truncated += 1;
            state.bounds.cap_bytes = state.cap;
            state.bounds.saved_bytes += bounded.original_bytes - bounded.text.len();
            Value::String(bounded.text)
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .enumerate()
                .map(|(index, item)| walk(item, &format!("{path}/{index}"), state))
                .collect(),
        ),
        Value::Object(map) => {
            let mut out = serde_json::Map::with_capacity(map.len());
            for (key, item) in map {
                let child = if path.is_empty() {
                    format!("/{key}")
                } else {
                    format!("{path}/{key}")
                };
                out.insert(key.clone(), walk(item, &child, state));
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn huge(kind: &str, lines: usize) -> String {
        let mut text = format!("java.lang.AssertionError: {kind}: the app failed\n");
        for i in 0..lines {
            text.push_str(&format!(
                "\tat dev.acos.Fixture.call{i}(Fixture.java:{i})\n"
            ));
        }
        text.push_str("\tcaused by: java.lang.IllegalStateException: deepest\n");
        text
    }

    #[test]
    fn short_strings_are_untouched() {
        let bounded = bound_string("boom", 4000, "/error/message", None);
        assert_eq!(bounded.text, "boom");
        assert!(!bounded.duplicate);
    }

    #[test]
    fn a_huge_message_is_capped_and_names_its_artifacts() {
        let text = huge("verify", 400);
        let bounded = bound_string(&text, 4000, "/error/message", Some("out/run/verdict.json"));
        assert!(bounded.text.len() <= 4000 + 400, "{} bytes", bounded.text.len());
        assert!(bounded.text.contains("AssertionError"));
        assert!(bounded.text.contains("caused by: java.lang.IllegalStateException"));
        assert!(bounded.text.contains("ACOS MCP: /error/message bounded for size"));
        assert!(bounded.text.contains("out/run/verdict.json"));
        assert!(bounded.text.contains(&format!("{} bytes total", text.len())));
        assert_eq!(bounded.original_bytes, text.len());
        // No silent loss: the marker says how much was dropped.
        assert!(bounded.head_bytes > 0 && bounded.tail_bytes > 0);
    }

    #[test]
    fn a_reply_keeps_the_full_text_once_and_back_references_repeats() {
        let text = huge("verify", 400);
        let envelope = json!({
            "ok": false,
            "error": {"message": text},
            "result": {
                "verdict_path": "out/gaten/verdict.json",
                "steps": [{"error": {"message": text}}],
                "verdict": {"failure": {"error": {"message": text}}}
            }
        });
        let (bounded, stats) = bound_value(&envelope, 2000);
        assert_eq!(stats.truncated, 1);
        assert_eq!(stats.duplicates, 2);
        let first = bounded["error"]["message"].as_str().unwrap();
        assert!(first.contains("ACOS MCP: /error/message bounded"));
        // The artifact path the envelope itself names is in the marker.
        assert!(first.contains("out/gaten/verdict.json"));
        for pointer in [
            "/result/steps/0/error/message",
            "/result/verdict/failure/error/message",
        ] {
            let copy = bounded.pointer(pointer).unwrap().as_str().unwrap();
            assert!(
                copy.starts_with("[ACOS MCP: identical "),
                "a repeated tail must be a back-reference: {copy}"
            );
            assert!(copy.contains("/error/message"));
            assert!(copy.len() < 200);
        }
        // The whole reply stays a small multiple of the cap.
        let total = serde_json::to_string(&bounded).unwrap().len();
        assert!(total < 3 * 2000 + 1000, "reply is {total} bytes");
    }

    #[test]
    fn the_cap_is_configurable_and_zero_disables_it() {
        // Below the default cap: untouched.
        let short = huge("verify", 5);
        assert!(short.len() < DEFAULT_MAX_TEXT_BYTES);
        let default = bound_value(&Value::String(short.clone()), DEFAULT_MAX_TEXT_BYTES).0;
        assert_eq!(default.as_str().unwrap(), short, "under the cap: untouched");
        // Above it: bounded, and the marker names the (empty) path as `text`.
        let text = huge("verify", 100);
        let small = bound_value(&Value::String(text.clone()), 500).0;
        let small = small.as_str().unwrap();
        assert!(small.contains("bounded for size"));
        assert!(small.contains("ACOS MCP: text bounded"), "{small}");
        // `0` disables the bound entirely (the CLI behaviour).
        let off = bound_value(&Value::String(text.clone()), 0).0;
        assert_eq!(off.as_str().unwrap(), text);
    }

    #[test]
    fn the_help_guide_is_exempt_but_an_error_tail_is_not() {
        let guide = "x".repeat(20_000);
        let value = json!({"result": {"guide": guide, "error": {"message": "y".repeat(20_000)}}});
        let (out, stats) = bound_value(&value, 4000);
        assert_eq!(out["result"]["guide"].as_str().unwrap().len(), 20_000);
        assert!(out["result"]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("bounded for size"));
        assert_eq!(stats.truncated, 1);
    }

    #[test]
    fn repeated_short_strings_are_not_deduplicated() {
        let value = json!({"a": "same", "b": "same"});
        let (out, stats) = bound_value(&value, 4000);
        assert_eq!(out["a"], "same");
        assert_eq!(out["b"], "same");
        assert_eq!(stats.duplicates, 0);
    }
}
