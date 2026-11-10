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

/// Options for [`diff`].
#[derive(Clone, Debug)]
pub struct DiffOptions {
    /// A pixel "differs" when its largest RGB channel delta exceeds this.
    pub pixel_threshold: u8,
    /// SSIM window side (forced odd, minimum 3). The standard is 11.
    pub ssim_window: u32,
    /// Coarse structural block side in pixels.
    pub block_size: u32,
}

impl Default for DiffOptions {
    fn default() -> Self {
        Self {
            pixel_threshold: 2,
            ssim_window: 11,
            block_size: 32,
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

    let mut luma_a = Vec::with_capacity(n);
    let mut luma_b = Vec::with_capacity(n);

    for i in 0..n {
        let (ar, ag, ab, aa) = unpack_unpremul(a.pixels[i]);
        let (br, bg, bb, ba) = unpack_unpremul(b.pixels[i]);
        let dr = ar.abs_diff(br);
        let dg = ag.abs_diff(bg);
        let db = ab.abs_diff(bb);
        let da = aa.abs_diff(ba);
        let la = luma(ar, ag, ab);
        let lb = luma(br, bg, bb);
        let dl = (la - lb).abs().round() as u8;
        luma_a.push(la);
        luma_b.push(lb);

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
        block_sum[bi] += (dr as f64 + dg as f64 + db as f64) / 3.0;
        block_count[bi] += 1;
    }

    let stats = |c: usize| ChannelStats {
        mean_abs_error: abs_sum[c] / n as f64,
        rmse: (sq_sum[c] / n as f64).sqrt(),
        max_abs_error: max_abs[c],
    };
    let red = stats(0);
    let green = stats(1);
    let blue = stats(2);
    let alpha = stats(3);
    let luma_s = stats(4);

    let mean_abs_error = (red.mean_abs_error + green.mean_abs_error + blue.mean_abs_error) / 3.0;
    let mse = (sq_sum[0] + sq_sum[1] + sq_sum[2]) / (3.0 * n as f64);
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
    let total_blocks = (blocks_x * blocks_y) as u64;
    let blocks = BlockStats {
        block_size: block_size as u32,
        blocks_x: blocks_x as u32,
        blocks_y: blocks_y as u32,
        changed_blocks,
        changed_fraction: changed_blocks as f64 / total_blocks as f64,
        max_block_mean_abs_error: max_block_mean,
    };

    let win = normalize_window(opts.ssim_window);
    let (mean_ssim, min_ssim) = ssim_luma(&luma_a, &luma_b, w, h, win);
    let global_ssim = global_ssim_luma(&luma_a, &luma_b);

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
        differing_fraction: differing_pixels as f64 / n as f64,
        changed_bbox: bbox,
        mean_ssim,
        min_ssim,
        global_ssim,
        ssim_window: win as u32,
        blocks,
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
fn blur_valid(src: &[f32], w: usize, h: usize, k: &[f32]) -> (Vec<f32>, usize, usize) {
    let n = k.len();
    if w < n || h < n {
        return (src.to_vec(), w, h);
    }
    let ow = w - n + 1;
    let mut tmp = vec![0f32; ow * h];
    for y in 0..h {
        let row = &src[y * w..y * w + w];
        let out = &mut tmp[y * ow..y * ow + ow];
        for (x, o) in out.iter_mut().enumerate() {
            let mut acc = 0f32;
            for (i, &kv) in k.iter().enumerate() {
                acc += kv * row[x + i];
            }
            *o = acc;
        }
    }
    let oh = h - n + 1;
    let mut out = vec![0f32; ow * oh];
    for y in 0..oh {
        for x in 0..ow {
            let mut acc = 0f32;
            for (i, &kv) in k.iter().enumerate() {
                acc += kv * tmp[(y + i) * ow + x];
            }
            out[y * ow + x] = acc;
        }
    }
    (out, ow, oh)
}

fn gaussian_kernel(n: usize, sigma: f64) -> Vec<f32> {
    let c = (n as f64 - 1.0) / 2.0;
    let mut k = vec![0f64; n];
    let mut sum = 0f64;
    for (i, v) in k.iter_mut().enumerate() {
        let x = i as f64 - c;
        *v = (-(x * x) / (2.0 * sigma * sigma)).exp();
        sum += *v;
    }
    k.iter().map(|v| (v / sum) as f32).collect()
}

/// Canonical windowed SSIM over luma. Returns `(mean, min)`.
fn ssim_luma(a: &[f32], b: &[f32], w: usize, h: usize, win: usize) -> (f64, f64) {
    if w < win || h < win {
        let g = global_ssim_luma(a, b);
        return (g, g);
    }
    let k = gaussian_kernel(win, 1.5);
    let aa: Vec<f32> = a.iter().map(|v| v * v).collect();
    let bb: Vec<f32> = b.iter().map(|v| v * v).collect();
    let ab: Vec<f32> = a.iter().zip(b).map(|(x, y)| x * y).collect();
    let (mu_a, ow, oh) = blur_valid(a, w, h, &k);
    let (mu_b, _, _) = blur_valid(b, w, h, &k);
    let (mu_aa, _, _) = blur_valid(&aa, w, h, &k);
    let (mu_bb, _, _) = blur_valid(&bb, w, h, &k);
    let (mu_ab, _, _) = blur_valid(&ab, w, h, &k);

    const C1: f64 = 0.01 * 0.01;
    const C2: f64 = 0.03 * 0.03;
    let n = ow * oh;
    let mut sum = 0f64;
    let mut min = f64::INFINITY;
    for i in 0..n {
        let ma = mu_a[i] as f64;
        let mb = mu_b[i] as f64;
        let vaa = (mu_aa[i] as f64 - ma * ma).max(0.0);
        let vbb = (mu_bb[i] as f64 - mb * mb).max(0.0);
        let vab = mu_ab[i] as f64 - ma * mb;
        let s = ((2.0 * ma * mb + C1) * (2.0 * vab + C2))
            / ((ma * ma + mb * mb + C1) * (vaa + vbb + C2));
        sum += s;
        min = min.min(s);
    }
    (sum / n as f64, min)
}

/// Single-window SSIM over the whole luma plane.
fn global_ssim_luma(a: &[f32], b: &[f32]) -> f64 {
    let n = a.len() as f64;
    if n == 0.0 {
        return 1.0;
    }
    let mut sa = 0f64;
    let mut sb = 0f64;
    let mut saa = 0f64;
    let mut sbb = 0f64;
    let mut sab = 0f64;
    for (x, y) in a.iter().zip(b) {
        let x = *x as f64;
        let y = *y as f64;
        sa += x;
        sb += y;
        saa += x * x;
        sbb += y * y;
        sab += x * y;
    }
    let ma = sa / n;
    let mb = sb / n;
    let vaa = (saa / n - ma * ma).max(0.0);
    let vbb = (sbb / n - mb * mb).max(0.0);
    let vab = sab / n - ma * mb;
    const C1: f64 = 0.01 * 0.01;
    const C2: f64 = 0.03 * 0.03;
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
