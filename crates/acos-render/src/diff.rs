//! Numeric frame comparison: per-pixel error + structural similarity.
//!
//! `acos frame-diff` and the M3 render tests use this to score an ACOS-rendered
//! frame against an `art-reference` capture. It is deliberately *not* a
//! tolerance gate by default: it reports the real numbers (MAE, RMSE, max
//! error, PSNR, differing-pixel fraction, changed bounding box, mean/worst
//! windowed SSIM) and lets the caller decide. No metric is fabricated and no
//! image is silently resized or cropped — mismatched dimensions are a
//! structured error.
//!
//! ## What each number means
//!
//! * **Per-pixel**: per-channel and overall mean absolute error / RMSE / max
//!   error over the un-premultiplied RGB channels. Alpha is reported too.
//! * **Differing pixels**: a pixel counts as differing when the largest RGB
//!   channel delta exceeds `pixel_threshold`. The bounding box of those pixels
//!   localises the mismatch (e.g. the status bar vs the content area).
//! * **Structural (SSIM)**: canonical Wang et al. SSIM on luma with an
//!   `ssim_window` Gaussian window (sigma = 1.5), evaluated only where the
//!   full window fits (valid convolution). `global_ssim` is the single-window
//!   SSIM over the whole image. A perfect match is 1.0.
//! * **Blocks**: a coarse structural pass over `block_size` tiles, reporting the
//!   fraction of tiles whose mean RGB error exceeds the pixel threshold. This
//!   catches "the whole widget moved / is missing" even when SSIM is locally
//!   high.

use serde::Serialize;

use crate::color::unpack_unpremul;
use crate::error::{RenderError, RenderResult};
use crate::image::DecodedImage;

/// An exclusive `[x0, y0, x1, y1]` rectangle excluded from the comparison.
pub type MaskRect = [u32; 4];

/// Options for [`diff`].
#[derive(Clone, Debug)]
pub struct DiffOptions {
    /// A pixel "differs" when its largest RGB channel delta exceeds this.
    pub pixel_threshold: u8,
    /// SSIM window side (forced odd, minimum 3). The standard is 11.
    pub ssim_window: u32,
    /// Coarse structural block side in pixels.
    pub block_size: u32,
    /// Rectangles excluded from every metric. Used for the parts of a frame the
    /// ACOS window cannot control (the Android status/navigation bars); masked
    /// pixels contribute to no per-pixel statistic, no block, and no SSIM
    /// window that intersects them.
    pub mask: Vec<MaskRect>,
}

impl Default for DiffOptions {
    fn default() -> Self {
        Self {
            pixel_threshold: 2,
            ssim_window: 11,
            block_size: 32,
            mask: Vec::new(),
        }
    }
}

/// Error statistics for one channel (0..=255 scale).
#[derive(Clone, Copy, Debug, Serialize, PartialEq)]
pub struct ChannelStats {
    /// Mean absolute error over every pixel.
    pub mean_abs_error: f64,
    /// Root-mean-square error over every pixel.
    pub rmse: f64,
    /// Largest absolute error at any pixel.
    pub max_abs_error: u32,
}

/// Coarse structural summary over `block_size` tiles.
#[derive(Clone, Copy, Debug, Serialize, PartialEq)]
pub struct BlockStats {
    /// Tile side in pixels.
    pub block_size: u32,
    /// Tiles across.
    pub blocks_x: u32,
    /// Tiles down.
    pub blocks_y: u32,
    /// Tiles whose mean RGB error exceeds the pixel threshold.
    pub changed_blocks: u64,
    /// `changed_blocks / total_blocks`.
    pub changed_fraction: f64,
    /// Largest per-tile mean RGB error.
    pub max_block_mean_abs_error: f64,
}

/// The full comparison result.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct DiffReport {
    pub width: u32,
    pub height: u32,
    pub pixel_count: u64,
    /// True when every channel of every pixel is identical.
    pub identical: bool,
    /// Overall RGB mean absolute error (mean of the three channels).
    pub mean_abs_error: f64,
    /// Overall RGB RMSE.
    pub rmse: f64,
    /// Largest RGB channel error.
    pub max_abs_error: u32,
    /// Peak signal-to-noise ratio in dB; `None` when the images are identical.
    pub psnr_db: Option<f64>,
    pub red: ChannelStats,
    pub green: ChannelStats,
    pub blue: ChannelStats,
    pub alpha: ChannelStats,
    pub luma: ChannelStats,
    pub pixel_threshold: u8,
    pub differing_pixels: u64,
    pub differing_fraction: f64,
    /// Inclusive-exclusive `[x0, y0, x1, y1]` box around differing pixels.
    pub changed_bbox: Option<[u32; 4]>,
    /// Mean windowed SSIM over luma (1.0 = identical).
    pub mean_ssim: f64,
    /// Worst windowed SSIM value anywhere.
    pub min_ssim: f64,
    /// Whole-image single-window SSIM over luma.
    pub global_ssim: f64,
    /// The SSIM window side actually used.
    pub ssim_window: u32,
    pub blocks: BlockStats,
    /// Pixels excluded by [`DiffOptions::mask`].
    pub masked_pixels: u64,
    /// Pixels actually compared (`pixel_count - masked_pixels`).
    pub compared_pixels: u64,
    /// The mask rectangles used, after clamping to the image.
    pub mask: Vec<MaskRect>,
}

/// Compare two decoded images pixel-for-pixel and structurally.
///
/// Both images must have identical dimensions; this never resizes implicitly.
pub fn diff(a: &DecodedImage, b: &DecodedImage, opts: &DiffOptions) -> RenderResult<DiffReport> {
    if a.width != b.width || a.height != b.height {
        return Err(RenderError::InvalidArgument {
            msg: format!(
                "image dimensions differ: {}x{} vs {}x{}",
                a.width, a.height, b.width, b.height
            ),
        });
    }
    let w = a.width as usize;
    let h = a.height as usize;
    let n = w * h;
    if n == 0 {
        return Err(RenderError::InvalidArgument {
            msg: "zero-pixel image".to_string(),
        });
    }
    if a.pixels.len() != n || b.pixels.len() != n {
        return Err(RenderError::InvalidArgument {
            msg: format!(
                "pixel buffer length mismatch: {} and {} for {w}x{h}",
                a.pixels.len(),
                b.pixels.len()
            ),
        });
    }

    // Excluded regions (system UI). Build a per-pixel predicate and clamp the
    // reported rectangles to the image.
    let mut masked = vec![false; n];
    let mut mask = Vec::with_capacity(opts.mask.len());
    for &r in &opts.mask {
        let x0 = (r[0] as usize).min(w);
        let y0 = (r[1] as usize).min(h);
        let x1 = (r[2] as usize).min(w);
        let y1 = (r[3] as usize).min(h);
        if x0 >= x1 || y0 >= y1 {
            continue;
        }
        for y in y0..y1 {
            let row = y * w;
            masked[row + x0..row + x1].fill(true);
        }
        mask.push([x0 as u32, y0 as u32, x1 as u32, y1 as u32]);
    }
    let masked_pixels = masked.iter().filter(|m| **m).count() as u64;
    let compared = n as u64 - masked_pixels;
    if compared == 0 {
        return Err(RenderError::InvalidArgument {
            msg: "mask excludes every pixel".to_string(),
        });
    }

    // 0=red, 1=green, 2=blue, 3=alpha, 4=luma
    let mut abs_sum = [0f64; 5];
    let mut sq_sum = [0f64; 5];
    let mut max_abs = [0u32; 5];
    let mut differing_pixels = 0u64;
    let mut bbox: Option<[u32; 4]> = None;

    let block_size = opts.block_size.max(1) as usize;
    let blocks_x = w.div_ceil(block_size);
    let blocks_y = h.div_ceil(block_size);
    let mut block_sum = vec![0f64; blocks_x * blocks_y];
    let mut block_count = vec![0u32; blocks_x * blocks_y];
    let mut compared_blocks = 0u64;

    let mut luma_a = Vec::with_capacity(n);
    let mut luma_b = Vec::with_capacity(n);

    for (i, &is_masked) in masked.iter().enumerate() {
        let (ar, ag, ab, aa) = unpack_unpremul(a.pixels[i]);
        let (br, bg, bb, ba) = unpack_unpremul(b.pixels[i]);
        let la = luma(ar, ag, ab);
        let lb = luma(br, bg, bb);
        luma_a.push(la);
        luma_b.push(lb);
        if is_masked {
            continue;
        }
        let dr = ar.abs_diff(br);
        let dg = ag.abs_diff(bg);
        let db = ab.abs_diff(bb);
        let da = aa.abs_diff(ba);
        let dl = (la - lb).abs().round() as u8;

        let ds = [dr, dg, db, da, dl];
        for (c, &d) in ds.iter().enumerate() {
            let d = d as f64;
            abs_sum[c] += d;
            sq_sum[c] += d * d;
            max_abs[c] = max_abs[c].max(d as u32);
        }

        let pixel_d = dr.max(dg).max(db);
        if pixel_d > opts.pixel_threshold {
            differing_pixels += 1;
            let x = (i % w) as u32;
            let y = (i / w) as u32;
            bbox = Some(match bbox {
                None => [x, y, x + 1, y + 1],
                Some(mut bb) => {
                    bb[0] = bb[0].min(x);
                    bb[1] = bb[1].min(y);
                    bb[2] = bb[2].max(x + 1);
                    bb[3] = bb[3].max(y + 1);
                    bb
                }
            });
        }

        let bx = (i % w) / block_size;
        let by = (i / w) / block_size;
        let bi = by * blocks_x + bx;
        if block_count[bi] == 0 {
            compared_blocks += 1;
        }
        block_sum[bi] += (dr as f64 + dg as f64 + db as f64) / 3.0;
        block_count[bi] += 1;
    }

    let stats = |c: usize| ChannelStats {
        mean_abs_error: abs_sum[c] / compared as f64,
        rmse: (sq_sum[c] / compared as f64).sqrt(),
        max_abs_error: max_abs[c],
    };
    let red = stats(0);
    let green = stats(1);
    let blue = stats(2);
    let alpha = stats(3);
    let luma_s = stats(4);

    let mean_abs_error = (red.mean_abs_error + green.mean_abs_error + blue.mean_abs_error) / 3.0;
    let mse = (sq_sum[0] + sq_sum[1] + sq_sum[2]) / (3.0 * compared as f64);
    let rmse = mse.sqrt();
    let max_abs_error = max_abs[0].max(max_abs[1]).max(max_abs[2]);
    let identical = max_abs[0].max(max_abs[1]).max(max_abs[2]).max(max_abs[3]) == 0;
    let psnr_db = if mse == 0.0 {
        None
    } else {
        Some(10.0 * (255.0f64 * 255.0 / mse).log10())
    };

    let mut changed_blocks = 0u64;
    let mut max_block_mean = 0f64;
    for (bi, &sum) in block_sum.iter().enumerate() {
        let cnt = block_count[bi];
        if cnt == 0 {
            continue;
        }
        let mean = sum / cnt as f64;
        max_block_mean = max_block_mean.max(mean);
        if mean > opts.pixel_threshold as f64 {
            changed_blocks += 1;
        }
    }
    let blocks = BlockStats {
        block_size: block_size as u32,
        blocks_x: blocks_x as u32,
        blocks_y: blocks_y as u32,
        changed_blocks,
        changed_fraction: changed_blocks as f64 / compared_blocks.max(1) as f64,
        max_block_mean_abs_error: max_block_mean,
    };

    let win = normalize_window(opts.ssim_window);
    let (mean_ssim, min_ssim) = ssim_luma_masked(&luma_a, &luma_b, w, h, win, &masked);
    let global_ssim = global_ssim_luma_masked(&luma_a, &luma_b, &masked);

    Ok(DiffReport {
        width: a.width,
        height: a.height,
        pixel_count: n as u64,
        identical,
        mean_abs_error,
        rmse,
        max_abs_error,
        psnr_db,
        red,
        green,
        blue,
        alpha,
        luma: luma_s,
        pixel_threshold: opts.pixel_threshold,
        differing_pixels,
        differing_fraction: differing_pixels as f64 / compared as f64,
        changed_bbox: bbox,
        mean_ssim,
        min_ssim,
        global_ssim,
        ssim_window: win as u32,
        blocks,
        masked_pixels,
        compared_pixels: compared,
        mask,
    })
}

/// Rec. 601 luma on the 0..=255 scale, matching Skia/minikin's text coverage
/// scorer (see `tests/text_coverage.rs`).
#[inline]
fn luma(r: u8, g: u8, b: u8) -> f32 {
    0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32
}

fn normalize_window(win: u32) -> usize {
    let mut w = win.max(3) as usize;
    if w.is_multiple_of(2) {
        w += 1;
    }
    w
}

/// Separable Gaussian blur with **valid** convolution (no padding). Returns the
/// blurred plane plus its dimensions. If the source is smaller than the kernel
/// the source is returned unchanged (SSIM then falls back to the global value).
fn blur_valid(src: &[f64], w: usize, h: usize, k: &[f64]) -> (Vec<f64>, usize, usize) {
    let n = k.len();
    if w < n || h < n {
        return (src.to_vec(), w, h);
    }
    let ow = w - n + 1;
    let mut tmp = vec![0f64; ow * h];
    for y in 0..h {
        let row = &src[y * w..y * w + w];
        let out = &mut tmp[y * ow..y * ow + ow];
        for (x, o) in out.iter_mut().enumerate() {
            let mut acc = 0f64;
            for (i, &kv) in k.iter().enumerate() {
                acc += kv * row[x + i];
            }
            *o = acc;
        }
    }
    let oh = h - n + 1;
    let mut out = vec![0f64; ow * oh];
    for y in 0..oh {
        for x in 0..ow {
            let mut acc = 0f64;
            for (i, &kv) in k.iter().enumerate() {
                acc += kv * tmp[(y + i) * ow + x];
            }
            out[y * ow + x] = acc;
        }
    }
    (out, ow, oh)
}

fn gaussian_kernel(n: usize, sigma: f64) -> Vec<f64> {
    let c = (n as f64 - 1.0) / 2.0;
    let mut k = vec![0f64; n];
    let mut sum = 0f64;
    for (i, v) in k.iter_mut().enumerate() {
        let x = i as f64 - c;
        *v = (-(x * x) / (2.0 * sigma * sigma)).exp();
        sum += *v;
    }
    k.iter().map(|v| v / sum).collect()
}

/// Canonical windowed SSIM over luma, skipping every window that intersects
/// the mask. Returns `(mean, min)` over the remaining windows.
fn ssim_luma_masked(
    a: &[f32],
    b: &[f32],
    w: usize,
    h: usize,
    win: usize,
    masked: &[bool],
) -> (f64, f64) {
    if w < win || h < win {
        let g = global_ssim_luma_masked(a, b, masked);
        return (g, g);
    }
    let k = gaussian_kernel(win, 1.5);
    // The moments must be accumulated in f64: the covariance `mu_ab - mu_a*mu_b`
    // is the difference of two ~1e4-scale quantities, and `C2` is ~1e-3, so an
    // f32 blur would swamp the covariance with rounding noise and make the
    // windowed SSIM meaningless (it went negative on near-identical frames).
    let a64: Vec<f64> = a.iter().map(|v| *v as f64).collect();
    let b64: Vec<f64> = b.iter().map(|v| *v as f64).collect();
    let aa: Vec<f64> = a64.iter().map(|v| v * v).collect();
    let bb: Vec<f64> = b64.iter().map(|v| v * v).collect();
    let ab: Vec<f64> = a64.iter().zip(&b64).map(|(x, y)| x * y).collect();
    let (mu_a, ow, oh) = blur_valid(&a64, w, h, &k);
    let (mu_b, _, _) = blur_valid(&b64, w, h, &k);
    let (mu_aa, _, _) = blur_valid(&aa, w, h, &k);
    let (mu_bb, _, _) = blur_valid(&bb, w, h, &k);
    let (mu_ab, _, _) = blur_valid(&ab, w, h, &k);

    // O(1) "does this window contain a masked pixel" via a summed-area table.
    let mut prefix = vec![0u32; (h + 1) * (w + 1)];
    for y in 0..h {
        for x in 0..w {
            let v = masked[y * w + x] as u32;
            prefix[(y + 1) * (w + 1) + x + 1] = v
                + prefix[y * (w + 1) + x + 1]
                + prefix[(y + 1) * (w + 1) + x]
                - prefix[y * (w + 1) + x];
        }
    }
    // Compute in i64: the four terms are ordered only after the full
    // inclusion-exclusion, so an intermediate u32 subtraction can underflow.
    let window_masked = |x: usize, y: usize| -> i64 {
        let a = prefix[(y + win) * (w + 1) + x + win] as i64;
        let b = prefix[y * (w + 1) + x + win] as i64;
        let c = prefix[(y + win) * (w + 1) + x] as i64;
        let d = prefix[y * (w + 1) + x] as i64;
        a - b - c + d
    };

    // Standard SSIM stabilisers on the 0..255 luma scale.
    const C1: f64 = (0.01 * 255.0) * (0.01 * 255.0);
    const C2: f64 = (0.03 * 255.0) * (0.03 * 255.0);
    let mut sum = 0f64;
    let mut min = f64::INFINITY;
    let mut count = 0u64;
    for y in 0..oh {
        for x in 0..ow {
            if window_masked(x, y) != 0 {
                continue;
            }
            let i = y * ow + x;
            let ma = mu_a[i];
            let mb = mu_b[i];
            let vaa = (mu_aa[i] - ma * ma).max(0.0);
            let vbb = (mu_bb[i] - mb * mb).max(0.0);
            let vab = mu_ab[i] - ma * mb;
            let s = ((2.0 * ma * mb + C1) * (2.0 * vab + C2))
                / ((ma * ma + mb * mb + C1) * (vaa + vbb + C2));
            sum += s;
            min = min.min(s);
            count += 1;
        }
    }
    if count == 0 {
        let g = global_ssim_luma_masked(a, b, masked);
        return (g, g);
    }
    (sum / count as f64, min)
}

/// Single-window SSIM over the unmasked pixels of the luma planes.
fn global_ssim_luma_masked(a: &[f32], b: &[f32], masked: &[bool]) -> f64 {
    let mut n = 0f64;
    let mut sa = 0f64;
    let mut sb = 0f64;
    let mut saa = 0f64;
    let mut sbb = 0f64;
    let mut sab = 0f64;
    for (i, (x, y)) in a.iter().zip(b).enumerate() {
        if masked[i] {
            continue;
        }
        let x = *x as f64;
        let y = *y as f64;
        sa += x;
        sb += y;
        saa += x * x;
        sbb += y * y;
        sab += x * y;
        n += 1.0;
    }
    if n == 0.0 {
        return 1.0;
    }
    let ma = sa / n;
    let mb = sb / n;
    let vaa = (saa / n - ma * ma).max(0.0);
    let vbb = (sbb / n - mb * mb).max(0.0);
    let vab = sab / n - ma * mb;
    // Standard SSIM stabilisers on the 0..255 luma scale.
    const C1: f64 = (0.01 * 255.0) * (0.01 * 255.0);
    const C2: f64 = (0.03 * 255.0) * (0.03 * 255.0);
    ((2.0 * ma * mb + C1) * (2.0 * vab + C2))
        / ((ma * ma + mb * mb + C1) * (vaa + vbb + C2))
}

/// Build an RGBA heat map: unchanged pixels are dark grey, changed pixels go
/// white → yellow → red as the largest RGB error grows. Same dimensions as the
/// inputs.
pub fn heatmap_rgba(a: &DecodedImage, b: &DecodedImage, threshold: u8) -> RenderResult<Vec<u8>> {
    if a.width != b.width || a.height != b.height || a.pixels.len() != b.pixels.len() {
        return Err(RenderError::InvalidArgument {
            msg: "heatmap inputs must share dimensions".to_string(),
        });
    }
    let mut out = Vec::with_capacity(a.pixels.len() * 4);
    for (pa, pb) in a.pixels.iter().zip(&b.pixels) {
        let (ar, ag, ab, _) = unpack_unpremul(*pa);
        let (br, bg, bb, _) = unpack_unpremul(*pb);
        let d = ar.abs_diff(br).max(ag.abs_diff(bg)).max(ab.abs_diff(bb));
        if d <= threshold {
            out.extend_from_slice(&[24, 24, 24, 255]);
        } else {
            let t = d as f32 / 255.0;
            let r = 255u8;
            let g = (255.0 * (1.0 - t)) as u8;
            let bl = (255.0 * (1.0 - t * 2.0).max(0.0)) as u8;
            out.extend_from_slice(&[r, g, bl, 255]);
        }
    }
    Ok(out)
}

/// Encode a [`heatmap_rgba`] buffer as a PNG.
pub fn heatmap_png(a: &DecodedImage, b: &DecodedImage, threshold: u8) -> RenderResult<Vec<u8>> {
    let rgba = heatmap_rgba(a, b, threshold)?;
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, a.width, a.height);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::Fast);
        let mut writer = enc
            .write_header()
            .map_err(|e| RenderError::Decode { msg: e.to_string() })?;
        writer
            .write_image_data(&rgba)
            .map_err(|e| RenderError::Decode { msg: e.to_string() })?;
    }
    Ok(out)
}

/// Build an in-memory [`DecodedImage`] from un-premultiplied RGBA8 bytes.
pub fn decoded_from_rgba8(width: u32, height: u32, rgba: &[u8]) -> RenderResult<DecodedImage> {
    let n = width as usize * height as usize;
    if rgba.len() != n * 4 {
        return Err(RenderError::InvalidArgument {
            msg: format!(
                "rgba length {} does not match {width}x{height}",
                rgba.len()
            ),
        });
    }
    let mut pixels = Vec::with_capacity(n);
    for px in rgba.chunks_exact(4) {
        pixels.push(crate::color::pack_premul(px[0], px[1], px[2], px[3]));
    }
    Ok(DecodedImage {
        width,
        height,
        pixels,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, rgba: [u8; 4]) -> DecodedImage {
        decoded_from_rgba8(w, h, &rgba.repeat(w as usize * h as usize)).unwrap()
    }

    #[test]
    fn identical_images_are_perfect() {
        let a = solid(8, 8, [10, 20, 30, 255]);
        let b = a.clone();
        let r = diff(&a, &b, &DiffOptions::default()).unwrap();
        assert!(r.identical);
        assert_eq!(r.mean_abs_error, 0.0);
        assert_eq!(r.rmse, 0.0);
        assert_eq!(r.max_abs_error, 0);
        assert_eq!(r.psnr_db, None);
        assert_eq!(r.differing_pixels, 0);
        assert_eq!(r.changed_bbox, None);
        assert!((r.mean_ssim - 1.0).abs() < 1e-9);
        assert!((r.global_ssim - 1.0).abs() < 1e-9);
        assert_eq!(r.blocks.changed_blocks, 0);
    }

    #[test]
    fn uniform_offset_has_known_error() {
        // 4x4, every red channel differs by 10: MAE_RGB = 10/3.
        let a = solid(4, 4, [0, 0, 0, 255]);
        let b = solid(4, 4, [10, 0, 0, 255]);
        let r = diff(&a, &b, &DiffOptions::default()).unwrap();
        assert!(!r.identical);
        assert!((r.red.mean_abs_error - 10.0).abs() < 1e-9);
        assert_eq!(r.red.max_abs_error, 10);
        assert!((r.mean_abs_error - 10.0 / 3.0).abs() < 1e-9);
        assert_eq!(r.differing_pixels, 16, "all pixels exceed threshold 2");
        assert_eq!(r.changed_bbox, Some([0, 0, 4, 4]));
    }

    #[test]
    fn threshold_governs_changed_pixels() {
        let a = solid(4, 4, [0, 0, 0, 255]);
        let b = solid(4, 4, [1, 1, 1, 255]);
        let strict = DiffOptions {
            pixel_threshold: 0,
            ..DiffOptions::default()
        };
        let loose = DiffOptions {
            pixel_threshold: 2,
            ..DiffOptions::default()
        };
        assert_eq!(diff(&a, &b, &strict).unwrap().differing_pixels, 16);
        assert_eq!(diff(&a, &b, &loose).unwrap().differing_pixels, 0);
        // The error itself is unchanged by the threshold.
        assert!((diff(&a, &b, &loose).unwrap().mean_abs_error - 1.0).abs() < 1e-9);
    }

    #[test]
    fn dimension_mismatch_is_an_error() {
        let a = solid(4, 4, [0, 0, 0, 255]);
        let b = solid(4, 5, [0, 0, 0, 255]);
        let err = diff(&a, &b, &DiffOptions::default()).unwrap_err();
        assert_eq!(err.code(), "RENDER_INVALID_ARGUMENT");
    }

    #[test]
    fn mask_excludes_pixels_from_every_metric() {
        // Top four rows differ; bottom four are identical.
        let a = solid(8, 8, [0, 0, 0, 255]);
        let mut b = a.clone();
        for y in 0..4 {
            for x in 0..8 {
                b.pixels[y * 8 + x] = crate::color::pack_premul(200, 200, 200, 255);
            }
        }
        let unmasked = diff(&a, &b, &DiffOptions::default()).unwrap();
        assert!(unmasked.mean_abs_error > 0.0);
        assert_eq!(unmasked.differing_pixels, 32);

        let opts = DiffOptions {
            mask: vec![[0, 0, 8, 4]],
            ..DiffOptions::default()
        };
        let masked = diff(&a, &b, &opts).unwrap();
        assert_eq!(masked.masked_pixels, 32);
        assert_eq!(masked.compared_pixels, 32);
        assert_eq!(masked.mean_abs_error, 0.0, "masked pixels leave no error");
        assert_eq!(masked.differing_pixels, 0);
        assert_eq!(masked.changed_bbox, None);
        assert!((masked.global_ssim - 1.0).abs() < 1e-9);
        assert_eq!(masked.mask, vec![[0, 0, 8, 4]]);
        // The unmasked half still carries the difference.
        assert!(unmasked.global_ssim < 1.0);
    }

    #[test]
    fn heatmap_marks_the_changed_region() {
        let a = solid(4, 4, [0, 0, 0, 255]);
        let mut b = a.clone();
        // Change a single pixel to red.
        b.pixels[0] = crate::color::pack_premul(255, 0, 0, 255);
        let rgba = heatmap_rgba(&a, &b, 2).unwrap();
        assert_eq!(&rgba[0..4], &[255, 0, 0, 255], "changed pixel is red");
        assert_eq!(&rgba[4..8], &[24, 24, 24, 255], "unchanged pixel is grey");
        let png = heatmap_png(&a, &b, 2).unwrap();
        assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
    }
}
