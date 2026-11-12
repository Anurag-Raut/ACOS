//! O1: the PNG encoder's two checksums, checked against the code they replaced.
//!
//! The oracle below is the *original* implementation from before O1: a
//! bit-serial CRC-32 and an Adler-32 with a `%` per byte. It is kept only under
//! `cfg(test)` (this file is `#[cfg(test)]`-gated in `launch.rs`) so every
//! replacement is proved byte-identical, never merely "close enough".

use super::*;

// ---------------------------------------------------------------- the oracle

fn crc32_oracle(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

fn adler32_oracle(data: &[u8]) -> u32 {
    let mut a = 1u32;
    let mut b = 0u32;
    for &byte in data {
        a = (a + u32::from(byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    (b << 16) | a
}

// -------------------------------------------------------------- test helpers

/// Deterministic xorshift64*, so a failing case is always reproducible.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed | 1)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn fill(&mut self, len: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            out.extend_from_slice(&self.next_u64().to_le_bytes());
        }
        out.truncate(len);
        out
    }
}

fn check(data: &[u8], what: &str) {
    assert_eq!(
        crc32(data),
        crc32_oracle(data),
        "crc32 differs from the bit-serial oracle on {what} ({} bytes)",
        data.len()
    );
    assert_eq!(
        adler32(data),
        adler32_oracle(data),
        "adler32 differs from the %-per-byte oracle on {what} ({} bytes)",
        data.len()
    );
}

// --------------------------------------------------------------- known values

#[test]
fn crc32_matches_the_standard_check_vector() {
    // zlib crc32("123456789") == 0xCBF43926.
    assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
}

#[test]
fn adler32_matches_the_standard_check_vector() {
    // zlib adler32("Wikipedia") == 0x11E60398.
    assert_eq!(adler32(b"Wikipedia"), 0x11e6_0398);
}

#[test]
fn empty_and_single_byte_inputs_match_the_oracle() {
    check(b"", "an empty buffer");
    for byte in 0..=255u8 {
        check(&[byte], "a one-byte buffer");
    }
}

// -------------------------------------------------------------- the property

/// Random buffers at the Adler block boundaries, at zero/one, and at the exact
/// size of the 1080x2209 screenshot the D-M5 row measures.
#[test]
fn random_buffers_match_the_oracle_at_every_boundary() {
    let mut rng = Rng::new(0xAC05_0001_0000_0001);
    let lengths = [
        0usize,
        1,
        2,
        3,
        7,
        8,
        9,
        255,
        256,
        4096,
        5551,
        5552,
        5553,
        5554,
        11_104,
        65_535,
        1_000_003,
        9_500_000,
    ];
    for len in lengths {
        let data = rng.fill(len);
        check(&data, "a pseudo-random buffer");
    }
}

/// A few unaligned slices of the same buffer catch any chunking/alignment bug
/// the slice-by-8 loop could hide.
#[test]
fn unaligned_random_slices_match_the_oracle() {
    let mut rng = Rng::new(0xAC05_0001_0000_0002);
    let data = rng.fill(70_001);
    for (start, len) in [(0, 9_500), (1, 9_123), (3, 55_550), (7, 65_536)] {
        check(&data[start..start + len], "an unaligned slice");
    }
}

// --------------------------------------------------------------- the whole PNG

/// The pre-P2 encoder, kept verbatim under `cfg(test)` as the oracle for the
/// whole PNG file: the O1 checksums above *and* the chunk framing the streamed
/// encoder now emits. (Found the hard way: a shared body proves nothing about
/// the bytes it writes around the body.)
fn encode_png_rgba_old(
    width: u32,
    height: u32,
    stride: u32,
    rgba: &[u8],
    crc32: fn(&[u8]) -> u32,
    adler32: fn(&[u8]) -> u32,
) -> Vec<u8> {
    let row = (width as usize).saturating_mul(4);
    let stride_bytes = (stride as usize).saturating_mul(4);
    let mut filtered = Vec::with_capacity((row + 1).saturating_mul(height as usize));
    for y in 0..height as usize {
        filtered.push(0u8);
        let start = y * stride_bytes;
        let end = start + row;
        if end <= rgba.len() {
            filtered.extend_from_slice(&rgba[start..end]);
        } else {
            filtered.resize(filtered.len() + row, 0);
        }
    }
    let mut out = Vec::with_capacity(filtered.len() + 1024);
    out.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    png_chunk_old(&mut out, b"IHDR", &ihdr, crc32);

    let mut zlib = Vec::with_capacity(filtered.len() + 16);
    zlib.extend_from_slice(&[0x78, 0x01]);
    let mut offset = 0usize;
    while offset < filtered.len() {
        let len = (filtered.len() - offset).min(65_535);
        let final_block = offset + len == filtered.len();
        zlib.push(if final_block { 1 } else { 0 });
        zlib.extend_from_slice(&(len as u16).to_le_bytes());
        zlib.extend_from_slice(&(!(len as u16)).to_le_bytes());
        zlib.extend_from_slice(&filtered[offset..offset + len]);
        offset += len;
    }
    zlib.extend_from_slice(&adler32(&filtered).to_be_bytes());
    png_chunk_old(&mut out, b"IDAT", &zlib, crc32);
    png_chunk_old(&mut out, b"IEND", &[], crc32);
    out
}

fn png_chunk_old(out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8], crc32: fn(&[u8]) -> u32) {
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(body);
    let mut crc_input = Vec::with_capacity(4 + body.len());
    crc_input.extend_from_slice(kind);
    crc_input.extend_from_slice(body);
    let crc = crc32(&crc_input);
    out.extend_from_slice(&crc.to_be_bytes());
}

/// 1080x2209 is the D-M5 canvas. The old encoder is reproduced by handing the
/// shared body the oracle checksums, so this asserts the PNG bytes — and thus
/// `content_hash` — are unchanged, not just the individual checksums.
#[test]
fn whole_png_bytes_are_identical_to_the_old_encoder() {
    let width = 1080u32;
    let height = 2209u32;
    let stride = width;
    let mut rng = Rng::new(0xAC05_0001_0000_0003);
    let mut rgba = rng.fill((width as usize) * (height as usize) * 4);
    // A few opaque alpha values so the encoder sees realistic pixels too.
    for i in (3..rgba.len()).step_by(4) {
        rgba[i] = 0xff;
    }

    let new_png = encode_png_rgba(width, height, stride, &rgba);
    let old_png = encode_png_rgba_old(width, height, stride, &rgba, crc32_oracle, adler32_oracle);

    assert_eq!(new_png.len(), old_png.len(), "PNG length changed");
    assert_eq!(new_png, old_png, "PNG bytes changed");
    // 1080*4+1 filtered bytes per row, plus the zlib/PNG framing.
    assert!(new_png.len() > 9_000_000, "unexpectedly small canvas");
    assert_eq!(&new_png[..8], b"\x89PNG\r\n\x1a\n");
}

/// The streamed encoder (P2) writes the same file as the pre-P2 encoder, chunk
/// for chunk, for the real canvas size.
#[test]
fn the_streamed_encoder_writes_the_same_file_as_the_pre_p2_encoder() {
    let width = 1080u32;
    let height = 2209u32;
    let mut rng = Rng::new(0xAC05_0002_0000_0001);
    let mut rgba = rng.fill((width as usize) * (height as usize) * 4);
    for i in (3..rgba.len()).step_by(4) {
        rgba[i] = 0xff;
    }
    let buffered = encode_png_rgba_old(width, height, width, &rgba, crc32, adler32);
    let mut streamed = Vec::new();
    encode_png_rgba_stream(width, height, width, &rgba, |bytes| {
        streamed.extend_from_slice(bytes)
    });
    assert_eq!(streamed.len(), buffered.len(), "PNG length changed");
    assert!(streamed == buffered, "the streamed PNG differs from the pre-P2 PNG");
}
