//! SHOTZ (ADR-0027 #6): an image in an MCP reply is a *bounded preview*.
//!
//! A capture is a full 1080x2400 UI frame. Inlining it at full resolution spends
//! an agent's context on pixels it cannot read: the tool reply is base64 (4/3 of
//! the file) and the lossless file is already on disk, named by the evidence
//! path. So an image content block carries a preview whose long side is at most
//! [`DEFAULT_MAX_SIDE`] and whose file is at most
//! `ACOS_MCP_MAX_IMAGE_BYTES`, and the reply always names the lossless file.
//!
//! Lossy JPEG is not used: no JPEG encoder is available offline in this
//! workspace (`jpeg-decoder` is decode-only), so the preview is a PNG. A PNG
//! preview of a real capture is a few tens of KB, which is bounded and readable.

use std::path::Path;

/// Long edge of an inlined preview, in pixels (`ACOS_MCP_PREVIEW_MAX_SIDE`).
pub const DEFAULT_MAX_SIDE: u32 = 1280;

/// Bytes of an inlined image (`ACOS_MCP_MAX_IMAGE_BYTES`).
pub const DEFAULT_MAX_BYTES: usize = 512 * 1024;

/// The inline caps in force for this process: the environment, else the
/// defaults above. [`crate::server::ServerConfig`] uses the same two variables,
/// so a reply bounded inside a backend and one bounded by the server agree.
pub fn inline_caps() -> (u32, usize) {
    let side = std::env::var("ACOS_MCP_PREVIEW_MAX_SIDE")
        .ok()
        .and_then(|value| value.trim().parse::<u32>().ok())
        .map(|value| value.max(1))
        .unwrap_or(DEFAULT_MAX_SIDE);
    let bytes = std::env::var("ACOS_MCP_MAX_IMAGE_BYTES")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(DEFAULT_MAX_BYTES);
    (side, bytes)
}

/// One image ready to inline as MCP image content.
#[derive(Debug, Clone)]
pub struct Preview {
    /// MIME type of `data` (always `image/png` today).
    pub mime: &'static str,
    /// Encoded bytes to base64 into the reply.
    pub data: Vec<u8>,
    /// Pixel size of the preview.
    pub width: u32,
    /// Pixel size of the preview.
    pub height: u32,
    /// Pixel size of the file the reply also links to.
    pub source_width: u32,
    /// Pixel size of the file the reply also links to.
    pub source_height: u32,
    /// File size of the lossless original.
    pub source_bytes: usize,
    /// Whether `data` is a downscaled variant of the original.
    pub scaled: bool,
}

impl Preview {
    /// `_meta` block describing the preview, so an agent can tell a downscaled
    /// preview from the real thing (a silent downscale would read as exact).
    pub fn meta(&self) -> serde_json::Value {
        serde_json::json!({
            "preview": {
                "scaled": self.scaled,
                "width": self.width,
                "height": self.height,
                "bytes": self.data.len(),
                "source_width": self.source_width,
                "source_height": self.source_height,
                "source_bytes": self.source_bytes,
            }
        })
    }
}

/// Read an image file and return the block to inline, or `None` when it is not
/// an image this server can decode.
///
/// A file that already fits (`<= max_bytes`, long side `<= max_side`) is inlined
/// byte for byte — lossless, no re-encode. Anything larger is decoded,
/// downscaled so its long side is at most `max_side`, and re-encoded as PNG; if
/// the result still exceeds `max_bytes` nothing is inlined and the reply carries
/// only the path.
pub fn image_preview(path: &Path, max_side: u32, max_bytes: usize) -> Option<Preview> {
    let bytes = std::fs::read(path).ok()?;
    let lower = path.to_string_lossy().to_ascii_lowercase();
    if lower.ends_with(".jpg") || lower.ends_with(".jpeg") || lower.ends_with(".webp") {
        // Decodable by a browser but not by this server: link it, never inline
        // an unbounded blob.
        return None;
    }
    preview_of_png(&bytes, max_side, max_bytes)
}

/// The same, for PNG bytes already in memory (the Flutter backend's screenshot).
pub fn preview_of_png(bytes: &[u8], max_side: u32, max_bytes: usize) -> Option<Preview> {
    let (width, height, pixels) = decode_rgba(bytes)?;
    let fits = bytes.len() <= max_bytes && width.max(height) <= max_side;
    if fits {
        return Some(Preview {
            mime: "image/png",
            data: bytes.to_vec(),
            width,
            height,
            source_width: width,
            source_height: height,
            source_bytes: bytes.len(),
            scaled: false,
        });
    }
    // Downscale to the side cap; if that is still bigger than the byte cap (a
    // frame of high-entropy pixels), halve the side until it fits. Something
    // small and honest always beats nothing, and nothing here is ever multi-MB.
    let mut side = max_side;
    loop {
        let (pw, ph) = preview_size(width, height, side);
        let scaled_pixels = if (pw, ph) == (width, height) {
            pixels.clone()
        } else {
            downscale(&pixels, width, height, pw, ph)
        };
        let data = encode_png(pw, ph, &scaled_pixels);
        if data.len() <= max_bytes {
            return Some(Preview {
                mime: "image/png",
                data,
                width: pw,
                height: ph,
                source_width: width,
                source_height: height,
                source_bytes: bytes.len(),
                scaled: (pw, ph) != (width, height),
            });
        }
        if side <= 160 {
            return None;
        }
        side /= 2;
    }
}

/// The preview size for a frame: the long side is pinned to `max_side` (and the
/// aspect ratio kept), unless the frame is already smaller.
fn preview_size(width: u32, height: u32, max_side: u32) -> (u32, u32) {
    let longest = width.max(height);
    if longest == 0 || longest <= max_side {
        return (width, height);
    }
    let scale = max_side as f64 / longest as f64;
    let w = ((width as f64 * scale).round() as u32).max(1);
    let h = ((height as f64 * scale).round() as u32).max(1);
    (w.min(max_side), h.min(max_side))
}

/// Box-average `pixels` (RGBA8, straight alpha) down to `pw`x`ph`.
fn downscale(pixels: &[u8], width: u32, height: u32, pw: u32, ph: u32) -> Vec<u8> {
    let mut out = vec![0u8; (pw as usize) * (ph as usize) * 4];
    for oy in 0..ph as usize {
        let y0 = oy * height as usize / ph as usize;
        let y1 = ((oy + 1) * height as usize / ph as usize).max(y0 + 1).min(height as usize);
        for ox in 0..pw as usize {
            let x0 = ox * width as usize / pw as usize;
            let x1 = ((ox + 1) * width as usize / pw as usize)
                .max(x0 + 1)
                .min(width as usize);
            let mut sum = [0u64; 4];
            let mut count = 0u64;
            for y in y0..y1 {
                let row = y * width as usize * 4;
                for x in x0..x1 {
                    let i = row + x * 4;
                    for c in 0..4 {
                        sum[c] += u64::from(pixels[i + c]);
                    }
                    count += 1;
                }
            }
            let o = (oy * pw as usize + ox) * 4;
            for c in 0..4 {
                out[o + c] = (sum[c] / count.max(1)) as u8;
            }
        }
    }
    out
}

/// Decode a PNG to 8-bit RGBA.
fn decode_rgba(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder.read_info().ok()?;
    let mut buffer = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buffer).ok()?;
    let pixels = &buffer[..info.buffer_size()];
    let (width, height) = (info.width, info.height);
    let rgba = match info.color_type {
        png::ColorType::Rgba => pixels.to_vec(),
        png::ColorType::Rgb => pixels
            .chunks_exact(3)
            .flat_map(|p| [p[0], p[1], p[2], 0xff])
            .collect(),
        png::ColorType::GrayscaleAlpha => pixels
            .chunks_exact(2)
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        png::ColorType::Grayscale => pixels.iter().flat_map(|&v| [v, v, v, 0xff]).collect(),
        png::ColorType::Indexed => return None,
    };
    Some((width, height, rgba))
}

/// Encode RGBA8 as a PNG: fast deflate, Sub filter. A preview is throwaway, so
/// it trades bytes for time the other way round from the capture path.
fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgba.len() / 8 + 1024);
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Fast);
        encoder.set_filter(png::FilterType::Sub);
        encoder.set_adaptive_filter(png::AdaptiveFilterType::NonAdaptive);
        if let Ok(mut writer) = encoder.write_header() {
            let _ = writer.write_image_data(rgba);
        }
    }
    out
}

#[cfg(test)]
#[path = "preview_tests.rs"]
mod preview_tests;
