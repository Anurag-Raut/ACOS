//! APK signature-scheme *detection* (not cryptographic verification).
//!
//! v1 is detected from `META-INF/*.SF` + `*.RSA`/`*.DSA`/`*.EC` entries.
//! v2/v3/v3.1 are detected by locating the APK Signing Block magic
//! (`"APK Sig Block 42"`) and reading its id-value pairs.
//!
//! ACOS states explicitly that signatures are **not cryptographically
//! verified** here; this is presence/detection only.

use crate::error::Result;

const BLOCK_MAGIC: &[u8; 16] = b"APK Sig Block 42";
const ID_V2: u32 = 0x7109_871a;
const ID_V3: u32 = 0xf053_68c0;
const ID_V3_1: u32 = 0x1b93_ad61;
const ID_SOURCE_STAMP: u32 = 0x2b09_189e;

/// Detected signature schemes and a fidelity note.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SigningInfo {
    pub v1: bool,
    pub v2: bool,
    pub v3: bool,
    pub v3_1: bool,
    pub source_stamp: bool,
    /// Always present; spells out the non-verified status.
    pub note: String,
}

impl Default for SigningInfo {
    fn default() -> Self {
        SigningInfo {
            v1: false,
            v2: false,
            v3: false,
            v3_1: false,
            source_stamp: false,
            note: NOT_VERIFIED.to_string(),
        }
    }
}

/// The fidelity note attached to all signing detections.
pub const NOT_VERIFIED: &str = "signature schemes detected only; not cryptographically verified";

/// Detect the signing schemes used by an APK.
pub fn detect(entry_names: &[String], bytes: &[u8]) -> SigningInfo {
    let v1 = detect_v1(entry_names);
    let (v2, v3, v3_1, source_stamp) = detect_block(bytes);
    SigningInfo {
        v1,
        v2,
        v3,
        v3_1,
        source_stamp,
        note: NOT_VERIFIED.to_string(),
    }
}

fn detect_v1(entry_names: &[String]) -> bool {
    let mut has_sf = false;
    let mut has_block = false;
    for name in entry_names {
        let upper = name.to_ascii_uppercase();
        if !upper.starts_with("META-INF/") {
            continue;
        }
        if upper.ends_with(".SF") {
            has_sf = true;
        } else if upper.ends_with(".RSA") || upper.ends_with(".DSA") || upper.ends_with(".EC") {
            has_block = true;
        }
    }
    has_sf && has_block
}

fn detect_block(bytes: &[u8]) -> (bool, bool, bool, bool) {
    let magic_at = match find_magic(bytes) {
        Some(at) => at,
        None => return (false, false, false, false),
    };
    let pairs = match parse_block(bytes, magic_at) {
        Ok(p) => p,
        Err(_) => return (false, false, false, false),
    };
    let mut v2 = false;
    let mut v3 = false;
    let mut v3_1 = false;
    let mut source_stamp = false;
    for (id, _) in pairs {
        match id {
            ID_V2 => v2 = true,
            ID_V3 => v3 = true,
            ID_V3_1 => v3_1 = true,
            ID_SOURCE_STAMP => source_stamp = true,
            _ => {}
        }
    }
    (v2, v3, v3_1, source_stamp)
}

fn find_magic(bytes: &[u8]) -> Option<usize> {
    if bytes.len() < BLOCK_MAGIC.len() {
        return None;
    }
    bytes
        .windows(BLOCK_MAGIC.len())
        .rposition(|w| w == BLOCK_MAGIC)
}

/// Parse the id-value pairs of the APK Signing Block. `magic_at` is the offset
/// of the 16-byte magic at the end of the block.
fn parse_block(bytes: &[u8], magic_at: usize) -> Result<Vec<(u32, &[u8])>> {
    let err = || {
        crate::error::ApkError::new(
            crate::error::APK_SIGNING_MALFORMED,
            "malformed signing block",
        )
    };
    if magic_at < 8 {
        return Err(err());
    }
    // Layout: [size:u64][pairs][size:u64][magic:16]. The advertised size
    // covers pairs + trailing size + magic, so the block starts at
    // magic_at + 8 - size.
    let size2 = read_u64(bytes, magic_at - 8)? as usize;
    let pairs_end = magic_at.checked_sub(8).ok_or_else(err)?;
    let block_start = magic_at
        .checked_add(8)
        .and_then(|v| v.checked_sub(size2))
        .ok_or_else(err)?;
    let size1 = read_u64(bytes, block_start)? as usize;
    if size1 != size2 {
        return Err(err());
    }
    let mut pos = block_start + 8;
    let end = pairs_end;
    let mut out = Vec::new();
    while pos + 12 <= end {
        let len = read_u64(bytes, pos)? as usize;
        pos += 8;
        if len < 4 || pos.saturating_add(len) > end {
            break;
        }
        let id = read_u32_at(bytes, pos);
        let value = bytes.get(pos + 4..pos + len).unwrap_or(&[]);
        out.push((id, value));
        pos = pos.saturating_add(len);
    }
    Ok(out)
}

fn read_u32_at(bytes: &[u8], at: usize) -> u32 {
    match bytes.get(at..at + 4) {
        Some(b) => u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
        None => 0,
    }
}

fn read_u64(bytes: &[u8], at: usize) -> Result<u64> {
    let b = bytes.get(at..at + 8).ok_or_else(|| {
        crate::error::ApkError::new(crate::error::APK_SIGNING_MALFORMED, "short signing block")
    })?;
    Ok(u64::from_le_bytes([
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
    ]))
}
