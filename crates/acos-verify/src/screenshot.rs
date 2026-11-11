//! Screenshot comparison for `assert_screenshot`.
//!
//! The comparison runs on the **same** [`crate::backend::SessionObservation`]
//! that carries the UI tree, so a screenshot assertion is inherently bound to
//! one completed frame (`frame_seq`). The advisor rule is enforced by the
//! executor: a screenshot and the tree it is compared against can never come
//! from two different completed traversals.
//!
//! The metric is intentionally simple and explicit: the fraction of *compared*
//! pixels that differ, with a small per-channel threshold for anti-aliasing and
//! a set of documented [`RegionMask`]s excluded (clock, status bar, ...). A
//! masked pixel is removed from both the numerator and the denominator, so the
//! tolerance is a fraction of the pixels this comparison actually claims.

use acos_protocol::assertion::RegionMask;

/// The result of comparing one actual frame to a baseline.
#[derive(Debug, Clone, PartialEq)]
pub struct ImageDiff {
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Pixels in the frame.
    pub total_pixels: u64,
    /// Pixels that were actually compared (masks excluded).
    pub compared_pixels: u64,
    /// Compared pixels whose RGBA differs beyond the per-channel threshold.
    pub changed_pixels: u64,
    /// `changed_pixels / compared_pixels` (`0.0` when nothing was compared).
    pub changed_fraction: f64,
    /// The tolerance the caller supplied.
    pub tolerance: f64,
    /// True when `changed_fraction <= tolerance`.
    pub passed: bool,
    /// Region masks applied, for the artifact.
    pub masked_regions: usize,
}

/// Per-channel difference above which a pixel counts as changed. Small enough
/// to catch a moved glyph, large enough to ignore deterministic rounding.
const CHANNEL_THRESHOLD: i32 = 2;

/// Compare a baseline PNG with the actual encoded frame.
///
/// Returns a structured error string when either image is not a supported PNG;
/// that is a harness/artifact problem, not a failed assertion.
pub fn compare_png(
    baseline: &[u8],
    actual: &[u8],
    tolerance: f64,
    masks: &[RegionMask],
) -> Result<ImageDiff, String> {
    let (bw, bh, baseline_rgba) = decode_rgba8(baseline).map_err(|e| format!("baseline: {e}"))?;
    let (aw, ah, actual_rgba) = decode_rgba8(actual).map_err(|e| format!("actual: {e}"))?;
    if (bw, bh) != (aw, ah) {
        return Err(format!(
            "baseline is {bw}x{bh} but the frame is {aw}x{ah}"
        ));
    }

    let width = bw;
    let height = bh;
    let total_pixels = u64::from(width) * u64::from(height);
    let mut compared_pixels = 0u64;
    let mut changed_pixels = 0u64;
    for y in 0..height {
        for x in 0..width {
            if is_masked(masks, x, y) {
                continue;
            }
            compared_pixels += 1;
            let offset = ((u64::from(y) * u64::from(width) + u64::from(x)) * 4) as usize;
            let baseline_px = &baseline_rgba[offset..offset + 4];
            let actual_px = &actual_rgba[offset..offset + 4];
            if pixel_differs(baseline_px, actual_px) {
                changed_pixels += 1;
            }
        }
    }
    let changed_fraction = if compared_pixels == 0 {
        0.0
    } else {
        changed_pixels as f64 / compared_pixels as f64
    };
    Ok(ImageDiff {
        width,
        height,
        total_pixels,
        compared_pixels,
        changed_pixels,
        changed_fraction,
        tolerance,
        passed: changed_fraction <= tolerance,
        masked_regions: masks.len(),
    })
}

fn pixel_differs(a: &[u8], b: &[u8]) -> bool {
    a.iter()
        .zip(b.iter())
        .any(|(a, b)| (i32::from(*a) - i32::from(*b)).abs() > CHANNEL_THRESHOLD)
}

fn is_masked(masks: &[RegionMask], x: u32, y: u32) -> bool {
    masks.iter().any(|mask| {
        let b = mask.bounds;
        b.is_valid()
            && i64::from(x) >= i64::from(b.left())
            && i64::from(x) < i64::from(b.right())
            && i64::from(y) >= i64::from(b.top())
            && i64::from(y) < i64::from(b.bottom())
    })
}

/// Decode PNG bytes to tightly packed RGBA8. Only 8-bit RGBA/RGB/gray and
/// gray+alpha are accepted; the renderer emits 8-bit RGBA.
pub(crate) fn decode_rgba8(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let mut buffer = vec![0u8; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut buffer)
        .map_err(|e| e.to_string())?;
    if info.bit_depth != png::BitDepth::Eight {
        return Err(format!("unsupported PNG bit depth {:?}", info.bit_depth));
    }
    let data = &buffer[..info.buffer_size()];
    let rgba = match info.color_type {
        png::ColorType::Rgba => data.to_vec(),
        png::ColorType::Rgb => data
            .chunks_exact(3)
            .flat_map(|px| [px[0], px[1], px[2], 255])
            .collect(),
        png::ColorType::Grayscale => data.iter().flat_map(|g| [*g, *g, *g, 255]).collect(),
        png::ColorType::GrayscaleAlpha => data
            .chunks_exact(2)
            .flat_map(|px| [px[0], px[0], px[0], px[1]])
            .collect(),
        other => return Err(format!("unsupported PNG color type {other:?}")),
    };
    Ok((info.width, info.height, rgba))
}

#[cfg(test)]
mod tests {
    use super::*;
    use acos_protocol::ui::Bounds;

    fn encode(width: u32, height: u32, pixels: &[[u8; 4]]) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, width, height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            let flat: Vec<u8> = pixels.iter().flat_map(|p| p.iter().copied()).collect();
            writer.write_image_data(&flat).unwrap();
        }
        bytes
    }

    #[test]
    fn identical_images_pass() {
        let pixels = [[10u8, 20, 30, 255], [40, 50, 60, 255]];
        let png = encode(2, 1, &pixels);
        let diff = compare_png(&png, &png, 0.0, &[]).unwrap();
        assert!(diff.passed);
        assert_eq!(diff.changed_pixels, 0);
    }

    #[test]
    fn changed_pixel_fails_zero_tolerance() {
        let baseline = encode(2, 1, &[[0, 0, 0, 255], [0, 0, 0, 255]]);
        let actual = encode(2, 1, &[[255, 0, 0, 255], [0, 0, 0, 255]]);
        let diff = compare_png(&baseline, &actual, 0.0, &[]).unwrap();
        assert!(!diff.passed);
        assert_eq!(diff.changed_pixels, 1);
        assert_eq!(diff.compared_pixels, 2);
    }

    #[test]
    fn mask_excludes_a_region_from_both_terms() {
        let baseline = encode(2, 1, &[[0, 0, 0, 255], [0, 0, 0, 255]]);
        let actual = encode(2, 1, &[[255, 0, 0, 255], [0, 0, 0, 255]]);
        let masks = vec![RegionMask {
            bounds: Bounds::new(0, 0, 1, 1),
            reason: Some("test".to_string()),
        }];
        let diff = compare_png(&baseline, &actual, 0.0, &masks).unwrap();
        assert!(diff.passed);
        assert_eq!(diff.compared_pixels, 1);
        assert_eq!(diff.changed_pixels, 0);
    }

    #[test]
    fn size_mismatch_is_an_error_not_a_false_pass() {
        let baseline = encode(2, 1, &[[0, 0, 0, 255], [0, 0, 0, 255]]);
        let actual = encode(1, 1, &[[0, 0, 0, 255]]);
        assert!(compare_png(&baseline, &actual, 1.0, &[]).is_err());
    }
}
