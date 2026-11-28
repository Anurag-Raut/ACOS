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
//!    `source-maps/index.json` mapping the bundle path to the hash), so a later
//!    error in the same run — or after Metro stopped — still maps.
//! 3. Neither: the frames stay as they are and the error says so
//!    (`symbolicated: false`, `symbolication_reason`).
//!
//! Nothing here is guessed: a frame is only rewritten from a mapping Metro or
//! the map actually returned, and `symbolicated` is `true` only when every
//! frame with a line was mapped.

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::time::Duration;

use acos_protocol::rn::{JsError, JsFrame};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// The default bundle path a debug RN app fetches (`DevServerHelper`), used for
/// frames that carry no file (`JSStackTrace.format` drops it).
pub const DEFAULT_BUNDLE_PATH: &str =
    "/index.bundle?platform=android&dev=true&minify=false&modulesOnly=false&runModule=true";

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
        let mut reasons: Vec<String> = Vec::new();
        if let Some(port) = self.port {
            match self.via_metro(port, &error.frames) {
                Ok(frames) => {
                    if self.accept(error, frames, "metro /symbolicate") {
                        return;
                    }
                    reasons.push("metro /symbolicate returned bundle positions".to_string());
                }
                Err(reason) => reasons.push(format!("symbolicate_unreachable: {reason}")),
            }
            match self.fetch_map(port, &error.frames) {
                Ok(map) => {
                    let frames = map.map_frames(&error.frames, self);
                    if self.accept(error, frames, "source map") {
                        return;
                    }
                    reasons.push("source map has no mapping for these frames".to_string());
                }
                Err(reason) => reasons.push(format!("no map: {reason}")),
            }
        } else {
            reasons.push("no metro configured".to_string());
        }
        match self.cached_map(&error.frames) {
            Some(Ok(map)) => {
                let frames = map.map_frames(&error.frames, self);
                if self.accept(error, frames, "source map (cached)") {
                    return;
                }
                reasons.push("cached map has no mapping for these frames".to_string());
            }
            Some(Err(reason)) => reasons.push(format!("cached map unreadable: {reason}")),
            None => {
                if self.port.is_none() {
                    reasons.push("no map".to_string());
                }
            }
        }
        error.symbolicated = false;
        error.symbolication_reason = Some(reasons.join("; "));
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

    fn bundle_of<'a>(&'a self, frame: &'a JsFrame) -> String {
        match frame.file.as_deref() {
            Some(file) if file.contains(".bundle") => path_and_query(file),
            _ => self.bundle_path.clone(),
        }
    }

    fn via_metro(&self, port: u16, frames: &[JsFrame]) -> Result<Vec<JsFrame>, String> {
        let stack: Vec<Value> = frames
            .iter()
            .map(|frame| {
                let file = match frame.file.as_deref() {
                    Some(file) if file.contains(".bundle") => file.to_string(),
                    _ => format!("http://localhost:{port}{}", self.bundle_path),
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
            self.timeout,
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

    fn map_path(&self, frames: &[JsFrame]) -> String {
        let bundle = frames
            .iter()
            .find(|frame| frame.line.is_some())
            .map(|frame| self.bundle_of(frame))
            .unwrap_or_else(|| self.bundle_path.clone());
        bundle.replacen(".bundle", ".map", 1)
    }

    fn fetch_map(&self, port: u16, frames: &[JsFrame]) -> Result<SourceMap, String> {
        let path = self.map_path(frames);
        let (status, bytes) = http_call(&self.host, port, "GET", &path, None, self.timeout)?;
        if !(200..300).contains(&status) {
            return Err(format!("GET {path}: HTTP {status}"));
        }
        let map = SourceMap::parse(&bytes)?;
        self.store(&path, &bytes);
        Ok(map)
    }

    fn store(&self, map_path: &str, bytes: &[u8]) {
        let Some(dir) = &self.cache_dir else { return };
        if fs::create_dir_all(dir).is_err() {
            return;
        }
        let hash = hex(&Sha256::digest(bytes));
        let file = format!("{}.map", &hash[..16]);
        if !dir.join(&file).is_file() {
            let _ = fs::write(dir.join(&file), bytes);
        }
        let index_path = dir.join("index.json");
        let mut index: BTreeMap<String, String> = fs::read(&index_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        index.insert(map_path.to_string(), file);
        if let Ok(text) = serde_json::to_vec_pretty(&index) {
            let _ = fs::write(index_path, text);
        }
    }

    fn cached_map(&self, frames: &[JsFrame]) -> Option<Result<SourceMap, String>> {
        let dir = self.cache_dir.as_ref()?;
        let index: BTreeMap<String, String> =
            serde_json::from_slice(&fs::read(dir.join("index.json")).ok()?).ok()?;
        let file = index.get(&self.map_path(frames))?;
        Some(
            fs::read(dir.join(file))
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

fn is_source_frame(frame: &JsFrame) -> bool {
    frame.line.is_some()
        && frame
            .file
            .as_deref()
            .is_some_and(|file| !file.contains(".bundle") && !file.is_empty())
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
        Some((file, src_line + 1, src_column, name))
    }

    fn map_frames(&self, frames: &[JsFrame], owner: &Symbolicator) -> Vec<JsFrame> {
        frames
            .iter()
            .map(|frame| match frame.line {
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
            })
            .collect()
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

/// One HTTP/1.0 request with a strict connect + read budget; returns
/// `(status, body)` (a chunked body is decoded).
pub fn http_call(
    host: &str,
    port: u16,
    method: &str,
    path: &str,
    body: Option<&[u8]>,
    timeout: Duration,
) -> Result<(u16, Vec<u8>), String> {
    let addr: SocketAddr = (host, port)
        .to_socket_addrs()
        .map_err(|e| e.to_string())?
        .next()
        .ok_or("no address")?;
    let connect = timeout.min(Duration::from_secs(2));
    let mut stream = TcpStream::connect_timeout(&addr, connect).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|e| e.to_string())?;
    let body = body.unwrap_or(&[]);
    let head = format!(
        "{method} {path} HTTP/1.0\r\nHost: {host}:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream
        .write_all(head.as_bytes())
        .map_err(|e| e.to_string())?;
    stream.write_all(body).map_err(|e| e.to_string())?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).map_err(|e| e.to_string())?;
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("malformed HTTP reply")?;
    let head = String::from_utf8_lossy(&raw[..split]).to_ascii_lowercase();
    let status: u16 = head
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .ok_or("no HTTP status")?;
    let mut payload = raw[split + 4..].to_vec();
    if head.contains("transfer-encoding: chunked") {
        payload = dechunk(&payload)?;
    }
    Ok((status, payload))
}

fn dechunk(mut data: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    loop {
        let end = data
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or("bad chunk")?;
        let size_text = String::from_utf8_lossy(&data[..end]);
        let size = usize::from_str_radix(size_text.split(';').next().unwrap_or("").trim(), 16)
            .map_err(|_| "bad chunk size")?;
        data = &data[end + 2..];
        if size == 0 {
            return Ok(out);
        }
        if data.len() < size {
            return Err("truncated chunk".to_string());
        }
        out.extend_from_slice(&data[..size]);
        data = data.get(size + 2..).unwrap_or(&[]);
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
        let index: BTreeMap<String, String> =
            serde_json::from_slice(&fs::read(cache.join("index.json")).unwrap()).unwrap();
        assert_eq!(
            index["/index.map?platform=android&dev=true"],
            format!("{}.map", &hash[..16])
        );

        // Metro gone: the cached map still answers, and says it was the cache.
        let mut offline = Symbolicator::new(Some(closed_port()), Some(cache.clone()));
        offline.timeout = Duration::from_millis(500);
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
}
