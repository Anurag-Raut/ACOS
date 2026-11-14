//! SHOTZ (ADR-0027 #6): the capture PNG is compressed *losslessly*.
//!
//! `crates/acos-art`'s capture encoder used to write zlib stored blocks with
//! filter 0: a 1080x2400 frame was exactly 10 371 258 B, so every screenshot
//! wrote and MD5'd 9.9 MB. The default is now real deflate (fdeflate) over a
//! per-row adaptive Sub/Up filter: same pixels, ~80 KB, and a *cheaper* capture
//! because the write+hash shrinks with the file.
//!
//! These two tests pin the part that matters for the pixel gates
//! (`w54-dialog-pixels`, `w42-back-frame`, `T03-render-good/wrong`): the file is
//! a PNG whose decoded pixels are exactly the frame the compositor published.

use super::{encode_png_rgba, encode_png_stored_stream};
use std::path::PathBuf;

/// The frames are 1080x2400 ART captures, re-compressed losslessly for the repo
/// (a stored-block copy is 10.4 MB each; git is not a frame dump).
const FIXTURES: [&str; 2] = [
    "counter-1080x2400.png",
    "dialog-menu-1080x2400.png",
];

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/art-frames")
}

fn decode_png(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes.to_vec()));
    let mut reader = decoder.read_info().expect("png info");
    let mut buffer = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buffer).expect("png frame");
    assert_eq!(info.color_type, png::ColorType::Rgba, "expected RGBA");
    assert_eq!(info.bit_depth, png::BitDepth::Eight, "expected 8-bit");
    buffer.truncate(info.buffer_size());
    (info.width, info.height, buffer)
}

/// Every real frame: encode, decode, compare pixels. Lossless or nothing.
#[test]
fn real_frames_round_trip_pixel_exact() {
    for name in FIXTURES {
        let path = fixture_dir().join(name);
        let source = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let (width, height, pixels) = decode_png(&source);
        assert_eq!((width, height), (1080, 2400), "{name}: not the ART canvas");
        let png = encode_png_rgba(width, height, width, &pixels);
        let (pw, ph, decoded) = decode_png(&png);
        assert_eq!((pw, ph), (width, height), "{name}: dimensions changed");
        assert_eq!(decoded.len(), pixels.len(), "{name}: pixel count changed");
        assert!(
            decoded == pixels,
            "{name}: the encoder is not lossless ({} pixels differ)",
            decoded
                .iter()
                .zip(&pixels)
                .filter(|(a, b)| a != b)
                .count()
                / 4
        );
        assert!(
            png.len() < 1_000_000,
            "{name}: expected a compressed PNG under 1 MB, got {} B",
            png.len()
        );
        // A real UI frame compresses far below the 1 MB target: this is the
        // size assertion that would catch a silent return to stored blocks.
        assert!(
            png.len() < pixels.len() / 20,
            "{name}: {} B for {} B of pixels is not compression",
            png.len(),
            pixels.len()
        );
    }
}

/// The stored-block baseline is still byte-for-byte what v0.2.0 wrote, so the
/// "before" numbers below are the shipped ones; the new file is ~130x smaller
/// on the same pixels.
#[test]
fn compressed_frames_are_much_smaller_than_the_stored_baseline() {
    for name in FIXTURES {
        let path = fixture_dir().join(name);
        let source = std::fs::read(&path).expect("fixture");
        let (width, height, pixels) = decode_png(&source);
        let mut stored = Vec::new();
        encode_png_stored_stream(width, height, width, &pixels, |bytes| {
            stored.extend_from_slice(bytes)
        });
        // 1080*4+1 bytes per row plus framing, as shipped.
        assert_eq!(stored.len(), 10_371_258, "{name}: baseline changed");
        let png = encode_png_rgba(width, height, width, &pixels);
        assert!(
            png.len() * 50 < stored.len(),
            "{name}: {} B vs the baseline's {} B",
            png.len(),
            stored.len()
        );
    }
}

/// The measurement behind `results/handoff/SHOTZ.md`, run by hand:
///
/// ```text
/// SHOTZ_FRAMES=results/parity/w198a \
///   cargo test --release --offline -p acos-art \
///   png_encoder_candidates_on_real_frames -- --ignored --nocapture
/// ```
///
/// It writes nothing and asserts only losslessness; the table is the output.
#[test]
#[ignore = "measurement: prints the size/time table for the handoff, needs $SHOTZ_FRAMES"]
fn png_encoder_candidates_on_real_frames() {
    use std::time::Instant;
    let dir = std::env::var("SHOTZ_FRAMES").unwrap_or_else(|_| "results/parity/w198a".into());
    let mut names: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{dir}: {e}"))
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "png"))
        .collect();
    names.sort();
    println!(
        "{:<22} {:>9} {:>11} {:>10} {:>10} {:>8}",
        "frame", "stored_B", "compressed_B", "stored_ms", "comp_ms", "size"
    );
    for path in names {
        let source = std::fs::read(&path).expect("frame");
        let (width, height, pixels) = decode_png(&source);
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        let mut stored = (u128::MAX, 0usize);
        for _ in 0..5 {
            let t = Instant::now();
            let mut out = Vec::new();
            encode_png_stored_stream(width, height, width, &pixels, |bytes| {
                out.extend_from_slice(bytes)
            });
            stored = (stored.0.min(t.elapsed().as_micros()), out.len());
        }
        let mut compressed = (u128::MAX, 0usize);
        for _ in 0..5 {
            let t = Instant::now();
            let out = encode_png_rgba(width, height, width, &pixels);
            compressed = (compressed.0.min(t.elapsed().as_micros()), out.len());
        }
        println!(
            "{:<22} {:>9} {:>11} {:>10.2} {:>10.2} {:>7.1}%",
            name,
            stored.1,
            compressed.1,
            stored.0 as f64 / 1000.0,
            compressed.0 as f64 / 1000.0,
            100.0 * compressed.1 as f64 / stored.1 as f64
        );
        assert!(
            compressed.1 < stored.1,
            "{name}: the compressed frame is not smaller"
        );
        let (_, _, decoded) = decode_png(&encode_png_rgba(width, height, width, &pixels));
        assert!(decoded == pixels, "{name}: not lossless");
    }
}
