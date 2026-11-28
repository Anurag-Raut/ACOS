//! Per-session host-side visual annotation store (`acos_annotations`).
//!
//! Annotations live purely in the Session Controller (`acos serve`) alongside
//! each held session — 0 ms boot overhead, 0 MiB guest RAM, 0% idle CPU, and
//! zero guest window-stack contamination.
//!
//! Each annotation snapshots its own screen context (`activity`, `package`,
//! `display`, `bounds`, `nodes`, `comment`, `kind`, `created_ms`, and optional
//! `screenshot_path`) so multi-screen annotation batches stay intact as the
//! user navigates across Activities.

use std::path::{Path, PathBuf};

use acos_protocol::ErrorCode;
use acos_verify::BackendError;
use serde::Serialize;
use serde_json::{json, Value};

/// Maximum annotations retained per session (bounded memory / DoS cap).
pub const MAX_ANNOTATIONS_PER_SESSION: usize = 64;

/// Maximum decoded PNG payload size accepted for an annotation screenshot (4 MiB).
pub const MAX_ANNOTATION_PNG_BYTES: usize = 4 * 1024 * 1024;

/// Maximum comment or resolution-note byte length.
pub const MAX_ANNOTATION_COMMENT_BYTES: usize = 8 * 1024;

/// Maximum matched UI node snapshots attached to one annotation.
pub const MAX_ANNOTATION_NODES: usize = 32;

/// Maximum UTF-8 byte length for any projected string field inside a node snapshot.
pub const MAX_ANNOTATION_NODE_STR_BYTES: usize = 512;

/// Maximum total serialized JSON size across all `nodes` in one annotation (64 KiB).
pub const MAX_ANNOTATION_NODES_BYTES: usize = 64 * 1024;

const PNG_MAGIC: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

/// Display dimensions stored with an annotation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct DisplaySize {
    pub w: u32,
    pub h: u32,
}

/// Session metadata defaults used when an `add` call omits screen context.
#[derive(Clone, Debug, Default)]
pub struct AnnotationDefaults {
    pub activity: Option<String>,
    pub package: Option<String>,
    pub display: Option<DisplaySize>,
}

/// One visual annotation pinned to a specific screen snapshot.
#[derive(Clone, Debug, Serialize)]
pub struct Annotation {
    pub id: u64,
    pub kind: String,
    pub status: String,
    pub comment: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bounds: Option<[i32; 4]>,
    pub nodes: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display: Option<DisplaySize>,
    pub created_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution_note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot_path: Option<String>,
}

/// Bounded per-session annotation queue.
#[derive(Debug)]
pub struct AnnotationStore {
    next_id: u64,
    items: Vec<Annotation>,
}

impl Default for AnnotationStore {
    fn default() -> Self {
        Self::new()
    }
}

impl AnnotationStore {
    /// Create an empty store (zero allocations until the first annotation).
    #[must_use]
    pub fn new() -> Self {
        Self {
            next_id: 1,
            items: Vec::new(),
        }
    }

    /// Execute one `acos_annotations` action (`list`, `add`, `resolve`, `delete`, `clear`).
    pub fn apply(
        &mut self,
        args: &Value,
        artifacts_dir: &Path,
        defaults: AnnotationDefaults,
    ) -> Result<Value, BackendError> {
        let action = match args.get("action") {
            None | Some(Value::Null) => "list",
            Some(Value::String(s)) => s.trim(),
            Some(_) => {
                return Err(invalid_arg("`action` must be a string"));
            }
        };

        match action {
            "list" => self.list(args),
            "add" => self.add(args, artifacts_dir, defaults),
            "resolve" => self.resolve(args),
            "delete" => self.delete(args, artifacts_dir),
            "clear" => self.clear(args, artifacts_dir),
            other => Err(invalid_arg(format!(
                "unknown acos_annotations action `{other}` (expected list, add, resolve, delete, or clear)"
            ))),
        }
    }

    fn counts(&self) -> (usize, usize, usize) {
        let open = self.items.iter().filter(|a| a.status == "open").count();
        let total = self.items.len();
        (open, total - open, total)
    }

    fn list(&self, args: &Value) -> Result<Value, BackendError> {
        let status = parse_status_filter(args.get("status"), "open")?;
        let filtered: Vec<&Annotation> = self
            .items
            .iter()
            .filter(|a| status == "all" || a.status == status)
            .collect();
        let (open_count, resolved_count, total_count) = self.counts();
        Ok(json!({
            "action": "list",
            "status": status,
            "annotations": filtered,
            "count": filtered.len(),
            "open_count": open_count,
            "resolved_count": resolved_count,
            "total_count": total_count,
        }))
    }

    fn add(
        &mut self,
        args: &Value,
        artifacts_dir: &Path,
        defaults: AnnotationDefaults,
    ) -> Result<Value, BackendError> {
        if self.items.len() >= MAX_ANNOTATIONS_PER_SESSION {
            return Err(invalid_arg(format!(
                "annotation limit reached ({MAX_ANNOTATIONS_PER_SESSION} max per session); resolve, delete, or clear existing annotations first"
            )));
        }

        let comment = parse_required_text(args.get("comment"), "comment")?;
        let kind = parse_kind(args.get("kind"))?;
        let bounds = parse_bounds(args.get("bounds"))?;
        let nodes = parse_nodes(args.get("nodes"))?;
        let activity = parse_opt_nonempty_string(args.get("activity"), "activity")?
            .or(defaults.activity);
        let package = parse_opt_nonempty_string(args.get("package"), "package")?
            .or(defaults.package);
        let display = parse_display(args.get("display"))?.or(defaults.display);

        let id = self.next_id;
        let screenshot_path = match args.get("image_png_base64") {
            None | Some(Value::Null) => None,
            Some(Value::String(raw)) => {
                let trimmed = raw.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    let bytes = decode_png_base64(trimmed)?;
                    Some(write_annotation_png(artifacts_dir, id, &bytes)?)
                }
            }
            Some(_) => {
                return Err(invalid_arg("`image_png_base64` must be a base64 string"));
            }
        };

        self.next_id = self.next_id.saturating_add(1);
        let annotation = Annotation {
            id,
            kind: kind.to_string(),
            status: "open".to_string(),
            comment,
            bounds,
            nodes,
            activity,
            package,
            display,
            created_ms: crate::live_registry::now_ms(),
            resolved_ms: None,
            resolution_note: None,
            screenshot_path,
        };
        let value = serde_json::to_value(&annotation).unwrap_or(Value::Null);
        self.items.push(annotation);
        let (open_count, resolved_count, total_count) = self.counts();
        Ok(json!({
            "action": "add",
            "annotation": value,
            "open_count": open_count,
            "resolved_count": resolved_count,
            "total_count": total_count,
        }))
    }

    fn resolve(&mut self, args: &Value) -> Result<Value, BackendError> {
        let id = parse_id(args.get("id"))?;
        let note = match args.get("resolution_note") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => {
                let trimmed = s.trim();
                if trimmed.len() > MAX_ANNOTATION_COMMENT_BYTES {
                    return Err(invalid_arg(format!(
                        "`resolution_note` exceeds {MAX_ANNOTATION_COMMENT_BYTES} bytes"
                    )));
                }
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                }
            }
            Some(_) => return Err(invalid_arg("`resolution_note` must be a string")),
        };

        let Some(item) = self.items.iter_mut().find(|a| a.id == id) else {
            return Err(invalid_arg(format!("annotation #{id} not found")));
        };
        item.status = "resolved".to_string();
        item.resolved_ms = Some(crate::live_registry::now_ms());
        if note.is_some() {
            item.resolution_note = note;
        }
        let value = serde_json::to_value(&*item).unwrap_or(Value::Null);
        let (open_count, resolved_count, total_count) = self.counts();
        Ok(json!({
            "action": "resolve",
            "annotation": value,
            "open_count": open_count,
            "resolved_count": resolved_count,
            "total_count": total_count,
        }))
    }

    fn delete(&mut self, args: &Value, artifacts_dir: &Path) -> Result<Value, BackendError> {
        let id = parse_id(args.get("id"))?;
        let Some(pos) = self.items.iter().position(|a| a.id == id) else {
            return Err(invalid_arg(format!("annotation #{id} not found")));
        };
        let removed = self.items.remove(pos);
        cleanup_annotation_png(artifacts_dir, removed.screenshot_path.as_deref());
        let (open_count, resolved_count, total_count) = self.counts();
        Ok(json!({
            "action": "delete",
            "deleted_id": id,
            "open_count": open_count,
            "resolved_count": resolved_count,
            "total_count": total_count,
        }))
    }

    fn clear(&mut self, args: &Value, artifacts_dir: &Path) -> Result<Value, BackendError> {
        let status = parse_status_filter(args.get("status"), "all")?;
        let mut cleared = 0usize;
        let mut kept = Vec::with_capacity(self.items.len());
        for item in self.items.drain(..) {
            if status == "all" || item.status == status {
                cleanup_annotation_png(artifacts_dir, item.screenshot_path.as_deref());
                cleared += 1;
            } else {
                kept.push(item);
            }
        }
        self.items = kept;
        let (open_count, resolved_count, total_count) = self.counts();
        Ok(json!({
            "action": "clear",
            "status": status,
            "cleared": cleared,
            "open_count": open_count,
            "resolved_count": resolved_count,
            "total_count": total_count,
        }))
    }
}

fn invalid_arg(detail: impl Into<String>) -> BackendError {
    BackendError::Failed {
        code: ErrorCode::InvalidArgument,
        detail: detail.into(),
    }
}

fn parse_status_filter(value: Option<&Value>, default: &'static str) -> Result<&'static str, BackendError> {
    match value {
        None | Some(Value::Null) => Ok(default),
        Some(Value::String(s)) => match s.trim() {
            "open" => Ok("open"),
            "resolved" => Ok("resolved"),
            "all" => Ok("all"),
            other => Err(invalid_arg(format!(
                "invalid `status` `{other}` (expected open, resolved, or all)"
            ))),
        },
        Some(_) => Err(invalid_arg("`status` must be a string")),
    }
}

fn parse_kind(value: Option<&Value>) -> Result<&'static str, BackendError> {
    match value {
        None | Some(Value::Null) => Ok("element"),
        Some(Value::String(s)) => match s.trim() {
            "element" => Ok("element"),
            "box" => Ok("box"),
            "freehand" => Ok("freehand"),
            other => Err(invalid_arg(format!(
                "invalid `kind` `{other}` (expected element, box, or freehand)"
            ))),
        },
        Some(_) => Err(invalid_arg("`kind` must be a string")),
    }
}

fn parse_id(value: Option<&Value>) -> Result<u64, BackendError> {
    match value.and_then(Value::as_u64) {
        Some(id) if id >= 1 => Ok(id),
        _ => Err(invalid_arg("`id` must be a positive integer")),
    }
}

fn parse_required_text(value: Option<&Value>, field: &str) -> Result<String, BackendError> {
    let Some(raw) = value.and_then(Value::as_str) else {
        return Err(invalid_arg(format!("`{field}` is required and must be a string")));
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(invalid_arg(format!("`{field}` must not be empty")));
    }
    if trimmed.len() > MAX_ANNOTATION_COMMENT_BYTES {
        return Err(invalid_arg(format!(
            "`{field}` exceeds {MAX_ANNOTATION_COMMENT_BYTES} bytes"
        )));
    }
    Ok(trimmed.to_string())
}

fn parse_opt_nonempty_string(
    value: Option<&Value>,
    field: &str,
) -> Result<Option<String>, BackendError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else if trimmed.len() > 512 {
                Err(invalid_arg(format!("`{field}` exceeds 512 bytes")))
            } else {
                Ok(Some(trimmed.to_string()))
            }
        }
        Some(_) => Err(invalid_arg(format!("`{field}` must be a string"))),
    }
}

fn parse_bounds(value: Option<&Value>) -> Result<Option<[i32; 4]>, BackendError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(items)) => {
            if items.len() != 4 {
                return Err(invalid_arg(
                    "`bounds` must be a 4-element array [left, top, right, bottom]",
                ));
            }
            let mut coords = [0i32; 4];
            for (idx, item) in items.iter().enumerate() {
                let Some(num) = item.as_f64() else {
                    return Err(invalid_arg("`bounds` entries must be finite numbers"));
                };
                if !num.is_finite() || num < f64::from(i32::MIN) || num > f64::from(i32::MAX) {
                    return Err(invalid_arg("`bounds` entries must be finite 32-bit integers"));
                }
                coords[idx] = num.round() as i32;
            }
            let left = coords[0].min(coords[2]);
            let right = coords[0].max(coords[2]);
            let top = coords[1].min(coords[3]);
            let bottom = coords[1].max(coords[3]);
            Ok(Some([left, top, right, bottom]))
        }
        Some(_) => Err(invalid_arg(
            "`bounds` must be a 4-element array [left, top, right, bottom]",
        )),
    }
}

fn truncate_utf8(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn project_node_snapshot(raw: &serde_json::Map<String, Value>) -> Result<Value, BackendError> {
    let mut out = serde_json::Map::new();
    for key in [
        "class",
        "resource_id",
        "text",
        "content_desc",
        "package",
        "activity",
        "window",
    ] {
        if let Some(Value::String(s)) = raw.get(key) {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                out.insert(
                    key.to_string(),
                    Value::String(truncate_utf8(trimmed, MAX_ANNOTATION_NODE_STR_BYTES).to_string()),
                );
            }
        }
    }
    for key in ["clickable", "enabled", "scrollable"] {
        if let Some(Value::Bool(flag)) = raw.get(key) {
            out.insert(key.to_string(), Value::Bool(*flag));
        }
    }
    if let Some(raw_bounds) = raw.get("bounds") {
        match raw_bounds {
            Value::Null => {}
            Value::Array(_) => {
                if let Some(b) = parse_bounds(Some(raw_bounds))? {
                    out.insert("bounds".to_string(), json!(b));
                }
            }
            Value::String(s) => {
                let trimmed = s.trim();
                if !trimmed.is_empty() {
                    out.insert(
                        "bounds".to_string(),
                        Value::String(truncate_utf8(trimmed, 64).to_string()),
                    );
                }
            }
            _ => {
                return Err(invalid_arg(
                    "`nodes[*].bounds` must be a 4-element array or bounds string",
                ));
            }
        }
    }
    Ok(Value::Object(out))
}

fn parse_nodes(value: Option<&Value>) -> Result<Vec<Value>, BackendError> {
    match value {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => {
            if items.len() > MAX_ANNOTATION_NODES {
                return Err(invalid_arg(format!(
                    "`nodes` exceeds maximum of {MAX_ANNOTATION_NODES} items"
                )));
            }
            let mut projected = Vec::with_capacity(items.len());
            for item in items {
                let Some(map) = item.as_object() else {
                    return Err(invalid_arg("every entry in `nodes` must be an object"));
                };
                projected.push(project_node_snapshot(map)?);
            }
            let serialized_len = serde_json::to_vec(&projected)
                .map(|bytes| bytes.len())
                .unwrap_or(usize::MAX);
            if serialized_len > MAX_ANNOTATION_NODES_BYTES {
                return Err(invalid_arg(format!(
                    "`nodes` metadata ({serialized_len} bytes) exceeds maximum of {MAX_ANNOTATION_NODES_BYTES} bytes"
                )));
            }
            Ok(projected)
        }
        Some(_) => Err(invalid_arg("`nodes` must be an array of objects")),
    }
}

fn parse_display(value: Option<&Value>) -> Result<Option<DisplaySize>, BackendError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(map)) => {
            let w = map
                .get("w")
                .and_then(Value::as_u64)
                .and_then(|v| u32::try_from(v).ok())
                .filter(|&v| v > 0);
            let h = map
                .get("h")
                .and_then(Value::as_u64)
                .and_then(|v| u32::try_from(v).ok())
                .filter(|&v| v > 0);
            match (w, h) {
                (Some(w), Some(h)) => Ok(Some(DisplaySize { w, h })),
                _ => Err(invalid_arg(
                    "`display` must have positive integer `w` and `h`",
                )),
            }
        }
        Some(Value::Array(items)) if items.len() == 2 => {
            let w = items[0]
                .as_u64()
                .and_then(|v| u32::try_from(v).ok())
                .filter(|&v| v > 0);
            let h = items[1]
                .as_u64()
                .and_then(|v| u32::try_from(v).ok())
                .filter(|&v| v > 0);
            match (w, h) {
                (Some(w), Some(h)) => Ok(Some(DisplaySize { w, h })),
                _ => Err(invalid_arg("`display` must have positive integer [w, h]")),
            }
        }
        Some(_) => Err(invalid_arg(
            "`display` must be `{w, h}` with positive integers",
        )),
    }
}

/// Decode standard base64 (optionally prefixed with `data:image/png;base64,`),
/// enforcing [`MAX_ANNOTATION_PNG_BYTES`] and the 8-byte PNG signature.
pub fn decode_png_base64(raw: &str) -> Result<Vec<u8>, BackendError> {
    let encoded = raw
        .strip_prefix("data:image/png;base64,")
        .or_else(|| raw.strip_prefix("DATA:IMAGE/PNG;BASE64,"))
        .unwrap_or(raw);
    let bytes = decode_base64_bounded(encoded, MAX_ANNOTATION_PNG_BYTES)
        .map_err(invalid_arg)?;
    if bytes.len() < PNG_MAGIC.len() || &bytes[..PNG_MAGIC.len()] != PNG_MAGIC {
        return Err(invalid_arg(
            "`image_png_base64` is not a valid PNG (missing PNG signature)",
        ));
    }
    Ok(bytes)
}

fn decode_base64_bounded(input: &str, max_bytes: usize) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut quartet = [0u8; 4];
    let mut qlen = 0usize;
    let mut saw_pad = false;

    for byte in input.bytes() {
        if byte.is_ascii_whitespace() {
            continue;
        }
        if saw_pad && byte != b'=' {
            return Err("invalid base64 padding".to_string());
        }
        let val = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => {
                saw_pad = true;
                255
            }
            _ => return Err("invalid base64 character in `image_png_base64`".to_string()),
        };
        quartet[qlen] = val;
        qlen += 1;
        if qlen == 4 {
            emit_quartet(quartet, &mut out, max_bytes)?;
            qlen = 0;
        }
    }

    if qlen != 0 {
        return Err("truncated base64 payload in `image_png_base64`".to_string());
    }
    Ok(out)
}

fn emit_quartet(q: [u8; 4], out: &mut Vec<u8>, max_bytes: usize) -> Result<(), String> {
    if q[0] == 255 || q[1] == 255 {
        return Err("invalid base64 padding".to_string());
    }
    let produced = if q[2] == 255 {
        if q[3] != 255 {
            return Err("invalid base64 padding".to_string());
        }
        1
    } else if q[3] == 255 {
        2
    } else {
        3
    };
    if out.len().saturating_add(produced) > max_bytes {
        return Err(format!(
            "`image_png_base64` exceeds maximum decoded size ({max_bytes} bytes)"
        ));
    }
    out.push((q[0] << 2) | (q[1] >> 4));
    if produced >= 2 {
        out.push((q[1] << 4) | (q[2] >> 2));
    }
    if produced == 3 {
        out.push((q[2] << 6) | q[3]);
    }
    Ok(())
}

fn write_annotation_png(
    artifacts_dir: &Path,
    id: u64,
    bytes: &[u8],
) -> Result<String, BackendError> {
    let dir = artifacts_dir.join("annotations");
    std::fs::create_dir_all(&dir).map_err(|e| BackendError::Failed {
        code: ErrorCode::Internal,
        detail: format!("cannot create annotation artifact dir {}: {e}", dir.display()),
    })?;
    let path = dir.join(format!("{id}.png"));
    std::fs::write(&path, bytes).map_err(|e| BackendError::Failed {
        code: ErrorCode::Internal,
        detail: format!("cannot write annotation PNG {}: {e}", path.display()),
    })?;
    let abs = std::fs::canonicalize(&path).unwrap_or(path);
    Ok(abs.display().to_string())
}

fn cleanup_annotation_png(artifacts_dir: &Path, screenshot_path: Option<&str>) {
    let Some(path_str) = screenshot_path else {
        return;
    };
    let dir = artifacts_dir.join("annotations");
    let canonical_dir = std::fs::canonicalize(&dir).unwrap_or(dir);
    let candidate = PathBuf::from(path_str);
    let canonical_file = std::fs::canonicalize(&candidate).unwrap_or(candidate);
    if canonical_file.starts_with(&canonical_dir) {
        let _ = std::fs::remove_file(canonical_file);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TINY_PNG_B64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "acos-ann-test-{tag}-{}-{}",
            std::process::id(),
            crate::live_registry::now_ms()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    #[test]
    fn lifecycle_and_png_write_and_multiscreen_batch() {
        let dir = temp_dir("life");
        let mut store = AnnotationStore::new();

        // Screen 1: add #1 with PNG
        let added1 = store
            .apply(
                &json!({
                    "action": "add",
                    "kind": "element",
                    "comment": "Sign-in button overlaps subtitle",
                    "bounds": [120, 400, 960, 520],
                    "nodes": [{"class": "android.widget.Button", "resource_id": "com.app:id/btn_login", "text": "Sign in"}],
                    "activity": "com.app/.LoginActivity",
                    "package": "com.app",
                    "display": {"w": 1080, "h": 2400},
                    "image_png_base64": TINY_PNG_B64,
                }),
                &dir,
                AnnotationDefaults::default(),
            )
            .expect("add 1");
        assert_eq!(added1["annotation"]["id"], 1);
        assert_eq!(added1["annotation"]["status"], "open");
        assert_eq!(added1["annotation"]["activity"], "com.app/.LoginActivity");
        let shot1 = added1["annotation"]["screenshot_path"]
            .as_str()
            .expect("screenshot_path")
            .to_string();
        assert!(Path::new(&shot1).is_absolute(), "must be absolute: {shot1}");
        assert!(Path::new(&shot1).exists(), "png file must exist on disk");
        let written = std::fs::read(&shot1).expect("read png");
        assert!(written.starts_with(PNG_MAGIC));

        // Screen 2: add #2 using session defaults
        let added2 = store
            .apply(
                &json!({
                    "action": "add",
                    "kind": "box",
                    "comment": "Card spacing is uneven",
                    "bounds": [500, 800, 40, 200],
                }),
                &dir,
                AnnotationDefaults {
                    activity: Some("com.app/.SettingsActivity".to_string()),
                    package: Some("com.app".to_string()),
                    display: Some(DisplaySize { w: 1080, h: 2400 }),
                },
            )
            .expect("add 2");
        assert_eq!(added2["annotation"]["id"], 2);
        // Bounds normalized [min, min, max, max]
        assert_eq!(added2["annotation"]["bounds"], json!([40, 200, 500, 800]));
        assert_eq!(
            added2["annotation"]["activity"],
            "com.app/.SettingsActivity"
        );

        // List open preserves both screens
        let listed = store
            .apply(&json!({}), &dir, AnnotationDefaults::default())
            .expect("list default");
        assert_eq!(listed["count"], 2);
        assert_eq!(listed["open_count"], 2);
        assert_eq!(
            listed["annotations"][0]["activity"],
            "com.app/.LoginActivity"
        );
        assert_eq!(
            listed["annotations"][1]["activity"],
            "com.app/.SettingsActivity"
        );

        // Resolve #1
        let resolved = store
            .apply(
                &json!({"action": "resolve", "id": 1, "resolution_note": "Fixed margin"}),
                &dir,
                AnnotationDefaults::default(),
            )
            .expect("resolve 1");
        assert_eq!(resolved["annotation"]["status"], "resolved");
        assert_eq!(resolved["annotation"]["resolution_note"], "Fixed margin");
        assert_eq!(resolved["open_count"], 1);
        assert_eq!(resolved["resolved_count"], 1);

        // Default list shows only open (#2)
        let open_only = store
            .apply(
                &json!({"action": "list"}),
                &dir,
                AnnotationDefaults::default(),
            )
            .expect("list open");
        assert_eq!(open_only["count"], 1);
        assert_eq!(open_only["annotations"][0]["id"], 2);

        // Delete #2
        let deleted = store
            .apply(
                &json!({"action": "delete", "id": 2}),
                &dir,
                AnnotationDefaults::default(),
            )
            .expect("delete 2");
        assert_eq!(deleted["deleted_id"], 2);
        assert_eq!(deleted["total_count"], 1);

        // Clear all removes #1 and its PNG
        let cleared = store
            .apply(
                &json!({"action": "clear"}),
                &dir,
                AnnotationDefaults::default(),
            )
            .expect("clear");
        assert_eq!(cleared["cleared"], 1);
        assert_eq!(cleared["total_count"], 0);
        assert!(!Path::new(&shot1).exists(), "deleted annotation png cleaned up");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_invalid_png_and_enforces_capacity_cap() {
        let dir = temp_dir("cap");
        let mut store = AnnotationStore::new();

        // Non-PNG bytes
        let err = store
            .apply(
                &json!({
                    "action": "add",
                    "comment": "bad image",
                    "image_png_base64": "aGVsbG8gd29ybGQ=",
                }),
                &dir,
                AnnotationDefaults::default(),
            )
            .unwrap_err();
        assert!(format!("{err:?}").contains("PNG signature"));

        // Capacity cap
        for i in 0..MAX_ANNOTATIONS_PER_SESSION {
            store
                .apply(
                    &json!({"action": "add", "comment": format!("item {i}")}),
                    &dir,
                    AnnotationDefaults::default(),
                )
                .expect("within cap");
        }
        let overflow = store
            .apply(
                &json!({"action": "add", "comment": "one too many"}),
                &dir,
                AnnotationDefaults::default(),
            )
            .unwrap_err();
        assert!(format!("{overflow:?}").contains("limit reached"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bounds_node_metadata_projection_and_aggregate_size_cap() {
        let dir = temp_dir("nodes-cap");
        let mut store = AnnotationStore::new();

        // Oversized unknown field is dropped and long UTF-8 string fields are truncated cleanly.
        let huge_text = "é".repeat(1000); // 2000 UTF-8 bytes
        let huge_unknown = "X".repeat(200_000);
        let added = store
            .apply(
                &json!({
                    "action": "add",
                    "comment": "Check truncation",
                    "nodes": [{
                        "class": "android.widget.TextView",
                        "resource_id": "com.app:id/title",
                        "text": huge_text,
                        "clickable": true,
                        "enabled": true,
                        "bounds": [10, 20, 300, 400],
                        "arbitrary_blob": huge_unknown,
                    }],
                }),
                &dir,
                AnnotationDefaults::default(),
            )
            .expect("projected node should succeed");
        let stored_node = &added["annotation"]["nodes"][0];
        assert!(stored_node.get("arbitrary_blob").is_none(), "unknown fields must be stripped");
        assert_eq!(stored_node["clickable"], true);
        assert_eq!(stored_node["enabled"], true);
        assert_eq!(stored_node["bounds"], json!([10, 20, 300, 400]));
        let stored_text = stored_node["text"].as_str().expect("text");
        assert!(
            stored_text.len() <= MAX_ANNOTATION_NODE_STR_BYTES,
            "stored text len {} must be <= {}",
            stored_text.len(),
            MAX_ANNOTATION_NODE_STR_BYTES
        );
        assert!(!stored_text.is_empty());

        // Aggregate cap: 32 nodes each saturated across all 7 string fields (~115 KiB > 64 KiB) must be rejected.
        let saturated = "a".repeat(MAX_ANNOTATION_NODE_STR_BYTES);
        let oversized_nodes: Vec<Value> = (0..MAX_ANNOTATION_NODES)
            .map(|_| {
                json!({
                    "class": saturated,
                    "resource_id": saturated,
                    "text": saturated,
                    "content_desc": saturated,
                    "package": saturated,
                    "activity": saturated,
                    "window": saturated,
                    "bounds": [0, 0, 100, 100],
                    "clickable": true,
                })
            })
            .collect();
        let err = store
            .apply(
                &json!({
                    "action": "add",
                    "comment": "Should fail aggregate node byte cap",
                    "nodes": oversized_nodes,
                }),
                &dir,
                AnnotationDefaults::default(),
            )
            .unwrap_err();
        assert!(
            format!("{err:?}").contains("exceeds maximum of"),
            "expected aggregate node byte cap error, got: {err:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
