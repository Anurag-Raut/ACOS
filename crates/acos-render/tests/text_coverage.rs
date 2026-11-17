//! Text coverage golden test against the real Android 14 screenshot
//! (`ADR-0019`).
//!
//! The render-fidelity spike scored candidate text by recovering a per-pixel
//! *coverage* map from the reference screenshot's green channel (the device
//! screenshot has a small colour cast, so raw RGB is not comparable) and by
//! two metrics:
//!
//! * **whole string** — align by an integer shift and report mean |Δcoverage|
//!   and RMSE over the text region; and
//! * **per-glyph** — isolate every glyph as a connected component in *both* the
//!   reference and the candidate, align each independently with a sub-pixel
//!   shift, and pool the error. Advance/positioning error is removed, so this
//!   measures only the rasterised shape. This is the metric ADR-0019 accepted
//!   (PoC: 0.0163 mean / 0.0415 RMSE for FreeType vs 0.0458 / 0.0822 for the
//!   old path-filled outlines).
//!
//! The thresholds are the ADR-0019 acceptance numbers. A return to
//! path-filled outlines or disabled anti-aliasing fails loudly, while a
//! FreeType patch bump keeps enough headroom.
//!
//! Set `ACOS_RENDER_SKIP_GOLDEN=1` to skip (e.g. a host without the fixture).

use std::path::PathBuf;

use acos_render::{image, Scene};

/// Region that contains the title "Hello ACOS" in both the reference and the
/// rendered hello-views scene (matches `research/render-fidelity/diff_text.py`).
const BOX: (usize, usize, usize, usize) = (30, 300, 300, 390);

/// ADR-0019 acceptance thresholds for the per-glyph (positioning removed)
/// metric.
const MAX_PER_GLYPH_MEAN: f32 = 0.02;
const MAX_PER_GLYPH_RMSE: f32 = 0.05;

/// Coverage threshold used to isolate glyphs as connected components. Low
/// enough to include anti-aliased edges, high enough that adjacent glyphs in
/// this 36.75px run stay separate.
const COMPONENT_THRESHOLD: f32 = 0.05;

fn workspace_path(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn reference_path() -> PathBuf {
    workspace_path("../../fixtures/reference/hello-views/art-api34-x86_64/screenshot_initial.png")
}

fn region_size() -> (usize, usize) {
    (BOX.2 - BOX.0, BOX.3 - BOX.1)
}

/// Nearest-rank percentile of a small f32 sample.
fn percentile(values: &mut [f32], p: f32) -> f32 {
    values.sort_by(|a, b| a.partial_cmp(b).unwrap());
    if values.is_empty() {
        return 0.0;
    }
    let idx = ((p / 100.0) * (values.len() - 1) as f32).round() as usize;
    values[idx.min(values.len() - 1)]
}

/// Recover a coverage map from a green-channel region.
fn coverage_from_green(green: &[f32]) -> Vec<f32> {
    let mut sample = green.to_vec();
    let bg = percentile(&mut sample, 99.0);
    let fg = percentile(&mut sample, 0.3);
    let denom = (bg - fg).max(1e-6);
    green
        .iter()
        .map(|&g| ((bg - g) / denom).clamp(0.0, 1.0))
        .collect()
}

/// Extract the green channel of a region from a tightly packed RGBA buffer.
fn green_region(
    rgba: &[u8],
    stride: usize,
    (x0, y0, x1, y1): (usize, usize, usize, usize),
) -> Vec<f32> {
    let mut out = Vec::with_capacity((x1 - x0) * (y1 - y0));
    for y in y0..y1 {
        let row = y * stride;
        for x in x0..x1 {
            out.push(rgba[(row + x) * 4 + 1] as f32);
        }
    }
    out
}

/// Integer shift with zero fill.
fn shift_integer(src: &[f32], w: usize, h: usize, dx: i32, dy: i32) -> Vec<f32> {
    let mut out = vec![0.0f32; w * h];
    for y in 0..h as i32 {
        let sy = y - dy;
        if sy < 0 || sy >= h as i32 {
            continue;
        }
        for x in 0..w as i32 {
            let sx = x - dx;
            if sx < 0 || sx >= w as i32 {
                continue;
            }
            out[(y as usize) * w + x as usize] = src[(sy as usize) * w + sx as usize];
        }
    }
    out
}

/// Best integer alignment of `cand` onto `reference`.
fn best_integer_shift(reference: &[f32], cand: &[f32], w: usize, h: usize, win: i32) -> (i32, i32) {
    let mut best = (f32::INFINITY, 0, 0);
    for dy in -win..=win {
        for dx in -win..=win {
            let shifted = shift_integer(cand, w, h, dx, dy);
            let mut acc = 0.0f32;
            for (r, c) in reference.iter().zip(shifted.iter()) {
                acc += (r - c).abs();
            }
            let mean = acc / (w * h) as f32;
            if mean < best.0 {
                best = (mean, dx, dy);
            }
        }
    }
    (best.1, best.2)
}

/// A glyph isolated as an 8-connected component.
struct Component {
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
    /// Full-grid membership mask (`w * h`), so the window can be cropped
    /// without re-deriving the shape.
    mask: Vec<bool>,
}

impl Component {
    fn mask_in(&self, w: usize, x0: usize, y0: usize, x1: usize, y1: usize) -> Vec<bool> {
        let mut out = Vec::with_capacity((x1 - x0) * (y1 - y0));
        for y in y0..y1 {
            for x in x0..x1 {
                out.push(self.mask[y * w + x]);
            }
        }
        out
    }
}

/// 8-connected component bounding boxes + masks of pixels above `thr`.
fn components(cov: &[f32], w: usize, h: usize, thr: f32) -> Vec<Component> {
    let mut seen = vec![false; w * h];
    let mut out = Vec::new();
    let mut stack: Vec<(usize, usize)> = Vec::new();
    for y0 in 0..h {
        for x0 in 0..w {
            let i = y0 * w + x0;
            if seen[i] || cov[i] <= thr {
                continue;
            }
            let mut mask = vec![false; w * h];
            stack.clear();
            stack.push((x0, y0));
            seen[i] = true;
            mask[i] = true;
            let (mut min_x, mut min_y, mut max_x, mut max_y) = (x0, y0, x0, y0);
            while let Some((cx, cy)) = stack.pop() {
                min_x = min_x.min(cx);
                min_y = min_y.min(cy);
                max_x = max_x.max(cx);
                max_y = max_y.max(cy);
                for ny in cy.saturating_sub(1)..=(cy + 1).min(h - 1) {
                    for nx in cx.saturating_sub(1)..=(cx + 1).min(w - 1) {
                        let j = ny * w + nx;
                        if !seen[j] && cov[j] > thr {
                            seen[j] = true;
                            mask[j] = true;
                            stack.push((nx, ny));
                        }
                    }
                }
            }
            out.push(Component {
                x0: min_x,
                y0: min_y,
                x1: max_x + 1,
                y1: max_y + 1,
                mask,
            });
        }
    }
    out.sort_by_key(|c| c.x0);
    out
}

/// Sample `src` (w×h) at `(x - dx, y - dy)` with bilinear interpolation.
fn shift_bilinear(src: &[f32], w: usize, h: usize, dx: f32, dy: f32) -> Vec<f32> {
    let mut out = vec![0.0f32; w * h];
    for y in 0..h {
        let sy = y as f32 - dy;
        let y0 = (sy.floor() as i64).clamp(0, h as i64 - 1) as usize;
        let y1 = (y0 + 1).min(h - 1);
        let wy = (sy - y0 as f32).clamp(0.0, 1.0);
        for x in 0..w {
            let sx = x as f32 - dx;
            let x0 = (sx.floor() as i64).clamp(0, w as i64 - 1) as usize;
            let x1 = (x0 + 1).min(w - 1);
            let wx = (sx - x0 as f32).clamp(0.0, 1.0);
            let top = src[y0 * w + x0] * (1.0 - wx) + src[y0 * w + x1] * wx;
            let bot = src[y1 * w + x0] * (1.0 - wx) + src[y1 * w + x1] * wx;
            out[y * w + x] = top * (1.0 - wy) + bot * wy;
        }
    }
    out
}

/// Search a sub-pixel shift of `cand` that minimises mean |Δcoverage|; returns
/// `(dx, dy, shifted)`.
fn best_subpixel_shift(
    reference: &[f32],
    cand: &[f32],
    w: usize,
    h: usize,
) -> (f32, f32, Vec<f32>) {
    let mut best: Option<(f32, f32, f32, Vec<f32>)> = None;
    let mut dx = -2.0f32;
    while dx <= 2.0001 {
        let mut dy = -1.0f32;
        while dy <= 1.0001 {
            let shifted = shift_bilinear(cand, w, h, dx, dy);
            let mut acc = 0.0f32;
            for (r, c) in reference.iter().zip(shifted.iter()) {
                acc += (r - c).abs();
            }
            let mean = acc / (w * h) as f32;
            match &best {
                Some(b) if b.0 <= mean => {}
                _ => best = Some((mean, dx, dy, shifted)),
            }
            dy += 0.25;
        }
        dx += 0.1;
    }
    let (_, dx, dy, shifted) = best.expect("non-empty search");
    (dx, dy, shifted)
}

#[derive(Default, Clone, Copy)]
struct Metrics {
    abs_sum: f64,
    sq_sum: f64,
    count: u64,
}

impl Metrics {
    fn add(&mut self, reference: &[f32], other: &[f32]) {
        for (r, c) in reference.iter().zip(other.iter()) {
            let d = (r - c) as f64;
            self.abs_sum += d.abs();
            self.sq_sum += d * d;
            self.count += 1;
        }
    }

    fn mean(&self) -> f32 {
        if self.count == 0 {
            0.0
        } else {
            (self.abs_sum / self.count as f64) as f32
        }
    }

    fn rmse(&self) -> f32 {
        if self.count == 0 {
            0.0
        } else {
            (self.sq_sum / self.count as f64).sqrt() as f32
        }
    }
}

fn pooled_metrics(reference: &[f32], other: &[f32]) -> (f32, f32) {
    let mut m = Metrics::default();
    m.add(reference, other);
    (m.mean(), m.rmse())
}

#[test]
fn text_coverage_matches_device_reference() {
    if std::env::var("ACOS_RENDER_SKIP_GOLDEN").is_ok() {
        return;
    }
    let reference_png = reference_path();
    if !reference_png.exists() {
        eprintln!(
            "SKIP: no reference screenshot at {}",
            reference_png.display()
        );
        return;
    }

    // --- candidate: the real hello-views scene rendered through FreeType ---
    let scene_path = workspace_path("tests/scenes/hello-views.json");
    let scene = Scene::load(&scene_path).expect("hello-views scene");
    let mut ctx = acos_render::RenderContext::new();
    if !ctx.has_fonts() {
        eprintln!("SKIP: no fonts available (set ACOS_FONT_DIR)");
        return;
    }
    assert!(
        acos_render::text::freetype::available(),
        "bundled FreeType failed to initialise"
    );
    let out = scene.render(&mut ctx, 1).expect("render hello-views");
    let rgba = out.frame.rgba8();
    let stride = out.frame.width as usize;
    let cand_green = green_region(rgba, stride, BOX);

    // --- reference: the real ART emulator screenshot ---
    let ref_bytes = std::fs::read(&reference_png).unwrap();
    let ref_img = image::decode_png(&ref_bytes).expect("decode reference png");
    let mut ref_green = Vec::with_capacity(region_size().0 * region_size().1);
    for y in BOX.1..BOX.3 {
        for x in BOX.0..BOX.2 {
            let word = ref_img.pixels[y * ref_img.width as usize + x];
            let (_r, g, _b, _a) = acos_render::color::unpack_unpremul(word);
            ref_green.push(g as f32);
        }
    }

    let (w, h) = region_size();
    let reference = coverage_from_green(&ref_green);
    let candidate = coverage_from_green(&cand_green);

    // Whole-string metric: align by an integer shift.
    let (gdx, gdy) = best_integer_shift(&reference, &candidate, w, h, 6);
    let aligned = shift_integer(&candidate, w, h, gdx, gdy);
    let whole = pooled_metrics(&reference, &aligned);

    // Per-glyph metric: isolate each glyph in *both* images so a neighbouring
    // glyph's anti-aliasing cannot leak into the window, align each with a
    // sub-pixel shift, and pool.
    let ref_components = components(&reference, w, h, COMPONENT_THRESHOLD);
    let cand_components = components(&aligned, w, h, COMPONENT_THRESHOLD);
    assert_eq!(
        ref_components.len(),
        cand_components.len(),
        "glyph component count mismatch: reference {} vs candidate {}",
        ref_components.len(),
        cand_components.len()
    );
    assert!(
        ref_components.len() >= 8,
        "expected ~9 glyph components, found {}",
        ref_components.len()
    );

    let pad = 2usize;
    let mut per_glyph = Metrics::default();
    let mut recon = aligned.clone();
    for (rc, cc) in ref_components.iter().zip(cand_components.iter()) {
        let x0 = rc.x0.min(cc.x0).saturating_sub(pad);
        let y0 = rc.y0.min(cc.y0).saturating_sub(pad);
        let x1 = rc.x1.max(cc.x1).saturating_add(pad).min(w);
        let y1 = rc.y1.max(cc.y1).saturating_add(pad).min(h);
        let ww = x1 - x0;
        let hh = y1 - y0;
        let rmask = rc.mask_in(w, x0, y0, x1, y1);
        let cmask = cc.mask_in(w, x0, y0, x1, y1);

        let mut rw = Vec::with_capacity(ww * hh);
        let mut cw = Vec::with_capacity(ww * hh);
        for y in y0..y1 {
            for x in x0..x1 {
                let i = y * w + x;
                rw.push(if rmask[(y - y0) * ww + (x - x0)] {
                    reference[i]
                } else {
                    0.0
                });
                cw.push(if cmask[(y - y0) * ww + (x - x0)] {
                    aligned[i]
                } else {
                    0.0
                });
            }
        }
        let (_dx, _dy, shifted) = best_subpixel_shift(&rw, &cw, ww, hh);
        per_glyph.add(&rw, &shifted);
        for y in 0..hh {
            for x in 0..ww {
                // Only the glyph's own pixels are replaced; background and
                // other components stay from the globally aligned image.
                if cmask[y * ww + x] {
                    recon[(y0 + y) * w + (x0 + x)] = shifted[y * ww + x];
                }
            }
        }
    }
    let whole_per_glyph = pooled_metrics(&reference, &recon);

    let mean = per_glyph.mean();
    let rmse = per_glyph.rmse();
    eprintln!(
        "text coverage: whole-string shift=({gdx},{gdy}) mean={:.5} rmse={:.5} | \
whole after per-glyph align mean={:.5} rmse={:.5} | \
per-glyph ({} glyphs) mean={:.5} rmse={:.5}",
        whole.0,
        whole.1,
        whole_per_glyph.0,
        whole_per_glyph.1,
        ref_components.len(),
        mean,
        rmse,
    );

    record_results(
        ref_components.len(),
        (gdx, gdy),
        whole,
        whole_per_glyph,
        (mean, rmse),
    );

    assert!(
        mean <= MAX_PER_GLYPH_MEAN,
        "per-glyph mean coverage error {mean:.5} > {MAX_PER_GLYPH_MEAN}"
    );
    assert!(
        rmse <= MAX_PER_GLYPH_RMSE,
        "per-glyph RMSE {rmse:.5} > {MAX_PER_GLYPH_RMSE}"
    );
}

/// Write the measured numbers next to the other render evidence.
fn record_results(
    glyphs: usize,
    global_shift: (i32, i32),
    whole: (f32, f32),
    whole_per_glyph: (f32, f32),
    per_glyph: (f32, f32),
) {
    let dir = workspace_path("../../results/render");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let body = format!(
        "# ADR-0019 text coverage vs fixtures/reference/hello-views/art-api34-x86_64/screenshot_initial.png\n\
# region [30,300,300,390], coverage recovered from the green channel\n\
# metrics: mean|dCoverage| and RMSE (lower is better)\n\
reference=art-api34-x86_64 title=\"Hello ACOS\" 14sp=36.75px glyphs={glyphs}\n\
renderer=freetype FT_LOAD_NO_HINTING|FT_LOAD_RENDER (bundled FreeType 2.13.2) + rustybuzz\n\
whole_string_shift_x={}\nwhole_string_shift_y={}\n\
whole_string_mean={:.6}\nwhole_string_rmse={:.6}\n\
whole_after_per_glyph_align_mean={:.6}\nwhole_after_per_glyph_align_rmse={:.6}\n\
per_glyph_mean={:.6}\nper_glyph_rmse={:.6}\n\
thresholds_per_glyph_mean<=0.02 per_glyph_rmse<=0.05\n",
        global_shift.0,
        global_shift.1,
        whole.0,
        whole.1,
        whole_per_glyph.0,
        whole_per_glyph.1,
        per_glyph.0,
        per_glyph.1,
    );
    let _ = std::fs::write(dir.join("text-coverage.txt"), body);
}
