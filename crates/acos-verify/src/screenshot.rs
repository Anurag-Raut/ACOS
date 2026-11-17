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

/// Convert display-space region masks into the frame's pixel space (W58).
///
/// Masks are **subtractive**: a mask the frame does not cover at all (the status
/// bar band) excludes nothing and is dropped, and a mask that straddles the
/// frame edge is clipped to the frame instead of failing the comparison. Only
/// pixels that exist in the frame can be excluded from a comparison over it.
#[must_use]
pub fn masks_in_frame_space(
    frame: &acos_protocol::FrameMeta,
    masks: &[RegionMask],
) -> Vec<RegionMask> {
    let covered = frame.display_rect();
    let origin = frame.display_origin_px();
    masks
        .iter()
        .filter_map(|mask| {
            let b = mask.bounds;
            let left = b.left().max(covered.left());
            let top = b.top().max(covered.top());
            let right = b.right().min(covered.right());
            let bottom = b.bottom().min(covered.bottom());
            if left >= right || top >= bottom {
                return None;
            }
            Some(RegionMask {
                bounds: acos_protocol::ui::Bounds::new(
                    left - origin[0],
                    top - origin[1],
                    right - origin[0],
                    bottom - origin[1],
                ),
                reason: mask.reason.clone(),
            })
        })
        .collect()
}

/// Per-channel difference above which a pixel counts as changed. Small enough
/// to catch a moved glyph, large enough to ignore deterministic rounding.
const CHANNEL_THRESHOLD: i32 = 2;

/// Compare a baseline PNG with the actual encoded frame.
///
/// `masks` are in **frame** pixels here: callers convert scenario masks from
/// display space with [`masks_in_frame_space`] first (W58).
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
        return Err(format!("baseline is {bw}x{bh} but the frame is {aw}x{ah}"));
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
    let info = reader.next_frame(&mut buffer).map_err(|e| e.to_string())?;
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

    fn app_area_frame() -> acos_protocol::FrameMeta {
        acos_protocol::FrameMeta::new(1, "t", "c", 8, 16, 420, "h", true)
            .with_display_geometry([0, 4], [8, 20])
    }

    #[test]
    fn display_space_masks_are_shifted_onto_the_frame() {
        // A mask at display [2,7][4,9] excludes frame [2,3][4,5].
        let masks = masks_in_frame_space(
            &app_area_frame(),
            &[RegionMask {
                bounds: Bounds::new(2, 7, 4, 9),
                reason: Some("test".to_string()),
            }],
        );
        assert_eq!(masks.len(), 1);
        assert_eq!(masks[0].bounds, Bounds::new(2, 3, 4, 5));
        assert_eq!(masks[0].reason.as_deref(), Some("test"));
    }

    #[test]
    fn a_mask_outside_the_frame_excludes_nothing_and_a_straddling_one_is_clipped() {
        let masks = masks_in_frame_space(
            &app_area_frame(),
            &[
                // Entirely in the status bar: no frame pixels, dropped.
                RegionMask {
                    bounds: Bounds::new(0, 0, 8, 4),
                    reason: None,
                },
                // Straddles the frame top: clipped to frame row 0.
                RegionMask {
                    bounds: Bounds::new(0, 0, 8, 6),
                    reason: None,
                },
            ],
        );
        assert_eq!(masks.len(), 1, "the empty mask must be dropped: {masks:?}");
        assert_eq!(masks[0].bounds, Bounds::new(0, 0, 8, 2));

        // And the masked comparison still works with a shifted mask: the only
        // changed pixel is inside the mask, so a zero tolerance passes.
        let actual = encode(2, 1, &[[255, 0, 0, 255], [0, 0, 0, 255]]);
        let baseline = encode(2, 1, &[[0, 0, 0, 255], [0, 0, 0, 255]]);
        let shifted = masks_in_frame_space(
            &acos_protocol::FrameMeta::new(1, "t", "c", 2, 1, 420, "h", true)
                .with_display_geometry([0, 128], [2, 129]),
            &[RegionMask {
                bounds: Bounds::new(0, 128, 1, 129),
                reason: None,
            }],
        );
        let diff = compare_png(&baseline, &actual, 0.0, &shifted).unwrap();
        assert!(diff.passed);
        assert_eq!(diff.compared_pixels, 1);
    }

    #[test]
    fn size_mismatch_is_an_error_not_a_false_pass() {
        let baseline = encode(2, 1, &[[0, 0, 0, 255], [0, 0, 0, 255]]);
        let actual = encode(1, 1, &[[0, 0, 0, 255]]);
        assert!(compare_png(&baseline, &actual, 1.0, &[]).is_err());
    }
}
