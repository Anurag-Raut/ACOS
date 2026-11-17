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
        0usize, 1, 2, 3, 7, 8, 9, 255, 256, 4096, 5551, 5552, 5553, 5554, 11_104, 65_535,
        1_000_003, 9_500_000,
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

/// SHOTZ: the A/B baseline the new encoder is measured against must be the
/// encoder v0.2.0 shipped. `encode_png_stored_stream` (the production path
/// before ADR-0027 #6, kept under `cfg(test)`) is compared against the
/// independent `encode_png_rgba_old` oracle above: same bytes, so the "before"
/// size and time in `results/handoff/SHOTZ.md` are the real ones.
#[test]
fn the_stored_baseline_is_the_encoder_v0_2_0_shipped() {
    for (width, height, stride) in [(17u32, 5u32, 23u32), (1080, 64, 1080), (0, 2, 0)] {
        let mut rng = Rng::new(0xAC05_0001_0000_0004);
        let rgba = rng.fill((stride as usize) * 4 * (height as usize));
        let mut shipped = Vec::new();
        encode_png_stored_stream(width, height, stride, &rgba, |bytes| {
            shipped.extend_from_slice(bytes)
        });
        let oracle = encode_png_rgba_old(width, height, stride, &rgba, crc32, adler32);
        assert_eq!(
            shipped, oracle,
            "the retained baseline diverged for {width}x{height} stride {stride}"
        );
    }
}

/// `None` is not a candidate any more: the default encoder writes real deflate,
/// so a real-sized frame must be far smaller than its 10.4 MB pixel buffer.
#[test]
fn a_high_entropy_canvas_is_never_materially_worse_than_stored() {
    let width = 1080u32;
    let height = 2209u32;
    let mut rng = Rng::new(0xAC05_0001_0000_0005);
    // Random pixels are the worst case for deflate; even so it beats stored.
    let mut rgba = rng.fill((width as usize) * (height as usize) * 4);
    for i in (3..rgba.len()).step_by(4) {
        rgba[i] = 0xff;
    }
    let png = encode_png_rgba(width, height, width, &rgba);
    let stored = {
        let mut out = Vec::new();
        encode_png_stored_stream(width, height, width, &rgba, |bytes| {
            out.extend_from_slice(bytes)
        });
        out
    };
    assert!(
        stored.len() > 9_000_000,
        "the stored baseline is still ~9.5 MB"
    );
    // Pure noise is the one input fixed-Huffman deflate cannot shrink: the
    // encoder falls back to the stored layout, so the weight never grows past
    // the pre-SHOTZ file (the 12 bytes per IDAT chunk are the only difference).
    assert!(
        png.len() <= stored.len() + 1024,
        "the encoder must never be materially worse than stored ({} vs {})",
        png.len(),
        stored.len()
    );
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
}

/// A stride wider than the row, and a pixel buffer that is short of the last
/// rows, must still produce a readable PNG whose rows are the zero-padded
/// rows the stored encoder emitted.
#[test]
fn stride_padding_and_short_rows_still_decode_to_the_padded_rows() {
    let mut rng = Rng::new(0xAC05_0003_0000_0002);
    for (width, height, stride, bytes) in [
        (17u32, 5u32, 23u32, 23 * 4 * 5),
        (17, 5, 23, 23 * 4 * 5 - 13),
        (1, 3, 1, 1 * 4 * 3 - 2),
        (2, 1, 3, 3 * 4),
        // No zero-size case: PNG has no 0-pixel image, and C1 never publishes a
        // zero-dimension buffer (`capture_c1` returns None without a frame), so
        // the encoder is never asked for one.
    ] {
        let rgba = rng.fill(bytes);
        let png = encode_png_rgba(width, height, stride, &rgba);
        let decoded = decode_png(&png);
        assert_eq!(
            (decoded.0, decoded.1),
            (width, height),
            "dimensions changed for {width}x{height} stride {stride}"
        );
        let row = width as usize * 4;
        let stride_bytes = stride as usize * 4;
        for y in 0..height as usize {
            let start = y * stride_bytes;
            let end = start + row;
            let mut want = vec![0u8; row];
            if end <= rgba.len() {
                want.copy_from_slice(&rgba[start..end]);
            } else if start < rgba.len() {
                let have = rgba.len() - start;
                want[..have].copy_from_slice(&rgba[start..start + have]);
            }
            let got = &decoded.2[y * row..(y + 1) * row];
            assert_eq!(
                got,
                &want[..],
                "row {y} differs for {width}x{height} stride {stride}"
            );
        }
    }
}

/// The streamed encoder (P2) hands over exactly the bytes the buffered encoder
/// builds, so `content_hash` cannot depend on how the sink splits the file.
#[test]
fn the_streamed_encoder_writes_the_same_file_as_the_buffered_encoder() {
    let width = 1080u32;
    let height = 64u32;
    let mut rng = Rng::new(0xAC05_0002_0000_0002);
    let rgba = rng.fill((width as usize) * (height as usize) * 4);
    let buffered = encode_png_rgba(width, height, width, &rgba);
    let mut streamed = Vec::new();
    encode_png_rgba_stream(width, height, width, &rgba, |bytes| {
        streamed.extend_from_slice(bytes)
    });
    assert_eq!(streamed.len(), buffered.len(), "PNG length changed");
    assert!(
        streamed == buffered,
        "the streamed PNG differs from the buffered one"
    );
    // Deterministic: a second encode is byte-identical (the capture path hashes
    // exactly these bytes, and the same frame must hash the same).
    assert_eq!(encode_png_rgba(width, height, width, &rgba), buffered);
}

/// Decode a PNG written by the capture encoder: (width, height, RGBA pixels).
fn decode_png(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes.to_vec()));
    let mut reader = decoder.read_info().expect("png info");
    let mut buffer = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buffer).expect("png frame");
    assert_eq!(info.color_type, png::ColorType::Rgba, "expected RGBA");
    assert_eq!(info.bit_depth, png::BitDepth::Eight);
    buffer.truncate(info.buffer_size());
    (info.width, info.height, buffer)
}
