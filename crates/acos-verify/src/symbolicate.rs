//! METRO6: map React Native JS frames from bundle positions to source positions.
//!
//! Three sources, in order, each with a strict timeout so a verdict never waits
//! on a Metro that is down:
//!
//! 1. Metro's own `POST /symbolicate` (`metro/src/Server.js` `_symbolicate`):
//!    `{stack: [{file, lineNumber, column, methodName}]}` -> `{stack: [...]}`.
//!    Metro reads `column` as **0-based** and answers 0-based columns
//!    (`metro/src/Server/symbolicate.js`); this module keeps that convention.
//! 2. The bundle's source map (`GET /index.map?...`, the same query as the
//!    bundle), decoded here (source map v3, base64 VLQ `mappings`) and cached in
//!    the artifacts directory under its SHA-256 (`source-maps/<hash>.map`, with
//!    `source-maps/index.json` mapping bundle revision + map path to the hash),
//!    so a later error **from the same bundle revision** — or one logged before
//!    Metro stopped — still maps.
//! 3. Neither: the frames stay as they are and the error says so
//!    (`symbolicated: false`, `symbolication_reason`).
//!
//! A refresh changes a bundle without changing its URL, so a map is only ever
//! reused for the [`Symbolicator::revision`] it was fetched under. A caller that
//! does not know the revision (`None`) reads and writes no cache at all, and a
//! caller that knows Metro has moved on ([`Symbolicator::metro_serves_revision`]
//! `false`) gets no answer from Metro's current maps.
//!
//! Nothing here is guessed: a frame is only rewritten from a mapping Metro or
//! the map actually returned, and `symbolicated` is `true` only when every
//! frame with a line was mapped.

use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::rn_errors::{is_generated_frame_id, is_source_frame};

use acos_protocol::rn::{JsError, JsFrame};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// The default bundle path a debug RN app fetches (`DevServerHelper`), used for
/// frames that carry no file (`JSStackTrace.format` drops it).
pub const DEFAULT_BUNDLE_PATH: &str =
    "/index.bundle?platform=android&dev=true&minify=false&modulesOnly=false&runModule=true";

/// The largest `/symbolicate` JSON reply read (a stack of a few hundred frames
/// is kilobytes; anything near this is not Metro).
pub const MAX_SYMBOLICATE_BYTES: usize = 4 * 1024 * 1024;

/// The largest source map read (a dev bundle's map is ~10–20 MiB).
pub const MAX_MAP_BYTES: usize = 64 * 1024 * 1024;

/// `source-maps/index.json`: bundle revision -> map path -> cached file.
type CacheIndex = BTreeMap<String, BTreeMap<String, String>>;

/// The most results a [`SymbolicationMemo`] keeps (oldest dropped first).
const MEMO_ENTRIES: usize = 32;

/// Where Metro is and where to keep maps.
#[derive(Debug, Clone)]
pub struct Symbolicator {
    /// Metro's host address as seen from the **host** (`127.0.0.1`).
    pub host: String,
    /// Metro's port (`None`: only the cache can be used).
    pub port: Option<u16>,
    /// Bundle path (+ query) for frames without a file.
    pub bundle_path: String,
    /// `<artifacts>/source-maps` (`None`: no cache).
    pub cache_dir: Option<PathBuf>,
    /// A prefix stripped from mapped source paths (the project copy root), so
    /// `file` reads `App.js`, not `/tmp/acos-dev-server-.../project/App.js`.
    pub strip_prefix: Option<PathBuf>,
    /// Per-request timeout (connect + read).
    pub timeout: Duration,
    /// Bound on one whole [`Symbolicator::symbolicate`] call, over all of its
    /// requests (`None`: only the per-request timeout).
    pub budget: Option<Duration>,
    /// The revision of the bundle that produced the frames (`None`: unknown, so
    /// no cached map is read or written).
    pub revision: Option<String>,
    /// `false`: Metro is known to serve another revision than the frames', so
    /// only maps cached under [`Symbolicator::revision`] may answer.
    pub metro_serves_revision: bool,
}

impl Symbolicator {
    /// A symbolicator for Metro on `127.0.0.1:port`.
    #[must_use]
    pub fn new(port: Option<u16>, cache_dir: Option<PathBuf>) -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port,
            bundle_path: DEFAULT_BUNDLE_PATH.to_string(),
            cache_dir,
            strip_prefix: None,
            timeout: Duration::from_secs(5),
            budget: None,
            revision: None,
            metro_serves_revision: true,
        }
    }

    /// Symbolicate `error` in place. Never fails: the outcome is recorded in
    /// `symbolicated` / `symbolication_reason`.
    pub fn symbolicate(&self, error: &mut JsError) {
        if error.symbolicated {
            return;
        }
        if !error.frames.iter().any(|frame| frame.line.is_some()) {
            error.symbolication_reason = Some("no frames with a position".to_string());
            return;
        }
        // `JSStackTrace.kt` ids (`1.js`, `seg-1_2.js`) name RAM-bundle modules /
        // segments: neither Metro's dev `/symbolicate` nor the dev bundle's map
        // can place them, so they are never "mapped" by guesswork.
        if let Some(id) = error
            .frames
            .iter()
            .filter(|frame| frame.line.is_some())
            .find_map(|frame| frame.file.as_deref().filter(|file| is_generated_frame_id(file)))
        {
            error.symbolicated = false;
            error.symbolication_reason = Some(format!(
                "generated frame id `{id}` (RAM bundle module / segment) needs that bundle's own segment map; not supported"
            ));
            return;
        }
        let deadline = self.budget.map(|budget| Instant::now() + budget);
        let mut reasons: Vec<String> = Vec::new();
        let mut frames = error.frames.clone();
        match self.port {
            Some(port) if self.metro_serves_revision => {
                match self.via_metro(port, &error.frames, deadline) {
                    Ok(mapped) => {
                        if self.accept(error, mapped, "metro /symbolicate") {
                            self.warm_cache(port, &frames, deadline);
                            return;
                        }
                        reasons.push("metro /symbolicate returned bundle positions".to_string());
                    }
                    Err(reason) => reasons.push(format!("symbolicate_unreachable: {reason}")),
                }
                let (mut from_cache, mut fetched) = (false, false);
                let (mapped, missing) = self.map_by_bundle(&frames, |path| {
                    if let Some(Ok(map)) = self.cached_map(path) {
                        from_cache = true;
                        return Ok(map);
                    }
                    let map = self.fetch_map(port, path, deadline)?;
                    fetched = true;
                    Ok(map)
                });
                frames = mapped;
                let how = match (from_cache, fetched) {
                    (true, false) => "source map (cached)",
                    (true, true) => "source map (online + cached)",
                    _ => "source map",
                };
                if self.accept(error, frames.clone(), how) {
                    return;
                }
                reasons.extend(missing.into_iter().map(|reason| format!("no map: {reason}")));
            }
            Some(_) => reasons.push(
                "the bundle changed since this error was logged; metro's current maps do not describe its frames"
                    .to_string(),
            ),
            None => reasons.push("no metro configured".to_string()),
        }
        if self.revision.is_none() && self.cache_dir.is_some() {
            reasons.push("bundle revision unknown: cached maps are not used".to_string());
        }
        let mapped_online = frames != error.frames;
        let (mapped, missing) = self.map_by_bundle(&frames, |path| match self.cached_map(path) {
            Some(result) => result,
            None => Err(format!("{path}: not cached")),
        });
        let how = if !mapped_online {
            "source map (cached)"
        } else {
            "source map (online + cached)"
        };
        if self.accept(error, mapped, how) {
            return;
        }
        if self.port.is_none() && missing.iter().all(|reason| reason.ends_with("not cached")) {
            reasons.push("no map".to_string());
        } else {
            reasons.extend(missing.into_iter().map(|reason| format!("cache: {reason}")));
        }
        error.symbolicated = false;
        error.symbolication_reason = Some(reasons.join("; "));
    }

    /// Keep this revision's maps for offline use while Metro is up (best effort:
    /// the online answer stands whatever happens here). Each map is downloaded at
    /// most once per revision.
    fn warm_cache(&self, port: u16, frames: &[JsFrame], deadline: Option<Instant>) {
        if self.cache_dir.is_none() || self.revision.is_none() {
            return;
        }
        for path in self.map_paths(frames) {
            if self.cached_file(&path).is_none() {
                let _ = self.fetch_map(port, &path, deadline);
            }
        }
    }

    /// The timeout of the next request: the per-request one, cut to what is left
    /// of the call's budget.
    fn request_timeout(&self, deadline: Option<Instant>) -> Result<Duration, String> {
        let Some(deadline) = deadline else {
            return Ok(self.timeout);
        };
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err("the symbolication time budget is spent".to_string());
        }
        Ok(left.min(self.timeout))
    }

    /// Adopt `frames` when every positioned frame is now a source position.
    fn accept(&self, error: &mut JsError, frames: Vec<JsFrame>, how: &str) -> bool {
        let complete = frames.len() == error.frames.len()
            && frames
                .iter()
                .zip(&error.frames)
                .all(|(mapped, original)| original.line.is_none() || is_source_frame(mapped));
        if complete {
            error.frames = frames;
            error.symbolicated = true;
            error.symbolication_reason = Some(how.to_string());
        }
        complete
    }

    /// The map path of every bundle the generated frames came from.
    fn map_paths(&self, frames: &[JsFrame]) -> Vec<String> {
        let mut paths: Vec<String> = frames
            .iter()
            .filter(|frame| frame.line.is_some() && !is_source_frame(frame))
            .map(|frame| self.map_path_of(frame))
            .collect();
        paths.sort();
        paths.dedup();
        paths
    }

    /// Map each generated frame with **its own bundle's** map (an `index.bundle`
    /// frame and a split-bundle frame never share one), leaving frames that are
    /// already source positions untouched. Returns the frames and, per bundle
    /// that could not be mapped, why.
    fn map_by_bundle(
        &self,
        frames: &[JsFrame],
        mut get: impl FnMut(&str) -> Result<SourceMap, String>,
    ) -> (Vec<JsFrame>, Vec<String>) {
        let mut out = frames.to_vec();
        let mut missing = Vec::new();
        for path in self.map_paths(frames) {
            match get(&path) {
                Ok(map) => {
                    for frame in out.iter_mut() {
                        if frame.line.is_some()
                            && !is_source_frame(frame)
                            && self.map_path_of(frame) == path
                        {
                            *frame = map.map_frame(frame, self);
                        }
                    }
                }
                Err(reason) => missing.push(reason),
            }
        }
        (out, missing)
    }

    /// The path and query of the bundle a frame came from. A full URL keeps its
    /// own. The redbox shows only the file name (`StackTraceHelper`:
    /// `index.bundle`), so the configured entry's name stands for the configured
    /// path and query; any other bare name keeps what it says.
    fn bundle_of(&self, frame: &JsFrame) -> String {
        match frame.file.as_deref() {
            Some(file) if file.contains("://") && file.contains(".bundle") => path_and_query(file),
            Some(file) if file.contains(".bundle") => {
                let name = file.trim_start_matches('/');
                let entry = self.bundle_path.trim_start_matches('/');
                if entry.split('?').next() == Some(name) {
                    self.bundle_path.clone()
                } else {
                    format!("/{name}")
                }
            }
            _ => self.bundle_path.clone(),
        }
    }

    fn via_metro(
        &self,
        port: u16,
        frames: &[JsFrame],
        deadline: Option<Instant>,
    ) -> Result<Vec<JsFrame>, String> {
        let stack: Vec<Value> = frames
            .iter()
            .map(|frame| {
                // Metro finds a frame's map by its URL, so the configured
                // entry (a frame with no file, or the redbox's bare name) and a
                // name with its own query are sent as one. Another bare bundle
                // name is left as it is: a URL without a query would be a guess.
                let path = self.bundle_of(frame);
                let file = match frame.file.as_deref() {
                    Some(file) if file.contains(".bundle") => {
                        if file.contains("://") || !path.contains('?') {
                            file.to_string()
                        } else {
                            format!("http://localhost:{port}{path}")
                        }
                    }
                    _ => format!("http://localhost:{port}{path}"),
                };
                json!({
                    "file": file,
                    "lineNumber": frame.line,
                    "column": frame.column,
                    "methodName": frame.function.clone().unwrap_or_default(),
                })
            })
            .collect();
        let body = serde_json::to_vec(&json!({"stack": stack})).map_err(|e| e.to_string())?;
        let (status, reply) = http_call(
            &self.host,
            port,
            "POST",
            "/symbolicate",
            Some(&body),
            self.request_timeout(deadline)?,
            MAX_SYMBOLICATE_BYTES,
        )?;
        if !(200..300).contains(&status) {
            return Err(format!("HTTP {status}"));
        }
        let value: Value = serde_json::from_slice(&reply).map_err(|e| format!("bad JSON: {e}"))?;
        let mapped = value
            .get("stack")
            .and_then(Value::as_array)
            .ok_or("no `stack` in the reply")?;
        if mapped.len() != frames.len() {
            return Err(format!(
                "the reply has {} frames for {} asked",
                mapped.len(),
                frames.len()
            ));
        }
        Ok(mapped
            .iter()
            .zip(frames)
            .map(|(frame, original)| {
                if original.line.is_none() {
                    return original.clone();
                }
                JsFrame {
                    file: frame
                        .get("file")
                        .and_then(Value::as_str)
                        .map(|file| self.strip(file)),
                    line: frame
                        .get("lineNumber")
                        .and_then(Value::as_u64)
                        .and_then(|v| u32::try_from(v).ok()),
                    column: frame
                        .get("column")
                        .and_then(Value::as_u64)
                        .and_then(|v| u32::try_from(v).ok()),
                    function: frame
                        .get("methodName")
                        .and_then(Value::as_str)
                        .filter(|name| !name.is_empty())
                        .map(str::to_string)
                        .or_else(|| original.function.clone()),
                }
            })
            .collect())
    }

    fn map_path_of(&self, frame: &JsFrame) -> String {
        self.bundle_of(frame).replacen(".bundle", ".map", 1)
    }

    fn fetch_map(
        &self,
        port: u16,
        path: &str,
        deadline: Option<Instant>,
    ) -> Result<SourceMap, String> {
        let timeout = self.request_timeout(deadline)?;
        let (status, bytes) =
            http_call(&self.host, port, "GET", path, None, timeout, MAX_MAP_BYTES)?;
        if !(200..300).contains(&status) {
            return Err(format!("GET {path}: HTTP {status}"));
        }
        let map = SourceMap::parse(&bytes).map_err(|e| format!("{path}: {e}"))?;
        self.store(path, &bytes);
        Ok(map)
    }

    fn cache_index(dir: &Path) -> Option<CacheIndex> {
        serde_json::from_slice(&fs::read(dir.join("index.json")).ok()?).ok()
    }

    fn store(&self, map_path: &str, bytes: &[u8]) {
        let (Some(dir), Some(revision)) = (&self.cache_dir, &self.revision) else {
            return;
        };
        if fs::create_dir_all(dir).is_err() {
            return;
        }
        let hash = hex(&Sha256::digest(bytes));
        let file = format!("{}.map", &hash[..16]);
        if !dir.join(&file).is_file() {
            let _ = fs::write(dir.join(&file), bytes);
        }
        let mut index = Self::cache_index(dir).unwrap_or_default();
        index
            .entry(revision.clone())
            .or_default()
            .insert(map_path.to_string(), file);
        if let Ok(text) = serde_json::to_vec_pretty(&index) {
            let _ = fs::write(dir.join("index.json"), text);
        }
    }

    /// The map cached for `map_path` under this symbolicator's revision.
    fn cached_file(&self, map_path: &str) -> Option<PathBuf> {
        let dir = self.cache_dir.as_ref()?;
        let revision = self.revision.as_ref()?;
        let file = dir.join(Self::cache_index(dir)?.get(revision)?.get(map_path)?);
        file.is_file().then_some(file)
    }

    fn cached_map(&self, map_path: &str) -> Option<Result<SourceMap, String>> {
        let file = self.cached_file(map_path)?;
        Some(
            fs::read(file)
                .map_err(|e| e.to_string())
                .and_then(|bytes| SourceMap::parse(&bytes)),
        )
    }

    fn strip(&self, file: &str) -> String {
        if let Some(prefix) = &self.strip_prefix {
            if let Ok(rest) = Path::new(file).strip_prefix(prefix) {
                return rest.display().to_string();
            }
        }
        file.to_string()
    }
}

/// Results already worked out, so the same error is not mapped (nor its maps
/// downloaded) again: a wait, the step that judges it, the end of the run and a
/// repeated `acos_logs` all see one answer.
///
/// An error is identified by its log record (`occurrence`) and the bundle
/// revision, with its name, message and generated frames. A successful mapping
/// is kept: it belongs to the bundle as it was when first asked. A failed one is
/// reused only for `retry_after`.
#[derive(Debug, Default)]
pub struct SymbolicationMemo {
    entries: VecDeque<MemoEntry>,
}

#[derive(Debug)]
struct MemoEntry {
    revision: Option<String>,
    occurrence: Option<u64>,
    asked: JsError,
    answer: JsError,
    at: Instant,
}

impl SymbolicationMemo {
    /// [`Symbolicator::symbolicate`] through the memo. With neither an
    /// `occurrence` nor a revision the error cannot be told from a look-alike of
    /// another bundle, so nothing is remembered.
    pub fn symbolicate(
        &mut self,
        symbolicator: &Symbolicator,
        occurrence: Option<u64>,
        error: &mut JsError,
        retry_after: Duration,
    ) {
        if error.symbolicated {
            return;
        }
        if occurrence.is_none() && symbolicator.revision.is_none() {
            symbolicator.symbolicate(error);
            return;
        }
        let found = self.entries.iter().position(|entry| {
            entry.occurrence == occurrence
                && entry.revision == symbolicator.revision
                && entry.asked.name == error.name
                && entry.asked.message == error.message
                && entry.asked.frames == error.frames
        });
        if let Some(index) = found {
            let entry = &self.entries[index];
            if entry.answer.symbolicated || entry.at.elapsed() < retry_after {
                error.frames.clone_from(&entry.answer.frames);
                error.symbolicated = entry.answer.symbolicated;
                error
                    .symbolication_reason
                    .clone_from(&entry.answer.symbolication_reason);
                return;
            }
            self.entries.remove(index);
        }
        let asked = error.clone();
        symbolicator.symbolicate(error);
        if self.entries.len() >= MEMO_ENTRIES {
            self.entries.pop_front();
        }
        self.entries.push_back(MemoEntry {
            revision: symbolicator.revision.clone(),
            occurrence,
            asked,
            answer: error.clone(),
            at: Instant::now(),
        });
    }
}

/// `http://localhost:8081/index.bundle?x` -> `/index.bundle?x`.
fn path_and_query(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    match rest.find('/') {
        Some(index) => rest[index..].to_string(),
        None => "/".to_string(),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A decoded source map v3 (`mappings` only; enough for position lookup).
#[derive(Debug, Clone)]
pub struct SourceMap {
    sources: Vec<String>,
    names: Vec<String>,
    /// Per generated line (0-based): segments sorted by generated column.
    lines: Vec<Vec<Segment>>,
}

#[derive(Debug, Clone, Copy)]
struct Segment {
    gen_column: u32,
    source: Option<(u32, u32, u32)>,
    name: Option<u32>,
}

impl SourceMap {
    /// Parse a source map v3 document.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let value: Value = serde_json::from_slice(bytes).map_err(|e| format!("not JSON: {e}"))?;
        if value.get("sections").is_some() {
            return Err("indexed source maps (`sections`) are not supported".to_string());
        }
        if value.get("version").and_then(Value::as_u64) != Some(3) {
            return Err("not a version 3 source map".to_string());
        }
        let root = value
            .get("sourceRoot")
            .and_then(Value::as_str)
            .unwrap_or("");
        let strings = |key: &str| -> Vec<String> {
            value
                .get(key)
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .map(|item| item.as_str().unwrap_or("").to_string())
                        .collect()
                })
                .unwrap_or_default()
        };
        let sources = strings("sources")
            .into_iter()
            .map(|source| {
                if root.is_empty() {
                    source
                } else {
                    format!("{}/{}", root.trim_end_matches('/'), source)
                }
            })
            .collect();
        let names = strings("names");
        let mappings = value
            .get("mappings")
            .and_then(Value::as_str)
            .ok_or("no `mappings`")?;
        Ok(Self {
            sources,
            names,
            lines: decode_mappings(mappings)?,
        })
    }

    /// The original position of generated `(line 1-based, column 0-based)`:
    /// the segment with the greatest generated column `<= column` on that line.
    #[must_use]
    pub fn lookup(&self, line: u32, column: u32) -> Option<(String, u32, u32, Option<String>)> {
        let segments = self
            .lines
            .get(usize::try_from(line.checked_sub(1)?).ok()?)?;
        let index = segments.partition_point(|segment| segment.gen_column <= column);
        let segment = segments.get(index.checked_sub(1)?)?;
        let (source, src_line, src_column) = segment.source?;
        let file = self.sources.get(source as usize)?.clone();
        let name = segment
            .name
            .and_then(|name| self.names.get(name as usize).cloned());
        // 0-based -> 1-based, checked: a corrupt map's `u32::MAX` line is a
        // failed lookup, never a panic or a wrapped-around line 0.
        Some((file, src_line.checked_add(1)?, src_column, name))
    }

    fn map_frame(&self, frame: &JsFrame, owner: &Symbolicator) -> JsFrame {
        match frame.line {
            Some(line) => match self.lookup(line, frame.column.unwrap_or(0)) {
                Some((file, line, column, name)) => JsFrame {
                    file: Some(owner.strip(&file)),
                    line: Some(line),
                    column: Some(column),
                    function: frame.function.clone().filter(|f| !f.is_empty()).or(name),
                },
                None => frame.clone(),
            },
            None => frame.clone(),
        }
    }
}

fn vlq_digit(byte: u8) -> Option<u32> {
    Some(match byte {
        b'A'..=b'Z' => byte - b'A',
        b'a'..=b'z' => byte - b'a' + 26,
        b'0'..=b'9' => byte - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => return None,
    } as u32)
}

/// Decode one segment's base64-VLQ fields.
fn decode_segment(segment: &str) -> Result<Vec<i64>, String> {
    let mut values = Vec::new();
    let mut value: i64 = 0;
    let mut shift = 0u32;
    for byte in segment.bytes() {
        let digit =
            vlq_digit(byte).ok_or_else(|| format!("bad VLQ character {:?}", byte as char))?;
        value += i64::from(digit & 31) << shift;
        if digit & 32 != 0 {
            shift += 5;
            if shift > 60 {
                return Err("VLQ value too large".to_string());
            }
            continue;
        }
        let negative = value & 1 == 1;
        let magnitude = value >> 1;
        values.push(if negative { -magnitude } else { magnitude });
        value = 0;
        shift = 0;
    }
    if shift != 0 {
        return Err("truncated VLQ segment".to_string());
    }
    Ok(values)
}

fn decode_mappings(mappings: &str) -> Result<Vec<Vec<Segment>>, String> {
    let mut lines = Vec::new();
    let (mut source, mut src_line, mut src_column, mut name) = (0i64, 0i64, 0i64, 0i64);
    for line in mappings.split(';') {
        let mut gen_column = 0i64;
        let mut segments = Vec::new();
        for raw in line.split(',').filter(|raw| !raw.is_empty()) {
            let fields = decode_segment(raw)?;
            gen_column += fields[0];
            let mut segment = Segment {
                gen_column: u32::try_from(gen_column).map_err(|_| "negative column")?,
                source: None,
                name: None,
            };
            if fields.len() >= 4 {
                source += fields[1];
                src_line += fields[2];
                src_column += fields[3];
                segment.source = Some((
                    u32::try_from(source).map_err(|_| "negative source index")?,
                    u32::try_from(src_line).map_err(|_| "negative source line")?,
                    u32::try_from(src_column).map_err(|_| "negative source column")?,
                ));
                if fields.len() >= 5 {
                    name += fields[4];
                    segment.name = u32::try_from(name).ok();
                }
            }
            segments.push(segment);
        }
        segments.sort_by_key(|segment| segment.gen_column);
        lines.push(segments);
    }
    Ok(lines)
}

/// One HTTP/1.0 request under **one absolute deadline** (connect, write and
/// every read together stay within `timeout`) and a hard `max_bytes` body cap
/// (an oversized reply is an error before it is buffered). Reading stops as
/// soon as `Content-Length` bytes (or the chunked terminator) arrived, so a
/// server that keeps the connection open cannot hold the call. Returns
/// `(status, body)`; a chunked body is decoded.
pub fn http_call(
    host: &str,
    port: u16,
    method: &str,
    path: &str,
    body: Option<&[u8]>,
    timeout: Duration,
    max_bytes: usize,
) -> Result<(u16, Vec<u8>), String> {
    let deadline = Instant::now() + timeout;
    let addr: SocketAddr = (host, port)
        .to_socket_addrs()
        .map_err(|e| e.to_string())?
        .next()
        .ok_or("no address")?;
    let mut stream =
        crate::deadline_io::DeadlineStream::connect(&addr, deadline).map_err(|e| e.to_string())?;
    let body = body.unwrap_or(&[]);
    let head = format!(
        "{method} {path} HTTP/1.0\r\nHost: {host}:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream
        .write_all(head.as_bytes())
        .map_err(|e| e.to_string())?;
    stream.write_all(body).map_err(|e| e.to_string())?;

    const MAX_HEAD: usize = 64 * 1024;
    let mut raw: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 64 * 1024];
    let split = loop {
        if let Some(split) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
            break split;
        }
        if raw.len() > MAX_HEAD {
            return Err("oversized HTTP header".to_string());
        }
        let n = stream.read(&mut chunk).map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("malformed HTTP reply".to_string());
        }
        raw.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&raw[..split]).to_ascii_lowercase();
    let status: u16 = head
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .ok_or("no HTTP status")?;
    let chunked = head.contains("transfer-encoding: chunked");
    let content_length: Option<usize> = head.lines().find_map(|line| {
        line.strip_prefix("content-length:")
            .and_then(|value| value.trim().parse().ok())
    });
    if let Some(length) = content_length {
        if length > max_bytes {
            return Err(format!("response is {length} bytes, larger than the {max_bytes}-byte cap"));
        }
    }
    let mut payload = raw[split + 4..].to_vec();
    let too_large = |len: usize| format!("response of {len}+ bytes is larger than the {max_bytes}-byte cap");
    let mut chunks = Chunks::default();
    loop {
        let complete = match content_length {
            _ if chunked => chunks.scan(&payload)?,
            Some(length) => payload.len() >= length,
            None => false,
        };
        if complete {
            break;
        }
        // a chunked body may carry framing beyond the cap; allow a little slack
        if payload.len() > max_bytes + if chunked { 64 * 1024 } else { 0 } {
            return Err(too_large(payload.len()));
        }
        let n = stream.read(&mut chunk).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        payload.extend_from_slice(&chunk[..n]);
    }
    if let Some(length) = content_length.filter(|_| !chunked) {
        if payload.len() < length {
            return Err(format!("truncated reply: {} of {length} bytes", payload.len()));
        }
        payload.truncate(length);
    }
    if chunked {
        if !chunks.scan(&payload)? {
            return Err("truncated chunked reply".to_string());
        }
        payload = chunks.body(&payload);
    }
    if payload.len() > max_bytes {
        return Err(too_large(payload.len()));
    }
    Ok((status, payload))
}

/// The longest chunk-size or trailer line accepted.
const MAX_CHUNK_LINE: usize = 8 * 1024;

/// Incremental RFC 9112 §7.1 chunked framing over a growing buffer: the one
/// answer to "is the reply complete, incomplete or malformed?".
#[derive(Debug, Default)]
struct Chunks {
    /// Offset of the first chunk not yet taken.
    next: usize,
    /// The data of every complete chunk.
    data: Vec<std::ops::Range<usize>>,
}

impl Chunks {
    /// `Ok(true)`: the last chunk, its trailers and the closing empty line are
    /// all there. `Ok(false)`: more bytes are needed.
    fn scan(&mut self, raw: &[u8]) -> Result<bool, String> {
        let line = |from: usize| -> Result<Option<(&[u8], usize)>, String> {
            let rest = raw.get(from..).unwrap_or(&[]);
            match rest.windows(2).position(|w| w == b"\r\n") {
                Some(end) if end <= MAX_CHUNK_LINE => Ok(Some((&rest[..end], from + end + 2))),
                None if rest.len() <= MAX_CHUNK_LINE => Ok(None),
                _ => Err("oversized chunk framing line".to_string()),
            }
        };
        loop {
            let Some((size_line, start)) = line(self.next)? else {
                return Ok(false);
            };
            let size_text = String::from_utf8_lossy(size_line);
            let digits = size_text.split(';').next().unwrap_or("").trim();
            if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err("bad chunk size".to_string());
            }
            let size = usize::from_str_radix(digits, 16).map_err(|_| "bad chunk size")?;
            if size == 0 {
                // trailer fields, then the empty line
                let mut at = start;
                loop {
                    let Some((field, after)) = line(at)? else {
                        return Ok(false);
                    };
                    if field.is_empty() {
                        return Ok(true);
                    }
                    if !field.contains(&b':') {
                        return Err("bad chunk trailer".to_string());
                    }
                    at = after;
                }
            }
            let end = start.checked_add(size).ok_or("bad chunk size")?;
            let after = end.checked_add(2).ok_or("bad chunk size")?;
            if raw.len() < after {
                return Ok(false);
            }
            if &raw[end..after] != b"\r\n" {
                return Err("a chunk is longer than its size".to_string());
            }
            self.data.push(start..end);
            self.next = after;
        }
    }

    fn body(&self, raw: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.data.iter().map(ExactSizeIterator::len).sum());
        for range in &self.data {
            out.extend_from_slice(&raw[range.clone()]);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    /// A mock Metro: answers each request with `respond(method, path, body)`.
    fn mock_metro(
        respond: impl Fn(&str, &str, &str) -> (u16, String) + Send + 'static,
    ) -> (u16, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                // read head, then Content-Length bytes
                loop {
                    let n = stream.read(&mut chunk).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(split) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..split]).to_string();
                        let len = head
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length: ")
                                    .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        if buf.len() >= split + 4 + len {
                            break;
                        }
                    }
                }
                let text = String::from_utf8_lossy(&buf).to_string();
                let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
                let mut parts = head.split_whitespace();
                let method = parts.next().unwrap_or("").to_string();
                let path = parts.next().unwrap_or("").to_string();
                log.lock().unwrap().push(format!("{method} {path}"));
                let (status, reply) = respond(&method, &path, body);
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 {status} X\r\nContent-Length: {}\r\n\r\n{reply}",
                        reply.len()
                    )
                    .as_bytes(),
                );
            }
        });
        (port, seen)
    }

    fn error_with(frames: Vec<JsFrame>) -> JsError {
        JsError {
            name: "TypeError".into(),
            message: "x".into(),
            frames,
            component_stack: None,
            kind: "fatal".into(),
            source: "log".into(),
            log_tail: Vec::new(),
            symbolicated: false,
            symbolication_reason: None,
        }
    }

    fn bundle_frame(line: u32, column: u32) -> JsFrame {
        JsFrame {
            file: Some("http://localhost:8081/index.bundle?platform=android&dev=true".into()),
            line: Some(line),
            column: Some(column),
            function: Some("onPress".into()),
        }
    }

    fn dechunk(data: &[u8]) -> Result<Vec<u8>, String> {
        let mut chunks = Chunks::default();
        if !chunks.scan(data)? {
            return Err("truncated chunk".to_string());
        }
        Ok(chunks.body(data))
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("acos-symb-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    /// `App.js` line 26 col 4 for generated (1, 100), `helper` in `util.js`
    /// line 3 col 0 for generated (2, 0..).
    fn sample_map() -> String {
        // line 1: [col 0 -> src0 L0 C0], [col 100 -> src0 L25 C4 name0]
        // line 2: [col 0 -> src1 L2 C0]
        // segments: "AAAA" = (0,0,0,0); "oGAyBI,A" ...; build with an encoder
        let mut mappings = String::new();
        mappings.push_str(&encode(&[0, 0, 0, 0]));
        mappings.push(',');
        mappings.push_str(&encode(&[100, 0, 25, 4, 0]));
        mappings.push(';');
        mappings.push_str(&encode(&[0, 1, -23, -4]));
        json!({
            "version": 3,
            "sources": ["/tmp/acos-dev-server-1/project/App.js", "/tmp/acos-dev-server-1/project/util.js"],
            "names": ["onPress"],
            "mappings": mappings,
        })
        .to_string()
    }

    fn encode(values: &[i64]) -> String {
        const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for &value in values {
            let mut v = if value < 0 {
                ((-value) << 1) | 1
            } else {
                value << 1
            } as u64;
            loop {
                let mut digit = (v & 31) as usize;
                v >>= 5;
                if v > 0 {
                    digit |= 32;
                }
                out.push(CHARS[digit] as char);
                if v == 0 {
                    break;
                }
            }
        }
        out
    }

    #[test]
    fn vlq_decodes_known_segments() {
        assert_eq!(decode_segment("AAAA").unwrap(), vec![0, 0, 0, 0]);
        assert_eq!(decode_segment("AAgBC").unwrap(), vec![0, 0, 16, 1]);
        assert_eq!(decode_segment("D").unwrap(), vec![-1]);
        assert_eq!(decode_segment("2H").unwrap(), vec![123]);
        assert!(
            decode_segment("g").is_err(),
            "a continuation with no end is truncated"
        );
        let map = SourceMap::parse(sample_map().as_bytes()).unwrap();
        assert_eq!(
            map.lookup(1, 150),
            Some((
                "/tmp/acos-dev-server-1/project/App.js".into(),
                26,
                4,
                Some("onPress".into())
            ))
        );
        assert_eq!(map.lookup(1, 50).unwrap().1, 1);
        assert_eq!(
            map.lookup(2, 7).unwrap().0,
            "/tmp/acos-dev-server-1/project/util.js"
        );
        assert_eq!(map.lookup(2, 7).unwrap().1, 3);
        assert!(map.lookup(9, 0).is_none());
        assert!(SourceMap::parse(br#"{"version":3,"sections":[]}"#).is_err());
    }

    #[test]
    fn metro_symbolicate_success() {
        let (port, seen) = mock_metro(|method, path, body| {
            assert_eq!((method, path), ("POST", "/symbolicate"));
            let request: Value = serde_json::from_str(body).unwrap();
            assert_eq!(request["stack"][0]["lineNumber"], 1);
            assert_eq!(request["stack"][0]["column"], 483920);
            (
                200,
                json!({"stack": [{"file": "/tmp/acos-dev-server-1/project/App.js", "lineNumber": 26, "column": 4, "methodName": "onPress"}], "codeFrame": null}).to_string(),
            )
        });
        let mut symbolicator = Symbolicator::new(Some(port), None);
        symbolicator.strip_prefix = Some(PathBuf::from("/tmp/acos-dev-server-1/project"));
        let mut error = error_with(vec![bundle_frame(1, 483920)]);
        symbolicator.symbolicate(&mut error);
        assert!(error.symbolicated, "{:?}", error.symbolication_reason);
        assert_eq!(
            error.symbolication_reason.as_deref(),
            Some("metro /symbolicate")
        );
        assert_eq!(error.frames[0].file.as_deref(), Some("App.js"));
        assert_eq!(error.frames[0].line, Some(26));
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    #[test]
    fn map_fallback_caches_by_hash_and_serves_offline() {
        let map = sample_map();
        let (port, seen) = mock_metro(move |method, path, _| match (method, path) {
            ("POST", "/symbolicate") => (404, "not found".into()),
            ("GET", p) if p.starts_with("/index.map?") => (200, map.clone()),
            _ => (500, String::new()),
        });
        let cache = scratch("cache");
        let mut symbolicator = Symbolicator::new(Some(port), Some(cache.clone()));
        symbolicator.strip_prefix = Some(PathBuf::from("/tmp/acos-dev-server-1/project"));
        symbolicator.revision = Some("r1".into());
        let mut error = error_with(vec![bundle_frame(1, 120)]);
        symbolicator.symbolicate(&mut error);
        assert!(error.symbolicated, "{:?}", error.symbolication_reason);
        assert_eq!(error.symbolication_reason.as_deref(), Some("source map"));
        assert_eq!(error.frames[0].file.as_deref(), Some("App.js"));
        assert_eq!(error.frames[0].line, Some(26));
        assert_eq!(error.frames[0].column, Some(4));
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            [
                "POST /symbolicate",
                "GET /index.map?platform=android&dev=true"
            ]
        );
        // cached under the map's own hash, indexed by the map path
        let hash = hex(&Sha256::digest(sample_map().as_bytes()));
        assert!(cache.join(format!("{}.map", &hash[..16])).is_file());
        let index: CacheIndex =
            serde_json::from_slice(&fs::read(cache.join("index.json")).unwrap()).unwrap();
        assert_eq!(
            index["r1"]["/index.map?platform=android&dev=true"],
            format!("{}.map", &hash[..16])
        );

        // Metro gone: the cached map still answers, and says it was the cache.
        let mut offline = Symbolicator::new(Some(closed_port()), Some(cache.clone()));
        offline.timeout = Duration::from_millis(500);
        offline.revision = Some("r1".into());
        let mut error = error_with(vec![bundle_frame(1, 120)]);
        offline.symbolicate(&mut error);
        assert!(error.symbolicated);
        assert_eq!(
            error.symbolication_reason.as_deref(),
            Some("source map (cached)")
        );
        let _ = fs::remove_dir_all(&cache);
    }

    fn closed_port() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
        // dropped: nothing listens there now
    }

    #[test]
    fn honest_fallback_when_metro_404s_or_is_down() {
        let (port, _) = mock_metro(|_, _, _| (404, "nope".into()));
        let mut error = error_with(vec![bundle_frame(1, 5)]);
        Symbolicator::new(Some(port), None).symbolicate(&mut error);
        assert!(!error.symbolicated);
        let reason = error.symbolication_reason.clone().unwrap();
        assert!(
            reason.contains("symbolicate_unreachable: HTTP 404"),
            "{reason}"
        );
        assert!(reason.contains("no map"), "{reason}");
        assert_eq!(error.frames[0].line, Some(1), "frames are left untouched");

        let started = std::time::Instant::now();
        let mut down = Symbolicator::new(Some(closed_port()), None);
        down.timeout = Duration::from_millis(500);
        let mut error = error_with(vec![bundle_frame(1, 5)]);
        down.symbolicate(&mut error);
        assert!(!error.symbolicated);
        assert!(error
            .symbolication_reason
            .unwrap()
            .contains("symbolicate_unreachable"));
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "a dead Metro never hangs"
        );

        let mut error = error_with(vec![bundle_frame(1, 5)]);
        Symbolicator::new(None, None).symbolicate(&mut error);
        assert_eq!(
            error.symbolication_reason.as_deref(),
            Some("no metro configured; no map")
        );
    }

    #[test]
    fn frames_without_a_file_use_the_default_bundle_and_chunked_replies_decode() {
        assert_eq!(
            dechunk(b"3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n").unwrap(),
            b"abcde"
        );
        assert_eq!(
            path_and_query("http://localhost:8081/index.bundle?a=1"),
            "/index.bundle?a=1"
        );
        let (port, _) = mock_metro(|_, _, body| {
            let request: Value = serde_json::from_str(body).unwrap();
            let file = request["stack"][0]["file"].as_str().unwrap().to_string();
            assert!(file.ends_with(DEFAULT_BUNDLE_PATH), "{file}");
            (200, json!({"stack": [{"file": "App.js", "lineNumber": 3, "column": 1, "methodName": ""}]}).to_string())
        });
        let mut error = error_with(vec![JsFrame {
            file: None,
            line: Some(1),
            column: Some(10),
            function: Some("f".into()),
        }]);
        Symbolicator::new(Some(port), None).symbolicate(&mut error);
        assert!(error.symbolicated);
        assert_eq!(
            error.frames[0].function.as_deref(),
            Some("f"),
            "an empty methodName keeps the original"
        );
    }

    #[test]
    fn a_u32_max_source_line_is_a_failed_lookup_not_a_panic() {
        let map = json!({
            "version": 3,
            "sources": ["App.js"],
            "names": [],
            "mappings": encode(&[0, 0, i64::from(u32::MAX), 0]),
        })
        .to_string();
        let map = SourceMap::parse(map.as_bytes()).unwrap();
        assert_eq!(map.lookup(1, 0), None);
    }

    /// A server that streams an endless body: the cap stops it before it is
    /// buffered, a declared oversized `Content-Length` is refused up front, and
    /// a kept-open connection returns as soon as `Content-Length` bytes arrived.
    #[test]
    fn replies_are_capped_and_end_at_content_length() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for (index, stream) in listener.incoming().flatten().enumerate() {
                let mut stream = stream;
                let mut head = [0u8; 4096];
                let _ = stream.read(&mut head);
                match index {
                    0 => {
                        let _ = stream.write_all(b"HTTP/1.1 200 OK\r\n\r\n");
                        let block = [b'x'; 16 * 1024];
                        while stream.write_all(&block).is_ok() {}
                    }
                    1 => {
                        let _ = stream
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 999999999\r\n\r\nabc");
                    }
                    _ => {
                        // keeps the socket open after the body
                        let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
                        std::thread::sleep(Duration::from_secs(5));
                    }
                }
            }
        });
        let timeout = Duration::from_secs(3);
        let error = http_call("127.0.0.1", port, "GET", "/", None, timeout, 64 * 1024).unwrap_err();
        assert!(error.contains("larger than the 65536-byte cap"), "{error}");
        let error = http_call("127.0.0.1", port, "GET", "/", None, timeout, 64 * 1024).unwrap_err();
        assert!(error.contains("999999999 bytes"), "{error}");
        let started = Instant::now();
        let (status, body) = http_call("127.0.0.1", port, "GET", "/", None, timeout, 1024).unwrap();
        assert_eq!((status, body.as_slice()), (200, &b"ok"[..]));
        assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
    }

    /// A trickling server cannot stretch one call past its total budget.
    #[test]
    fn one_deadline_bounds_the_whole_exchange() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Some(Ok(mut stream)) = listener.incoming().next() {
                let mut head = [0u8; 4096];
                let _ = stream.read(&mut head);
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n");
                for _ in 0..100 {
                    if stream.write_all(b"x").is_err() {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        });
        let started = Instant::now();
        let error = http_call("127.0.0.1", port, "GET", "/", None, Duration::from_millis(600), 1024)
            .unwrap_err();
        assert!(started.elapsed() < Duration::from_millis(1500), "{:?}", started.elapsed());
        assert!(!error.is_empty());
    }

    /// Frames from two bundles: each maps with its own bundle's map.
    #[test]
    fn split_bundle_frames_map_against_their_own_maps() {
        let index_map = sample_map();
        let split_map = json!({
            "version": 3,
            "sources": ["/tmp/acos-dev-server-1/project/Lazy.js"],
            "names": [],
            "mappings": encode(&[0, 0, 41, 2]),
        })
        .to_string();
        let (port, seen) = mock_metro(move |method, path, _| match (method, path) {
            ("POST", "/symbolicate") => (404, String::new()),
            ("GET", p) if p.starts_with("/index.map?") => (200, index_map.clone()),
            ("GET", p) if p.starts_with("/Lazy.map?") => (200, split_map.clone()),
            _ => (500, String::new()),
        });
        let mut symbolicator = Symbolicator::new(Some(port), None);
        symbolicator.strip_prefix = Some(PathBuf::from("/tmp/acos-dev-server-1/project"));
        let lazy = JsFrame {
            file: Some("http://localhost:8081/Lazy.bundle?platform=android&dev=true".into()),
            line: Some(1),
            column: Some(5),
            function: Some("render".into()),
        };
        let mut error = error_with(vec![lazy, bundle_frame(1, 120)]);
        symbolicator.symbolicate(&mut error);
        assert!(error.symbolicated, "{:?}", error.symbolication_reason);
        assert_eq!(error.frames[0].file.as_deref(), Some("Lazy.js"));
        assert_eq!(error.frames[0].line, Some(42));
        assert_eq!(error.frames[1].file.as_deref(), Some("App.js"));
        assert_eq!(error.frames[1].line, Some(26));
        let seen = seen.lock().unwrap();
        assert!(seen.iter().any(|r| r.starts_with("GET /Lazy.map?")), "{seen:?}");
        assert!(seen.iter().any(|r| r.starts_with("GET /index.map?")), "{seen:?}");
    }

    #[test]
    fn generated_frame_ids_stay_unsymbolicated_with_a_reason() {
        for id in ["1.js", "seg-1_2.js"] {
            let mut error = error_with(vec![JsFrame {
                file: Some(id.into()),
                line: Some(3),
                column: Some(7),
                function: Some("f".into()),
            }]);
            Symbolicator::new(None, None).symbolicate(&mut error);
            assert!(!error.symbolicated, "{id}");
            let reason = error.symbolication_reason.unwrap();
            assert!(reason.contains("generated frame id"), "{reason}");
            assert_eq!(error.frames[0].file.as_deref(), Some(id));
        }
    }

    /// After an online `/symbolicate` success the map is fetched and cached, so
    /// a later error still maps once Metro is gone.
    #[test]
    fn an_online_success_warms_the_offline_cache() {
        let map = sample_map();
        let (port, seen) = mock_metro(move |method, path, _| match (method, path) {
            ("POST", "/symbolicate") => (
                200,
                json!({"stack": [{"file": "/tmp/acos-dev-server-1/project/App.js", "lineNumber": 26, "column": 4, "methodName": "onPress"}]}).to_string(),
            ),
            ("GET", p) if p.starts_with("/index.map?") => (200, map.clone()),
            _ => (500, String::new()),
        });
        let cache = scratch("warm");
        let mut online = Symbolicator::new(Some(port), Some(cache.clone()));
        online.revision = Some("r1".into());
        let mut error = error_with(vec![bundle_frame(1, 120)]);
        online.symbolicate(&mut error);
        assert_eq!(error.symbolication_reason.as_deref(), Some("metro /symbolicate"));
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            ["POST /symbolicate", "GET /index.map?platform=android&dev=true"]
        );
        // the same revision's map is not downloaded again
        let mut error = error_with(vec![bundle_frame(1, 120)]);
        online.symbolicate(&mut error);
        assert_eq!(
            seen.lock().unwrap().iter().filter(|r| r.starts_with("GET ")).count(),
            1
        );
        let mut offline = Symbolicator::new(Some(closed_port()), Some(cache.clone()));
        offline.timeout = Duration::from_millis(500);
        offline.revision = Some("r1".into());
        let mut error = error_with(vec![bundle_frame(1, 120)]);
        offline.symbolicate(&mut error);
        assert!(error.symbolicated, "{:?}", error.symbolication_reason);
        assert_eq!(error.symbolication_reason.as_deref(), Some("source map (cached)"));
        let _ = fs::remove_dir_all(&cache);
    }

    /// A one-segment map placing generated `(1, 0..)` at `file` line `line`.
    fn map_to(file: &str, line: i64) -> String {
        json!({"version": 3, "sources": [file], "names": [], "mappings": encode(&[0, 0, line - 1, 0])})
            .to_string()
    }

    /// A refresh keeps the URL: an older error must never be mapped by the newer
    /// bundle's map.
    #[test]
    fn a_cached_map_answers_only_for_its_bundle_revision() {
        let served = Arc::new(Mutex::new(map_to("Old.js", 3)));
        let current = Arc::clone(&served);
        let (port, seen) = mock_metro(move |method, _, _| match method {
            "GET" => (200, current.lock().unwrap().clone()),
            _ => (404, String::new()),
        });
        let cache = scratch("revision");
        let at = |port: Option<u16>, revision: Option<&str>| {
            let mut symbolicator = Symbolicator::new(port, Some(cache.clone()));
            symbolicator.revision = revision.map(str::to_string);
            symbolicator
        };
        let map = |symbolicator: &Symbolicator| {
            let mut error = error_with(vec![bundle_frame(1, 120)]);
            symbolicator.symbolicate(&mut error);
            error
        };
        assert_eq!(map(&at(Some(port), Some("r1"))).frames[0].file.as_deref(), Some("Old.js"));
        // the refresh: same URL, other content
        *served.lock().unwrap() = map_to("New.js", 9);
        assert_eq!(map(&at(Some(port), Some("r2"))).frames[0].file.as_deref(), Some("New.js"));

        // the old error, after Metro stopped, still gets the old bundle's map
        let old = map(&at(None, Some("r1")));
        assert_eq!(old.frames[0].file.as_deref(), Some("Old.js"), "{old:?}");
        // an unknown revision, or one never cached, is not mapped by guesswork
        for revision in [None, Some("r3")] {
            let error = map(&at(None, revision));
            assert!(!error.symbolicated, "{revision:?}: {error:?}");
            assert_eq!(error.frames[0].file, bundle_frame(1, 120).file);
        }
        // an error of a revision Metro no longer serves is not sent to Metro
        let requests = seen.lock().unwrap().len();
        let mut stale = at(Some(port), Some("r3"));
        stale.metro_serves_revision = false;
        let error = map(&stale);
        assert!(!error.symbolicated);
        assert!(error.symbolication_reason.unwrap().contains("the bundle changed"));
        stale.revision = Some("r1".into());
        assert_eq!(map(&stale).frames[0].file.as_deref(), Some("Old.js"));
        assert_eq!(seen.lock().unwrap().len(), requests);
        // and an unknown revision writes nothing
        let _ = fs::remove_dir_all(&cache);
        map(&at(Some(port), None));
        assert!(!cache.exists());
    }

    #[test]
    fn a_memo_asks_metro_once_per_error_occurrence() {
        let up = Arc::new(Mutex::new(true));
        let answers = Arc::clone(&up);
        let (port, seen) = mock_metro(move |method, _, _| match (method, *answers.lock().unwrap()) {
            ("POST", true) => (200, json!({"stack": [{"file": "App.js", "lineNumber": 3, "column": 1}]}).to_string()),
            _ => (500, String::new()),
        });
        let posts = || seen.lock().unwrap().iter().filter(|r| r.starts_with("POST")).count();
        let symbolicator = Symbolicator::new(Some(port), None);
        let mut memo = SymbolicationMemo::default();
        let mut ask = |occurrence: u64, retry_after: Duration| {
            let mut error = error_with(vec![bundle_frame(1, 120)]);
            memo.symbolicate(&symbolicator, Some(occurrence), &mut error, retry_after);
            error
        };
        let first = ask(7, Duration::ZERO);
        assert!(first.symbolicated);
        *up.lock().unwrap() = false;
        // a success is kept, whatever Metro does now
        assert_eq!(ask(7, Duration::ZERO), first);
        assert_eq!(posts(), 1);
        // another occurrence is another error; its failure is reused for a while
        assert!(!ask(8, Duration::MAX).symbolicated);
        assert!(!ask(8, Duration::MAX).symbolicated);
        assert_eq!(posts(), 2);
        // and retried once that window is over
        *up.lock().unwrap() = true;
        assert!(ask(8, Duration::ZERO).symbolicated);
        assert_eq!(posts(), 3);
    }

    /// The redbox prints a bare file name; the map is the configured entry's.
    #[test]
    fn a_bare_bundle_name_resolves_to_the_configured_bundle() {
        let map = sample_map();
        let (port, seen) = mock_metro(move |method, path, body| match (method, path) {
            ("POST", "/symbolicate") => {
                let request: Value = serde_json::from_str(body).unwrap();
                let file = request["stack"][0]["file"].as_str().unwrap();
                assert!(
                    file.starts_with("http://localhost:") && file.ends_with(DEFAULT_BUNDLE_PATH),
                    "{file}"
                );
                (404, String::new())
            }
            ("GET", p) if p == DEFAULT_BUNDLE_PATH.replacen(".bundle", ".map", 1) => (200, map.clone()),
            _ => (500, String::new()),
        });
        let mut frame = bundle_frame(1, 120);
        frame.file = Some("index.bundle".into());
        let mut error = error_with(vec![frame.clone()]);
        let symbolicator = Symbolicator::new(Some(port), None);
        symbolicator.symbolicate(&mut error);
        assert!(error.symbolicated, "{:?} {:?}", error.symbolication_reason, seen.lock().unwrap());
        assert_eq!(error.frames[0].line, Some(26));
        // a name that carries its own query, or names another bundle, keeps it
        frame.file = Some("index.bundle?platform=android".into());
        assert_eq!(symbolicator.bundle_of(&frame), "/index.bundle?platform=android");
        frame.file = Some("Lazy.bundle".into());
        assert_eq!(symbolicator.bundle_of(&frame), "/Lazy.bundle");
        frame.file = Some("http://localhost:8081/index.bundle?dev=false".into());
        assert_eq!(symbolicator.bundle_of(&frame), "/index.bundle?dev=false");
    }

    #[test]
    fn chunk_framing_tells_complete_incomplete_and_malformed_apart() {
        let scan = |raw: &[u8]| Chunks::default().scan(raw);
        assert_eq!(scan(b"3\r\nabc\r\n0\r\n\r\n"), Ok(true));
        assert_eq!(scan(b"3\r\nabc\r\n0\r\nX-Sum: 1\r\nY: 2\r\n\r\n"), Ok(true));
        for partial in [&b"3"[..], b"3\r\nab", b"3\r\nabc\r\n", b"3\r\nabc\r\n0\r\n", b"0\r\nX: 1\r\n"] {
            assert_eq!(scan(partial), Ok(false), "{partial:?}");
            assert!(dechunk(partial).is_err(), "{partial:?}");
        }
        for bad in [&b"zz\r\n"[..], b"+3\r\nabc\r\n", b"2\r\nabc\r\n", b"0\r\nnot a field\r\n\r\n", b"ffffffffffffffff\r\n"] {
            assert!(scan(bad).is_err(), "{bad:?}");
        }
        // fed a byte at a time, the scan resumes where it stopped
        let raw = b"3\r\nabc\r\n2\r\nde\r\n0\r\nT: 1\r\n\r\n";
        let mut chunks = Chunks::default();
        for end in 1..raw.len() {
            assert_eq!(chunks.scan(&raw[..end]), Ok(false), "{end}");
        }
        assert_eq!(chunks.scan(raw), Ok(true));
        assert_eq!(chunks.body(raw), b"abcde");
    }

    #[test]
    fn a_chunked_reply_ends_after_its_trailers_and_never_before() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            // kept-open sockets outlive their request
            let mut held = Vec::new();
            for (index, stream) in listener.incoming().flatten().enumerate() {
                let mut stream = stream;
                let mut head = [0u8; 4096];
                let _ = stream.read(&mut head);
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nok\r\n0\r\n");
                if index == 0 {
                    // trailers, then the socket stays open
                    let _ = stream.write_all(b"X-Sum: 1\r\n\r\n");
                    held.push(stream);
                }
                // else: EOF without the closing empty line
            }
        });
        let timeout = Duration::from_secs(3);
        let started = Instant::now();
        let (_, body) = http_call("127.0.0.1", port, "GET", "/", None, timeout, 1024).unwrap();
        assert_eq!(body, b"ok");
        assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
        let error = http_call("127.0.0.1", port, "GET", "/", None, timeout, 1024).unwrap_err();
        assert!(error.contains("truncated chunked reply"), "{error}");
    }

    /// The budget bounds one call over all its requests, not each of them.
    #[test]
    fn the_budget_bounds_the_whole_call() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let mut held = Vec::new();
            for stream in listener.incoming().flatten() {
                held.push(stream); // never answers
            }
        });
        let mut symbolicator = Symbolicator::new(Some(port), None);
        symbolicator.timeout = Duration::from_secs(5);
        symbolicator.budget = Some(Duration::from_millis(400));
        let mut error = error_with(vec![bundle_frame(1, 5)]);
        let started = Instant::now();
        symbolicator.symbolicate(&mut error);
        assert!(started.elapsed() < Duration::from_millis(1500), "{:?}", started.elapsed());
        assert!(!error.symbolicated);
        assert!(error.symbolication_reason.unwrap().contains("budget is spent"));
    }
}
