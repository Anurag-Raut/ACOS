//! SHOTZ: the MCP image bound. An image in a reply is a preview (`<= max_side`
//! long edge, `<= max_bytes` file) plus the path to the lossless original.

use super::{image_preview, preview_of_png, preview_size, DEFAULT_MAX_SIDE};

/// A real capture: 1080x2400, the ART canvas. Patterned so a downscale is
/// visible in the pixels, and encoded losslessly (stored-block style is what the
/// capture path used to write, so this is the honest "big file" case).
fn big_png(width: u32, height: u32) -> Vec<u8> {
    let mut rgba = vec![0u8; (width as usize) * (height as usize) * 4];
    // Deterministic xorshift noise: incompressible, so the file is the full
    // ~4 bytes per pixel a stored-block capture used to be.
    let mut state = 0x2545_f491_4f6c_dd1du64;
    for (i, byte) in rgba.iter_mut().enumerate() {
        if i % 4 == 3 {
            *byte = 0xff;
            continue;
        }
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        *byte = (state.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 32) as u8;
    }
    encode(&width, &height, &rgba, png::Compression::Default)
}

fn encode(width: &u32, height: &u32, rgba: &[u8], level: png::Compression) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, *width, *height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(level);
        let mut writer = encoder.write_header().expect("header");
        writer.write_image_data(rgba).expect("data");
        writer.finish().expect("finish");
    }
    out
}

fn decode(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes.to_vec()));
    let mut reader = decoder.read_info().expect("info");
    let mut buffer = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buffer).expect("frame");
    buffer.truncate(info.buffer_size());
    (info.width, info.height, buffer)
}

/// A real 1080x2400 ART capture (the repo's losslessly-recompressed frame).
fn real_frame() -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/art-frames/counter-1080x2400.png");
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The whole point of SHOTZ: a full frame is inlined as a bounded preview, not
/// at its 1080x2400 size.
#[test]
fn a_full_frame_is_inlined_as_a_bounded_preview() {
    let png = real_frame();
    let max_bytes = 512 * 1024;
    let preview = preview_of_png(&png, DEFAULT_MAX_SIDE, max_bytes).expect("preview");
    assert!(preview.scaled, "a 2400 px-tall frame must be downscaled");
    assert_eq!(
        preview.width.max(preview.height),
        DEFAULT_MAX_SIDE,
        "the long side is pinned to the cap"
    );
    // Aspect ratio kept (1080x2400 -> 576x1280).
    assert_eq!((preview.width, preview.height), (576, 1280));
    assert!(
        preview.data.len() <= max_bytes,
        "preview is {} B, cap {max_bytes}",
        preview.data.len()
    );
    assert_eq!((preview.source_width, preview.source_height), (1080, 2400));
    assert_eq!(preview.source_bytes, png.len());
    // The preview is a real PNG of the announced size.
    let (w, h, pixels) = decode(&preview.data);
    assert_eq!((w, h), (preview.width, preview.height));
    assert_eq!(pixels.len(), (w as usize) * (h as usize) * 4);
    // Not a blank image: the downscale keeps the frame's content.
    assert!(
        pixels.chunks_exact(4).filter(|p| p[3] > 0).count() > (w * h) as usize / 2,
        "the preview is mostly transparent"
    );
    let meta = preview.meta();
    assert_eq!(meta["preview"]["scaled"], serde_json::json!(true));
    assert_eq!(
        meta["preview"]["bytes"],
        serde_json::json!(preview.data.len())
    );
}

/// High-entropy pixels (the worst case: a full 4 bytes per pixel) still inline as
/// *something* bounded instead of a multi-MB blob.
#[test]
fn a_noisy_frame_is_bounded_by_the_byte_cap() {
    let png = big_png(1080, 2400);
    assert!(
        png.len() > 1_000_000,
        "the fixture must be big: {}",
        png.len()
    );
    let max_bytes = 512 * 1024;
    let preview = preview_of_png(&png, DEFAULT_MAX_SIDE, max_bytes).expect("a bounded preview");
    assert!(preview.scaled);
    assert!(
        preview.data.len() <= max_bytes,
        "preview is {} B, cap {max_bytes}",
        preview.data.len()
    );
    assert!(preview.width <= DEFAULT_MAX_SIDE && preview.height <= DEFAULT_MAX_SIDE);
    let (w, h, _) = decode(&preview.data);
    assert_eq!((w, h), (preview.width, preview.height));
    // Aspect ratio survives the extra halving (within a pixel of rounding).
    let want = 1080.0 / 2400.0;
    let got = w as f64 / h as f64;
    assert!((want - got).abs() < 0.01, "aspect {got} vs {want}");
}

/// A capture that already fits is inlined byte for byte: no re-encode, no
/// quality question, and `scaled: false` says so.
#[test]
fn a_small_image_is_inlined_losslessly_and_unscaled() {
    let mut rgba = vec![0u8; 40 * 30 * 4];
    for (i, byte) in rgba.iter_mut().enumerate() {
        *byte = (i % 251) as u8;
    }
    let png = encode(&40, &30, &rgba, png::Compression::Fast);
    let preview = preview_of_png(&png, DEFAULT_MAX_SIDE, 512 * 1024).expect("preview");
    assert!(!preview.scaled);
    assert_eq!((preview.width, preview.height), (40, 30));
    assert_eq!(preview.data, png, "a fitting file must not be re-encoded");
    assert_eq!(preview.source_bytes, png.len());
}

/// `ACOS_MCP_MAX_IMAGE_BYTES` still means what it says: nothing larger is
/// inlined, and the caller then emits only a link.
#[test]
fn an_impossible_cap_yields_no_inline_image() {
    let png = big_png(400, 400);
    assert!(preview_of_png(&png, DEFAULT_MAX_SIDE, 64).is_none());
}

/// A file that is not a PNG is linked, never inlined.
#[test]
fn a_non_png_file_is_not_inlined() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.cache/shotz-mcp-tests");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("not-an-image-{}.png", std::process::id()));
    std::fs::write(&path, b"not a png").unwrap();
    assert!(image_preview(&path, DEFAULT_MAX_SIDE, 512 * 1024).is_none());
    let jpeg = dir.join(format!("x-{}.jpg", std::process::id()));
    std::fs::write(&jpeg, b"\xff\xd8\xff").unwrap();
    assert!(image_preview(&jpeg, DEFAULT_MAX_SIDE, 512 * 1024).is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

/// The preview size rule, including the "already smaller" and degenerate cases.
#[test]
fn preview_size_keeps_the_aspect_ratio() {
    assert_eq!(preview_size(1080, 2400, 1280), (576, 1280));
    assert_eq!(preview_size(2400, 1080, 1280), (1280, 576));
    assert_eq!(preview_size(300, 300, 1280), (300, 300));
    assert_eq!(preview_size(0, 0, 1280), (0, 0));
}
