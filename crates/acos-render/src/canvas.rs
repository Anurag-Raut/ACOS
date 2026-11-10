//! `android.graphics.Canvas` / `RecordingCanvas` rasteriser.
//!
//! The heavy lifting is done by free functions that operate on a
//! `(&CanvasState, &mut Pixmap)` pair, so the [`crate::RenderContext`] can
//! resolve a canvas's target pixmap (surface, bitmap or save-layer) with
//! disjoint field borrows while keeping the canvas state intact.

use crate::color::{premultiply_channel, unpack_unpremul};
use crate::error::{RenderError, RenderResult};
use crate::geom::{Rect, RectF};
use crate::matrix::Matrix3x3;
use crate::paint::{BlurStyle, ColorFilterSpec, MaskFilterSpec, Paint, PaintStyle};
use crate::path::Path;
use tiny_skia::{
    BlendMode, FillRule, Mask, PathBuilder, Pixmap, PixmapPaint, PixmapRef, Transform,
};

/// Android `Canvas.ClipOp`.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum ClipOp {
    Difference = 0,
    Intersect = 1,
    Replace = 2,
    ReverseDifference = 3,
}

impl ClipOp {
    pub fn from_ordinal(o: i32) -> Option<Self> {
        Some(match o {
            0 => ClipOp::Difference,
            1 => ClipOp::Intersect,
            2 => ClipOp::Replace,
            3 => ClipOp::ReverseDifference,
            _ => return None,
        })
    }
}

/// Canvas save/restore state that is not part of the save stack.
#[derive(Clone, Default)]
pub struct CanvasState {
    pub matrix: Matrix3x3,
    pub clip: Option<Mask>,
}

struct SavedCanvas {
    state: CanvasState,
    layer: bool,
}

pub(crate) struct Layer {
    pub(crate) pixmap: Pixmap,
    alpha: u8,
    blend: BlendMode,
}

/// The kind of object a canvas rasterises into.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum CanvasTarget {
    Bitmap(crate::handle::Handle),
    Surface(crate::handle::Handle),
    /// Recording canvas for a `RenderNode` display list.
    RenderNode(crate::handle::Handle),
}

/// Rasterising canvas.
pub struct Canvas {
    pub width: u32,
    pub height: u32,
    pub target: Option<CanvasTarget>,
    pub state: CanvasState,
    save_stack: Vec<SavedCanvas>,
    pub(crate) layers: Vec<Layer>,
    /// Unsupported ops observed on this canvas (surfaced as reduced fidelity).
    pub unsupported: Vec<String>,
    /// When true, unsupported ops return an error instead of being recorded.
    pub strict: bool,
    pub is_opaque: bool,
}

impl Canvas {
    pub fn new(width: u32, height: u32, target: Option<CanvasTarget>) -> Self {
        Canvas {
            width,
            height,
            target,
            state: CanvasState::default(),
            save_stack: Vec::new(),
            layers: Vec::new(),
            unsupported: Vec::new(),
            strict: false,
            is_opaque: false,
        }
    }

    pub fn save_count(&self) -> i32 {
        1 + self.save_stack.len() as i32
    }

    /// Reset all transient canvas state (new frame).
    pub fn reset_state(&mut self) {
        self.state = CanvasState::default();
        self.save_stack.clear();
        self.layers.clear();
        self.unsupported.clear();
    }

    pub fn save(&mut self) -> i32 {
        self.save_stack.push(SavedCanvas {
            state: self.state.clone(),
            layer: false,
        });
        self.save_count()
    }

    pub fn set_matrix(&mut self, m: &Matrix3x3) {
        self.state.matrix = *m;
    }

    pub fn concat(&mut self, m: &Matrix3x3) {
        self.state.matrix.pre_concat(m);
    }

    pub fn translate(&mut self, dx: f32, dy: f32) {
        self.state.matrix.pre_translate(dx, dy);
    }

    pub fn scale(&mut self, sx: f32, sy: f32) {
        self.state.matrix.pre_scale(sx, sy);
    }

    pub fn rotate(&mut self, degrees: f32) {
        self.state.matrix.pre_rotate(degrees);
    }

    pub fn skew(&mut self, kx: f32, ky: f32) {
        self.state.matrix.pre_concat(&Matrix3x3::skew_matrix(kx, ky));
    }

    pub fn get_matrix(&self) -> Matrix3x3 {
        self.state.matrix
    }

    /// Push a save layer for `saveLayerAlpha`. The layer is full-size (Android
    /// clips it to bounds; we approximate by compositing only within the
    /// current clip).
    pub fn save_layer_alpha(&mut self, bounds: Option<RectF>, alpha: u8) -> RenderResult<i32> {
        self.save_stack.push(SavedCanvas {
            state: self.state.clone(),
            layer: true,
        });
        let mut pixmap = Pixmap::new(self.width, self.height).ok_or_else(|| {
            RenderError::size("saveLayer allocation failed")
        })?;
        // Layer bounds are honoured approximately: the framework clips the
        // layer to `bounds`, while ACOS allocates a full-size layer and relies
        // on the current clip. Documented in the fidelity list.
        let _ = bounds;
        pixmap.fill(tiny_skia::Color::TRANSPARENT);
        self.layers.push(Layer {
            pixmap,
            alpha,
            blend: BlendMode::SourceOver,
        });
        Ok(self.save_count())
    }

    /// Pop a save. If it was a layer, composite it onto `base`.
    pub fn restore(&mut self, base: &mut Pixmap) -> bool {
        let Some(saved) = self.save_stack.pop() else {
            return false;
        };
        self.state = saved.state;
        if saved.layer {
            if let Some(layer) = self.layers.pop() {
                let clip = self.state.clip.as_ref();
                let paint = PixmapPaint {
                    opacity: layer.alpha as f32 / 255.0,
                    blend_mode: layer.blend,
                    quality: tiny_skia::FilterQuality::Bilinear,
                };
                // Composite onto the parent layer if one is still active,
                // otherwise onto the base pixmap.
                let dest = match self.layers.last_mut() {
                    Some(parent) => &mut parent.pixmap,
                    None => base,
                };
                dest.draw_pixmap(
                    0,
                    0,
                    layer.pixmap.as_ref(),
                    &paint,
                    Transform::identity(),
                    clip,
                );
            }
        }
        true
    }

    /// Set the blend mode used when the topmost save-layer is composited.
    pub(crate) fn set_top_layer_blend(&mut self, blend: BlendMode) {
        if let Some(layer) = self.layers.last_mut() {
            layer.blend = blend;
        }
    }

    pub fn restore_to_count(&mut self, base: &mut Pixmap, count: i32) {
        while self.save_count() > count.max(1) {
            if !self.restore(base) {
                break;
            }
        }
    }

    fn affine(&self) -> RenderResult<Transform> {
        self.state
            .matrix
            .to_affine()
            .ok_or_else(|| RenderError::unsupported("Canvas perspective matrix"))
    }

    /// Clip to a path. `op` selects intersect/difference/replace.
    pub fn clip_path(&mut self, path: &Path, op: ClipOp, aa: bool) -> RenderResult<()> {
        if op == ClipOp::ReverseDifference {
            return Err(RenderError::unsupported("Canvas.clipPath ReverseDifference"));
        }
        let Some(sk) = path.build_tiny_skia() else {
            // An empty path with Intersect clips everything away.
            if op == ClipOp::Intersect {
                self.state.clip = Some(empty_mask(self.width, self.height));
            }
            return Ok(());
        };
        let ts = self.affine()?;
        let mut m = Mask::new(self.width, self.height)
            .ok_or_else(|| RenderError::size("clip mask allocation failed"))?;
        m.fill_path(&sk, path.fill_type().fill_rule(), aa, ts);
        if path.fill_type().is_inverse() {
            m.invert();
        }
        self.apply_clip_mask(m, op);
        Ok(())
    }

    /// Clip to a rectangle.
    pub fn clip_rect(&mut self, rect: RectF, op: ClipOp, aa: bool) -> RenderResult<()> {
        if op == ClipOp::ReverseDifference {
            return Err(RenderError::unsupported("Canvas.clipRect ReverseDifference"));
        }
        let path = rect_path(rect);
        let ts = self.affine()?;
        let mut m = Mask::new(self.width, self.height)
            .ok_or_else(|| RenderError::size("clip mask allocation failed"))?;
        m.fill_path(&path, FillRule::Winding, aa, ts);
        self.apply_clip_mask(m, op);
        Ok(())
    }

    fn apply_clip_mask(&mut self, mut m: Mask, op: ClipOp) {
        match op {
            ClipOp::Replace => {
                self.state.clip = Some(m);
            }
            ClipOp::Intersect => match &mut self.state.clip {
                Some(existing) => intersect_masks(existing, &m),
                None => self.state.clip = Some(m),
            },
            ClipOp::Difference => {
                m.invert();
                match &mut self.state.clip {
                    Some(existing) => intersect_masks(existing, &m),
                    None => self.state.clip = Some(m),
                }
            }
            ClipOp::ReverseDifference => {}
        }
    }

    pub fn get_clip_bounds(&self) -> Rect {
        match &self.state.clip {
            Some(m) => {
                let mut min_x = i32::MAX;
                let mut min_y = i32::MAX;
                let mut max_x = i32::MIN;
                let mut max_y = i32::MIN;
                for y in 0..m.height() as i32 {
                    for x in 0..m.width() as i32 {
                        if m.data()[y as usize * m.width() as usize + x as usize] > 0 {
                            min_x = min_x.min(x);
                            min_y = min_y.min(y);
                            max_x = max_x.max(x + 1);
                            max_y = max_y.max(y + 1);
                        }
                    }
                }
                if min_x > max_x {
                    Rect::new(0, 0, 0, 0)
                } else {
                    Rect::new(min_x, min_y, max_x, max_y)
                }
            }
            None => Rect::new(0, 0, self.width as i32, self.height as i32),
        }
    }

    /// Android `Canvas.quickReject(rect)` — conservative.
    pub fn quick_reject(&self, rect: RectF) -> bool {
        if rect.is_empty() {
            return true;
        }
        let device = match self.state.matrix.map_rect(&rect) {
            Some(r) => r,
            None => return true,
        };
        let clip = self.get_clip_bounds();
        device.right <= clip.left as f32
            || device.left >= clip.right as f32
            || device.bottom <= clip.top as f32
            || device.top >= clip.bottom as f32
    }

}

/// Intersect two alpha masks in place.
pub(crate) fn intersect_masks(a: &mut Mask, b: &Mask) {
    for (x, y) in a.data_mut().iter_mut().zip(b.data()) {
        *x = premultiply_channel(*x, *y);
    }
}

fn empty_mask(w: u32, h: u32) -> Mask {
    let mut m = Mask::new(w, h).unwrap_or_else(|| Mask::new(1, 1).unwrap());
    m.clear();
    m
}

/// Build a tiny-skia rectangle path.
pub(crate) fn rect_path(r: RectF) -> tiny_skia::Path {
    let mut b = PathBuilder::new();
    b.move_to(r.left, r.top);
    b.line_to(r.right, r.top);
    b.line_to(r.right, r.bottom);
    b.line_to(r.left, r.bottom);
    b.close();
    b.finish().unwrap_or_else(|| {
        let mut e = PathBuilder::new();
        e.move_to(0.0, 0.0);
        e.finish().unwrap()
    })
}

/// Rasterise a filled path, applying colour/mask filters if present.
#[allow(clippy::too_many_arguments)]
/// Device-space bounding box of a transformed path, expanded by `expand`.
fn filter_device_bounds(
    path: &tiny_skia::Path,
    transform: Transform,
    expand: f32,
    w: u32,
    h: u32,
) -> Option<(i32, i32, u32, u32)> {
    let b = path.bounds();
    let mut min_x = f32::INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    for (x, y) in [
        (b.left(), b.top()),
        (b.right(), b.top()),
        (b.left(), b.bottom()),
        (b.right(), b.bottom()),
    ] {
        let mut p = tiny_skia::Point::from_xy(x, y);
        transform.map_point(&mut p);
        min_x = min_x.min(p.x);
        min_y = min_y.min(p.y);
        max_x = max_x.max(p.x);
        max_y = max_y.max(p.y);
    }
    if !min_x.is_finite() || !min_y.is_finite() {
        return None;
    }
    let x0 = (min_x - expand).floor().max(0.0) as i32;
    let y0 = (min_y - expand).floor().max(0.0) as i32;
    let x1 = (max_x + expand).ceil().min(w as f32).max(0.0) as i32;
    let y1 = (max_y + expand).ceil().min(h as f32).max(0.0) as i32;
    if x1 <= x0 || y1 <= y0 {
        None
    } else {
        Some((x0, y0, (x1 - x0) as u32, (y1 - y0) as u32))
    }
}

fn mask_expand(mask_filter: Option<&MaskFilterSpec>) -> f32 {
    match mask_filter {
        Some(MaskFilterSpec::Blur { radius, .. }) => radius * 3.0 + 2.0,
        None => 1.0,
    }
}

pub(crate) fn fill_path_filtered(
    target: &mut Pixmap,
    clip: Option<&Mask>,
    path: &tiny_skia::Path,
    paint: &tiny_skia::Paint,
    fill_rule: FillRule,
    transform: Transform,
    color_filter: Option<&ColorFilterSpec>,
    mask_filter: Option<&MaskFilterSpec>,
) {
    if color_filter.is_none() && mask_filter.is_none() {
        target.fill_path(path, paint, fill_rule, transform, clip);
        return;
    }
    // Rasterise only into the shape's device bounding box (expanded for the
    // blur) instead of allocating and blurring a full-screen buffer. This is
    // what makes elevation shadows cheap instead of O(canvas) each frame.
    let expand = mask_expand(mask_filter);
    let Some((x0, y0, tw, th)) = filter_device_bounds(path, transform, expand, target.width(), target.height())
    else {
        return;
    };
    let Some(mut tmp) = Pixmap::new(tw, th) else {
        return;
    };
    let shifted = transform.post_concat(Transform::from_translate(-x0 as f32, -y0 as f32));
    let mut src_paint = paint.clone();
    src_paint.blend_mode = BlendMode::Source;
    tmp.fill_path(path, &src_paint, fill_rule, shifted, None);
    if let Some(mf) = mask_filter {
        apply_mask_filter(&mut tmp, mf);
    }
    if let Some(cf) = color_filter {
        apply_color_filter(&mut tmp, cf);
    }
    let p = PixmapPaint {
        opacity: 1.0,
        blend_mode: paint.blend_mode,
        quality: tiny_skia::FilterQuality::Nearest,
    };
    target.draw_pixmap(x0, y0, tmp.as_ref(), &p, Transform::identity(), clip);
}

/// Rasterise a stroked path, applying colour/mask filters if present.
#[allow(clippy::too_many_arguments)]
pub(crate) fn stroke_path_filtered(
    target: &mut Pixmap,
    clip: Option<&Mask>,
    path: &tiny_skia::Path,
    paint: &tiny_skia::Paint,
    stroke: &tiny_skia::Stroke,
    transform: Transform,
    color_filter: Option<&ColorFilterSpec>,
    mask_filter: Option<&MaskFilterSpec>,
) {
    if color_filter.is_none() && mask_filter.is_none() {
        target.stroke_path(path, paint, stroke, transform, clip);
        return;
    }
    let expand = mask_expand(mask_filter) + stroke.width * 0.5 + 2.0;
    let Some((x0, y0, tw, th)) = filter_device_bounds(path, transform, expand, target.width(), target.height())
    else {
        return;
    };
    let Some(mut tmp) = Pixmap::new(tw, th) else {
        return;
    };
    let shifted = transform.post_concat(Transform::from_translate(-x0 as f32, -y0 as f32));
    let mut src_paint = paint.clone();
    src_paint.blend_mode = BlendMode::Source;
    tmp.stroke_path(path, &src_paint, stroke, shifted, None);
    if let Some(mf) = mask_filter {
        apply_mask_filter(&mut tmp, mf);
    }
    if let Some(cf) = color_filter {
        apply_color_filter(&mut tmp, cf);
    }
    let p = PixmapPaint {
        opacity: 1.0,
        blend_mode: paint.blend_mode,
        quality: tiny_skia::FilterQuality::Nearest,
    };
    target.draw_pixmap(x0, y0, tmp.as_ref(), &p, Transform::identity(), clip);
}

/// Apply a paint style to a path/shape. Returns the stroke if the paint
/// requests one, and whether a fill is requested.
pub(crate) fn emit_shape(
    target: &mut Pixmap,
    clip: Option<&Mask>,
    path: &tiny_skia::Path,
    paint: &Paint,
    sk_paint: &tiny_skia::Paint,
    fill_rule: FillRule,
    transform: Transform,
) {
    match paint.style {
        PaintStyle::Fill => fill_path_filtered(
            target,
            clip,
            path,
            sk_paint,
            fill_rule,
            transform,
            paint.color_filter.as_ref(),
            paint.mask_filter.as_ref(),
        ),
        PaintStyle::Stroke => stroke_path_filtered(
            target,
            clip,
            path,
            sk_paint,
            &paint.skia_stroke(),
            transform,
            paint.color_filter.as_ref(),
            paint.mask_filter.as_ref(),
        ),
        PaintStyle::FillAndStroke => {
            fill_path_filtered(
                target,
                clip,
                path,
                sk_paint,
                fill_rule,
                transform,
                paint.color_filter.as_ref(),
                paint.mask_filter.as_ref(),
            );
            stroke_path_filtered(
                target,
                clip,
                path,
                sk_paint,
                &paint.skia_stroke(),
                transform,
                paint.color_filter.as_ref(),
                paint.mask_filter.as_ref(),
            );
        }
    }
}

/// Paint a whole rectangle with a solid colour and blend mode.
pub(crate) fn fill_color(
    target: &mut Pixmap,
    clip: Option<&Mask>,
    color: u32,
    blend: BlendMode,
) {
    let mut paint = tiny_skia::Paint::default();
    paint.set_color(crate::color::argb_to_sk(color));
    paint.blend_mode = blend;
    paint.anti_alias = false;
    let rect = tiny_skia::Rect::from_xywh(0.0, 0.0, target.width() as f32, target.height() as f32)
        .unwrap();
    target.fill_rect(rect, &paint, Transform::identity(), clip);
}

/// Draw a bitmap into `target`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_bitmap(
    target: &mut Pixmap,
    clip: Option<&Mask>,
    pixmap: PixmapRef,
    src: Option<RectF>,
    dst: RectF,
    transform: Transform,
    paint: &Paint,
    filter: bool,
) {
    let quality = if filter {
        tiny_skia::FilterQuality::Bilinear
    } else {
        tiny_skia::FilterQuality::Nearest
    };
    // The local pattern transform maps the (possibly cropped) source rect to
    // the destination rect. Drawing at (0,0) with `transform` as geometry means
    // the pattern is transformed by both.
    let (w, h) = (pixmap.width() as f32, pixmap.height() as f32);
    let (src_left, src_top, src_w, src_h) = match src {
        Some(s) => (s.left, s.top, s.width().max(1e-6), s.height().max(1e-6)),
        None => (0.0, 0.0, w.max(1e-6), h.max(1e-6)),
    };
    let sx = dst.width() / src_w;
    let sy = dst.height() / src_h;
    let mut m = Matrix3x3::identity();
    m.m[0] = sx;
    m.m[4] = sy;
    m.m[2] = dst.left - src_left * sx;
    m.m[5] = dst.top - src_top * sy;
    let local = m.to_affine().unwrap_or(Transform::identity());
    let sk_paint = tiny_skia::Paint {
        shader: tiny_skia::Pattern::new(pixmap, tiny_skia::SpreadMode::Pad, quality, 1.0, local),
        blend_mode: paint.blend_mode,
        anti_alias: false,
        force_hq_pipeline: false,
    };
    let dst_path = rect_path(dst);
    target.fill_path(&dst_path, &sk_paint, FillRule::Winding, transform, clip);
}

/// Nine-patch: stretch the 9 regions.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_nine_patch(
    target: &mut Pixmap,
    clip: Option<&Mask>,
    pixmap: PixmapRef,
    src: Rect,
    dst: Rect,
    paint: &Paint,
    filter: bool,
) {
    let quality = if filter {
        tiny_skia::FilterQuality::Bilinear
    } else {
        tiny_skia::FilterQuality::Nearest
    };
    // The nine-patch chunk's actual divs are resolved by the caller into
    // `src`; here we simply scale the whole image into `dst`. Widget
    // backgrounds whose PNG has 1px border rows/columns are approximated by
    // this uniform scale (labelled approximate).
    let w = pixmap.width() as f32;
    let h = pixmap.height() as f32;
    let sx = dst.width() as f32 / w.max(1.0);
    let sy = dst.height() as f32 / h.max(1.0);
    let local = Transform::from_row(sx, 0.0, 0.0, sy, dst.left as f32, dst.top as f32);
    let sk_paint = tiny_skia::Paint {
        shader: tiny_skia::Pattern::new(pixmap, tiny_skia::SpreadMode::Pad, quality, 1.0, local),
        blend_mode: paint.blend_mode,
        anti_alias: true,
        force_hq_pipeline: false,
    };
    let dst_path = rect_path(dst.to_rectf());
    target.fill_path(&dst_path, &sk_paint, FillRule::Winding, Transform::identity(), clip);
    let _ = src;
}

/// Colour filter application over an RGBA8 pixmap.
pub(crate) fn apply_color_filter(pixmap: &mut Pixmap, filter: &ColorFilterSpec) {
    match filter {
        ColorFilterSpec::None => {}
        ColorFilterSpec::PorterDuff { color, mode } => {
            let (cr, cg, cb, ca) = unpack_unpremul(pack_word(*color));
            let src = [
                cr as f32 / 255.0,
                cg as f32 / 255.0,
                cb as f32 / 255.0,
                ca as f32 / 255.0,
            ];
            let mode = *mode;
            for p in pixmap.pixels_mut() {
                let (r, g, b, a) = unpack_unpremul(word_of(p));
                let dst = [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, a as f32 / 255.0];
                let out = crate::color::porter_duff_reference(mode, src, dst);
                set_word(p, out);
            }
        }
        ColorFilterSpec::BlendMode { color, mode } => {
            let blend = mode.to_blend();
            let c = *color;
            for p in pixmap.pixels_mut() {
                let (r, g, b, a) = unpack_unpremul(word_of(p));
                let (cr, cg, cb, ca) = unpack_unpremul(c);
                let (nr, ng, nb, na) = blend_pixel(blend, (cr, cg, cb, ca), (r, g, b, a));
                set_premul(p, nr, ng, nb, na);
            }
        }
        ColorFilterSpec::ColorMatrix { matrix } => {
            for p in pixmap.pixels_mut() {
                let (r, g, b, a) = unpack_unpremul(word_of(p));
                let m = matrix;
                let rr = m[0] * r as f32 + m[1] * g as f32 + m[2] * b as f32 + m[3] * a as f32 + m[4];
                let rg = m[5] * r as f32 + m[6] * g as f32 + m[7] * b as f32 + m[8] * a as f32 + m[9];
                let rb = m[10] * r as f32 + m[11] * g as f32 + m[12] * b as f32 + m[13] * a as f32 + m[14];
                let ra = m[15] * r as f32 + m[16] * g as f32 + m[17] * b as f32 + m[18] * a as f32 + m[19];
                let clamp = |v: f32| v.clamp(0.0, 255.0) as u8;
                set_premul(p, clamp(rr), clamp(rg), clamp(rb), clamp(ra));
            }
        }
    }
}

fn blend_pixel(
    blend: BlendMode,
    src: (u8, u8, u8, u8),
    dst: (u8, u8, u8, u8),
) -> (u8, u8, u8, u8) {
    // Map tiny-skia blend modes to un-premultiplied reference via a small
    // subset; unsupported blends fall back to source-over.
    let (sr, sg, sb, sa) = (
        src.0 as f32 / 255.0,
        src.1 as f32 / 255.0,
        src.2 as f32 / 255.0,
        src.3 as f32 / 255.0,
    );
    let (dr, dg, db, da) = (
        dst.0 as f32 / 255.0,
        dst.1 as f32 / 255.0,
        dst.2 as f32 / 255.0,
        dst.3 as f32 / 255.0,
    );
    let mode = match blend {
        BlendMode::Multiply => Some(crate::color::PorterDuffMode::Multiply),
        BlendMode::Screen => Some(crate::color::PorterDuffMode::Screen),
        BlendMode::Darken => Some(crate::color::PorterDuffMode::Darken),
        BlendMode::Lighten => Some(crate::color::PorterDuffMode::Lighten),
        BlendMode::Overlay => Some(crate::color::PorterDuffMode::Overlay),
        BlendMode::SourceIn => Some(crate::color::PorterDuffMode::SrcIn),
        BlendMode::DestinationIn => Some(crate::color::PorterDuffMode::DstIn),
        BlendMode::Source => Some(crate::color::PorterDuffMode::Src),
        BlendMode::Destination => Some(crate::color::PorterDuffMode::Dst),
        BlendMode::Plus => Some(crate::color::PorterDuffMode::Add),
        _ => None,
    };
    let out = match mode {
        Some(m) => crate::color::porter_duff_reference(m, [sr, sg, sb, sa], [dr, dg, db, da]),
        None => crate::color::porter_duff_reference(
            crate::color::PorterDuffMode::SrcOver,
            [sr, sg, sb, sa],
            [dr, dg, db, da],
        ),
    };
    (
        (out[0] * 255.0).clamp(0.0, 255.0) as u8,
        (out[1] * 255.0).clamp(0.0, 255.0) as u8,
        (out[2] * 255.0).clamp(0.0, 255.0) as u8,
        (out[3] * 255.0).clamp(0.0, 255.0) as u8,
    )
}

fn word_of(p: &tiny_skia::PremultipliedColorU8) -> u32 {
    (p.red() as u32)
        | ((p.green() as u32) << 8)
        | ((p.blue() as u32) << 16)
        | ((p.alpha() as u32) << 24)
}

fn set_word(p: &mut tiny_skia::PremultipliedColorU8, out: [f32; 4]) {
    let (r, g, b, a) = (
        (out[0] * 255.0).clamp(0.0, 255.0) as u8,
        (out[1] * 255.0).clamp(0.0, 255.0) as u8,
        (out[2] * 255.0).clamp(0.0, 255.0) as u8,
        (out[3] * 255.0).clamp(0.0, 255.0) as u8,
    );
    set_premul(p, r, g, b, a);
}

fn set_premul(p: &mut tiny_skia::PremultipliedColorU8, r: u8, g: u8, b: u8, a: u8) {
    let (pr, pg, pb) = (
        premultiply_channel(r, a),
        premultiply_channel(g, a),
        premultiply_channel(b, a),
    );
    if let Some(v) = tiny_skia::PremultipliedColorU8::from_rgba(pr, pg, pb, a) {
        *p = v;
    }
}

fn pack_word(argb: u32) -> u32 {
    let (r, g, b, a) = unpack_unpremul(argb | 0xff00_0000);
    crate::color::pack_premul(r, g, b, a)
}

/// Apply a mask filter (blur) over an RGBA8 pixmap. Box blur approximates a
/// Gaussian; fidelity is labelled "approximate".
pub(crate) fn apply_mask_filter(pixmap: &mut Pixmap, filter: &MaskFilterSpec) {
    match filter {
        MaskFilterSpec::Blur { radius, style } => {
            if *radius <= 0.0 {
                return;
            }
            let original: Option<Vec<u32>> = if *style != BlurStyle::Normal {
                Some(pixmap.pixels().iter().map(word_of).collect())
            } else {
                None
            };
            box_blur(pixmap, *radius);
            if let Some(orig) = original {
                match style {
                    BlurStyle::Outer => {
                        // Blurred minus original (shadow outside the shape).
                        for (i, p) in pixmap.pixels_mut().iter_mut().enumerate() {
                            let (br, bg, bb, ba) = unpack_unpremul(word_of(p));
                            let (_or, _og, _ob, oa) = unpack_unpremul(orig[i]);
                            let na = ba.saturating_sub(oa);
                            set_premul(p, br, bg, bb, na);
                        }
                    }
                    BlurStyle::Inner => {
                        // Original minus blurred, clipped to original alpha.
                        for (i, p) in pixmap.pixels_mut().iter_mut().enumerate() {
                            let (_br, _bg, _bb, ba) = unpack_unpremul(word_of(p));
                            let (or, og, ob, oa) = unpack_unpremul(orig[i]);
                            let na = oa.saturating_sub(ba).min(oa);
                            set_premul(p, or, og, ob, na);
                        }
                    }
                    BlurStyle::Solid => {
                        // Opaque inside the original shape.
                        for (i, p) in pixmap.pixels_mut().iter_mut().enumerate() {
                            let (or, og, ob, oa) = unpack_unpremul(orig[i]);
                            set_premul(p, or, og, ob, oa);
                        }
                    }
                    BlurStyle::Normal => {}
                }
            }
        }
    }
}

/// Three box-blur passes approximate a Gaussian well enough for the shadow
/// workload; documented as approximate in the fidelity list.
fn box_blur(pixmap: &mut Pixmap, radius: f32) {
    let r = radius.round().max(1.0) as i32;
    for _ in 0..3 {
        blur_pass(pixmap, r);
    }
    fn blur_pass(pixmap: &mut Pixmap, r: i32) {
        let w = pixmap.width() as i32;
        let h = pixmap.height() as i32;
        let src: Vec<u32> = pixmap.pixels().iter().map(word_of).collect();
        let mut out = src.clone();
        // Horizontal.
        let mut tmp = vec![0u32; src.len()];
        for y in 0..h {
            let mut sum = [0u32; 4];
            let mut count = 0i32;
            for x in -r..=r {
                if (0..w).contains(&x) {
                    add_px(&mut sum, src[(y * w + x) as usize]);
                    count += 1;
                }
            }
            for x in 0..w {
                tmp[(y * w + x) as usize] = avg_px(&sum, count);
                let out_x = x - r;
                if (0..w).contains(&out_x) {
                    sub_px(&mut sum, src[(y * w + out_x) as usize]);
                    count -= 1;
                }
                let in_x = x + r + 1;
                if (0..w).contains(&in_x) {
                    add_px(&mut sum, src[(y * w + in_x) as usize]);
                    count += 1;
                }
            }
        }
        // Vertical.
        for x in 0..w {
            let mut sum = [0u32; 4];
            let mut count = 0i32;
            for y in -r..=r {
                if (0..h).contains(&y) {
                    add_px(&mut sum, tmp[(y * w + x) as usize]);
                    count += 1;
                }
            }
            for y in 0..h {
                out[(y * w + x) as usize] = avg_px(&sum, count);
                let out_y = y - r;
                if (0..h).contains(&out_y) {
                    sub_px(&mut sum, tmp[(out_y * w + x) as usize]);
                    count -= 1;
                }
                let in_y = y + r + 1;
                if (0..h).contains(&in_y) {
                    add_px(&mut sum, tmp[(in_y * w + x) as usize]);
                    count += 1;
                }
            }
        }
        for (p, word) in pixmap.pixels_mut().iter_mut().zip(out.iter()) {
            let (r, g, b, a) = unpack_unpremul(*word);
            set_premul(p, r, g, b, a);
        }
    }
    fn add_px(sum: &mut [u32; 4], word: u32) {
        let (r, g, b, a) = unpack_unpremul(word);
        sum[0] += r as u32;
        sum[1] += g as u32;
        sum[2] += b as u32;
        sum[3] += a as u32;
    }
    fn sub_px(sum: &mut [u32; 4], word: u32) {
        let (r, g, b, a) = unpack_unpremul(word);
        sum[0] = sum[0].saturating_sub(r as u32);
        sum[1] = sum[1].saturating_sub(g as u32);
        sum[2] = sum[2].saturating_sub(b as u32);
        sum[3] = sum[3].saturating_sub(a as u32);
    }
    fn avg_px(sum: &[u32; 4], count: i32) -> u32 {
        if count <= 0 {
            return 0;
        }
        let c = count as u32;
        let r = (sum[0] / c) as u8;
        let g = (sum[1] / c) as u8;
        let b = (sum[2] / c) as u8;
        let a = (sum[3] / c) as u8;
        crate::color::pack_premul(r, g, b, a)
    }
}

