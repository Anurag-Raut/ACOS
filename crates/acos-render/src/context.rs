//! `RenderContext`: the handle-table owner and the API the `android.graphics`
//! natives bind to.
//!
//! Every object (bitmap, canvas, paint, path, matrix, region, typeface,
//! render node, surface) lives here behind a generation-checked [`Handle`].
//! Methods take primitive arguments only — no Rust lifetimes leak across the
//! API — so the JNI binding layer can call them with `jlong` handles and
//! `jfloat`/`jint` values.
//!
//! The mapping from framework natives (e.g. `Canvas.nDrawRect`,
//! `Paint.nSetColor`, `RenderNode.nSetLeftTopRightBottom`) to methods here is
//! documented in `results/render/README.md`.

use std::sync::Arc;

use crate::bitmap::{Bitmap, BitmapConfig};
use crate::canvas::{self, Canvas, CanvasTarget, ClipOp};
use crate::error::{RenderError, RenderResult};
use crate::geom::{NinePatchChunk, Outline, Rect, RectF};
use crate::handle::{Handle, HandleTable};
use crate::image;
use crate::matrix::Matrix3x3;
use crate::color::{AndroidBlendMode, PorterDuffMode};
use crate::paint::{
    BlurStyle, ColorFilterSpec, MaskFilterSpec, Paint, PaintStyle, PathEffectSpec,
    RenderEffectSpec, ShaderSpec, StrokeCap, StrokeJoin, TextAlign, TileMode,
};
use crate::path::{FillType, Path, PathOp};
use crate::region::{Region, RegionOp};
use crate::render_node::{DisplayOp, LayerType, RenderNode};
use crate::surface::{Frame, PngSetting, Surface};
use crate::text::fonts::{FontCollection, FontRef};
use crate::text::{FontMetrics, FontMetricsInt, TextEngine, TextStyle};

/// Diagnostic switch: force text through the outline path even when the
/// FreeType mask path is available. Used to benchmark/compare the two paths in
/// one build (`ACOS_RENDER_FORCE_TEXT_PATH=1`).
fn force_text_path() -> bool {
    static FLAG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FLAG.get_or_init(|| std::env::var_os("ACOS_RENDER_FORCE_TEXT_PATH").is_some())
}

/// Node properties snapshotted for rendering (avoids holding a borrow on the
/// node table while rasterising).
#[derive(Clone)]
struct NodeProps {
    matrix: Matrix3x3,
    alpha: f32,
    clip_to_bounds: bool,
    clip_to_outline: bool,
    outline: Outline,
    elevation: f32,
    ambient_shadow_color: u32,
    spot_shadow_color: u32,
    local_bounds: RectF,
    render_effect: Option<RenderEffectSpec>,
}

/// The renderer context.
pub struct RenderContext {
    pub bitmaps: HandleTable<Bitmap>,
    pub canvases: HandleTable<Canvas>,
    pub paints: HandleTable<Paint>,
    pub paths: HandleTable<Path>,
    pub matrices: HandleTable<Matrix3x3>,
    pub regions: HandleTable<Region>,
    pub typefaces: HandleTable<FontRef>,
    pub render_nodes: HandleTable<RenderNode>,
    pub surfaces: HandleTable<Surface>,

    /// Text engine (present iff fonts were found).
    pub text: Option<TextEngine>,

    /// Unsupported ops observed during the current frame.
    unsupported_ops: Vec<String>,
    /// Strict mode escalates unsupported ops to errors.
    pub strict: bool,
    /// Total draw operations rasterised since creation (diagnostics).
    pub raster_ops: u64,
    /// Fonts that failed to load are reported here.
    pub font_errors: Vec<String>,
}

impl Default for RenderContext {
    fn default() -> Self {
        Self::new()
    }
}

impl RenderContext {
    /// Create a context, loading real Android fonts when available.
    ///
    /// The font collection is shared process-wide (mmap'd once), so creating
    /// many contexts/surfaces does not re-load or re-map fonts.
    pub fn new() -> Self {
        let text = FontCollection::shared().map(TextEngine::new);
        RenderContext {
            bitmaps: HandleTable::new(),
            canvases: HandleTable::new(),
            paints: HandleTable::new(),
            paths: HandleTable::new(),
            matrices: HandleTable::new(),
            regions: HandleTable::new(),
            typefaces: HandleTable::new(),
            render_nodes: HandleTable::new(),
            surfaces: HandleTable::new(),
            text,
            unsupported_ops: Vec::new(),
            strict: false,
            raster_ops: 0,
            font_errors: Vec::new(),
        }
    }

    /// Create a context with an explicit font collection.
    pub fn with_fonts(fonts: FontCollection) -> Self {
        let mut ctx = RenderContext::new();
        ctx.text = Some(TextEngine::new(Arc::new(fonts)));
        ctx
    }

    pub fn has_fonts(&self) -> bool {
        self.text.is_some()
    }

    pub fn text_engine(&self) -> RenderResult<&TextEngine> {
        self.text
            .as_ref()
            .ok_or_else(|| RenderError::FontNotFound {
                name: "no font collection loaded (set ACOS_FONT_DIR)".to_string(),
            })
    }

    fn text_engine_mut(&mut self) -> RenderResult<&mut TextEngine> {
        self.text
            .as_mut()
            .ok_or_else(|| RenderError::FontNotFound {
                name: "no font collection loaded (set ACOS_FONT_DIR)".to_string(),
            })
    }

    /// Record or escalate an unsupported op.
    pub fn unsupported(&mut self, op: impl Into<String>) -> RenderResult<()> {
        let op = op.into();
        if self.strict {
            return Err(RenderError::Strict { msg: op });
        }
        if !self.unsupported_ops.iter().any(|o| o == &op) {
            self.unsupported_ops.push(op);
        }
        Ok(())
    }

    /// Unsupported ops seen in the last frame.
    pub fn unsupported_ops(&self) -> &[String] {
        &self.unsupported_ops
    }

    fn take_unsupported(&mut self) -> Vec<String> {
        std::mem::take(&mut self.unsupported_ops)
    }

    // -----------------------------------------------------------------
    // Bitmaps
    // -----------------------------------------------------------------

    pub fn bitmap_create(&mut self, width: u32, height: u32, config: BitmapConfig) -> RenderResult<Handle> {
        Ok(self.bitmaps.insert(Bitmap::new(width, height, config)?))
    }

    /// Decode PNG/JPEG/WebP bytes into a bitmap.
    pub fn bitmap_create_from_bytes(&mut self, data: &[u8]) -> RenderResult<Handle> {
        let decoded = image::decode(data)?;
        Ok(self.bitmaps.insert(Bitmap::from_decoded(decoded)?))
    }

    pub fn bitmap_erase(&mut self, bitmap: Handle, argb: u32) -> RenderResult<()> {
        self.bitmaps.get_mut(bitmap).ok_or(RenderError::BadHandle)?.erase(argb);
        Ok(())
    }

    pub fn bitmap_get_pixel(&self, bitmap: Handle, x: i32, y: i32) -> RenderResult<u32> {
        Ok(self.bitmaps.get(bitmap).ok_or(RenderError::BadHandle)?.get_pixel(x, y))
    }

    pub fn bitmap_set_pixel(&mut self, bitmap: Handle, x: i32, y: i32, argb: u32) -> RenderResult<()> {
        self.bitmaps
            .get_mut(bitmap)
            .ok_or(RenderError::BadHandle)?
            .set_pixel(x, y, argb);
        Ok(())
    }

    pub fn bitmap_width(&self, bitmap: Handle) -> RenderResult<u32> {
        Ok(self.bitmaps.get(bitmap).ok_or(RenderError::BadHandle)?.width())
    }

    pub fn bitmap_height(&self, bitmap: Handle) -> RenderResult<u32> {
        Ok(self.bitmaps.get(bitmap).ok_or(RenderError::BadHandle)?.height())
    }

    pub fn bitmap_config(&self, bitmap: Handle) -> RenderResult<i32> {
        Ok(self.bitmaps.get(bitmap).ok_or(RenderError::BadHandle)?.config() as i32)
    }

    pub fn bitmap_copy(&mut self, bitmap: Handle, config: Option<BitmapConfig>) -> RenderResult<Handle> {
        let b = self.bitmaps.get(bitmap).ok_or(RenderError::BadHandle)?.copy(config, true)?;
        Ok(self.bitmaps.insert(b))
    }

    pub fn bitmap_extract_alpha(&mut self, bitmap: Handle) -> RenderResult<Handle> {
        let b = self
            .bitmaps
            .get(bitmap)
            .ok_or(RenderError::BadHandle)?
            .extract_alpha()?;
        Ok(self.bitmaps.insert(b))
    }

    pub fn bitmap_pixels(&self, bitmap: Handle) -> RenderResult<Vec<u32>> {
        Ok(self.bitmaps.get(bitmap).ok_or(RenderError::BadHandle)?.pixels().to_vec())
    }

    /// Android `Bitmap.compress(PNG, quality, stream)`.
    pub fn bitmap_compress_png(&self, bitmap: Handle) -> RenderResult<Vec<u8>> {
        let pm = self
            .bitmaps
            .get(bitmap)
            .ok_or(RenderError::BadHandle)?
            .to_pixmap()?;
        Ok(crate::surface::encode_png(&pm, PngSetting::FastFiltered))
    }

    // -----------------------------------------------------------------
    // Canvases
    // -----------------------------------------------------------------

    pub fn canvas_for_bitmap(&mut self, bitmap: Handle) -> RenderResult<Handle> {
        let (w, h) = {
            let b = self.bitmaps.get(bitmap).ok_or(RenderError::BadHandle)?;
            (b.width(), b.height())
        };
        Ok(self
            .canvases
            .insert(Canvas::new(w, h, Some(CanvasTarget::Bitmap(bitmap)))))
    }

    pub fn canvas_for_surface(&mut self, surface: Handle) -> RenderResult<Handle> {
        let (w, h, existing) = {
            let s = self.surfaces.get(surface).ok_or(RenderError::BadHandle)?;
            (s.width, s.height, s.canvas)
        };
        if let Some(c) = existing {
            return Ok(c);
        }
        let canvas = self
            .canvases
            .insert(Canvas::new(w, h, Some(CanvasTarget::Surface(surface))));
        self.surfaces.get_mut(surface).unwrap().ensure_canvas(canvas);
        Ok(canvas)
    }

    /// Create a recording canvas for a render node.
    pub fn canvas_for_render_node(&mut self, node: Handle) -> RenderResult<Handle> {
        let _ = self.render_nodes.get(node).ok_or(RenderError::BadHandle)?;
        Ok(self
            .canvases
            .insert(Canvas::new(1, 1, Some(CanvasTarget::RenderNode(node)))))
    }

    /// Resolve the active pixmap target for a surface canvas.
    fn canvas_parts_mut(
        &mut self,
        surface: Handle,
    ) -> RenderResult<(&canvas::CanvasState, &mut tiny_skia::Pixmap)> {
        let canvas_h = self
            .surfaces
            .get(surface)
            .and_then(|s| s.canvas)
            .ok_or(RenderError::BadHandle)?;
        let surface_ref = self.surfaces.get_mut(surface).ok_or(RenderError::BadHandle)?;
        let canvas_ref = self.canvases.get_mut(canvas_h).ok_or(RenderError::BadHandle)?;
        let canvas::Canvas {
            state,
            layers,
            target: _,
            ..
        } = canvas_ref;
        let target: &mut tiny_skia::Pixmap = match layers.last_mut() {
            Some(layer) => &mut layer.pixmap,
            None => &mut surface_ref.pixmap,
        };
        Ok((state, target))
    }

    pub fn canvas_save(&mut self, surface: Handle) -> RenderResult<i32> {
        Ok(self.surface_canvas_mut(surface)?.save())
    }

    pub fn canvas_restore(&mut self, surface: Handle) -> RenderResult<bool> {
        let canvas_h = self
            .surfaces
            .get(surface)
            .and_then(|s| s.canvas)
            .ok_or(RenderError::BadHandle)?;
        let surface_ref = self.surfaces.get_mut(surface).ok_or(RenderError::BadHandle)?;
        let canvas_ref = self.canvases.get_mut(canvas_h).ok_or(RenderError::BadHandle)?;
        Ok(canvas_ref.restore(&mut surface_ref.pixmap))
    }

    pub fn canvas_restore_to_count(&mut self, surface: Handle, count: i32) -> RenderResult<()> {
        let canvas_h = self
            .surfaces
            .get(surface)
            .and_then(|s| s.canvas)
            .ok_or(RenderError::BadHandle)?;
        let surface_ref = self.surfaces.get_mut(surface).ok_or(RenderError::BadHandle)?;
        let canvas_ref = self.canvases.get_mut(canvas_h).ok_or(RenderError::BadHandle)?;
        canvas_ref.restore_to_count(&mut surface_ref.pixmap, count);
        Ok(())
    }

    pub fn canvas_get_save_count(&mut self, surface: Handle) -> RenderResult<i32> {
        Ok(self.surface_canvas_mut(surface)?.save_count())
    }

    fn surface_canvas_mut(&mut self, surface: Handle) -> RenderResult<&mut Canvas> {
        let canvas_h = self
            .surfaces
            .get(surface)
            .and_then(|s| s.canvas)
            .ok_or(RenderError::BadHandle)?;
        self.canvases.get_mut(canvas_h).ok_or(RenderError::BadHandle)
    }

    fn surface_canvas(&self, surface: Handle) -> RenderResult<&Canvas> {
        let canvas_h = self
            .surfaces
            .get(surface)
            .and_then(|s| s.canvas)
            .ok_or(RenderError::BadHandle)?;
        self.canvases.get(canvas_h).ok_or(RenderError::BadHandle)
    }

    pub fn canvas_save_layer_alpha(
        &mut self,
        surface: Handle,
        bounds: Option<RectF>,
        alpha: u8,
    ) -> RenderResult<i32> {
        self.surface_canvas_mut(surface)?.save_layer_alpha(bounds, alpha)
    }

    pub fn canvas_concat(&mut self, surface: Handle, matrix: Handle) -> RenderResult<()> {
        let m = *self.matrices.get(matrix).ok_or(RenderError::BadHandle)?;
        self.surface_canvas_mut(surface)?.concat(&m);
        Ok(())
    }

    pub fn canvas_set_matrix(&mut self, surface: Handle, matrix: Handle) -> RenderResult<()> {
        let m = *self.matrices.get(matrix).ok_or(RenderError::BadHandle)?;
        self.surface_canvas_mut(surface)?.set_matrix(&m);
        Ok(())
    }

    pub fn canvas_translate(&mut self, surface: Handle, dx: f32, dy: f32) -> RenderResult<()> {
        self.surface_canvas_mut(surface)?.translate(dx, dy);
        Ok(())
    }

    pub fn canvas_scale(&mut self, surface: Handle, sx: f32, sy: f32) -> RenderResult<()> {
        self.surface_canvas_mut(surface)?.scale(sx, sy);
        Ok(())
    }

    pub fn canvas_rotate(&mut self, surface: Handle, degrees: f32) -> RenderResult<()> {
        self.surface_canvas_mut(surface)?.rotate(degrees);
        Ok(())
    }

    pub fn canvas_skew(&mut self, surface: Handle, kx: f32, ky: f32) -> RenderResult<()> {
        self.surface_canvas_mut(surface)?.skew(kx, ky);
        Ok(())
    }

    pub fn canvas_get_matrix(&mut self, surface: Handle) -> RenderResult<Matrix3x3> {
        Ok(self.surface_canvas_mut(surface)?.get_matrix())
    }

    pub fn canvas_clip_rect(
        &mut self,
        surface: Handle,
        rect: RectF,
        op: ClipOp,
        aa: bool,
    ) -> RenderResult<()> {
        self.surface_canvas_mut(surface)?.clip_rect(rect, op, aa)
    }

    pub fn canvas_clip_path(&mut self, surface: Handle, path: Handle, op: ClipOp, aa: bool) -> RenderResult<()> {
        let p = self.paths.get(path).ok_or(RenderError::BadHandle)?.clone();
        self.surface_canvas_mut(surface)?.clip_path(&p, op, aa)
    }

    fn canvas_clip_path_value(
        &mut self,
        surface: Handle,
        path: &Path,
        op: ClipOp,
        aa: bool,
    ) -> RenderResult<()> {
        self.surface_canvas_mut(surface)?.clip_path(path, op, aa)
    }

    pub fn canvas_quick_reject(&mut self, surface: Handle, rect: RectF) -> RenderResult<bool> {
        Ok(self.surface_canvas(surface)?.quick_reject(rect))
    }

    pub fn canvas_get_clip_bounds(&mut self, surface: Handle) -> RenderResult<Rect> {
        Ok(self.surface_canvas(surface)?.get_clip_bounds())
    }

    pub fn canvas_width(&self, surface: Handle) -> RenderResult<u32> {
        Ok(self.surfaces.get(surface).ok_or(RenderError::BadHandle)?.width)
    }

    pub fn canvas_height(&self, surface: Handle) -> RenderResult<u32> {
        Ok(self.surfaces.get(surface).ok_or(RenderError::BadHandle)?.height)
    }

    pub fn canvas_is_opaque(&mut self, surface: Handle) -> RenderResult<bool> {
        Ok(self.surface_canvas(surface)?.is_opaque)
    }

    // -----------------------------------------------------------------
    // Surfaces + frame contract
    // -----------------------------------------------------------------

    pub fn surface_create(&mut self, width: u32, height: u32, density: f32) -> RenderResult<Handle> {
        Ok(self.surfaces.insert(Surface::new(width, height, density)?))
    }

    /// Present a new frame after the caller has drawn directly into the canvas.
    pub fn surface_present(&mut self, surface: Handle, cause_id: i64) -> RenderResult<u64> {
        let unsupported = self.take_unsupported();
        Ok(self
            .surfaces
            .get_mut(surface)
            .ok_or(RenderError::BadHandle)?
            .present(cause_id, unsupported))
    }

    pub fn surface_capture(&self, surface: Handle) -> RenderResult<Option<Frame>> {
        Ok(self.surfaces.get(surface).ok_or(RenderError::BadHandle)?.capture())
    }

    pub fn surface_capture_fresh(&self, surface: Handle, after_seq: u64) -> RenderResult<Frame> {
        self.surfaces
            .get(surface)
            .ok_or(RenderError::BadHandle)?
            .capture_fresh(after_seq)
    }

    pub fn surface_last_seq(&self, surface: Handle) -> RenderResult<u64> {
        Ok(self.surfaces.get(surface).ok_or(RenderError::BadHandle)?.last_seq())
    }

    /// Encode the latest frame as PNG with the requested setting.
    pub fn surface_png(&self, surface: Handle, setting: PngSetting) -> RenderResult<Vec<u8>> {
        let s = self.surfaces.get(surface).ok_or(RenderError::BadHandle)?;
        let frame = s.capture().ok_or(RenderError::NoFreshFrame {
            after: 0,
            latest: 0,
        })?;
        Ok(frame.png(setting))
    }

    pub fn surface_rgba8(&self, surface: Handle) -> RenderResult<Vec<u8>> {
        let s = self.surfaces.get(surface).ok_or(RenderError::BadHandle)?;
        let frame = s.capture().ok_or(RenderError::NoFreshFrame {
            after: 0,
            latest: 0,
        })?;
        Ok(frame.rgba8().to_vec())
    }

    // -----------------------------------------------------------------
    // Paints
    // -----------------------------------------------------------------

    pub fn paint_create(&mut self) -> Handle {
        self.paints.insert(Paint::default())
    }

    fn paint_mut(&mut self, paint: Handle) -> RenderResult<&mut Paint> {
        self.paints.get_mut(paint).ok_or(RenderError::BadHandle)
    }

    fn paint_ref(&self, paint: Handle) -> RenderResult<&Paint> {
        self.paints.get(paint).ok_or(RenderError::BadHandle)
    }

    pub fn paint_set_color(&mut self, paint: Handle, argb: u32) -> RenderResult<()> {
        self.paint_mut(paint)?.color = argb;
        Ok(())
    }

    pub fn paint_get_color(&self, paint: Handle) -> RenderResult<u32> {
        Ok(self.paint_ref(paint)?.color)
    }

    pub fn paint_set_alpha(&mut self, paint: Handle, alpha: u32) -> RenderResult<()> {
        self.paint_mut(paint)?.alpha = alpha.min(255);
        Ok(())
    }

    pub fn paint_set_style(&mut self, paint: Handle, style: i32) -> RenderResult<()> {
        let style = PaintStyle::from_ordinal(style)
            .ok_or_else(|| RenderError::invalid("bad Paint.Style"))?;
        self.paint_mut(paint)?.style = style;
        Ok(())
    }

    pub fn paint_get_style(&self, paint: Handle) -> RenderResult<i32> {
        Ok(self.paint_ref(paint)?.style as i32)
    }

    pub fn paint_set_stroke_width(&mut self, paint: Handle, w: f32) -> RenderResult<()> {
        self.paint_mut(paint)?.stroke_width = w;
        Ok(())
    }

    pub fn paint_get_stroke_width(&self, paint: Handle) -> RenderResult<f32> {
        Ok(self.paint_ref(paint)?.stroke_width)
    }

    pub fn paint_set_stroke_cap(&mut self, paint: Handle, cap: i32) -> RenderResult<()> {
        let cap = StrokeCap::from_ordinal(cap).ok_or_else(|| RenderError::invalid("bad Cap"))?;
        self.paint_mut(paint)?.stroke_cap = cap;
        Ok(())
    }

    pub fn paint_set_stroke_join(&mut self, paint: Handle, join: i32) -> RenderResult<()> {
        let join = StrokeJoin::from_ordinal(join).ok_or_else(|| RenderError::invalid("bad Join"))?;
        self.paint_mut(paint)?.stroke_join = join;
        Ok(())
    }

    pub fn paint_set_stroke_miter(&mut self, paint: Handle, m: f32) -> RenderResult<()> {
        self.paint_mut(paint)?.stroke_miter = m;
        Ok(())
    }

    pub fn paint_set_anti_alias(&mut self, paint: Handle, v: bool) -> RenderResult<()> {
        self.paint_mut(paint)?.anti_alias = v;
        Ok(())
    }

    pub fn paint_set_dither(&mut self, paint: Handle, v: bool) -> RenderResult<()> {
        self.paint_mut(paint)?.dither = v;
        Ok(())
    }

    pub fn paint_set_filter_bitmap(&mut self, paint: Handle, v: bool) -> RenderResult<()> {
        self.paint_mut(paint)?.filter_bitmap = v;
        Ok(())
    }

    /// Android `Paint.setXfermode(PorterDuffXfermode(mode))` — mode ordinals.
    pub fn paint_set_xfermode(&mut self, paint: Handle, mode: i32) -> RenderResult<()> {
        let pd = PorterDuffMode::from_ordinal(mode)
            .ok_or_else(|| RenderError::invalid("bad PorterDuff mode"))?;
        let p = self.paint_mut(paint)?;
        p.blend_mode = pd.to_blend();
        p.blend_mode_android = None;
        Ok(())
    }

    /// Android `Paint.setBlendMode(BlendMode)`.
    pub fn paint_set_blend_mode(&mut self, paint: Handle, mode: i32) -> RenderResult<()> {
        let bm = AndroidBlendMode::from_ordinal(mode)
            .ok_or_else(|| RenderError::invalid("bad BlendMode"))?;
        let p = self.paint_mut(paint)?;
        p.blend_mode = bm.to_blend();
        p.blend_mode_android = Some(bm);
        Ok(())
    }

    pub fn paint_set_shader_linear(
        &mut self,
        paint: Handle,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        colors: Vec<u32>,
        positions: Option<Vec<f32>>,
        tile: TileMode,
        local: Option<Matrix3x3>,
    ) -> RenderResult<()> {
        self.paint_mut(paint)?.shader = Some(ShaderSpec::Linear {
            start: (x0, y0),
            end: (x1, y1),
            colors,
            positions,
            tile,
            local: local.unwrap_or_default(),
        });
        Ok(())
    }

    pub fn paint_set_shader_radial(
        &mut self,
        paint: Handle,
        cx: f32,
        cy: f32,
        radius: f32,
        colors: Vec<u32>,
        positions: Option<Vec<f32>>,
        tile: TileMode,
        local: Option<Matrix3x3>,
    ) -> RenderResult<()> {
        self.paint_mut(paint)?.shader = Some(ShaderSpec::Radial {
            center: (cx, cy),
            radius,
            colors,
            positions,
            tile,
            local: local.unwrap_or_default(),
            focal: (cx, cy),
        });
        Ok(())
    }

    pub fn paint_set_shader_sweep(
        &mut self,
        paint: Handle,
        cx: f32,
        cy: f32,
        colors: Vec<u32>,
        positions: Option<Vec<f32>>,
        local: Option<Matrix3x3>,
    ) -> RenderResult<()> {
        self.paint_mut(paint)?.shader = Some(ShaderSpec::Sweep {
            center: (cx, cy),
            colors,
            positions,
            local: local.unwrap_or_default(),
        });
        Ok(())
    }

    pub fn paint_set_shader_bitmap(
        &mut self,
        paint: Handle,
        bitmap: Handle,
        tile_x: TileMode,
        tile_y: TileMode,
        local: Option<Matrix3x3>,
    ) -> RenderResult<()> {
        if !self.bitmaps.contains(bitmap) {
            return Err(RenderError::BadHandle);
        }
        if tile_x != tile_y {
            self.unsupported("BitmapShader independent tileX/tileY")?;
        }
        self.paint_mut(paint)?.shader = Some(ShaderSpec::Bitmap {
            bitmap,
            tile_x,
            tile_y,
            local: local.unwrap_or_default(),
        });
        Ok(())
    }

    pub fn paint_clear_shader(&mut self, paint: Handle) -> RenderResult<()> {
        self.paint_mut(paint)?.shader = None;
        Ok(())
    }

    pub fn paint_set_color_filter_porter_duff(
        &mut self,
        paint: Handle,
        color: u32,
        mode: i32,
    ) -> RenderResult<()> {
        let mode = PorterDuffMode::from_ordinal(mode)
            .ok_or_else(|| RenderError::invalid("bad PorterDuff mode"))?;
        self.paint_mut(paint)?.color_filter = Some(ColorFilterSpec::PorterDuff { color, mode });
        Ok(())
    }

    pub fn paint_set_color_filter_blend_mode(
        &mut self,
        paint: Handle,
        color: u32,
        mode: i32,
    ) -> RenderResult<()> {
        let mode = AndroidBlendMode::from_ordinal(mode)
            .ok_or_else(|| RenderError::invalid("bad BlendMode"))?;
        self.paint_mut(paint)?.color_filter = Some(ColorFilterSpec::BlendMode { color, mode });
        Ok(())
    }

    pub fn paint_set_color_filter_matrix(&mut self, paint: Handle, matrix: [f32; 20]) -> RenderResult<()> {
        self.paint_mut(paint)?.color_filter = Some(ColorFilterSpec::ColorMatrix { matrix });
        Ok(())
    }

    pub fn paint_set_mask_filter_blur(&mut self, paint: Handle, radius: f32, style: i32) -> RenderResult<()> {
        let style = BlurStyle::from_ordinal(style).ok_or_else(|| RenderError::invalid("bad Blur style"))?;
        self.paint_mut(paint)?.mask_filter = Some(MaskFilterSpec::Blur { radius, style });
        Ok(())
    }

    pub fn paint_set_path_effect_dash(&mut self, paint: Handle, intervals: Vec<f32>, phase: f32) -> RenderResult<()> {
        self.paint_mut(paint)?.path_effect = Some(PathEffectSpec::Dash { intervals, phase });
        Ok(())
    }

    pub fn paint_set_path_effect_corner(&mut self, paint: Handle, radius: f32) -> RenderResult<()> {
        self.paint_mut(paint)?.path_effect = Some(PathEffectSpec::Corner { radius });
        Ok(())
    }

    pub fn paint_set_shadow_layer(
        &mut self,
        paint: Handle,
        radius: f32,
        dx: f32,
        dy: f32,
        color: u32,
    ) -> RenderResult<()> {
        self.paint_mut(paint)?.shadow_layer = Some(crate::paint::ShadowLayer {
            radius,
            dx,
            dy,
            color,
        });
        Ok(())
    }

    // Text attributes.
    pub fn paint_set_text_size(&mut self, paint: Handle, size: f32) -> RenderResult<()> {
        self.paint_mut(paint)?.text_size = size;
        Ok(())
    }

    pub fn paint_get_text_size(&self, paint: Handle) -> RenderResult<f32> {
        Ok(self.paint_ref(paint)?.text_size)
    }

    pub fn paint_set_text_align(&mut self, paint: Handle, align: i32) -> RenderResult<()> {
        let align = TextAlign::from_ordinal(align).ok_or_else(|| RenderError::invalid("bad Align"))?;
        self.paint_mut(paint)?.text_align = align;
        Ok(())
    }

    pub fn paint_set_text_scale_x(&mut self, paint: Handle, scale: f32) -> RenderResult<()> {
        self.paint_mut(paint)?.text_scale_x = scale;
        Ok(())
    }

    pub fn paint_set_text_skew_x(&mut self, paint: Handle, skew: f32) -> RenderResult<()> {
        self.paint_mut(paint)?.text_skew_x = skew;
        Ok(())
    }

    pub fn paint_set_letter_spacing(&mut self, paint: Handle, spacing: f32) -> RenderResult<()> {
        self.paint_mut(paint)?.letter_spacing = spacing;
        Ok(())
    }

    pub fn paint_set_fake_bold(&mut self, paint: Handle, v: bool) -> RenderResult<()> {
        self.paint_mut(paint)?.fake_bold = v;
        Ok(())
    }

    pub fn paint_set_underline(&mut self, paint: Handle, v: bool) -> RenderResult<()> {
        self.paint_mut(paint)?.underline = v;
        Ok(())
    }

    pub fn paint_set_strike_thru(&mut self, paint: Handle, v: bool) -> RenderResult<()> {
        self.paint_mut(paint)?.strike_thru = v;
        Ok(())
    }

    pub fn paint_set_font_feature_settings(&mut self, paint: Handle, settings: Option<String>) -> RenderResult<()> {
        self.paint_mut(paint)?.font_feature_settings = settings;
        Ok(())
    }

    pub fn paint_set_font_variation_settings(&mut self, paint: Handle, settings: Option<String>) -> RenderResult<()> {
        self.paint_mut(paint)?.font_variation_settings = settings;
        Ok(())
    }

    pub fn paint_set_typeface(&mut self, paint: Handle, typeface: Handle) -> RenderResult<()> {
        if !self.typefaces.contains(typeface) {
            return Err(RenderError::BadHandle);
        }
        self.paint_mut(paint)?.typeface = Some(typeface);
        Ok(())
    }

    // -----------------------------------------------------------------
    // Typefaces
    // -----------------------------------------------------------------

    pub fn typeface_create(&mut self, family: Option<&str>, weight: u16, italic: bool) -> RenderResult<Handle> {
        let font = self
            .text_engine()?
            .fonts()
            .resolve_typeface(family, weight, italic)
            .ok_or_else(|| RenderError::FontNotFound {
                name: family.unwrap_or("sans-serif").to_string(),
            })?;
        Ok(self.typefaces.insert(font))
    }

    pub fn typeface_default(&mut self) -> RenderResult<Handle> {
        self.typeface_create(None, 400, false)
    }

    pub fn typeface_style(&self, typeface: Handle) -> RenderResult<i32> {
        let f = self.typefaces.get(typeface).ok_or(RenderError::BadHandle)?;
        Ok(if f.italic { 2 } else { 0 })
    }

    pub fn typeface_weight(&self, typeface: Handle) -> RenderResult<i32> {
        Ok(self.typefaces.get(typeface).ok_or(RenderError::BadHandle)?.weight as i32)
    }

    // -----------------------------------------------------------------
    // Paths
    // -----------------------------------------------------------------

    pub fn path_create(&mut self) -> Handle {
        self.paths.insert(Path::new())
    }

    pub fn path_create_from(&mut self, other: Handle) -> RenderResult<Handle> {
        let p = self.paths.get(other).ok_or(RenderError::BadHandle)?.clone();
        Ok(self.paths.insert(p))
    }

    fn path_mut(&mut self, path: Handle) -> RenderResult<&mut Path> {
        self.paths.get_mut(path).ok_or(RenderError::BadHandle)
    }

    pub fn path_move_to(&mut self, path: Handle, x: f32, y: f32) -> RenderResult<()> {
        self.path_mut(path)?.move_to(x, y);
        Ok(())
    }

    pub fn path_line_to(&mut self, path: Handle, x: f32, y: f32) -> RenderResult<()> {
        self.path_mut(path)?.line_to(x, y);
        Ok(())
    }

    pub fn path_quad_to(&mut self, path: Handle, x1: f32, y1: f32, x2: f32, y2: f32) -> RenderResult<()> {
        self.path_mut(path)?.quad_to(x1, y1, x2, y2);
        Ok(())
    }

    pub fn path_cubic_to(
        &mut self,
        path: Handle,
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
        x3: f32,
        y3: f32,
    ) -> RenderResult<()> {
        self.path_mut(path)?.cubic_to(x1, y1, x2, y2, x3, y3);
        Ok(())
    }

    pub fn path_conic_to(&mut self, path: Handle, x1: f32, y1: f32, x2: f32, y2: f32, w: f32) -> RenderResult<()> {
        self.path_mut(path)?.conic_to(x1, y1, x2, y2, w);
        Ok(())
    }

    pub fn path_arc_to(
        &mut self,
        path: Handle,
        oval: RectF,
        start: f32,
        sweep: f32,
        force_move: bool,
    ) -> RenderResult<()> {
        self.path_mut(path)?.arc_to(oval, start, sweep, force_move);
        Ok(())
    }

    pub fn path_add_rect(&mut self, path: Handle, r: RectF, ccw: bool) -> RenderResult<()> {
        self.path_mut(path)?.add_rect(r.left, r.top, r.right, r.bottom, ccw);
        Ok(())
    }

    pub fn path_add_round_rect(&mut self, path: Handle, r: RectF, rx: f32, ry: f32, ccw: bool) -> RenderResult<()> {
        self.path_mut(path)?.add_round_rect(&r, rx, ry, ccw);
        Ok(())
    }

    pub fn path_add_oval(&mut self, path: Handle, r: RectF, ccw: bool) -> RenderResult<()> {
        self.path_mut(path)?.add_oval(r.left, r.top, r.right, r.bottom, ccw);
        Ok(())
    }

    pub fn path_add_circle(&mut self, path: Handle, cx: f32, cy: f32, radius: f32, ccw: bool) -> RenderResult<()> {
        self.path_mut(path)?.add_circle(cx, cy, radius, ccw);
        Ok(())
    }

    pub fn path_add_path(&mut self, path: Handle, other: Handle) -> RenderResult<()> {
        let o = self.paths.get(other).ok_or(RenderError::BadHandle)?.clone();
        self.path_mut(path)?.add_path(&o);
        Ok(())
    }

    pub fn path_close(&mut self, path: Handle) -> RenderResult<()> {
        self.path_mut(path)?.close();
        Ok(())
    }

    pub fn path_reset(&mut self, path: Handle) -> RenderResult<()> {
        self.path_mut(path)?.reset();
        Ok(())
    }

    pub fn path_rewind(&mut self, path: Handle) -> RenderResult<()> {
        self.path_mut(path)?.rewind();
        Ok(())
    }

    pub fn path_set_fill_type(&mut self, path: Handle, fill: i32) -> RenderResult<()> {
        let ft = FillType::from_ordinal(fill).ok_or_else(|| RenderError::invalid("bad FillType"))?;
        self.path_mut(path)?.set_fill_type(ft);
        Ok(())
    }

    pub fn path_get_fill_type(&self, path: Handle) -> RenderResult<i32> {
        Ok(self.paths.get(path).ok_or(RenderError::BadHandle)?.fill_type() as i32)
    }

    pub fn path_compute_bounds(&self, path: Handle) -> RenderResult<Option<RectF>> {
        Ok(self.paths.get(path).ok_or(RenderError::BadHandle)?.compute_bounds())
    }

    pub fn path_is_empty(&self, path: Handle) -> RenderResult<bool> {
        Ok(self.paths.get(path).ok_or(RenderError::BadHandle)?.is_empty())
    }

    pub fn path_transform(&mut self, path: Handle, matrix: Handle) -> RenderResult<()> {
        let m = *self.matrices.get(matrix).ok_or(RenderError::BadHandle)?;
        self.path_mut(path)?.transform(&m);
        Ok(())
    }

    /// `Path.op` is unsupported (tiny-skia has no boolean path ops).
    pub fn path_op(&mut self, path: Handle, other: Handle, op: i32) -> RenderResult<bool> {
        let op = PathOp::from_ordinal(op).ok_or_else(|| RenderError::invalid("bad Path.Op"))?;
        let _ = other;
        let _ = path;
        self.unsupported(format!("Path.op {op:?}"))?;
        Ok(false)
    }

    // -----------------------------------------------------------------
    // Matrices
    // -----------------------------------------------------------------

    pub fn matrix_create(&mut self) -> Handle {
        self.matrices.insert(Matrix3x3::identity())
    }

    pub fn matrix_create_from(&mut self, values: [f32; 9]) -> Handle {
        let mut m = Matrix3x3::identity();
        m.set_values(values);
        self.matrices.insert(m)
    }

    fn matrix_get(&self, matrix: Handle) -> RenderResult<Matrix3x3> {
        Ok(*self.matrices.get(matrix).ok_or(RenderError::BadHandle)?)
    }

    fn matrix_set(&mut self, matrix: Handle, m: Matrix3x3) -> RenderResult<()> {
        *self.matrices.get_mut(matrix).ok_or(RenderError::BadHandle)? = m;
        Ok(())
    }

    pub fn matrix_reset(&mut self, matrix: Handle) -> RenderResult<()> {
        self.matrix_set(matrix, Matrix3x3::identity())
    }

    pub fn matrix_is_identity(&self, matrix: Handle) -> RenderResult<bool> {
        Ok(self.matrix_get(matrix)?.is_identity())
    }

    pub fn matrix_get_values(&self, matrix: Handle) -> RenderResult<[f32; 9]> {
        Ok(self.matrix_get(matrix)?.get_values())
    }

    pub fn matrix_set_values(&mut self, matrix: Handle, values: [f32; 9]) -> RenderResult<()> {
        self.matrix_set(matrix, Matrix3x3 { m: values })
    }

    pub fn matrix_set_from(&mut self, matrix: Handle, other: Handle) -> RenderResult<()> {
        let m = self.matrix_get(other)?;
        self.matrix_set(matrix, m)
    }

    pub fn matrix_pre_translate(&mut self, matrix: Handle, dx: f32, dy: f32) -> RenderResult<()> {
        let mut m = self.matrix_get(matrix)?;
        m.pre_translate(dx, dy);
        self.matrix_set(matrix, m)
    }

    pub fn matrix_post_translate(&mut self, matrix: Handle, dx: f32, dy: f32) -> RenderResult<()> {
        let mut m = self.matrix_get(matrix)?;
        m.post_translate(dx, dy);
        self.matrix_set(matrix, m)
    }

    pub fn matrix_set_translate(&mut self, matrix: Handle, dx: f32, dy: f32) -> RenderResult<()> {
        self.matrix_set(matrix, Matrix3x3::translate_matrix(dx, dy))
    }

    pub fn matrix_pre_scale(&mut self, matrix: Handle, sx: f32, sy: f32) -> RenderResult<()> {
        let mut m = self.matrix_get(matrix)?;
        m.pre_scale(sx, sy);
        self.matrix_set(matrix, m)
    }

    pub fn matrix_post_scale(&mut self, matrix: Handle, sx: f32, sy: f32) -> RenderResult<()> {
        let mut m = self.matrix_get(matrix)?;
        m.post_scale(sx, sy);
        self.matrix_set(matrix, m)
    }

    pub fn matrix_set_scale(&mut self, matrix: Handle, sx: f32, sy: f32, px: f32, py: f32) -> RenderResult<()> {
        let mut m = Matrix3x3::identity();
        m.set_scale_pivot(sx, sy, px, py);
        self.matrix_set(matrix, m)
    }

    pub fn matrix_pre_rotate(&mut self, matrix: Handle, degrees: f32) -> RenderResult<()> {
        let mut m = self.matrix_get(matrix)?;
        m.pre_rotate(degrees);
        self.matrix_set(matrix, m)
    }

    pub fn matrix_post_rotate(&mut self, matrix: Handle, degrees: f32) -> RenderResult<()> {
        let mut m = self.matrix_get(matrix)?;
        m.post_rotate(degrees);
        self.matrix_set(matrix, m)
    }

    pub fn matrix_set_rotate(&mut self, matrix: Handle, degrees: f32, px: f32, py: f32) -> RenderResult<()> {
        let mut m = Matrix3x3::identity();
        m.set_rotate_pivot(degrees, px, py);
        self.matrix_set(matrix, m)
    }

    pub fn matrix_set_skew(&mut self, matrix: Handle, kx: f32, ky: f32, px: f32, py: f32) -> RenderResult<()> {
        let mut m = Matrix3x3::identity();
        m.set_skew_pivot(kx, ky, px, py);
        self.matrix_set(matrix, m)
    }

    pub fn matrix_pre_concat(&mut self, matrix: Handle, other: Handle) -> RenderResult<()> {
        let o = self.matrix_get(other)?;
        let mut m = self.matrix_get(matrix)?;
        m.pre_concat(&o);
        self.matrix_set(matrix, m)
    }

    pub fn matrix_post_concat(&mut self, matrix: Handle, other: Handle) -> RenderResult<()> {
        let o = self.matrix_get(other)?;
        let mut m = self.matrix_get(matrix)?;
        m.post_concat(&o);
        self.matrix_set(matrix, m)
    }

    pub fn matrix_invert(&mut self, matrix: Handle, out: Handle) -> RenderResult<bool> {
        let m = self.matrix_get(matrix)?;
        match m.inverted() {
            Some(inv) => {
                self.matrix_set(out, inv)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    pub fn matrix_map_points(&self, matrix: Handle, points: &mut [crate::geom::PointF]) -> RenderResult<()> {
        self.matrix_get(matrix)?.map_points(points);
        Ok(())
    }

    pub fn matrix_map_rect(&self, matrix: Handle, rect: RectF) -> RenderResult<Option<RectF>> {
        Ok(self.matrix_get(matrix)?.map_rect(&rect))
    }

    pub fn matrix_map_radius(&self, matrix: Handle, radius: f32) -> RenderResult<f32> {
        Ok(self.matrix_get(matrix)?.map_radius(radius))
    }

    pub fn matrix_set_rect_to_rect(
        &mut self,
        matrix: Handle,
        src: RectF,
        dst: RectF,
        mode: i32,
    ) -> RenderResult<bool> {
        let mode = crate::matrix::ScaleToFit::from_ordinal(mode)
            .ok_or_else(|| RenderError::invalid("bad ScaleToFit"))?;
        let mut m = Matrix3x3::identity();
        let ok = m.set_rect_to_rect(&src, &dst, mode);
        self.matrix_set(matrix, m)?;
        Ok(ok)
    }

    pub fn matrix_set_poly_to_poly(
        &mut self,
        matrix: Handle,
        src: &[crate::geom::PointF],
        dst: &[crate::geom::PointF],
    ) -> RenderResult<bool> {
        let mut m = Matrix3x3::identity();
        let ok = m.set_poly_to_poly(src, dst);
        self.matrix_set(matrix, m)?;
        Ok(ok)
    }

    pub fn matrix_rect_stays_rect(&self, matrix: Handle) -> RenderResult<bool> {
        Ok(self.matrix_get(matrix)?.rect_stays_rect())
    }

    pub fn matrix_equals(&self, a: Handle, b: Handle) -> RenderResult<bool> {
        Ok(self.matrix_get(a)? == self.matrix_get(b)?)
    }

    // -----------------------------------------------------------------
    // Regions
    // -----------------------------------------------------------------

    pub fn region_create(&mut self) -> Handle {
        self.regions.insert(Region::new())
    }

    pub fn region_create_from_rect(&mut self, r: Rect) -> Handle {
        self.regions.insert(Region::from_rect(r))
    }

    pub fn region_set_rect(&mut self, region: Handle, r: Rect) -> RenderResult<bool> {
        let reg = self.regions.get_mut(region).ok_or(RenderError::BadHandle)?;
        reg.set_rect(r);
        Ok(!reg.is_empty())
    }

    pub fn region_set_path(&mut self, region: Handle, path: Handle, clip: Option<Handle>) -> RenderResult<bool> {
        let p = self.paths.get(path).ok_or(RenderError::BadHandle)?.clone();
        let c = match clip {
            Some(h) => Some(self.regions.get(h).ok_or(RenderError::BadHandle)?.clone()),
            None => None,
        };
        let reg = self.regions.get_mut(region).ok_or(RenderError::BadHandle)?;
        reg.set_path(&p, c.as_ref())?;
        Ok(!reg.is_empty())
    }

    pub fn region_set_region(&mut self, region: Handle, other: Handle) -> RenderResult<()> {
        let o = self.regions.get(other).ok_or(RenderError::BadHandle)?.clone();
        self.regions.get_mut(region).ok_or(RenderError::BadHandle)?.set_region(&o);
        Ok(())
    }

    pub fn region_op(&mut self, region: Handle, other: Handle, op: i32) -> RenderResult<bool> {
        let op = RegionOp::from_ordinal(op).ok_or_else(|| RenderError::invalid("bad Region.Op"))?;
        let o = self.regions.get(other).ok_or(RenderError::BadHandle)?.clone();
        self.regions.get_mut(region).ok_or(RenderError::BadHandle)?.op(&o, op)
    }

    pub fn region_contains(&self, region: Handle, x: i32, y: i32) -> RenderResult<bool> {
        Ok(self.regions.get(region).ok_or(RenderError::BadHandle)?.contains(x, y))
    }

    pub fn region_is_empty(&self, region: Handle) -> RenderResult<bool> {
        Ok(self.regions.get(region).ok_or(RenderError::BadHandle)?.is_empty())
    }

    pub fn region_bounds(&self, region: Handle) -> RenderResult<Option<Rect>> {
        Ok(self.regions.get(region).ok_or(RenderError::BadHandle)?.bounds())
    }

    pub fn region_quick_reject(&self, region: Handle, r: Rect) -> RenderResult<bool> {
        Ok(self.regions.get(region).ok_or(RenderError::BadHandle)?.quick_reject(r))
    }

    pub fn region_translate(&mut self, region: Handle, dx: i32, dy: i32, out: Handle) -> RenderResult<()> {
        let t = self.regions.get(region).ok_or(RenderError::BadHandle)?.translated(dx, dy);
        *self.regions.get_mut(out).ok_or(RenderError::BadHandle)? = t;
        Ok(())
    }

    pub fn region_scale(&mut self, region: Handle, scale: f32, out: Handle) -> RenderResult<()> {
        let t = self.regions.get(region).ok_or(RenderError::BadHandle)?.scaled(scale);
        *self.regions.get_mut(out).ok_or(RenderError::BadHandle)? = t;
        Ok(())
    }

    pub fn region_boundary_path(&mut self, region: Handle) -> RenderResult<Handle> {
        let p = self.regions.get(region).ok_or(RenderError::BadHandle)?.boundary_path();
        Ok(self.paths.insert(p))
    }

    // -----------------------------------------------------------------
    // Text measurement
    // -----------------------------------------------------------------

    fn text_style_for(&self, paint: Handle) -> RenderResult<TextStyle> {
        let p = self.paint_ref(paint)?;
        let font = match p.typeface {
            Some(h) => self.typefaces.get(h).ok_or(RenderError::BadHandle)?.clone(),
            None => self
                .text_engine()?
                .fonts()
                .resolve_typeface(None, 400, false)
                .ok_or_else(|| RenderError::FontNotFound {
                    name: "sans-serif".to_string(),
                })?,
        };
        let family = self
            .text_engine()?
            .fonts()
            .families
            .get(font.family_key)
            .and_then(|f| f.name.clone());
        let mut style = TextStyle::from_paint(p, font);
        style.family = family;
        Ok(style)
    }

    pub fn paint_measure_text(&mut self, paint: Handle, text: &[u16]) -> RenderResult<f32> {
        let style = self.text_style_for(paint)?;
        Ok(self.text_engine_mut()?.measure_text(text, &style))
    }

    /// Writes per-UTF-16-char advances into `out`; returns total.
    pub fn paint_get_text_advances(
        &mut self,
        paint: Handle,
        text: &[u16],
        out: &mut [f32],
    ) -> RenderResult<f32> {
        let style = self.text_style_for(paint)?;
        Ok(self.text_engine_mut()?.get_text_run_advances(text, &style, out))
    }

    pub fn paint_get_font_metrics(&mut self, paint: Handle) -> RenderResult<FontMetrics> {
        let style = self.text_style_for(paint)?;
        Ok(self.text_engine()?.font_metrics(&style))
    }

    pub fn paint_get_font_metrics_int(&mut self, paint: Handle) -> RenderResult<FontMetricsInt> {
        let style = self.text_style_for(paint)?;
        Ok(self.text_engine()?.font_metrics_int(&style))
    }

    pub fn paint_get_text_bounds(&mut self, paint: Handle, text: &[u16]) -> RenderResult<Option<RectF>> {
        let style = self.text_style_for(paint)?;
        Ok(self.text_engine_mut()?.text_bounds(text, &style))
    }

    pub fn paint_break_text(&mut self, paint: Handle, text: &[u16], max_width: f32) -> RenderResult<f32> {
        let style = self.text_style_for(paint)?;
        Ok(self.text_engine_mut()?.break_text(text, &style, max_width))
    }

    pub fn paint_get_run_advance(&mut self, paint: Handle, text: &[u16], index: usize) -> RenderResult<f32> {
        let style = self.text_style_for(paint)?;
        Ok(self.text_engine_mut()?.get_run_advance(text, &style, index))
    }

    pub fn paint_get_offset_for_advance(&mut self, paint: Handle, text: &[u16], advance: f32) -> RenderResult<usize> {
        let style = self.text_style_for(paint)?;
        Ok(self.text_engine_mut()?.get_offset_for_advance(text, &style, advance))
    }

    pub fn paint_has_glyph(&self, paint: Handle, ch: char) -> RenderResult<bool> {
        let style = self.text_style_for(paint)?;
        Ok(self.text_engine()?.has_glyph(ch, &style))
    }

    pub fn paint_get_text_path(&mut self, paint: Handle, text: &[u16], x: f32, y: f32) -> RenderResult<Handle> {
        let style = self.text_style_for(paint)?;
        let path = self.text_engine_mut()?.text_path(text, x, y, &style);
        Ok(self.paths.insert(path))
    }

    /// Greedy line breaking over a paint + text.
    pub fn line_break(
        &mut self,
        paint: Handle,
        text: &[u16],
        width: f32,
    ) -> RenderResult<Vec<crate::text::layout::LineBreak>> {
        let style = self.text_style_for(paint)?;
        let breaker = crate::text::layout::LineBreaker::default();
        let constraints = crate::text::layout::ParagraphConstraints {
            width,
            first_width: width,
            indent: 0.0,
        };
        Ok(breaker.compute_breaks(self.text_engine_mut()?, text, &style, &constraints))
    }

    // -----------------------------------------------------------------
    // Drawing
    // -----------------------------------------------------------------

    /// Build a tiny-skia paint for `paint`, resolving any bitmap/sweep shader
    /// into `owned` first (caller keeps it alive).
    fn prepare_paint<'a>(&mut self, paint: &Paint, owned: &'a mut Vec<tiny_skia::Pixmap>) -> tiny_skia::Paint<'a> {
        if matches!(paint.path_effect, Some(PathEffectSpec::Corner { .. })) {
            self.unsupported("PathEffect CornerPathEffect (not applied)").ok();
        }
        let mut sk = tiny_skia::Paint {
            anti_alias: paint.anti_alias,
            blend_mode: paint.blend_mode,
            ..Default::default()
        };
        sk.set_color(crate::color::argb_to_sk(paint.effective_argb()));
        match &paint.shader {
            None => {}
            Some(ShaderSpec::Linear {
                start,
                end,
                colors,
                positions,
                tile,
                local,
            }) => {
                let stops = make_stops(colors, positions.as_deref());
                let ts = local.to_affine().unwrap_or(tiny_skia::Transform::identity());
                if let Some(s) = tiny_skia::LinearGradient::new(
                    tiny_skia::Point::from_xy(start.0, start.1),
                    tiny_skia::Point::from_xy(end.0, end.1),
                    stops,
                    tile.to_skia(),
                    ts,
                ) {
                    sk.shader = s;
                } else {
                    self.unsupported("LinearGradient degenerate").ok();
                }
            }
            Some(ShaderSpec::Radial {
                center,
                radius,
                colors,
                positions,
                tile,
                local,
                focal,
            }) => {
                let stops = make_stops(colors, positions.as_deref());
                let ts = local.to_affine().unwrap_or(tiny_skia::Transform::identity());
                if let Some(s) = tiny_skia::RadialGradient::new(
                    tiny_skia::Point::from_xy(focal.0, focal.1),
                    tiny_skia::Point::from_xy(center.0, center.1),
                    *radius,
                    stops,
                    tile.to_skia(),
                    ts,
                ) {
                    sk.shader = s;
                } else {
                    self.unsupported("RadialGradient degenerate").ok();
                }
            }
            Some(ShaderSpec::Sweep {
                center,
                colors,
                positions,
                local,
            }) => {
                let pm = make_sweep_pixmap(colors, positions.as_deref());
                owned.push(pm);
                // A 512px analysis square is large enough for typical UI.
                let size = owned.last().unwrap().width() as f32;
                let r = 512.0f32;
                let scale = 2.0 * r / size;
                let mut m = Matrix3x3::identity();
                m.m[0] = scale;
                m.m[4] = scale;
                m.m[2] = center.0 - size * 0.5 * scale;
                m.m[5] = center.1 - size * 0.5 * scale;
                if let Some(l) = local.to_affine() {
                    m = m.mul(&Matrix3x3::from_affine(&l));
                }
                let ts = m.to_affine().unwrap_or(tiny_skia::Transform::identity());
                let pm_ref = owned.last().unwrap();
                let shader = tiny_skia::Pattern::new(
                    pm_ref.as_ref(),
                    tiny_skia::SpreadMode::Pad,
                    tiny_skia::FilterQuality::Bilinear,
                    1.0,
                    ts,
                );
                sk.shader = shader;
            }
            Some(ShaderSpec::Bitmap {
                bitmap,
                tile_x,
                tile_y: _,
                local,
            }) => {
                if let Some(b) = self.bitmaps.get(*bitmap) {
                    if let Ok(pm) = b.to_pixmap() {
                        owned.push(pm);
                        let ts = local.to_affine().unwrap_or(tiny_skia::Transform::identity());
                        let pm_ref = owned.last().unwrap();
                        let shader = tiny_skia::Pattern::new(
                            pm_ref.as_ref(),
                            tile_x.to_skia(),
                            tiny_skia::FilterQuality::Bilinear,
                            1.0,
                            ts,
                        );
                        sk.shader = shader;
                    }
                }
            }
            Some(ShaderSpec::Compose { .. }) => {
                // `ComposeShader` is modelled and reachable through the
                // framework natives; rasterising an arbitrary two-shader
                // PorterDuff composition needs an offscreen shader blend that
                // tiny-skia does not expose. Reported structurally rather than
                // silently drawing only one operand.
                self.unsupported("ComposeShader two-shader composition")
                    .ok();
            }
        }
        sk
    }

    /// Fill a path on a surface with a prepared paint.
    fn draw_path_with_paint(&mut self, surface: Handle, path: &Path, paint: &Paint) -> RenderResult<()> {
        let Some(sk_path) = path.build_tiny_skia() else {
            return Ok(());
        };
        let ts_opt = { self.surface_canvas(surface)?.state.matrix.to_affine() };
        let Some(ts) = ts_opt else {
            self.unsupported("Canvas perspective matrix")?;
            return Ok(());
        };
        if let Some(ShaderSpec::Compose { a, b, mode }) = &paint.shader {
            let (a, b, mode) = (a.clone(), b.clone(), *mode);
            return self.draw_compose_shader(surface, path, paint, &a, &b, mode);
        }
        let mut owned: Vec<tiny_skia::Pixmap> = Vec::new();
        let sk_paint = self.prepare_paint(paint, &mut owned);
        let fill_rule = path.fill_type().fill_rule();
        {
            let (state, target) = self.canvas_parts_mut(surface)?;
            let clip = state.clip.as_ref();
            canvas::emit_shape(target, clip, &sk_path, paint, &sk_paint, fill_rule, ts);
        }
        self.raster_ops += 1;
        if let Some(s) = self.surfaces.get_mut(surface) {
            s.raster_ops += 1;
        }
        Ok(())
    }

    /// Rasterise a `ComposeShader` by rendering both sub-shaders over the path
    /// into offscreen layers and combining them with the PorterDuff operator.
    /// tiny-skia cannot compose two shaders directly, so the composition is
    /// done per-pixel in device-independent layer space.
    fn draw_compose_shader(
        &mut self,
        surface: Handle,
        path: &Path,
        paint: &Paint,
        shader_a: &ShaderSpec,
        shader_b: &ShaderSpec,
        mode: PorterDuffMode,
    ) -> RenderResult<()> {
        let Some(sk_path) = path.build_tiny_skia() else {
            return Ok(());
        };
        let fill_rule = path.fill_type().fill_rule();
        let (w, h) = {
            let s = self.surfaces.get(surface).ok_or(RenderError::BadHandle)?;
            (s.width, s.height)
        };
        let render_one = |ctx: &mut Self, shader: &ShaderSpec| -> Option<tiny_skia::Pixmap> {
            let mut owned: Vec<tiny_skia::Pixmap> = Vec::new();
            let mut p = paint.clone();
            p.shader = Some(shader.clone());
            p.color_filter = None;
            p.mask_filter = None;
            p.alpha = 255;
            p.color = 0xffff_ffff;
            p.blend_mode = tiny_skia::BlendMode::Source;
            let sk = ctx.prepare_paint(&p, &mut owned);
            let mut pm = tiny_skia::Pixmap::new(w, h)?;
            pm.fill(tiny_skia::Color::TRANSPARENT);
            // Evaluate in the path's local space; the final `emit_shape`
            // applies the canvas matrix.
            pm.fill_path(&sk_path, &sk, fill_rule, tiny_skia::Transform::identity(), None);
            Some(pm)
        };
        let Some(mut dst) = render_one(self, shader_b) else {
            return Ok(());
        };
        let Some(src) = render_one(self, shader_a) else {
            return Ok(());
        };
        let compose_paint = tiny_skia::PixmapPaint {
            opacity: 1.0,
            blend_mode: mode.to_blend(),
            quality: tiny_skia::FilterQuality::Nearest,
        };
        dst.draw_pixmap(
            0,
            0,
            src.as_ref(),
            &compose_paint,
            tiny_skia::Transform::identity(),
            None,
        );
        drop(src);
        let owned: Vec<tiny_skia::Pixmap> = vec![dst];
        let composed = owned.last().unwrap();
        let pattern = tiny_skia::Pattern::new(
            composed.as_ref(),
            tiny_skia::SpreadMode::Pad,
            tiny_skia::FilterQuality::Bilinear,
            1.0,
            tiny_skia::Transform::identity(),
        );
        let mut sk = tiny_skia::Paint {
            shader: pattern,
            blend_mode: paint.blend_mode,
            anti_alias: paint.anti_alias,
            force_hq_pipeline: false,
        };
        sk.set_color(crate::color::argb_to_sk(paint.effective_argb()));
        let ts_opt = { self.surface_canvas(surface)?.state.matrix.to_affine() };
        let Some(ts) = ts_opt else {
            self.unsupported("Canvas perspective matrix")?;
            return Ok(());
        };
        {
            let (state, target) = self.canvas_parts_mut(surface)?;
            let clip = state.clip.as_ref();
            canvas::emit_shape(target, clip, &sk_path, paint, &sk, fill_rule, ts);
        }
        self.raster_ops += 1;
        if let Some(s) = self.surfaces.get_mut(surface) {
            s.raster_ops += 1;
        }
        Ok(())
    }

    pub fn canvas_draw_path(&mut self, surface: Handle, path: Handle, paint: Handle) -> RenderResult<()> {
        let p = self.paths.get(path).ok_or(RenderError::BadHandle)?.clone();
        let paint = self.paint_ref(paint)?.clone();
        self.draw_path_with_paint(surface, &p, &paint)
    }

    pub fn canvas_draw_rect(&mut self, surface: Handle, rect: RectF, paint: Handle) -> RenderResult<()> {
        let mut p = Path::new();
        p.add_rect(rect.left, rect.top, rect.right, rect.bottom, false);
        let paint = self.paint_ref(paint)?.clone();
        self.draw_path_with_paint(surface, &p, &paint)
    }

    pub fn canvas_draw_round_rect(
        &mut self,
        surface: Handle,
        rect: RectF,
        rx: f32,
        ry: f32,
        paint: Handle,
    ) -> RenderResult<()> {
        let mut p = Path::new();
        p.add_round_rect(&rect, rx, ry, false);
        let paint = self.paint_ref(paint)?.clone();
        self.draw_path_with_paint(surface, &p, &paint)
    }

    pub fn canvas_draw_oval(&mut self, surface: Handle, rect: RectF, paint: Handle) -> RenderResult<()> {
        let mut p = Path::new();
        p.add_oval(rect.left, rect.top, rect.right, rect.bottom, false);
        let paint = self.paint_ref(paint)?.clone();
        self.draw_path_with_paint(surface, &p, &paint)
    }

    pub fn canvas_draw_circle(
        &mut self,
        surface: Handle,
        cx: f32,
        cy: f32,
        radius: f32,
        paint: Handle,
    ) -> RenderResult<()> {
        let mut p = Path::new();
        p.add_circle(cx, cy, radius, false);
        let paint = self.paint_ref(paint)?.clone();
        self.draw_path_with_paint(surface, &p, &paint)
    }

    pub fn canvas_draw_arc(
        &mut self,
        surface: Handle,
        oval: RectF,
        start: f32,
        sweep: f32,
        use_center: bool,
        paint: Handle,
    ) -> RenderResult<()> {
        let mut p = Path::new();
        if use_center {
            p.move_to(oval.center_x(), oval.center_y());
            p.line_to(
                oval.center_x() + oval.width() * 0.5 * start.to_radians().cos(),
                oval.center_y() + oval.height() * 0.5 * start.to_radians().sin(),
            );
            p.arc_to(oval, start, sweep, false);
            p.close();
        } else {
            p.arc_to(oval, start, sweep, true);
        }
        let paint = self.paint_ref(paint)?.clone();
        self.draw_path_with_paint(surface, &p, &paint)
    }

    pub fn canvas_draw_line(
        &mut self,
        surface: Handle,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        paint: Handle,
    ) -> RenderResult<()> {
        let mut p = Path::new();
        p.move_to(x0, y0);
        p.line_to(x1, y1);
        let mut paint = self.paint_ref(paint)?.clone();
        // A line with fill style draws nothing; Android strokes lines.
        if paint.style == PaintStyle::Fill && paint.stroke_width == 0.0 {
            paint.style = PaintStyle::Stroke;
        }
        self.draw_path_with_paint(surface, &p, &paint)
    }

    pub fn canvas_draw_point(&mut self, surface: Handle, x: f32, y: f32, paint: Handle) -> RenderResult<()> {
        let mut paint = self.paint_ref(paint)?.clone();
        if paint.style == PaintStyle::Fill && paint.stroke_width == 0.0 {
            paint.style = PaintStyle::Stroke;
        }
        let mut p = Path::new();
        p.move_to(x, y);
        p.line_to(x, y);
        self.draw_path_with_paint(surface, &p, &paint)
    }

    /// Android `Canvas.drawPaint` — fill the current clip with the paint in
    /// device space (the canvas matrix does not transform the fill).
    pub fn canvas_draw_paint(&mut self, surface: Handle, paint: Handle) -> RenderResult<()> {
        let p = self.paint_ref(paint)?.clone();
        let clip = self.surface_canvas(surface)?.get_clip_bounds();
        let mut path = Path::new();
        path.add_rect(
            clip.left as f32,
            clip.top as f32,
            clip.right as f32,
            clip.bottom as f32,
            false,
        );
        self.surface_canvas_mut(surface)?.save();
        let identity = Matrix3x3::identity();
        self.surface_canvas_mut(surface)?.set_matrix(&identity);
        let r = self.draw_path_with_paint(surface, &path, &p);
        self.canvas_restore(surface)?;
        r
    }

    /// Android `Canvas.drawLines(float[] pts, int offset, int count, Paint)`.
    pub fn canvas_draw_lines(&mut self, surface: Handle, points: &[f32], paint: Handle) -> RenderResult<()> {
        let p = self.paint_ref(paint)?.clone();
        let mut path = Path::new();
        for pair in points.chunks_exact(4) {
            path.move_to(pair[0], pair[1]);
            path.line_to(pair[2], pair[3]);
        }
        self.draw_path_with_paint(surface, &path, &p)
    }

    /// Android `Canvas.drawPoints(float[] pts, int offset, int count, Paint)`.
    pub fn canvas_draw_points(&mut self, surface: Handle, points: &[f32], paint: Handle) -> RenderResult<()> {
        let mut p = self.paint_ref(paint)?.clone();
        if p.style == PaintStyle::Fill && p.stroke_width == 0.0 {
            p.style = PaintStyle::Stroke;
        }
        let mut path = Path::new();
        for pair in points.chunks_exact(2) {
            path.move_to(pair[0], pair[1]);
            path.line_to(pair[0], pair[1]);
        }
        self.draw_path_with_paint(surface, &path, &p)
    }

    /// Android `BaseCanvas.nDrawBitmapMatrix(bitmap, matrix, paint)`.
    pub fn canvas_draw_bitmap_matrix(
        &mut self,
        surface: Handle,
        bitmap: Handle,
        matrix: Handle,
        paint: Option<Handle>,
    ) -> RenderResult<()> {
        let pm = self.bitmaps.get(bitmap).ok_or(RenderError::BadHandle)?.to_pixmap()?;
        let m = *self.matrices.get(matrix).ok_or(RenderError::BadHandle)?;
        let canvas_m = self.surface_canvas(surface)?.state.matrix;
        let combined = canvas_m.mul(&m);
        let Some(local) = combined.to_affine() else {
            self.unsupported("Canvas perspective matrix")?;
            return Ok(());
        };
        let paint = match paint {
            Some(h) => self.paint_ref(h)?.clone(),
            None => Paint::default(),
        };
        let (w, h) = (pm.width() as f32, pm.height() as f32);
        let quality = tiny_skia::FilterQuality::Bilinear;
        let sk_paint = tiny_skia::Paint {
            shader: tiny_skia::Pattern::new(pm.as_ref(), tiny_skia::SpreadMode::Pad, quality, 1.0, local),
            blend_mode: paint.blend_mode,
            anti_alias: true,
            force_hq_pipeline: false,
        };
        let mut path = Path::new();
        path.add_rect(0.0, 0.0, w, h, false);
        let Some(sk_path) = path.build_tiny_skia() else {
            return Ok(());
        };
        {
            let (state, target) = self.canvas_parts_mut(surface)?;
            target.fill_path(&sk_path, &sk_paint, tiny_skia::FillRule::Winding, tiny_skia::Transform::identity(), state.clip.as_ref());
        }
        self.raster_ops += 1;
        Ok(())
    }

    /// Android `Canvas.saveLayer(bounds, paint)` — ACOS honours the paint's
    /// alpha and blend mode; colour filters on the layer paint are recorded as
    /// approximate.
    pub fn canvas_save_layer(
        &mut self,
        surface: Handle,
        bounds: Option<RectF>,
        paint: Handle,
    ) -> RenderResult<i32> {
        let (alpha, blend) = {
            let p = self.paint_ref(paint)?;
            (p.alpha as u8, p.blend_mode)
        };
        let count = self.surface_canvas_mut(surface)?.save_layer_alpha(bounds, alpha)?;
        self.surface_canvas_mut(surface)?.set_top_layer_blend(blend);
        Ok(count)
    }

    pub fn canvas_draw_color(&mut self, surface: Handle, color: u32) -> RenderResult<()> {
        let (state, target) = self.canvas_parts_mut(surface)?;
        let blend = tiny_skia::BlendMode::SourceOver;
        let clip = state.clip.as_ref();
        canvas::fill_color(target, clip, color, blend);
        self.raster_ops += 1;
        Ok(())
    }

    /// Draw text at the given baseline.
    pub fn canvas_draw_text(
        &mut self,
        surface: Handle,
        text: &[u16],
        x: f32,
        y: f32,
        paint: Handle,
    ) -> RenderResult<()> {
        if text.is_empty() {
            return Ok(());
        }
        let p = self.paint_ref(paint)?.clone();
        let style = self.text_style_for(paint)?;
        let shaped = self.text_engine_mut()?.shape(text, &style);
        let advance = shaped.total_advance;
        let origin_x = match p.text_align {
            TextAlign::Left => x,
            TextAlign::Center => x - advance * 0.5,
            TextAlign::Right => x - advance,
        };
        let mut text_paint = p.clone();
        // Text never shaders/filters through the glyph path today; keep that
        // contract so the FreeType mask path can composite a solid colour.
        text_paint.shader = None;
        text_paint.color_filter = None;
        if text_paint.style == PaintStyle::Fill {
            text_paint.style = PaintStyle::Fill;
        }
        // Underline / strikethrough.
        if text_paint.underline || text_paint.strike_thru {
            let metrics = self.text_engine()?.font_metrics(&style);
            let mut decorations = Path::new();
            let start_x = origin_x;
            let end_x = origin_x + advance;
            if text_paint.underline {
                let uy = y - metrics.descent * 0.5;
                decorations.add_rect(start_x, uy, end_x, uy + style.size * 0.05, false);
            }
            if text_paint.strike_thru {
                let sy = y - style.size * 0.3;
                decorations.add_rect(start_x, sy, end_x, sy + style.size * 0.05, false);
            }
            let mut deco_paint = text_paint.clone();
            deco_paint.style = PaintStyle::Fill;
            self.draw_path_with_paint(surface, &decorations, &deco_paint)?;
        }

        // ADR-0019: rasterise glyphs with FreeType and blit the A8 masks.
        // The direct blit is only exact for a translation-only canvas matrix,
        // a solid source-over fill and no mask filter; anything else keeps the
        // outline path (which is also the fallback when FreeType cannot load a
        // face or produce a grayscale bitmap).
        let ts = { self.surface_canvas(surface)?.state.matrix.to_affine() };
        let translate_only = ts
            .map(|t| t.sx == 1.0 && t.sy == 1.0 && t.kx == 0.0 && t.ky == 0.0)
            .unwrap_or(false);
        let use_masks = text_paint.style == PaintStyle::Fill
            && text_paint.anti_alias
            && text_paint.mask_filter.is_none()
            && !text_paint.fake_bold
            && text_paint.blend_mode == tiny_skia::BlendMode::SourceOver
            && translate_only
            && !shaped.runs.is_empty()
            && !force_text_path();
        if use_masks {
            let ts = ts.expect("translate_only implies an affine matrix");
            // Collect first so a single un-rasterisable glyph falls back to the
            // outline path without half-drawing the string.
            let mut placed: Vec<(Arc<crate::text::freetype::GlyphMask>, i32, i32)> = Vec::new();
            let mut rasterised = true;
            let mut pen_x = origin_x;
            'runs: for run in &shaped.runs {
                // The same effective design coordinates the shaper used for
                // this run, so the FreeType instance matches the advances.
                // Normalise once per run; the glyph loop only clones the key.
                let run_variations =
                    crate::text::effective_variations(&run.font, &style.variations);
                let run_coords = crate::text::freetype::VariationCoords::new(&run_variations);
                for g in &run.glyphs {
                    match crate::text::freetype::rasterize_coords(
                        &run.font,
                        g.glyph_id,
                        style.size,
                        &run_coords,
                    ) {
                        Some(mask) if !mask.is_empty() => {
                            // Android's unhinted grid places each mask on an
                            // integer pixel; the accumulated advances stay
                            // fractional (matching the PoC's `hb_nohint`).
                            let gx = pen_x + g.x_offset;
                            let gy = y + g.y_offset - style.skew_x * pen_x;
                            let dev_x = (ts.tx + gx).round() as i32 + mask.left;
                            let dev_y = (ts.ty + gy).round() as i32 - mask.top;
                            placed.push((mask, dev_x, dev_y));
                        }
                        Some(_) => {} // e.g. a space: no coverage to draw
                        None => {
                            rasterised = false;
                            break 'runs;
                        }
                    }
                    pen_x += g.advance;
                }
            }
            if rasterised {
                let argb = text_paint.effective_argb();
                {
                    let (state, target) = self.canvas_parts_mut(surface)?;
                    let clip = state.clip.as_ref();
                    for (mask, dx, dy) in &placed {
                        if mask.is_color() {
                            canvas::blit_color(target, clip, mask, *dx, *dy, argb);
                        } else {
                            canvas::blit_coverage(target, clip, mask, *dx, *dy, argb);
                        }
                    }
                }
                self.raster_ops += 1;
                if let Some(s) = self.surfaces.get_mut(surface) {
                    s.raster_ops += 1;
                }
                return Ok(());
            }
        }

        if text_paint.fake_bold {
            text_paint.style = PaintStyle::FillAndStroke;
            text_paint.stroke_width = (style.size / 24.0).max(0.5);
        }
        let path = self.text_engine_mut()?.text_path(text, origin_x, y, &style);
        self.draw_path_with_paint(surface, &path, &text_paint)
    }

    /// Draw `text` constrained to `max_width` device pixels, truncating on a
    /// UTF-16 boundary with the Unicode ellipsis (`U+2026`) at the requested
    /// end. This is the renderer side of Android `TextUtils.ellipsize` (a
    /// single-line ellipsized `TextView`); `Canvas.drawText` itself has no
    /// ellipsize API, so the plain [`Self::canvas_draw_text`] is unchanged and
    /// the natives need no edit.
    pub fn canvas_draw_text_layout(
        &mut self,
        surface: Handle,
        text: &[u16],
        x: f32,
        y: f32,
        paint: Handle,
        max_width: f32,
        mode: crate::render_node::EllipsizeMode,
    ) -> RenderResult<()> {
        if text.is_empty()
            || max_width <= 0.0
            || mode == crate::render_node::EllipsizeMode::None
        {
            return self.canvas_draw_text(surface, text, x, y, paint);
        }
        let style = self.text_style_for(paint)?;
        let shaped = self.text_engine_mut()?.shape(text, &style);
        if shaped.total_advance <= max_width {
            return self.canvas_draw_text(surface, text, x, y, paint);
        }
        let ellipsis = [0x2026u16];
        let ellipsis_advance = self.text_engine_mut()?.measure_text(&ellipsis, &style);
        let advances = shaped.char_advances.clone();
        let budget = (max_width - ellipsis_advance).max(0.0);

        let truncated: Vec<u16> = match mode {
            crate::render_node::EllipsizeMode::None | crate::render_node::EllipsizeMode::End => {
                let mut acc = 0.0f32;
                let mut n = 0usize;
                for (i, a) in advances.iter().enumerate() {
                    if acc + a > budget {
                        break;
                    }
                    acc += a;
                    n = i + 1;
                }
                let mut out = text[..n].to_vec();
                out.push(0x2026);
                out
            }
            crate::render_node::EllipsizeMode::Start => {
                let mut acc = 0.0f32;
                let mut start = text.len();
                for i in (0..advances.len()).rev() {
                    if acc + advances[i] > budget {
                        break;
                    }
                    acc += advances[i];
                    start = i;
                }
                let mut out = vec![0x2026u16];
                out.extend_from_slice(&text[start..]);
                out
            }
            crate::render_node::EllipsizeMode::Middle => {
                let mut pre_n = 0usize;
                let mut pre = 0.0f32;
                while pre_n < text.len() {
                    let a = advances[pre_n];
                    if pre + a > budget * 0.5 {
                        break;
                    }
                    pre += a;
                    pre_n += 1;
                }
                let mut suf_n = 0usize;
                let mut suf = 0.0f32;
                while pre_n + suf_n < text.len() {
                    let i = text.len() - 1 - suf_n;
                    if pre + suf + advances[i] > budget {
                        break;
                    }
                    suf += advances[i];
                    suf_n += 1;
                }
                let mut out = text[..pre_n].to_vec();
                out.push(0x2026);
                out.extend_from_slice(&text[text.len() - suf_n..]);
                out
            }
        };
        if truncated.is_empty() {
            return Ok(());
        }
        self.canvas_draw_text(surface, &truncated, x, y, paint)
    }

    pub fn canvas_draw_bitmap(
        &mut self,
        surface: Handle,
        bitmap: Handle,
        src: Option<RectF>,
        dst: RectF,
        paint: Option<Handle>,
        filter: bool,
    ) -> RenderResult<()> {
        let pm = self
            .bitmaps
            .get(bitmap)
            .ok_or(RenderError::BadHandle)?
            .to_pixmap()?;
        let paint = match paint {
            Some(h) => self.paint_ref(h)?.clone(),
            None => Paint::default(),
        };
        let (state, target) = self.canvas_parts_mut(surface)?;
        let Some(ts) = state.matrix.to_affine() else {
            self.unsupported("Canvas perspective matrix")?;
            return Ok(());
        };
        let clip = state.clip.as_ref();
        canvas::draw_bitmap(target, clip, pm.as_ref(), src, dst, ts, &paint, filter);
        self.raster_ops += 1;
        Ok(())
    }

    pub fn canvas_draw_nine_patch(
        &mut self,
        surface: Handle,
        bitmap: Handle,
        chunk: NinePatchChunk,
        dst: Rect,
        paint: Option<Handle>,
        filter: bool,
    ) -> RenderResult<()> {
        let pm = self
            .bitmaps
            .get(bitmap)
            .ok_or(RenderError::BadHandle)?
            .to_pixmap()?;
        let paint = match paint {
            Some(h) => self.paint_ref(h)?.clone(),
            None => Paint::default(),
        };
        let (state, target) = self.canvas_parts_mut(surface)?;
        let clip = state.clip.as_ref();
        canvas::draw_nine_patch(target, clip, pm.as_ref(), &chunk, dst, &paint, filter);
        self.raster_ops += 1;
        Ok(())
    }

    // -----------------------------------------------------------------
    // Render nodes
    // -----------------------------------------------------------------

    pub fn render_node_create(&mut self, name: &str) -> Handle {
        self.render_nodes.insert(RenderNode::new(name))
    }

    pub fn render_node_set_bounds(&mut self, node: Handle, l: i32, t: i32, r: i32, b: i32) -> RenderResult<()> {
        self.render_nodes
            .get_mut(node)
            .ok_or(RenderError::BadHandle)?
            .set_left_top_right_bottom(l, t, r, b);
        Ok(())
    }

    pub fn render_node_set_alpha(&mut self, node: Handle, alpha: f32) -> RenderResult<()> {
        self.render_nodes.get_mut(node).ok_or(RenderError::BadHandle)?.set_alpha(alpha);
        Ok(())
    }

    pub fn render_node_set_translation(&mut self, node: Handle, x: f32, y: f32) -> RenderResult<()> {
        self.render_nodes.get_mut(node).ok_or(RenderError::BadHandle)?.set_translation(x, y);
        Ok(())
    }

    pub fn render_node_set_translation_z(&mut self, node: Handle, z: f32) -> RenderResult<()> {
        self.render_nodes
            .get_mut(node)
            .ok_or(RenderError::BadHandle)?
            .set_translation_z(z);
        Ok(())
    }

    pub fn render_node_set_scale(&mut self, node: Handle, x: f32, y: f32) -> RenderResult<()> {
        self.render_nodes.get_mut(node).ok_or(RenderError::BadHandle)?.set_scale(x, y);
        Ok(())
    }

    pub fn render_node_set_rotation(&mut self, node: Handle, deg: f32) -> RenderResult<()> {
        self.render_nodes.get_mut(node).ok_or(RenderError::BadHandle)?.set_rotation(deg);
        Ok(())
    }

    pub fn render_node_set_rotation_xyz(&mut self, node: Handle, x: f32, y: f32, z: f32) -> RenderResult<()> {
        self.render_nodes
            .get_mut(node)
            .ok_or(RenderError::BadHandle)?
            .set_rotation_xyz(x, y, z);
        Ok(())
    }

    pub fn render_node_set_pivot(&mut self, node: Handle, x: f32, y: f32) -> RenderResult<()> {
        self.render_nodes.get_mut(node).ok_or(RenderError::BadHandle)?.set_pivot(x, y);
        Ok(())
    }

    pub fn render_node_set_clip_to_bounds(&mut self, node: Handle, v: bool) -> RenderResult<()> {
        self.render_nodes
            .get_mut(node)
            .ok_or(RenderError::BadHandle)?
            .set_clip_to_bounds(v);
        Ok(())
    }

    pub fn render_node_set_clip_to_outline(&mut self, node: Handle, v: bool) -> RenderResult<()> {
        self.render_nodes
            .get_mut(node)
            .ok_or(RenderError::BadHandle)?
            .set_clip_to_outline(v);
        Ok(())
    }

    pub fn render_node_set_outline_round_rect(
        &mut self,
        node: Handle,
        l: i32,
        t: i32,
        r: i32,
        b: i32,
        rx: f32,
        ry: f32,
    ) -> RenderResult<()> {
        self.render_nodes
            .get_mut(node)
            .ok_or(RenderError::BadHandle)?
            .set_outline_round_rect(l, t, r, b, rx, ry);
        Ok(())
    }

    pub fn render_node_set_outline_rect(&mut self, node: Handle, r: RectF) -> RenderResult<()> {
        self.render_nodes.get_mut(node).ok_or(RenderError::BadHandle)?.set_outline_rect(r);
        Ok(())
    }

    pub fn render_node_set_outline_path(&mut self, node: Handle, path: Handle, alpha: f32) -> RenderResult<()> {
        let p = self.paths.get(path).ok_or(RenderError::BadHandle)?.clone();
        self.render_nodes
            .get_mut(node)
            .ok_or(RenderError::BadHandle)?
            .set_outline_path(p, alpha);
        Ok(())
    }

    pub fn render_node_set_outline_none(&mut self, node: Handle) -> RenderResult<()> {
        self.render_nodes.get_mut(node).ok_or(RenderError::BadHandle)?.set_outline_none();
        Ok(())
    }

    pub fn render_node_set_elevation(&mut self, node: Handle, e: f32) -> RenderResult<()> {
        self.render_nodes.get_mut(node).ok_or(RenderError::BadHandle)?.set_elevation(e);
        Ok(())
    }

    /// Override the ambient/spot shadow colours used by [`Self::draw_node_shadow`]
    /// (`RenderNode.setAmbientShadowColor` / `setSpotShadowColor`).
    pub fn render_node_set_shadow_colors(
        &mut self,
        node: Handle,
        ambient: u32,
        spot: u32,
    ) -> RenderResult<()> {
        let n = self.render_nodes.get_mut(node).ok_or(RenderError::BadHandle)?;
        n.ambient_shadow_color = ambient;
        n.spot_shadow_color = spot;
        Ok(())
    }

    pub fn render_node_set_overlapping(&mut self, node: Handle, v: bool) -> RenderResult<()> {
        self.render_nodes
            .get_mut(node)
            .ok_or(RenderError::BadHandle)?
            .set_has_overlapping_rendering(v);
        Ok(())
    }

    pub fn render_node_set_layer_type(&mut self, node: Handle, layer: i32) -> RenderResult<()> {
        let lt = LayerType::from_ordinal(layer).ok_or_else(|| RenderError::invalid("bad LayerType"))?;
        self.render_nodes.get_mut(node).ok_or(RenderError::BadHandle)?.set_layer_type(lt);
        Ok(())
    }

    pub fn render_node_set_layer_paint(&mut self, node: Handle, paint: Option<Handle>) -> RenderResult<()> {
        self.render_nodes
            .get_mut(node)
            .ok_or(RenderError::BadHandle)?
            .set_layer_paint(paint);
        Ok(())
    }

    pub fn render_node_add_child(&mut self, node: Handle, child: Handle) -> RenderResult<()> {
        if !self.render_nodes.contains(child) {
            return Err(RenderError::BadHandle);
        }
        self.render_nodes.get_mut(node).ok_or(RenderError::BadHandle)?.add_child(child);
        Ok(())
    }

    pub fn render_node_remove_child(&mut self, node: Handle, child: Handle) -> RenderResult<()> {
        self.render_nodes
            .get_mut(node)
            .ok_or(RenderError::BadHandle)?
            .remove_child(child);
        Ok(())
    }

    pub fn render_node_clear_display_list(&mut self, node: Handle) -> RenderResult<()> {
        self.render_nodes
            .get_mut(node)
            .ok_or(RenderError::BadHandle)?
            .clear_display_list();
        Ok(())
    }

    /// Append a recorded op (the `RecordingCanvas.nDraw*` sink).
    pub fn render_node_push_op(&mut self, node: Handle, op: DisplayOp) -> RenderResult<()> {
        self.render_nodes.get_mut(node).ok_or(RenderError::BadHandle)?.push_op(op);
        Ok(())
    }

    pub fn render_node_get_property(&self, node: Handle) -> RenderResult<RenderNodeInfo> {
        let n = self.render_nodes.get(node).ok_or(RenderError::BadHandle)?;
        Ok(RenderNodeInfo {
            left: n.left,
            top: n.top,
            right: n.right,
            bottom: n.bottom,
            alpha: n.alpha,
            translation_x: n.translation_x,
            translation_y: n.translation_y,
            scale_x: n.scale_x,
            scale_y: n.scale_y,
            rotation: n.rotation,
            elevation: n.elevation,
            version: n.version,
        })
    }

    /// Render a render-node tree into a surface with damage tracking.
    ///
    /// If the tree version is unchanged since the last render, no raster work
    /// happens: a new frame sequence is produced reusing the previous pixels.
    pub fn surface_render_node(&mut self, surface: Handle, root: Handle, cause_id: i64) -> RenderResult<u64> {
        if !self.render_nodes.contains(root) {
            return Err(RenderError::BadHandle);
        }
        let version = RenderNode::subtree_version(&self.render_nodes, root);
        let cached = {
            let s = self.surfaces.get(surface).ok_or(RenderError::BadHandle)?;
            s.last_tree_version == Some(version) && s.frame.is_some()
        };
        if cached {
            return Ok(self
                .surfaces
                .get_mut(surface)
                .ok_or(RenderError::BadHandle)?
                .present_cached(cause_id));
        }
        // Fresh raster.
        self.canvas_for_surface(surface)?;
        {
            let s = self.surfaces.get_mut(surface).ok_or(RenderError::BadHandle)?;
            s.pixmap.fill(tiny_skia::Color::TRANSPARENT);
        }
        self.surface_canvas_mut(surface)?.reset_state();
        self.unsupported_ops.clear();
        self.render_node_recursive(surface, root, 0)?;
        let unsupported = self.take_unsupported();
        let seq = self
            .surfaces
            .get_mut(surface)
            .ok_or(RenderError::BadHandle)?
            .present(cause_id, unsupported);
        self.surfaces
            .get_mut(surface)
            .ok_or(RenderError::BadHandle)?
            .last_tree_version = Some(version);
        Ok(seq)
    }

    /// Rasterise a render node's display list **over** the current contents of
    /// a bitmap. This is the backing store for `Canvas(bitmap)`: the bitmap is
    /// copied into a scratch surface, the display list is replayed (without the
    /// transparent clear `surface_render_node` performs), and the result is
    /// copied back. Used by the raster-canvas natives.
    pub fn bitmap_render_node(&mut self, bitmap: Handle, root: Handle) -> RenderResult<()> {
        let base = self
            .bitmaps
            .get(bitmap)
            .ok_or(RenderError::BadHandle)?
            .pixels()
            .to_vec();
        self.bitmap_render_node_over(bitmap, root, &base)
    }

    /// Like [`bitmap_render_node`](Self::bitmap_render_node), but seeds the
    /// scratch surface from an explicit base snapshot instead of the bitmap's
    /// live pixels. `Canvas(bitmap)` uses this so replaying the accumulating
    /// display list never double-applies earlier ops.
    pub fn bitmap_render_node_over(
        &mut self,
        bitmap: Handle,
        root: Handle,
        base: &[u32],
    ) -> RenderResult<()> {
        if !self.bitmaps.contains(bitmap) {
            return Err(RenderError::BadHandle);
        }
        if !self.render_nodes.contains(root) {
            return Err(RenderError::BadHandle);
        }
        let (w, h) = {
            let b = self.bitmaps.get(bitmap).ok_or(RenderError::BadHandle)?;
            (b.width(), b.height())
        };
        let surface = self.surface_create(w, h, 1.0)?;
        // Seed the scratch surface from the base snapshot so drawing composites
        // over the bitmap's original content (Android `Canvas(bitmap)`).
        {
            let s = self.surfaces.get_mut(surface).ok_or(RenderError::BadHandle)?;
            s.pixmap.fill(tiny_skia::Color::TRANSPARENT);
            for (d, &word) in s.pixmap.pixels_mut().iter_mut().zip(base.iter()) {
                *d = tiny_skia::PremultipliedColorU8::from_rgba(
                    (word & 0xff) as u8,
                    ((word >> 8) & 0xff) as u8,
                    ((word >> 16) & 0xff) as u8,
                    ((word >> 24) & 0xff) as u8,
                )
                .unwrap_or(tiny_skia::PremultipliedColorU8::TRANSPARENT);
            }
        }
        let canvas = self.canvas_for_surface(surface)?;
        self.surface_canvas_mut(surface)?.reset_state();
        self.unsupported_ops.clear();
        self.render_node_recursive(surface, root, 0)?;
        {
            let pm = &self.surfaces.get(surface).ok_or(RenderError::BadHandle)?.pixmap;
            self.bitmaps
                .get_mut(bitmap)
                .ok_or(RenderError::BadHandle)?
                .from_pixmap_mut(pm);
        }
        // The scratch surface/canvas are transient.
        let _ = self.surfaces.remove(surface);
        let _ = self.canvases.remove(canvas);
        Ok(())
    }

    fn render_node_recursive(&mut self, surface: Handle, node: Handle, depth: usize) -> RenderResult<()> {
        if depth > 512 {
            return Err(RenderError::invalid("render node recursion too deep"));
        }
        let Some(n) = self.render_nodes.get(node) else {
            return Err(RenderError::BadHandle);
        };
        let props = NodeProps {
            matrix: n.local_matrix(),
            alpha: n.alpha,
            clip_to_bounds: n.clip_to_bounds,
            clip_to_outline: n.clip_to_outline,
            outline: n.outline.clone(),
            elevation: n.elevation,
            ambient_shadow_color: n.ambient_shadow_color,
            spot_shadow_color: n.spot_shadow_color,
            local_bounds: RectF::new(
                0.0,
                0.0,
                n.width() as f32,
                n.height() as f32,
            ),
            render_effect: n.render_effect.clone(),
        };
        let ops = n.display_list.clone();
        let mut children: Vec<(f32, u64, Handle)> = n
            .children
            .iter()
            .filter_map(|c| {
                self.render_nodes
                    .get(*c)
                    .map(|cn| (cn.elevation, cn.version, *c))
            })
            .collect();

        self.canvas_save(surface)?;
        self.canvas_concat_matrix(surface, &props.matrix)?;
        if props.clip_to_bounds {
            self.canvas_clip_rect(surface, props.local_bounds, ClipOp::Intersect, false)?;
        }
        if props.clip_to_outline {
            if let Some(p) = props.outline.as_path() {
                self.canvas_clip_path_value(surface, &p, ClipOp::Intersect, true)?;
            }
        }
        let alpha = props.alpha;
        if alpha < 1.0 {
            self.canvas_save_layer_alpha(surface, Some(props.local_bounds), (alpha * 255.0) as u8)?;
        }
        // A node-level `RenderEffect` renders the node into its own offscreen
        // layer; the effect is applied to that layer just before compositing.
        let effect = props.render_effect.clone();
        if effect.is_some() {
            self.canvas_save_layer_alpha(surface, Some(props.local_bounds), 255)?;
        }
        if props.elevation > 0.0 {
            self.draw_node_shadow(surface, &props)?;
        }

        for op in &ops {
            self.execute_display_op(surface, op)?;
        }

        // Siblings draw in insertion order (Android `ViewGroup` child index),
        // except that elevation lifts a node above its lower siblings. The
        // previous version-tuple tiebreak reordered siblings by how many ops
        // they happened to record, which put a later-created scrim *under* an
        // earlier high-op-count app bar and broke AlertDialog dimming.
        children.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        for (_, _, child) in children {
            self.render_node_recursive(surface, child, depth + 1)?;
        }

        if let Some(effect) = &effect {
            self.apply_render_effect(surface, effect)?;
            self.canvas_restore(surface)?;
        }
        if alpha < 1.0 {
            self.canvas_restore(surface)?;
        }
        self.canvas_restore(surface)?;
        if let Some(n) = self.render_nodes.get_mut(node) {
            n.raster_ops += ops.len() as u64;
        }
        Ok(())
    }

    /// Apply a node `RenderEffect` to the topmost active save-layer. The layer
    /// holds the node's rasterised content (including children).
    fn apply_render_effect(&mut self, surface: Handle, effect: &RenderEffectSpec) -> RenderResult<()> {
        let canvas_h = self
            .surfaces
            .get(surface)
            .and_then(|s| s.canvas)
            .ok_or(RenderError::BadHandle)?;
        let canvas = self.canvases.get_mut(canvas_h).ok_or(RenderError::BadHandle)?;
        let Some(pixmap) = canvas.top_layer_mut() else {
            return Ok(());
        };
        match effect {
            RenderEffectSpec::Blur { sigma_x, sigma_y, .. } => {
                // `box_blur` is isotropic; use the geometric mean of the two
                // sigmas so an anisotropic blur spreads by the same area.
                let sigma = (*sigma_x * *sigma_y).max(0.0).sqrt();
                if sigma > 0.0 {
                    canvas::blur_gaussian(pixmap, sigma);
                }
            }
            RenderEffectSpec::ColorFilter(cf) => canvas::apply_color_filter(pixmap, cf),
        }
        Ok(())
    }

    fn draw_node_shadow(&mut self, surface: Handle, props: &NodeProps) -> RenderResult<()> {
        // Two-lobe approximation of Android's `SkShadowUtils` shadow: a broad,
        // soft **ambient** lobe that hugs the outline and a tighter, stronger
        // **spot** lobe displaced down by the elevation. Both are drawn as the
        // node outline blurred with the Skia radius->sigma conversion (the
        // mask-filter path in `canvas.rs`). Android derives the exact lobe
        // sigmas and light-direction offset from the light position; ACOS uses
        // the vertical-light default (`lightX=0, lightY=-1`) that Material
        // elevations assume. Labelled approximate in the fidelity list.
        let e = props.elevation;
        let path = props
            .outline
            .as_path()
            .unwrap_or_else(|| Path::from_rect(props.local_bounds));
        let ambient = Paint {
            color: props.ambient_shadow_color,
            style: PaintStyle::Fill,
            anti_alias: true,
            mask_filter: Some(MaskFilterSpec::Blur {
                radius: (e * 0.6).max(1.0),
                style: BlurStyle::Normal,
            }),
            ..Default::default()
        };
        let spot = Paint {
            color: props.spot_shadow_color,
            style: PaintStyle::Fill,
            anti_alias: true,
            mask_filter: Some(MaskFilterSpec::Blur {
                radius: e.max(1.0),
                style: BlurStyle::Normal,
            }),
            ..Default::default()
        };
        self.canvas_save(surface)?;
        self.draw_path_with_paint(surface, &path, &ambient)?;
        self.canvas_translate(surface, 0.0, e)?;
        self.draw_path_with_paint(surface, &path, &spot)?;
        self.canvas_restore(surface)?;
        Ok(())
    }

    fn execute_display_op(&mut self, surface: Handle, op: &DisplayOp) -> RenderResult<()> {
        match op {
            DisplayOp::Save => {
                self.canvas_save(surface)?;
            }
            DisplayOp::Restore => {
                self.canvas_restore(surface)?;
            }
            DisplayOp::SaveLayerAlpha(bounds, alpha) => {
                self.canvas_save_layer_alpha(surface, *bounds, *alpha)?;
            }
            DisplayOp::Concat(m) => {
                self.canvas_concat_matrix(surface, m)?;
            }
            DisplayOp::SetMatrix(m) => {
                self.canvas_set_matrix_value(surface, m)?;
            }
            DisplayOp::ClipRect(r, op, aa) => {
                self.canvas_clip_rect(surface, *r, *op, *aa)?;
            }
            DisplayOp::ClipPath(p, op, aa) => {
                self.canvas_clip_path(surface, *p, *op, *aa)?;
            }
            DisplayOp::DrawColor(c) => {
                self.canvas_draw_color(surface, *c)?;
            }
            DisplayOp::DrawRect(r, p) => {
                self.canvas_draw_rect(surface, *r, *p)?;
            }
            DisplayOp::DrawRoundRect(r, rx, ry, p) => {
                self.canvas_draw_round_rect(surface, *r, *rx, *ry, *p)?;
            }
            DisplayOp::DrawOval(r, p) => {
                self.canvas_draw_oval(surface, *r, *p)?;
            }
            DisplayOp::DrawCircle(cx, cy, radius, p) => {
                self.canvas_draw_circle(surface, *cx, *cy, *radius, *p)?;
            }
            DisplayOp::DrawArc(r, start, sweep, use_center, p) => {
                self.canvas_draw_arc(surface, *r, *start, *sweep, *use_center, *p)?;
            }
            DisplayOp::DrawLine(x0, y0, x1, y1, p) => {
                self.canvas_draw_line(surface, *x0, *y0, *x1, *y1, *p)?;
            }
            DisplayOp::DrawPoint(x, y, p) => {
                self.canvas_draw_point(surface, *x, *y, *p)?;
            }
            DisplayOp::DrawPath(path, p) => {
                self.canvas_draw_path(surface, *path, *p)?;
            }
            DisplayOp::DrawBitmap(b, src, dst, p, filter) => {
                self.canvas_draw_bitmap(surface, *b, *src, *dst, *p, *filter)?;
            }
            DisplayOp::DrawNinePatch(b, chunk, dst, p, filter) => {
                self.canvas_draw_nine_patch(surface, *b, chunk.clone(), *dst, Some(*p), *filter)?;
            }
            DisplayOp::DrawText(text, x, y, p) => {
                self.canvas_draw_text(surface, text, *x, *y, *p)?;
            }
            DisplayOp::DrawTextEllipsized(text, x, y, p, max_width, mode) => {
                self.canvas_draw_text_layout(surface, text, *x, *y, *p, *max_width, *mode)?;
            }
            DisplayOp::DrawRenderNode(child) => {
                self.render_node_recursive(surface, *child, 0)?;
            }
        }
        Ok(())
    }

    fn canvas_concat_matrix(&mut self, surface: Handle, m: &Matrix3x3) -> RenderResult<()> {
        self.surface_canvas_mut(surface)?.concat(m);
        Ok(())
    }

    fn canvas_set_matrix_value(&mut self, surface: Handle, m: &Matrix3x3) -> RenderResult<()> {
        self.surface_canvas_mut(surface)?.set_matrix(m);
        Ok(())
    }

    // -----------------------------------------------------------------
    // Diagnostics
    // -----------------------------------------------------------------

    pub fn shape_cache_hit_rate(&self) -> f64 {
        self.text
            .as_ref()
            .map(|t| t.cache_stats().hit_rate())
            .unwrap_or(0.0)
    }

    pub fn shape_cache_stats(&self) -> crate::text::ShapeCacheStats {
        self.text
            .as_ref()
            .map(|t| t.cache_stats())
            .unwrap_or_default()
    }

    pub fn reset_text_cache_stats(&mut self) {
        if let Some(t) = self.text.as_mut() {
            t.reset_cache_stats();
        }
    }
}

/// Snapshot of a render node's properties.
#[derive(Copy, Clone, Debug)]
pub struct RenderNodeInfo {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
    pub alpha: f32,
    pub translation_x: f32,
    pub translation_y: f32,
    pub scale_x: f32,
    pub scale_y: f32,
    pub rotation: f32,
    pub elevation: f32,
    pub version: u64,
}

fn make_stops(colors: &[u32], positions: Option<&[f32]>) -> Vec<tiny_skia::GradientStop> {
    let n = colors.len();
    colors
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let pos = match positions {
                Some(p) if i < p.len() => p[i],
                _ => {
                    if n <= 1 {
                        0.0
                    } else {
                        i as f32 / (n - 1) as f32
                    }
                }
            };
            tiny_skia::GradientStop::new(pos, crate::color::argb_to_sk(*c))
        })
        .collect()
}

/// Build an angle-coloured bitmap for a sweep gradient (approximate: 256×256
/// angular resolution, `Pad` outside the square).
fn make_sweep_pixmap(colors: &[u32], positions: Option<&[f32]>) -> tiny_skia::Pixmap {
    let size: u32 = 256;
    let mut pm = tiny_skia::Pixmap::new(size, size).unwrap();
    let center = size as f32 * 0.5;
    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 + 0.5 - center;
            let dy = y as f32 + 0.5 - center;
            let mut t = dy.atan2(dx) / (2.0 * std::f32::consts::PI);
            if t < 0.0 {
                t += 1.0;
            }
            let c = sample_gradient(colors, positions, t);
            pm.pixels_mut()[(y * size + x) as usize] = c;
        }
    }
    pm
}

fn sample_gradient(colors: &[u32], positions: Option<&[f32]>, t: f32) -> tiny_skia::PremultipliedColorU8 {
    if colors.is_empty() {
        return tiny_skia::PremultipliedColorU8::TRANSPARENT;
    }
    if colors.len() == 1 {
        return premul_color(colors[0]);
    }
    let n = colors.len();
    let mut lo = 0usize;
    let mut hi = n - 1;
    let pos = |i: usize| -> f32 {
        match positions {
            Some(p) if i < p.len() => p[i],
            _ => i as f32 / (n - 1) as f32,
        }
    };
    for i in 0..n - 1 {
        if t >= pos(i) && t <= pos(i + 1) {
            lo = i;
            hi = i + 1;
            break;
        }
    }
    let p0 = pos(lo);
    let p1 = pos(hi);
    let local = if p1 > p0 { (t - p0) / (p1 - p0) } else { 0.0 };
    let (r0, g0, b0, a0) = crate::color::argb_to_rgba(colors[lo]);
    let (r1, g1, b1, a1) = crate::color::argb_to_rgba(colors[hi]);
    let lerp = |a: u8, b: u8| -> u8 { (a as f32 + (b as f32 - a as f32) * local).round() as u8 };
    let word = crate::color::pack_premul(lerp(r0, r1), lerp(g0, g1), lerp(b0, b1), lerp(a0, a1));
    tiny_skia::PremultipliedColorU8::from_rgba(
        (word & 0xff) as u8,
        ((word >> 8) & 0xff) as u8,
        ((word >> 16) & 0xff) as u8,
        ((word >> 24) & 0xff) as u8,
    )
    .unwrap_or(tiny_skia::PremultipliedColorU8::TRANSPARENT)
}

fn premul_color(argb: u32) -> tiny_skia::PremultipliedColorU8 {
    let (r, g, b, a) = crate::color::argb_to_rgba(argb);
    let word = crate::color::pack_premul(r, g, b, a);
    tiny_skia::PremultipliedColorU8::from_rgba(
        (word & 0xff) as u8,
        ((word >> 8) & 0xff) as u8,
        ((word >> 16) & 0xff) as u8,
        ((word >> 24) & 0xff) as u8,
    )
    .unwrap_or(tiny_skia::PremultipliedColorU8::TRANSPARENT)
}

impl Matrix3x3 {
    /// Construct from a tiny-skia affine transform (inverse of `to_affine`).
    pub fn from_affine(t: &tiny_skia::Transform) -> Matrix3x3 {
        let mut m = Matrix3x3::identity();
        m.m[0] = t.sx;
        m.m[1] = t.kx;
        m.m[2] = t.tx;
        m.m[3] = t.ky;
        m.m[4] = t.sy;
        m.m[5] = t.ty;
        m
    }
}
