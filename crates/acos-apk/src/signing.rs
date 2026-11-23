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

/// Absolute cap for an untrusted signature block read out of the zip.
///
/// A v1 block is a PKCS#7 `SignedData`; real ones are a few KiB. The cap keeps
/// a crafted entry from making ingest allocate (and decompress) without bound.
pub const MAX_SIGNATURE_BLOCK_BYTES: usize = 1 << 20;
/// Deflated signature blocks compress extremely well; a high ratio on a tiny
/// compressed entry is the zip-bomb shape. Real PKCS#7 blocks stay well under
/// this because most bytes are already DER/ASN.1.
pub const MAX_SIGNATURE_BLOCK_RATIO: usize = 200;
/// How many v1 (JAR) signature block candidates one APK may declare.
///
/// A real APK of the v1 era ships one `.SF` + `.RSA|DSA|EC` pair; even a
/// multi-signer v1 APK stays in single digits. Without this cap a crafted
/// archive with ~10k pairs would make ingest read ~10k x [`MAX_SIGNATURE_BLOCK_BYTES`]
/// into host memory, so the count is checked before any block is read.
pub const MAX_V1_SIGNATURE_BLOCKS: usize = 16;
/// Summed cap for all v1 signature blocks of one APK, declared and actual.
///
/// 4 MiB is four times the single-block cap: a multi-signer v1 APK still fits
/// comfortably, while a crafted archive cannot accumulate reads without bound.
pub const MAX_TOTAL_V1_SIGNATURE_BYTES: usize = 4 << 20;
/// One X.509 certificate.
pub const MAX_CERT_BYTES: usize = 128 * 1024;
/// All `PackageInfo.signatures` certificates together.
pub const MAX_TOTAL_CERT_BYTES: usize = 1 << 20;

fn signing_err(message: impl Into<String>) -> crate::error::ApkError {
    crate::error::ApkError::new(crate::error::APK_SIGNING_MALFORMED, message)
}

/// DER-encoded X.509 **signer** certificates carried by the APK.
///
/// The array shape is the platform's `PackageInfo.signatures`: one entry per
/// signer, each the signer's own (leaf) certificate — **not** the signer's
/// whole chain. The APK Signing Block is parsed with the layout of
/// `ApkSignatureSchemeV2Verifier`/`V3Verifier` (AOSP
/// `frameworks/base/core/java/android/util/apk/`): the value is a
/// length-prefixed sequence of signers, each signer is
/// `[len:u32 signedData][len:u32 signatures][len:u32 publicKey]`, and the
/// signed data is `[len:u32 digests][len:u32 certificates][...]` where
/// `certificates` is a sequence of `[len:u32 der]` (leaf first).
///
/// Scheme selection mirrors the platform's refusal to downgrade: the highest
/// scheme present is authoritative. A malformed v3 value is an error, never a
/// silent fall back to v2; v1 (JAR) is used only when the block carries none of
/// v2/v3/v3.1. v1 identifies the signer by the `SignerInfo` issuer+serial, so a
/// chain whose first certificate is not the signer is handled correctly.
pub fn certificates(bytes: &[u8], v1_blocks: &[Vec<u8>]) -> Result<Vec<Vec<u8>>> {
    if let Some(magic_at) = find_magic(bytes) {
        let pairs = parse_block(bytes, magic_at)?;
        let chosen = [ID_V3_1, ID_V3, ID_V2]
            .iter()
            .find_map(|id| pairs.iter().find(|(i, _)| i == id));
        if let Some((id, value)) = chosen {
            let certs = scheme_certificates(value, *id)
                .map_err(|e| signing_err(format!("scheme {id:08x}: {e}")))?;
            return Ok(certs);
        }
        // A signing block without a known scheme: the platform falls back to v1.
    }
    if v1_blocks.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for block in v1_blocks {
        let cert = pkcs7_signer_certificate(block)
            .ok_or_else(|| signing_err("v1 signature block has no signer certificate"))?;
        push_cert(&mut out, cert)?;
    }
    Ok(out)
}

fn push_cert(out: &mut Vec<Vec<u8>>, cert: Vec<u8>) -> Result<()> {
    if cert.len() > MAX_CERT_BYTES {
        return Err(signing_err("signing certificate exceeds cap"));
    }
    let total: usize = out.iter().map(Vec::len).sum::<usize>() + cert.len();
    if total > MAX_TOTAL_CERT_BYTES {
        return Err(signing_err("signing certificates exceed cap"));
    }
    let mut pos = 0;
    let (tag, _, _, end) =
        der_tlv(&cert, &mut pos).ok_or_else(|| signing_err("signing certificate is not DER"))?;
    if tag != 0x30 || end != cert.len() {
        return Err(signing_err("signing certificate is not a DER SEQUENCE"));
    }
    out.push(cert);
    Ok(())
}

/// One **leaf** certificate per signer in a v2/v3/v3.1 value.
///
/// The three schemes share the signed-data layout but not the outer signer:
/// v2 is `{signed data, signatures, public key}` while v3/v3.1 insert the
/// minimum and maximum SDK versions (two little-endian `int32`s) between the
/// signed data and the signatures (`apksig` `V3SchemeSigner`). Reading those
/// eight bytes as a length prefix is how every v3-signed APK — which is every
/// APK `tools/build/build-apps.sh` produces — got **zero** certificates
/// (`APK_SIGNING_MALFORMED: scheme f05368c0: truncated signatures`), so
/// `PackageInfo.signatures` was always empty under `GET_SIGNATURES`.
fn scheme_certificates(
    block: &[u8],
    scheme: u32,
) -> std::result::Result<Vec<Vec<u8>>, &'static str> {
    if block.len() > MAX_SIGNATURE_BLOCK_BYTES {
        return Err("signature block exceeds cap");
    }
    // The block value is the **signers sequence**: `int32 byte-length` followed
    // by that many signer records, each itself `int32 byte-length` + payload
    // (`apksig` `V2SchemeSigner`/`V3SchemeSigner`, `generateSignerBlock`). The
    // previous parser read the sequence length as the first signer's length, so
    // `signed_data` swallowed the signatures and public key and the next read
    // failed with `truncated signatures` — every APK `tools/build/build-apps.sh`
    // signs (v2 + v3) reported **zero** certificates, so
    // `PackageInfo.signatures` was always an empty `Signature[]`.
    let mut pos = 0;
    let signers = der_len_prefixed(block, &mut pos).ok_or("truncated signer list")?;
    let mut out = Vec::new();
    let mut s_pos = 0;
    while s_pos < signers.len() {
        let signer = der_len_prefixed(signers, &mut s_pos).ok_or("truncated signer list")?;
        if signer.is_empty() {
            return Err("empty signer");
        }
        let mut p = 0;
        let signed_data = der_len_prefixed(signer, &mut p).ok_or("truncated signer")?;
        if scheme != ID_V2 {
            // v3 / v3.1: minSdkVersion and maxSdkVersion, little-endian int32.
            let next = p.checked_add(8).ok_or("truncated min/max SDK")?;
            if next > signer.len() {
                return Err("truncated min/max SDK");
            }
            p = next;
        }
        let _signatures = der_len_prefixed(signer, &mut p).ok_or("truncated signatures")?;
        let _public_key = der_len_prefixed(signer, &mut p).ok_or("truncated public key")?;
        let mut q = 0;
        let _digests = der_len_prefixed(signed_data, &mut q).ok_or("truncated digests")?;
        let cert_seq = der_len_prefixed(signed_data, &mut q).ok_or("truncated certificates")?;
        let mut c = 0;
        let leaf = der_len_prefixed(cert_seq, &mut c).ok_or("signer has no certificate")?;
        if leaf.is_empty() {
            return Err("empty certificate");
        }
        push_cert(&mut out, leaf.to_vec()).map_err(|_| "certificate exceeds cap")?;
        // The rest of the signed data (additional attributes) is irrelevant
        // here; the chain members after the leaf are deliberately skipped.
    }
    if out.is_empty() {
        return Err("no signers");
    }
    Ok(out)
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

/// The X.509 certificate of the v1 `SignerInfo`, matched by its
/// `issuerAndSerialNumber`.
///
/// `ContentInfo ::= SEQUENCE { OID, [0] EXPLICIT SignedData }`,
/// `SignedData ::= SEQUENCE { version, digestAlgorithms, contentInfo,
///   [0] certificates, [1] crls OPTIONAL, signerInfos }`, and
/// `SignerInfo ::= SEQUENCE { version, sid, ... }`. The signer is the
/// certificate whose `tbsCertificate.issuer` and `serialNumber` equal the
/// `sid`; a certificate chain's leaf is normally the signer, but matching the
/// sid is what makes that true rather than an assumption.
fn pkcs7_signer_certificate(block: &[u8]) -> Option<Vec<u8>> {
    if block.len() > MAX_SIGNATURE_BLOCK_BYTES {
        return None;
    }
    let mut pos = 0;
    let (tag, content, _, end) = der_tlv(block, &mut pos)?;
    if tag != 0x30 || end != block.len() {
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
    let (sd_tag, signed_data, _, sd_end) = der_tlv(wrapped, &mut w)?;
    if sd_tag != 0x30 || sd_end != wrapped.len() {
        return None;
    }
    let mut s = 0;
    let (_version, _, _, _) = der_tlv(signed_data, &mut s)?;
    let (_digests, _, _, _) = der_tlv(signed_data, &mut s)?;
    let (_content, _, _, _) = der_tlv(signed_data, &mut s)?;
    let mut certs: Vec<&[u8]> = Vec::new();
    let mut signer_infos: Option<&[u8]> = None;
    while s < signed_data.len() {
        let (t, c, _, _) = der_tlv(signed_data, &mut s)?;
        match t {
            0xA0 => {
                let mut cp = 0;
                while cp < c.len() {
                    let (ct, _, cs, ce) = der_tlv(c, &mut cp)?;
                    if ct == 0x30 {
                        certs.push(&c[cs..ce]);
                    }
                }
            }
            0x31 => signer_infos = Some(c),
            _ => {}
        }
    }
    let infos = signer_infos?;
    let mut ip = 0;
    let (_it, signer_info, _, _) = der_tlv(infos, &mut ip)?;
    let mut sp = 0;
    let (_v, _, _, _) = der_tlv(signer_info, &mut sp)?;
    let (sid_tag, sid, _, _) = der_tlv(signer_info, &mut sp)?;
    if sid_tag != 0x30 {
        // subjectKeyIdentifier signer ids are not used by the corpus; refusing
        // is explicit rather than returning the wrong certificate.
        return None;
    }
    let mut ap = 0;
    let (issuer_tag, _, issuer_start, issuer_end) = der_tlv(sid, &mut ap)?;
    let (serial_tag, serial, _, _) = der_tlv(sid, &mut ap)?;
    if issuer_tag != 0x30 || serial_tag != 0x02 {
        return None;
    }
    let issuer = &sid[issuer_start..issuer_end];
    for cert in certs {
        let (cert_issuer, cert_serial) = certificate_issuer_serial(cert)?;
        if cert_issuer == issuer && cert_serial == serial {
            return Some(cert.to_vec());
        }
    }
    None
}

/// `tbsCertificate.issuer` (full DER element) and `serialNumber` (contents).
fn certificate_issuer_serial(cert: &[u8]) -> Option<(&[u8], &[u8])> {
    let mut pos = 0;
    let (tag, content, _, end) = der_tlv(cert, &mut pos)?;
    if tag != 0x30 || end != cert.len() {
        return None;
    }
    let mut p = 0;
    let (tbs_tag, tbs, _, _) = der_tlv(content, &mut p)?;
    if tbs_tag != 0x30 {
        return None;
    }
    let mut t = 0;
    let mut first = der_tlv(tbs, &mut t)?;
    if first.0 == 0xA0 {
        first = der_tlv(tbs, &mut t)?;
    }
    if first.0 != 0x02 {
        return None;
    }
    let (_sig, _, _, _) = der_tlv(tbs, &mut t)?;
    let (issuer_tag, _, issuer_start, issuer_end) = der_tlv(tbs, &mut t)?;
    if issuer_tag != 0x30 {
        return None;
    }
    Some((&tbs[issuer_start..issuer_end], first.1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
        let mut out = vec![tag];
        if content.len() < 0x80 {
            out.push(content.len() as u8);
        } else {
            out.push(0x81);
            out.push(content.len() as u8);
        }
        out.extend_from_slice(content);
        out
    }

    fn u32p(bytes: &[u8]) -> Vec<u8> {
        let mut out = (bytes.len() as u32).to_le_bytes().to_vec();
        out.extend_from_slice(bytes);
        out
    }

    fn cert(serial: u8, issuer: &[u8]) -> Vec<u8> {
        let tbs = [
            tlv(0x02, &[serial]),
            tlv(0x30, &[0x06, 0x00]),
            issuer.to_vec(),
        ]
        .concat();
        tlv(0x30, &tlv(0x30, &tbs))
    }

    /// One v2 signer record (length-prefixed: signed data, signatures, key).
    fn signer_v2(certs: &[&[u8]]) -> Vec<u8> {
        let mut chain = Vec::new();
        for c in certs {
            chain.extend(u32p(c));
        }
        let mut signed_data = u32p(b"\x00\x00\x00\x00");
        signed_data.extend(u32p(&chain));
        signed_data.extend(u32p(&[]));
        [u32p(&signed_data), u32p(b"\x00\x00"), u32p(b"key")].concat()
    }

    /// A whole block value: the signers sequence of the given signer records.
    fn signers_value(signers: &[Vec<u8>]) -> Vec<u8> {
        let mut seq = Vec::new();
        for signer in signers {
            seq.extend(u32p(signer));
        }
        u32p(&seq)
    }

    fn scheme_value(certs: &[&[u8]]) -> Vec<u8> {
        signers_value(&[signer_v2(certs)])
    }

    /// The v3 / v3.1 signer shape: the same signed data, then min/max SDK as
    /// little-endian int32s, then the signatures and the public key.
    fn scheme_value_v3(certs: &[&[u8]]) -> Vec<u8> {
        let mut chain = Vec::new();
        for c in certs {
            chain.extend(u32p(c));
        }
        let mut signed_data = u32p(b"\x00\x00\x00\x00");
        signed_data.extend(u32p(&chain));
        signed_data.extend(u32p(&[])); // additional attributes
        let min_sdk = 26u32.to_le_bytes();
        let max_sdk = u32::MAX.to_le_bytes();
        let signer = [
            u32p(&signed_data),
            min_sdk.to_vec(),
            max_sdk.to_vec(),
            u32p(b"\x00\x00"),
            u32p(b"key"),
        ]
        .concat();
        signers_value(&[signer])
    }

    fn signing_block(pairs: &[(u32, Vec<u8>)]) -> Vec<u8> {
        let mut body = Vec::new();
        for (id, value) in pairs {
            let mut pair = id.to_le_bytes().to_vec();
            pair.extend_from_slice(value);
            body.extend(u32p_u64(&pair));
        }
        let size = (body.len() + 8 + 16) as u64;
        [
            &size.to_le_bytes()[..],
            &body,
            &size.to_le_bytes()[..],
            BLOCK_MAGIC,
        ]
        .concat()
    }

    fn u32p_u64(bytes: &[u8]) -> Vec<u8> {
        let mut out = (bytes.len() as u64).to_le_bytes().to_vec();
        out.extend_from_slice(bytes);
        out
    }

    fn pkcs7(issuer: &[u8], serials_and_certs: &[(u8, Vec<u8>)], signer_serial: u8) -> Vec<u8> {
        let mut certs = Vec::new();
        for (_, c) in serials_and_certs {
            certs.extend_from_slice(c);
        }
        let sid = tlv(
            0x30,
            &[issuer.to_vec(), tlv(0x02, &[signer_serial])].concat(),
        );
        let signer_info = tlv(0x30, &[tlv(0x02, &[1]), sid].concat());
        let signed_data = tlv(
            0x30,
            &[
                tlv(0x02, &[1]),
                tlv(0x31, &[]),
                tlv(0x30, &[]),
                tlv(0xA0, &certs),
                tlv(0x31, &signer_info),
            ]
            .concat(),
        );
        let ci = tlv(
            0x30,
            &[
                vec![
                    0x06, 0x09, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x07, 0x02,
                ],
                tlv(0xA0, &signed_data),
            ]
            .concat(),
        );
        ci
    }

    #[test]
    fn scheme_returns_one_leaf_per_signer_not_the_chain() {
        let issuer = tlv(
            0x30,
            &tlv(0x31, &tlv(0x30, &[0x06, 0x03, b'a', b'b', b'c'])),
        );
        let leaf1 = cert(1, &issuer);
        let chain1 = cert(9, &issuer);
        let leaf2 = cert(2, &issuer);
        let value = signers_value(&[signer_v2(&[&leaf1, &chain1]), signer_v2(&[&leaf2])]);
        let got = scheme_certificates(&value, ID_V2).expect("valid");
        assert_eq!(got, vec![leaf1, leaf2]);
    }

    #[test]
    fn highest_present_scheme_wins_and_never_downgrades() {
        let issuer = tlv(0x30, &[]);
        let v2 = cert(1, &issuer);
        let v3 = cert(2, &issuer);
        let bytes = signing_block(&[
            (ID_V2, scheme_value(&[&v2])),
            (ID_V3, scheme_value_v3(&[&v3])),
        ]);
        assert_eq!(certificates(&bytes, &[]).unwrap(), vec![v3]);

        let malformed_v3 = vec![0x01];
        let bytes = signing_block(&[
            (ID_V2, scheme_value(&[&cert(1, &issuer)])),
            (ID_V3, malformed_v3),
        ]);
        let v1 = pkcs7(&issuer, &[(1, cert(1, &issuer))], 1);
        let err = certificates(&bytes, &[v1]).unwrap_err();
        assert_eq!(err.code(), crate::error::APK_SIGNING_MALFORMED);
    }

    /// REL3P: a v3-signed APK (the shape `tools/build/build-apps.sh` produces) must
    /// yield its signer certificate; the min/max SDK words are not a length.
    #[test]
    fn v3_signer_reads_the_certificate_after_the_sdk_bounds() {
        let issuer = tlv(0x30, &[]);
        let leaf = cert(3, &issuer);
        let bytes = signing_block(&[(ID_V3, scheme_value_v3(&[&leaf]))]);
        assert_eq!(certificates(&bytes, &[]).unwrap(), vec![leaf]);

        let bytes = signing_block(&[(ID_V3_1, scheme_value_v3(&[&cert(4, &issuer)]))]);
        assert_eq!(certificates(&bytes, &[]).unwrap().len(), 1);
    }

    #[test]
    fn oversized_block_is_rejected_before_parsing() {
        let huge = vec![0u8; MAX_SIGNATURE_BLOCK_BYTES + 1];
        assert_eq!(
            scheme_certificates(&huge, ID_V2),
            Err("signature block exceeds cap")
        );
        assert_eq!(pkcs7_signer_certificate(&huge), None);
    }

    #[test]
    fn v1_uses_the_signer_sid_not_the_first_chain_certificate() {
        let issuer = tlv(0x30, &tlv(0x31, &[0x06, 0x03, b'x', b'y', b'z']));
        let leaf = cert(1, &issuer);
        let signer = cert(2, &issuer);
        let block = pkcs7(&issuer, &[(1, leaf.clone()), (2, signer.clone())], 2);
        assert_eq!(
            certificate_issuer_serial(&signer).map(|(i, _)| i),
            Some(&issuer[..])
        );
        assert_eq!(pkcs7_signer_certificate(&block), Some(signer));
    }
}
