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
    let old_png = encode_png_rgba_with(width, height, stride, &rgba, crc32_oracle, adler32_oracle);

    assert_eq!(new_png.len(), old_png.len(), "PNG length changed");
    assert_eq!(new_png, old_png, "PNG bytes changed");
    // 1080*4+1 filtered bytes per row, plus the zlib/PNG framing.
    assert!(new_png.len() > 9_000_000, "unexpectedly small canvas");
    assert_eq!(&new_png[..8], b"\x89PNG\r\n\x1a\n");
}
