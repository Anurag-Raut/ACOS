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

/// DER-encoded X.509 signing certificates carried by the APK.
///
/// `PackageManager` must answer the app's **real** signing certificate for
/// `GET_SIGNATURES`/`GET_SIGNING_CERTIFICATES`; `PackageInfo.signatures` was
/// previously empty, so an app that parses its own certificate
/// (`org.quantumbadger.redreader` does this in `Application.onCreate`) died in
/// conscrypt on a malformed/absent signature.
///
/// The block is parsed with the layout of `ApkSignatureSchemeV2Verifier` /
/// `V3Verifier` (AOSP `frameworks/base/core/java/android/util/apk/`):
/// `[len:u32 signers][signer...]`, each signer
/// `[len:u32 signedData][len:u32 signatures][len:u32 publicKey]`, and the
/// signed data `[len:u32 digests][len:u32 certificates][...]` where
/// `certificates` is a sequence of `[len:u32 der]`. The highest scheme the file
/// carries is preferred (v3.1, then v3, then v2), matching the platform's
/// `SigningDetails` selection. A v1-only APK falls back to the PKCS#7
/// `META-INF/*.RSA` block and its first `Certificate`.
#[must_use]
pub fn certificates(bytes: &[u8], v1_blocks: &[Vec<u8>]) -> Vec<Vec<u8>> {
    if let Some(magic_at) = find_magic(bytes) {
        if let Ok(pairs) = parse_block(bytes, magic_at) {
            for id in [ID_V3_1, ID_V3, ID_V2] {
                if let Some((_, value)) = pairs.iter().find(|(i, _)| *i == id) {
                    let certs = scheme_certificates(value);
                    if !certs.is_empty() {
                        return certs;
                    }
                }
            }
        }
    }
    for block in v1_blocks {
        if let Some(cert) = pkcs7_first_certificate(block) {
            return vec![cert];
        }
    }
    Vec::new()
}

fn scheme_certificates(block: &[u8]) -> Vec<Vec<u8>> {
    let mut pos = 0;
    let Some(signers) = der_len_prefixed(block, &mut pos) else {
        return Vec::new();
    };
    let mut spos = 0;
    let Some(signer) = der_len_prefixed(signers, &mut spos) else {
        return Vec::new();
    };
    let mut p = 0;
    let Some(signed_data) = der_len_prefixed(signer, &mut p) else {
        return Vec::new();
    };
    let mut q = 0;
    let _digests = der_len_prefixed(signed_data, &mut q);
    let Some(cert_seq) = der_len_prefixed(signed_data, &mut q) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut c = 0;
    while let Some(cert) = der_len_prefixed(cert_seq, &mut c) {
        if !cert.is_empty() {
            out.push(cert.to_vec());
        }
    }
    out
}

/// A little-endian `u32` length followed by that many bytes.
fn der_len_prefixed<'a>(bytes: &'a [u8], pos: &mut usize) -> Option<&'a [u8]> {
    let len_bytes = bytes.get(*pos..pos.checked_add(4)?)?;
    let len = u32::from_le_bytes([len_bytes[0], len_bytes[1], len_bytes[2], len_bytes[3]]) as usize;
    *pos += 4;
    let out = bytes.get(*pos..pos.checked_add(len)?)?;
    *pos += len;
    Some(out)
}

/// One DER TLV: `(tag, content, start, end)` where `start..end` is the whole
/// element (header included) relative to `bytes`.
fn der_tlv<'a>(bytes: &'a [u8], pos: &mut usize) -> Option<(u8, &'a [u8], usize, usize)> {
    let start = *pos;
    let tag = *bytes.get(*pos)?;
    *pos += 1;
    let first = *bytes.get(*pos)?;
    *pos += 1;
    let len = if first & 0x80 == 0 {
        usize::from(first)
    } else {
        let n = usize::from(first & 0x7f);
        if n == 0 || n > 4 {
            return None;
        }
        let mut v = 0usize;
        for _ in 0..n {
            v = (v << 8) | usize::from(*bytes.get(*pos)?);
            *pos += 1;
        }
        v
    };
    let content_start = *pos;
    let end = content_start.checked_add(len)?;
    let content = bytes.get(content_start..end)?;
    *pos = end;
    Some((tag, content, start, end))
}

/// First X.509 certificate in a PKCS#7 `SignedData` (v1 signature block).
///
/// `ContentInfo ::= SEQUENCE { OID, [0] EXPLICIT SignedData }`, and
/// `SignedData ::= SEQUENCE { INTEGER, SET, SEQUENCE, [0] IMPLICIT
/// CertificateSet, ... }`. Only the first certificate is needed: the platform's
/// `Signature[]` for a single-signer APK has one element, and corpus APKs are
/// single-signed.
fn pkcs7_first_certificate(block: &[u8]) -> Option<Vec<u8>> {
    let mut pos = 0;
    let (tag, content, _, _) = der_tlv(block, &mut pos)?;
    if tag != 0x30 {
        return None;
    }
    let mut p = 0;
    let (oid, _, _, _) = der_tlv(content, &mut p)?;
    if oid != 0x06 {
        return None;
    }
    let (wrapped_tag, wrapped, _, _) = der_tlv(content, &mut p)?;
    if wrapped_tag != 0xA0 {
        return None;
    }
    let mut w = 0;
    let (sd_tag, signed_data, _, _) = der_tlv(wrapped, &mut w)?;
    if sd_tag != 0x30 {
        return None;
    }
    let mut s = 0;
    while s < signed_data.len() {
        let (t, c, _, _) = der_tlv(signed_data, &mut s)?;
        if t == 0xA0 {
            // CertificateSet: the first element is the leaf certificate.
            let mut cpos = 0;
            let (ct, _, cs, ce) = der_tlv(c, &mut cpos)?;
            if ct == 0x30 {
                return Some(c[cs..ce].to_vec());
            }
            return None;
        }
    }
    None
}
