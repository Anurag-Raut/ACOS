//! P2: `content_hash` is the MD5 of the frame's PNG bytes.
//!
//! The launcher stopped forking `md5sum` (9.6 ms of one warm screenshot's 30 ms,
//! `results/art-runtime/D-M5.md` rank 4) and now streams the bytes it writes
//! through an in-process MD5. These tests pin the two things that make that
//! safe: the published value is byte-identical to `md5sum`'s on a real 9.5 MB
//! frame, and the streamed encoder emits exactly the bytes the buffered encoder
//! used to emit.

use super::{encode_png_rgba, encode_png_rgba_stream};
use acos_render::md5::{md5_hex, Md5};
use std::path::{Path, PathBuf};

/// Scratch under the worktree `.cache/` (never `/tmp`).
fn scratch_dir(name: &str) -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.cache/p2-tests");
    let dir = root.join(format!(
        "{}-{}-{name}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A frame of the size the counter app produces (1080x2209 RGBA), patterned so
/// the pixels are not all one value.
fn counter_frame() -> (u32, u32, Vec<u8>) {
    const WIDTH: u32 = 1080;
    const HEIGHT: u32 = 2209;
    let stride = WIDTH * 4;
    let mut rgba = vec![0u8; stride as usize * HEIGHT as usize];
    for y in 0..HEIGHT as usize {
        for x in 0..WIDTH as usize {
            let i = y * stride as usize + x * 4;
            rgba[i] = (x % 251) as u8;
            rgba[i + 1] = (y % 241) as u8;
            rgba[i + 2] = ((x ^ y) % 233) as u8;
            rgba[i + 3] = 0xff;
        }
    }
    (WIDTH, HEIGHT, rgba)
}

/// The streamed encoder hands over exactly the bytes the buffered encoder builds.
#[test]
fn streamed_encoder_is_byte_identical_to_the_buffered_encoder() {
    let (width, height, rgba) = counter_frame();
    let buffered = encode_png_rgba(width, height, width * 4, &rgba);
    let mut streamed = Vec::new();
    encode_png_rgba_stream(width, height, width * 4, &rgba, |bytes| {
        streamed.extend_from_slice(bytes)
    });
    assert_eq!(streamed.len(), buffered.len());
    assert_eq!(md5_hex(&streamed), md5_hex(&buffered));
    assert!(streamed == buffered, "the PNG bytes changed");
}

/// `content_hash` for a real 9.5 MB frame is what `md5sum` prints for the same
/// file, character for character.
#[test]
fn content_hash_matches_md5sum_on_a_real_frame() {
    let (width, height, rgba) = counter_frame();
    let png = encode_png_rgba(width, height, width * 4, &rgba);
    // The real frame is ~9.5 MB: the test is only meaningful at that size.
    assert!(
        png.len() > 9_000_000,
        "expected a ~9.5 MB PNG, got {} bytes",
        png.len()
    );
    let dir = scratch_dir("md5sum");
    let path = dir.join("frame.png");
    std::fs::write(&path, &png).unwrap();

    let ours = md5_hex(&png);
    assert_eq!(ours.len(), 32);
    let output = std::process::Command::new("md5sum")
        .arg(&path)
        .output()
        .expect("the host must provide md5sum for this comparison");
    assert!(output.status.success(), "md5sum failed: {output:?}");
    let theirs = String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .next()
        .expect("md5sum prints a digest")
        .to_string();
    assert_eq!(ours, theirs, "content_hash must not change");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The streaming hasher gives the same digest however the file write splits it.
#[test]
fn streaming_hash_of_a_written_file_matches_the_whole_buffer() {
    let (width, height, rgba) = counter_frame();
    let png = encode_png_rgba(width, height, width * 4, &rgba);
    let mut hasher = Md5::new();
    for chunk in png.chunks(1 << 20) {
        hasher.update(chunk);
    }
    assert_eq!(hasher.finish_hex(), md5_hex(&png));
}
