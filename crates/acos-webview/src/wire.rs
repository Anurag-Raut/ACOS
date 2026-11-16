//! The guest ⇄ host wire protocol (v1.1), framed as specified in
//! `results/handoff/WVB/WIRE.md` §1–§3.
//!
//! Every message is `u32be length` + `body[length]`, `body[0]` selects the kind:
//! `1` = a UTF-8 JSON object, `2` = a binary blob (frame pixels) whose id names
//! the JSON response it belongs to. Anything else — a zero or oversized length,
//! an unknown kind, a body that is not a JSON object — is a fatal framing error,
//! never a silently dropped message.

use std::io::{self, Read, Write};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Wire protocol version. Additive changes do not bump it; layout or semantic
/// changes do (WIRE.md §7). v1.1 added `view.setVisibility`, `view.settings`,
/// `view.scrollTo`, the scroll/content fields of the `render` response and the
/// `view.focusChanged` event (WV6).
pub const PROTO: f64 = 1.1;

/// Largest legal frame body, in bytes. A frame bigger than this is a protocol
/// error on both sides (a frame is a message, not a stream).
pub const MAX_FRAME: usize = 64 * 1024 * 1024;

pub const KIND_JSON: u8 = 1;
pub const KIND_BLOB: u8 = 2;

/// Blob pixel formats. v1 emits PNG only; RGBA is reserved so a guest never has
/// to guess what a `1` means.
pub const FMT_PNG: u8 = 0;
pub const FMT_RGBA8888: u8 = 1;

/// Stable error codes (WIRE.md §3). Unsupported behaviour always fails with one
/// of these; there is no silent success.
pub mod code {
    pub const BAD_REQUEST: &str = "BAD_REQUEST";
    pub const BAD_FRAME: &str = "BAD_FRAME";
    pub const PROTO_VERSION: &str = "PROTO_VERSION";
    pub const ALREADY_CONNECTED: &str = "ALREADY_CONNECTED";
    pub const NO_VIEW: &str = "NO_VIEW";
    pub const VIEW_EXISTS: &str = "VIEW_EXISTS";
    pub const INVALID_VIEWPORT: &str = "INVALID_VIEWPORT";
    pub const NAVIGATION_FAILED: &str = "NAVIGATION_FAILED";
    pub const JS_EXCEPTION: &str = "JS_EXCEPTION";
    pub const TIMEOUT: &str = "TIMEOUT";
    pub const UNSUPPORTED: &str = "UNSUPPORTED";
    pub const ENGINE_UNAVAILABLE: &str = "ENGINE_UNAVAILABLE";
    pub const ENGINE_CRASHED: &str = "ENGINE_CRASHED";
    pub const INTERCEPT_FAILED: &str = "INTERCEPT_FAILED";
    pub const FRAME_FAILED: &str = "FRAME_FAILED";
    pub const INTERNAL: &str = "INTERNAL";
}

/// A decoded wire frame.
#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    Json(Value),
    Blob(Blob),
}

/// A binary payload (one rendered frame) tied to the JSON response it follows.
#[derive(Debug, Clone, PartialEq)]
pub struct Blob {
    pub id: String,
    pub format: u8,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub seq: u64,
    pub data: Vec<u8>,
}

/// A fatal framing error. The connection cannot continue after one: the reader
/// has lost the message boundary.
#[derive(Debug, thiserror::Error)]
pub enum WireError {
    #[error("connection closed at a frame boundary")]
    Eof,
    #[error("bad frame: {0}")]
    BadFrame(String),
    #[error("frame too large: {0} bytes (limit {MAX_FRAME})")]
    TooLarge(usize),
    #[error("io: {0}")]
    Io(#[from] io::Error),
}

fn u32be(body: &[u8], at: usize) -> Result<u32, WireError> {
    body.get(at..at + 4)
        .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or_else(|| WireError::BadFrame(format!("truncated header at {at}")))
}

fn u64be(body: &[u8], at: usize) -> Result<u64, WireError> {
    body.get(at..at + 8)
        .map(|s| {
            u64::from_be_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]])
        })
        .ok_or_else(|| WireError::BadFrame(format!("truncated header at {at}")))
}

/// Encode a JSON message as one wire frame.
pub fn encode_json(msg: &Value) -> Result<Vec<u8>, WireError> {
    if !msg.is_object() {
        return Err(WireError::BadFrame("JSON message is not an object".into()));
    }
    let payload = serde_json::to_vec(msg).map_err(|e| WireError::BadFrame(e.to_string()))?;
    encode_body(KIND_JSON, &payload)
}

/// Encode a blob as one wire frame.
pub fn encode_blob(blob: &Blob) -> Result<Vec<u8>, WireError> {
    let id = blob.id.as_bytes();
    let mut payload = Vec::with_capacity(id.len() + 26 + blob.data.len());
    payload.extend_from_slice(&(id.len() as u32).to_be_bytes());
    payload.extend_from_slice(id);
    payload.push(blob.format);
    payload.extend_from_slice(&blob.width.to_be_bytes());
    payload.extend_from_slice(&blob.height.to_be_bytes());
    payload.extend_from_slice(&blob.stride.to_be_bytes());
    payload.extend_from_slice(&blob.seq.to_be_bytes());
    payload.extend_from_slice(&blob.data);
    encode_body(KIND_BLOB, &payload)
}

fn encode_body(kind: u8, payload: &[u8]) -> Result<Vec<u8>, WireError> {
    let len = payload.len() + 1;
    if len > MAX_FRAME {
        return Err(WireError::TooLarge(len));
    }
    let mut out = Vec::with_capacity(4 + len);
    out.extend_from_slice(&(len as u32).to_be_bytes());
    out.push(kind);
    out.extend_from_slice(payload);
    Ok(out)
}

/// Read one frame. `Eof` is returned only when the peer closed at a frame
/// boundary — a truncated frame is `BadFrame`, so a half-written message can
/// never be mistaken for a complete one.
pub fn read_frame<R: Read>(reader: &mut R) -> Result<Frame, WireError> {
    let mut len_buf = [0u8; 4];
    match read_exact(reader, &mut len_buf)? {
        false => return Err(WireError::Eof),
        true => {}
    }
    let len = u32::from_be_bytes(len_buf) as usize;
    if len == 0 {
        return Err(WireError::BadFrame("zero-length frame".into()));
    }
    if len > MAX_FRAME {
        return Err(WireError::TooLarge(len));
    }
    let mut body = vec![0u8; len];
    if !read_exact(reader, &mut body)? {
        return Err(WireError::BadFrame("truncated frame body".into()));
    }
    let (kind, payload) = body.split_first().expect("len > 0");
    match *kind {
        KIND_JSON => {
            let msg: Value = serde_json::from_slice(payload)
                .map_err(|e| WireError::BadFrame(format!("JSON: {e}")))?;
            if !msg.is_object() {
                return Err(WireError::BadFrame("JSON message is not an object".into()));
            }
            Ok(Frame::Json(msg))
        }
        KIND_BLOB => {
            let nid = u32be(payload, 0)? as usize;
            let id_end = 4 + nid;
            let id = payload
                .get(4..id_end)
                .ok_or_else(|| WireError::BadFrame("truncated blob id".into()))
                .and_then(|b| {
                    std::str::from_utf8(b)
                        .map(str::to_owned)
                        .map_err(|_| WireError::BadFrame("blob id is not UTF-8".into()))
                })?;
            let format = *payload
                .get(id_end)
                .ok_or_else(|| WireError::BadFrame("truncated blob format".into()))?;
            let width = u32be(payload, id_end + 1)?;
            let height = u32be(payload, id_end + 5)?;
            let stride = u32be(payload, id_end + 9)?;
            let seq = u64be(payload, id_end + 13)?;
            let data_at = id_end + 21;
            let data = payload
                .get(data_at..)
                .ok_or_else(|| WireError::BadFrame("truncated blob data".into()))?
                .to_vec();
            Ok(Frame::Blob(Blob {
                id,
                format,
                width,
                height,
                stride,
                seq,
                data,
            }))
        }
        other => Err(WireError::BadFrame(format!("unknown frame kind {other}"))),
    }
}

fn read_exact<R: Read>(reader: &mut R, buf: &mut [u8]) -> Result<bool, WireError> {
    let mut done = 0;
    while done < buf.len() {
        match reader.read(&mut buf[done..]) {
            Ok(0) => {
                return if done == 0 {
                    Ok(false)
                } else {
                    Err(WireError::BadFrame("short read".into()))
                }
            }
            Ok(n) => done += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(WireError::Io(e)),
        }
    }
    Ok(true)
}

/// Write one pre-encoded frame.
pub fn write_frame<W: Write>(writer: &mut W, frame: &[u8]) -> io::Result<()> {
    writer.write_all(frame)?;
    writer.flush()
}

/// A structured error body (`WIRE.md` §3).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<Value>,
}

impl ErrorBody {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            detail: None,
        }
    }

    pub fn with_detail(mut self, detail: Value) -> Self {
        self.detail = Some(detail);
        self
    }
}

/// `{"id": id, "ok": true, "result": …}`
pub fn ok_response(id: &str, result: Value) -> Value {
    json!({"id": id, "ok": true, "result": result})
}

/// `{"id": id, "ok": false, "error": {code, message, detail?}}`
pub fn err_response(id: &str, error: &ErrorBody) -> Value {
    json!({"id": id, "ok": false, "error": error})
}

/// `{"event": name, …fields}`
pub fn event(name: &str, mut fields: Value) -> Value {
    if let Value::Object(ref mut map) = fields {
        map.insert("event".to_string(), Value::String(name.to_string()));
        Value::Object(map.clone())
    } else {
        json!({"event": name})
    }
}

/// Default per-request timeout used by the connection for host→guest requests.
pub const HOST_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_frame_round_trips() {
        let msg = json!({"id": "g1", "op": "hello", "proto": PROTO, "ü": "ünïcode"});
        let bytes = encode_json(&msg).unwrap();
        // length prefix is big-endian and counts the kind byte
        assert_eq!(
            u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize,
            bytes.len() - 4
        );
        assert_eq!(bytes[4], KIND_JSON);
        let mut cur = std::io::Cursor::new(bytes);
        assert_eq!(read_frame(&mut cur).unwrap(), Frame::Json(msg));
        // a second read at a boundary is a clean Eof, not an error
        assert!(matches!(read_frame(&mut cur), Err(WireError::Eof)));
    }

    #[test]
    fn blob_frame_round_trips() {
        let blob = Blob {
            id: "g42".into(),
            format: FMT_PNG,
            width: 1080,
            height: 2400,
            stride: 72_031,
            seq: 9,
            data: vec![0x89, b'P', b'N', b'G', 1, 2, 3, 0, 255],
        };
        let mut cur = std::io::Cursor::new(encode_blob(&blob).unwrap());
        assert_eq!(read_frame(&mut cur).unwrap(), Frame::Blob(blob));
    }

    #[test]
    fn blob_and_json_stay_ordered_on_one_stream() {
        let mut buf = Vec::new();
        write_frame(&mut buf, &encode_json(&json!({"id": "g7", "ok": true})).unwrap()).unwrap();
        write_frame(
            &mut buf,
            &encode_blob(&Blob {
                id: "g7".into(),
                format: FMT_PNG,
                width: 2,
                height: 2,
                stride: 4,
                seq: 1,
                data: vec![1, 2, 3, 4],
            })
            .unwrap(),
        )
        .unwrap();
        let mut cur = std::io::Cursor::new(buf);
        assert!(matches!(read_frame(&mut cur).unwrap(), Frame::Json(_)));
        assert!(matches!(read_frame(&mut cur).unwrap(), Frame::Blob(_)));
    }

    #[test]
    fn zero_length_frame_is_rejected() {
        let mut cur = std::io::Cursor::new(vec![0u8, 0, 0, 0]);
        assert!(matches!(read_frame(&mut cur), Err(WireError::BadFrame(_))));
    }

    #[test]
    fn oversized_frame_is_rejected_before_allocating() {
        let mut cur = std::io::Cursor::new((MAX_FRAME as u32 + 1).to_be_bytes().to_vec());
        match read_frame(&mut cur) {
            Err(WireError::TooLarge(n)) => assert_eq!(n, MAX_FRAME + 1),
            other => panic!("expected TooLarge, got {other:?}"),
        }
    }

    #[test]
    fn truncated_body_is_bad_frame_not_eof() {
        let full = encode_json(&json!({"id": "g1", "op": "ping"})).unwrap();
        let mut cur = std::io::Cursor::new(full[..full.len() - 3].to_vec());
        assert!(matches!(read_frame(&mut cur), Err(WireError::BadFrame(_))));
    }

    #[test]
    fn unknown_kind_and_non_object_json_are_rejected() {
        let mut cur = std::io::Cursor::new(vec![0, 0, 0, 2, 9, 0]);
        assert!(matches!(read_frame(&mut cur), Err(WireError::BadFrame(_))));
        let body = b"[1,2]";
        let mut bytes = ((body.len() + 1) as u32).to_be_bytes().to_vec();
        bytes.push(KIND_JSON);
        bytes.extend_from_slice(body);
        assert!(matches!(
            read_frame(&mut std::io::Cursor::new(bytes)),
            Err(WireError::BadFrame(_))
        ));
    }

    #[test]
    fn responses_and_events_have_the_documented_shape() {
        assert_eq!(
            ok_response("g1", json!({"a": 1})),
            json!({"id": "g1", "ok": true, "result": {"a": 1}})
        );
        let e = err_response("g2", &ErrorBody::new(code::NO_VIEW, "no view 9"));
        assert_eq!(e["ok"], json!(false));
        assert_eq!(e["error"]["code"], json!("NO_VIEW"));
        assert!(e["error"].get("detail").is_none());
        let ev = event("navigation.finished", json!({"view": 1}));
        assert_eq!(ev["event"], json!("navigation.finished"));
        assert_eq!(ev["view"], json!(1));
    }
}
