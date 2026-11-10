//! `android.graphics.Paint`, `ColorFilter`, `BlendModeColorFilter` and
//! `PorterDuffColorFilter` natives, backed by a generation-checked handle table
//! ([`super::gfx_handles`]).
//!
//! Each `jlong` the framework passes is a handle into [`paints()`] (or, for the
//! filter classes, [`filters()`]). A stale/forged handle becomes a structured
//! VM error, never a panic. Text metrics are delegated to
//! [`super::gfx_text`], which shapes with `acos-render` (ADR-0019), so
//! `Paint.measureText` agrees with the renderer.
//!
//! `Paint` values are held as plain primitives (no borrowed rasteriser state),
//! matching `acos_render::Paint` field-for-field where the two overlap, so a
//! future canvas native can build the renderer's paint without loss.

use std::sync::{Mutex, OnceLock};

use acos_render::color::PorterDuffMode;
use acos_render::matrix::Matrix3x3;
use acos_render::paint::{ColorFilterSpec, PaintStyle, ShaderSpec, StrokeCap, StrokeJoin, TileMode};
use acos_render::path::Path;

use crate::heap::{ArrayData, Object};

use crate::class::NativeCtx;
use crate::error::{VmError, VmResult};
use crate::natives::gfx_handles::{Handle, HandleError, HandleTable};
use crate::natives::gfx_text::{self, TextAttrs};
use crate::vm::Vm;

// `android.graphics.Paint` flag bits (frameworks/base Paint.java, android-14).
const ANTI_ALIAS_FLAG: i32 = 0x01;
const FILTER_BITMAP_FLAG: i32 = 0x02;
const DITHER_FLAG: i32 = 0x04;
const UNDERLINE_TEXT_FLAG: i32 = 0x08;
const STRIKE_THRU_TEXT_FLAG: i32 = 0x10;
const FAKE_BOLD_TEXT_FLAG: i32 = 0x20;
const LINEAR_TEXT_FLAG: i32 = 0x40;
const SUBPIXEL_TEXT_FLAG: i32 = 0x80;
const HINTING_ON: i32 = 0x200;

/// A native `Paint`.
#[derive(Clone, Debug)]
pub struct PaintRec {
    pub flags: i32,
    pub color: u32,
    pub alpha: i32,
    pub style: i32,
    pub stroke_width: f32,
    pub stroke_miter: f32,
    pub stroke_cap: i32,
    pub stroke_join: i32,
    pub text_align: i32,
    pub text_size: f32,
    pub text_scale_x: f32,
    pub text_skew_x: f32,
    pub letter_spacing: f32,
    pub word_spacing: f32,
    pub hinting: i32,
    pub start_hyphen: i32,
    pub end_hyphen: i32,
    pub typeface: i64,
    pub shader: i64,
    pub color_filter: i64,
    pub mask_filter: i64,
    pub path_effect: i64,
    pub xfermode: i32,
    pub text_locales: i32,
    pub font_features: Option<String>,
    pub font_variations: Option<String>,
    pub shadow: Option<(f32, f32, f32, u32)>,
}

impl Default for PaintRec {
    fn default() -> Self {
        PaintRec {
            // `Paint()` calls `this(ANTI_ALIAS_FLAG)` on Android 14 and the
            // Java side applies the flag through `nSetFlags`; the native
            // object itself starts at 0 (Skia `SkPaint` default).
            flags: 0,
            color: 0xff00_0000,
            alpha: 255,
            style: 0,
            stroke_width: 0.0,
            stroke_miter: 4.0,
            stroke_cap: 0,
            stroke_join: 0,
            text_align: 0,
            text_size: 12.0,
            text_scale_x: 1.0,
            text_skew_x: 0.0,
            letter_spacing: 0.0,
            word_spacing: 0.0,
            hinting: 1,
            start_hyphen: 0,
            end_hyphen: 0,
            typeface: 0,
            shader: 0,
            color_filter: 0,
            mask_filter: 0,
            path_effect: 0,
            xfermode: 0,
            text_locales: 0,
            font_features: None,
            font_variations: None,
            shadow: None,
        }
    }
}

impl PaintRec {
    fn set_flag(&mut self, bit: i32, on: bool) {
        if on {
            self.flags |= bit;
        } else {
            self.flags &= !bit;
        }
    }

    fn flag(&self, bit: i32) -> bool {
        self.flags & bit != 0
    }

    /// The `TextAttrs` the shaping engine consumes.
    pub fn text_attrs(&self) -> TextAttrs {
        TextAttrs {
            typeface: self.typeface,
            text_size: self.text_size,
            scale_x: self.text_scale_x,
            skew_x: self.text_skew_x,
            letter_spacing: self.letter_spacing,
            fake_bold: self.flag(FAKE_BOLD_TEXT_FLAG),
            features: self.font_features.clone(),
            variations: self.font_variations.clone(),
        }
    }
}

static PAINTS: OnceLock<Mutex<HandleTable<PaintRec>>> = OnceLock::new();
static FILTERS: OnceLock<Mutex<HandleTable<ColorFilterSpec>>> = OnceLock::new();

fn paints() -> &'static Mutex<HandleTable<PaintRec>> {
    PAINTS.get_or_init(|| Mutex::new(HandleTable::new()))
}

fn filters() -> &'static Mutex<HandleTable<ColorFilterSpec>> {
    FILTERS.get_or_init(|| Mutex::new(HandleTable::new()))
}

/// Resolve a colour-filter `jlong` into its description (used by
/// `RenderEffect.createColorFilterEffect`).
pub(crate) fn filter_spec(handle: i64) -> Option<ColorFilterSpec> {
    filters()
        .lock()
        .ok()
        .and_then(|g| g.get(Handle::from_jlong(handle)).ok().cloned())
}

// W75-A: `android.graphics` shaders. A shader is a pure description, so the
// handle table stores a `ShaderSpec` directly (no borrowed rasteriser state).
// `gfx_state::snapshot_paint` resolves the `Paint`'s shader handle into the
// frozen renderer paint at record time.
static SHADERS: OnceLock<Mutex<HandleTable<ShaderSpec>>> = OnceLock::new();

fn shaders() -> &'static Mutex<HandleTable<ShaderSpec>> {
    SHADERS.get_or_init(|| Mutex::new(HandleTable::new()))
}

fn lock_shaders() -> VmResult<std::sync::MutexGuard<'static, HandleTable<ShaderSpec>>> {
    shaders()
        .lock()
        .map_err(|_| VmError::Internal("Shader handle table poisoned".to_string()))
}

/// Resolve a shader `jlong` into its description (used by the paint snapshot).
pub fn shader_spec(handle: i64) -> Option<ShaderSpec> {
    shaders()
        .lock()
        .ok()
        .and_then(|g| g.get(Handle::from_jlong(handle)).ok().cloned())
}

fn store_shader(spec: ShaderSpec) -> VmResult<i64> {
    Ok(lock_shaders()?
        .insert(spec)
        .to_jlong())
}

fn tile_mode(ordinal: i32) -> TileMode {
    TileMode::from_ordinal(ordinal).unwrap_or(TileMode::Clamp)
}

/// `Color.pack(int)` stores the ARGB word in the high 32 bits for sRGB.
fn color_array(vm: &Vm, h: u32) -> VmResult<Vec<u32>> {
    if h == 0 {
        return Err(VmError::Internal("shader colours: null long[]".to_string()));
    }
    match vm.heap.get(h) {
        Some(Object::Array(a)) => match &a.data {
            ArrayData::Long(v) => Ok(v.iter().map(|c| (*c >> 32) as u32).collect()),
            _ => Err(VmError::Internal("shader colours: expected long[]".to_string())),
        },
        _ => Err(VmError::Internal("shader colours: expected long[]".to_string())),
    }
}

fn optional_float_array(vm: &Vm, h: u32) -> VmResult<Option<Vec<f32>>> {
    if h == 0 {
        return Ok(None);
    }
    match vm.heap.get(h) {
        Some(Object::Array(a)) => match &a.data {
            ArrayData::Float(v) => Ok(Some(v.clone())),
            _ => Err(VmError::Internal("shader positions: expected float[]".to_string())),
        },
        _ => Ok(None),
    }
}

fn local_matrix(h: i64) -> VmResult<Matrix3x3> {
    if h == 0 {
        return Ok(Matrix3x3::identity());
    }
    let values = super::gfx_matrix::matrix_values(h)?;
    Ok(super::gfx_matrix::matrix_from_values(values))
}

fn bad(kind: &str, e: &HandleError) -> VmError {
    VmError::Internal(format!("{kind}: {e}"))
}

fn lock_paints() -> VmResult<std::sync::MutexGuard<'static, HandleTable<PaintRec>>> {
    paints()
        .lock()
        .map_err(|_| VmError::Internal("Paint handle table poisoned".to_string()))
}

/// Read a paint record for a `jlong` handle.
pub fn paint_rec(handle: i64) -> VmResult<PaintRec> {
    lock_paints()?
        .get(Handle::from_jlong(handle))
        .cloned()
        .map_err(|e| bad("Paint", &e))
}

fn with_paint<R>(handle: i64, f: impl FnOnce(&mut PaintRec) -> R) -> VmResult<R> {
    let mut g = lock_paints()?;
    let rec = g
        .get_mut(Handle::from_jlong(handle))
        .map_err(|e| bad("Paint", &e))?;
    Ok(f(rec))
}

// ---------------------------------------------------------------------------
// Accessor macros (each generates one native `fn(&mut Vm, NativeCtx)`)
// ---------------------------------------------------------------------------

macro_rules! set_i32 {
    ($name:ident, $field:ident) => {
        fn $name(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
            let h = vm.arg_i64(ctx, 0);
            let v = vm.arg_i32(ctx, 1);
            with_paint(h, |r| r.$field = v)?;
            Ok(())
        }
    };
}

macro_rules! get_i32 {
    ($name:ident, $field:ident) => {
        fn $name(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
            let h = vm.arg_i64(ctx, 0);
            vm.ret_i32(ctx, with_paint(h, |r| r.$field)?);
            Ok(())
        }
    };
}

macro_rules! set_f32 {
    ($name:ident, $field:ident) => {
        fn $name(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
            let h = vm.arg_i64(ctx, 0);
            let v = vm.arg_f32(ctx, 1);
            with_paint(h, |r| r.$field = v)?;
            Ok(())
        }
    };
}

macro_rules! get_f32 {
    ($name:ident, $field:ident) => {
        fn $name(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
            let h = vm.arg_i64(ctx, 0);
            vm.ret_f32(ctx, with_paint(h, |r| r.$field)?);
            Ok(())
        }
    };
}

macro_rules! set_flag {
    ($name:ident, $bit:expr) => {
        fn $name(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
            let h = vm.arg_i64(ctx, 0);
            let v = vm.arg_bool(ctx, 1);
            with_paint(h, |r| r.set_flag($bit, v))?;
            Ok(())
        }
    };
}

macro_rules! get_flag {
    ($name:ident, $bit:expr) => {
        fn $name(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
            let h = vm.arg_i64(ctx, 0);
            vm.ret_bool(ctx, with_paint(h, |r| r.flag($bit))?);
            Ok(())
        }
    };
}

set_i32!(paint_set_style, style);
get_i32!(paint_get_style, style);
set_f32!(paint_set_stroke_width, stroke_width);
get_f32!(paint_get_stroke_width, stroke_width);
set_f32!(paint_set_stroke_miter, stroke_miter);
get_f32!(paint_get_stroke_miter, stroke_miter);
set_i32!(paint_set_stroke_cap, stroke_cap);
get_i32!(paint_get_stroke_cap, stroke_cap);
set_i32!(paint_set_stroke_join, stroke_join);
get_i32!(paint_get_stroke_join, stroke_join);
set_i32!(paint_set_text_align, text_align);
get_i32!(paint_get_text_align, text_align);
set_f32!(paint_set_text_size, text_size);
get_f32!(paint_get_text_size, text_size);
set_f32!(paint_set_text_scale_x, text_scale_x);
get_f32!(paint_get_text_scale_x, text_scale_x);
set_f32!(paint_set_text_skew_x, text_skew_x);
get_f32!(paint_get_text_skew_x, text_skew_x);
set_f32!(paint_set_letter_spacing, letter_spacing);
get_f32!(paint_get_letter_spacing, letter_spacing);
set_f32!(paint_set_word_spacing, word_spacing);
get_f32!(paint_get_word_spacing, word_spacing);
set_i32!(paint_set_hinting, hinting);
get_i32!(paint_get_hinting, hinting);
set_i32!(paint_set_start_hyphen, start_hyphen);
get_i32!(paint_get_start_hyphen, start_hyphen);
set_i32!(paint_set_end_hyphen, end_hyphen);
get_i32!(paint_get_end_hyphen, end_hyphen);
set_i32!(paint_set_xfermode, xfermode);
set_i32!(paint_set_text_locales_id, text_locales);

set_flag!(paint_set_anti_alias, ANTI_ALIAS_FLAG);
set_flag!(paint_set_dither, DITHER_FLAG);
set_flag!(paint_set_filter_bitmap, FILTER_BITMAP_FLAG);
set_flag!(paint_set_fake_bold, FAKE_BOLD_TEXT_FLAG);
set_flag!(paint_set_linear_text, LINEAR_TEXT_FLAG);
set_flag!(paint_set_subpixel_text, SUBPIXEL_TEXT_FLAG);
set_flag!(paint_set_underline_text, UNDERLINE_TEXT_FLAG);
set_flag!(paint_set_strike_thru_text, STRIKE_THRU_TEXT_FLAG);
set_flag!(paint_set_elegant_text_height, HINTING_ON);
get_flag!(paint_is_elegant_text_height, HINTING_ON);

pub(crate) fn install(vm: &mut Vm) -> VmResult<()> {
    let p = "Landroid/graphics/Paint;";
    vm.register_native(p, "nInit", "()J", paint_init);
    vm.register_native(p, "nInitWithPaint", "(J)J", paint_init_with);
    vm.register_native(p, "nSet", "(J)V", paint_set);
    // API 34 declares `nSet(long dst, long src)`; the Java `Paint(Paint)` copy
    // constructor routes through it. Same copy semantics as the legacy form.
    vm.register_native(p, "nSet", "(JJ)V", paint_set);
    vm.register_native(p, "nReset", "(J)V", paint_reset);
    vm.register_native(p, "nGetNativeFinalizer", "()J", release_func);

    vm.register_native(p, "nGetFlags", "(J)I", paint_get_flags);
    vm.register_native(p, "nSetFlags", "(JI)V", paint_set_flags);
    vm.register_native(p, "nSetColor", "(JI)V", paint_set_color);
    vm.register_native(p, "nSetColor", "(JJ)V", paint_set_color_long);
    vm.register_native(p, "nSetAlpha", "(JI)V", paint_set_alpha);
    vm.register_native(p, "nSetAntiAlias", "(JZ)V", paint_set_anti_alias);
    vm.register_native(p, "nSetDither", "(JZ)V", paint_set_dither);
    vm.register_native(p, "nSetFilterBitmap", "(JZ)V", paint_set_filter_bitmap);
    vm.register_native(p, "nSetFakeBoldText", "(JZ)V", paint_set_fake_bold);
    vm.register_native(p, "nSetLinearText", "(JZ)V", paint_set_linear_text);
    vm.register_native(p, "nSetSubpixelText", "(JZ)V", paint_set_subpixel_text);
    vm.register_native(p, "nSetUnderlineText", "(JZ)V", paint_set_underline_text);
    vm.register_native(p, "nSetStrikeThruText", "(JZ)V", paint_set_strike_thru_text);
    vm.register_native(p, "nSetStyle", "(JI)V", paint_set_style);
    vm.register_native(p, "nGetStyle", "(J)I", paint_get_style);
    vm.register_native(p, "nSetStrokeWidth", "(JF)V", paint_set_stroke_width);
    vm.register_native(p, "nGetStrokeWidth", "(J)F", paint_get_stroke_width);
    vm.register_native(p, "nSetStrokeMiter", "(JF)V", paint_set_stroke_miter);
    vm.register_native(p, "nGetStrokeMiter", "(J)F", paint_get_stroke_miter);
    vm.register_native(p, "nSetStrokeCap", "(JI)V", paint_set_stroke_cap);
    vm.register_native(p, "nGetStrokeCap", "(J)I", paint_get_stroke_cap);
    vm.register_native(p, "nSetStrokeJoin", "(JI)V", paint_set_stroke_join);
    vm.register_native(p, "nGetStrokeJoin", "(J)I", paint_get_stroke_join);
    vm.register_native(p, "nSetTextAlign", "(JI)V", paint_set_text_align);
    vm.register_native(p, "nGetTextAlign", "(J)I", paint_get_text_align);
    vm.register_native(p, "nSetTextSize", "(JF)V", paint_set_text_size);
    vm.register_native(p, "nGetTextSize", "(J)F", paint_get_text_size);
    vm.register_native(p, "nSetTextScaleX", "(JF)V", paint_set_text_scale_x);
    vm.register_native(p, "nGetTextScaleX", "(J)F", paint_get_text_scale_x);
    vm.register_native(p, "nSetTextSkewX", "(JF)V", paint_set_text_skew_x);
    vm.register_native(p, "nGetTextSkewX", "(J)F", paint_get_text_skew_x);
    vm.register_native(p, "nSetLetterSpacing", "(JF)V", paint_set_letter_spacing);
    vm.register_native(p, "nGetLetterSpacing", "(J)F", paint_get_letter_spacing);
    vm.register_native(p, "nSetWordSpacing", "(JF)V", paint_set_word_spacing);
    vm.register_native(p, "nGetWordSpacing", "(J)F", paint_get_word_spacing);
    vm.register_native(p, "nSetHinting", "(JI)V", paint_set_hinting);
    vm.register_native(p, "nGetHinting", "(J)I", paint_get_hinting);
    vm.register_native(p, "nSetElegantTextHeight", "(JZ)V", paint_set_elegant_text_height);
    vm.register_native(p, "nIsElegantTextHeight", "(J)Z", paint_is_elegant_text_height);
    vm.register_native(p, "nSetStartHyphenEdit", "(JI)V", paint_set_start_hyphen);
    vm.register_native(p, "nGetStartHyphenEdit", "(J)I", paint_get_start_hyphen);
    vm.register_native(p, "nSetEndHyphenEdit", "(JI)V", paint_set_end_hyphen);
    vm.register_native(p, "nGetEndHyphenEdit", "(J)I", paint_get_end_hyphen);

    vm.register_native(p, "nSetTypeface", "(JJ)V", paint_set_typeface);
    vm.register_native(p, "nSetShader", "(JJ)J", paint_set_shader);
    vm.register_native(p, "nSetColorFilter", "(JJ)J", paint_set_color_filter);
    vm.register_native(p, "nSetMaskFilter", "(JJ)J", paint_set_mask_filter);
    vm.register_native(p, "nSetPathEffect", "(JJ)J", paint_set_path_effect);
    vm.register_native(p, "nSetXfermode", "(JI)V", paint_set_xfermode);
    vm.register_native(p, "nSetShadowLayer", "(JFFFJJ)V", paint_set_shadow);
    vm.register_native(p, "nHasShadowLayer", "(J)Z", paint_has_shadow);
    vm.register_native(p, "nGetUnderlinePosition", "(J)F", paint_get_underline_position);
    vm.register_native(p, "nGetUnderlineThickness", "(J)F", paint_get_underline_thickness);
    vm.register_native(p, "nGetStrikeThruPosition", "(J)F", paint_get_strike_position);
    vm.register_native(p, "nGetStrikeThruThickness", "(J)F", paint_get_strike_thickness);
    vm.register_native(p, "nSetTextLocales", "(JLjava/lang/String;)I", paint_set_text_locales);
    vm.register_native(p, "nSetTextLocalesByMinikinLocaleListId", "(JI)V", paint_set_text_locales_id);
    vm.register_native(p, "nSetFontFeatureSettings", "(JLjava/lang/String;)V", paint_set_font_features);
    vm.register_native(p, "nSetFontVariationSettings", "(JLjava/lang/String;)V", paint_set_font_variations);

    // Text measurement (routed through acos-render shaping).
    vm.register_native(p, "nGetFontMetrics", "(JLandroid/graphics/Paint$FontMetrics;)F", paint_font_metrics);
    vm.register_native(p, "nGetFontMetricsInt", "(JLandroid/graphics/Paint$FontMetricsInt;)I", paint_font_metrics_int);
    vm.register_native(p, "nGetFontMetricsIntForText", "(JLjava/lang/String;IIIIZLandroid/graphics/Paint$FontMetricsInt;)V", paint_font_metrics_text_string);
    vm.register_native(p, "nGetFontMetricsIntForText", "(J[CIIIIZLandroid/graphics/Paint$FontMetricsInt;)V", paint_font_metrics_text_chars);
    vm.register_native(p, "nGetTextAdvances", "(JLjava/lang/String;IIIII[FI)F", paint_text_advances_string);
    vm.register_native(p, "nGetTextAdvances", "(J[CIIIII[FI)F", paint_text_advances_chars);
    vm.register_native(p, "nGetRunCharacterAdvance", "(J[CIIIIZI[FI)F", paint_run_char_advance);
    vm.register_native(p, "nGetRunAdvance", "(J[CIIIIZI)F", paint_run_advance);
    vm.register_native(p, "nGetOffsetForAdvance", "(J[CIIIIZF)I", paint_offset_for_advance);
    vm.register_native(p, "nGetStringBounds", "(JLjava/lang/String;IIILandroid/graphics/Rect;)V", paint_string_bounds);
    vm.register_native(p, "nGetCharArrayBounds", "(J[CIIILandroid/graphics/Rect;)V", paint_char_array_bounds);
    vm.register_native(p, "nBreakText", "(JLjava/lang/String;ZFI[F)I", paint_break_text_string);
    vm.register_native(p, "nBreakText", "(J[CIIFI[F)I", paint_break_text_chars);
    vm.register_native(p, "nAscent", "(J)F", paint_ascent);
    vm.register_native(p, "nDescent", "(J)F", paint_descent);
    vm.register_native(p, "nHasGlyph", "(JILjava/lang/String;)Z", paint_has_glyph);
    vm.register_native(p, "nEqualsForTextMeasurement", "(JJ)Z", paint_equals_text);
    vm.register_native(p, "nGetTextPath", "(JI[CIIFFJ)V", paint_text_path_chars);
    vm.register_native(p, "nGetTextPath", "(JILjava/lang/String;IIFFJ)V", paint_text_path_string);
    vm.register_native(p, "nGetTextRunCursor", "(J[CIIIII)I", unsupported_text_cursor);
    vm.register_native(p, "nGetTextRunCursor", "(JLjava/lang/String;IIIII)I", unsupported_text_cursor);
    vm.register_native(p, "nGetFillPath", "(JJJ)Z", paint_get_fill_path);

    // Shaders (W75-A): gradient, bitmap-tile and compose shaders. A shader
    // handle is an `acos_render::paint::ShaderSpec`; `Paint.nSetShader` stores
    // it and the record-time snapshot resolves it into the frozen render paint.
    vm.register_native("Landroid/graphics/LinearGradient;", "nativeCreate", "(JFFFF[J[FIJ)J", lg_native_create);
    vm.register_native("Landroid/graphics/RadialGradient;", "nativeCreate", "(JFFFFFF[J[FIJ)J", rg_native_create);
    vm.register_native("Landroid/graphics/SweepGradient;", "nativeCreate", "(JFF[J[FJ)J", sg_native_create);
    vm.register_native("Landroid/graphics/BitmapShader;", "nativeCreate", "(JJIIZZ)J", bs_native_create);
    vm.register_native("Landroid/graphics/BitmapShader;", "nativeCreateWithMaxAniso", "(JJIIIZ)J", bs_native_create_aniso);
    vm.register_native("Landroid/graphics/ComposeShader;", "nativeCreate", "(JJJI)J", cs_native_create);
    vm.register_native("Landroid/graphics/Shader;", "nativeGetFinalizer", "()J", release_func);

    // `ColorSpace$Rgb` is constructed during `android.graphics` static init
    // (`new Paint()` transitively creates the sRGB colour space). ACOS models
    // it as an opaque handle: the renderer always works in sRGB/linear space,
    // so no RGB->XYZ transform state is needed. A non-sRGB space would change
    // that and is reported as unsupported by the renderer's conversion path.
    vm.register_native("Landroid/graphics/ColorSpace$Rgb;", "nativeCreate", "(FFFFFFF[F)J", colorspace_rgb_create);
    vm.register_native("Landroid/graphics/ColorSpace$Rgb;", "nativeGetNativeFinalizer", "()J", release_func);

    // Colour filters.
    vm.register_native("Landroid/graphics/BlendModeColorFilter;", "native_CreateBlendModeFilter", "(II)J", filter_create);
    vm.register_native("Landroid/graphics/PorterDuffColorFilter;", "native_CreateBlendModeFilter", "(II)J", filter_create);
    vm.register_native("Landroid/graphics/ColorFilter;", "nativeGetFinalizer", "()J", release_func);
    Ok(())
}

// ---------------------------------------------------------------------------
// Concrete natives
// ---------------------------------------------------------------------------

fn release_func(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i64(ctx, 0);
    Ok(())
}

fn paint_init(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = lock_paints()?.insert(PaintRec::default()).to_jlong();
    vm.ret_i64(ctx, h);
    Ok(())
}

fn paint_init_with(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let rec = paint_rec(vm.arg_i64(ctx, 0))?;
    let h = lock_paints()?.insert(rec).to_jlong();
    vm.ret_i64(ctx, h);
    Ok(())
}

fn paint_set(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let rec = paint_rec(vm.arg_i64(ctx, 1))?;
    with_paint(vm.arg_i64(ctx, 0), |r| *r = rec)?;
    Ok(())
}

fn paint_reset(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    with_paint(vm.arg_i64(ctx, 0), |r| *r = PaintRec::default())?;
    Ok(())
}

fn paint_get_flags(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i32(ctx, with_paint(vm.arg_i64(ctx, 0), |r| r.flags)?);
    Ok(())
}

fn paint_set_flags(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = vm.arg_i32(ctx, 1);
    with_paint(vm.arg_i64(ctx, 0), |r| r.flags = v)?;
    Ok(())
}

fn paint_set_color(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let c = vm.arg_i32(ctx, 1) as u32;
    with_paint(vm.arg_i64(ctx, 0), |r| {
        r.color = c;
        r.alpha = (c >> 24) as i32;
    })?;
    Ok(())
}

fn paint_set_color_long(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let c = vm.arg_i64(ctx, 1) as u32;
    with_paint(vm.arg_i64(ctx, 0), |r| {
        r.color = c;
        r.alpha = (c >> 24) as i32;
    })?;
    Ok(())
}

fn paint_set_alpha(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let a = vm.arg_i32(ctx, 1).clamp(0, 255);
    with_paint(vm.arg_i64(ctx, 0), |r| {
        r.alpha = a;
        r.color = (r.color & 0x00ff_ffff) | ((a as u32) << 24);
    })?;
    Ok(())
}

fn paint_set_typeface(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let tf = vm.arg_i64(ctx, 1);
    if tf != 0 {
        gfx_text::typeface_rec(tf)?;
    }
    with_paint(vm.arg_i64(ctx, 0), |r| r.typeface = tf)?;
    Ok(())
}

fn take_handle(vm: &mut Vm, ctx: NativeCtx, field: fn(&mut PaintRec) -> &mut i64) -> VmResult<()> {
    let new = vm.arg_i64(ctx, 1);
    let old = with_paint(vm.arg_i64(ctx, 0), |r| {
        let slot = field(r);
        let old = *slot;
        *slot = new;
        old
    })?;
    vm.ret_i64(ctx, old);
    Ok(())
}

fn paint_set_shader(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    take_handle(vm, ctx, |r| &mut r.shader)
}

// ---------------------------------------------------------------------------
// Shader natives (W75-A)
// ---------------------------------------------------------------------------

fn lg_native_create(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let local = local_matrix(vm.arg_i64(ctx, 0))?;
    let x0 = vm.arg_f32(ctx, 1);
    let y0 = vm.arg_f32(ctx, 2);
    let x1 = vm.arg_f32(ctx, 3);
    let y1 = vm.arg_f32(ctx, 4);
    let colors = color_array(vm, vm.arg_ref(ctx, 5))?;
    let positions = optional_float_array(vm, vm.arg_ref(ctx, 6))?;
    let tile = tile_mode(vm.arg_i32(ctx, 7));
    let spec = ShaderSpec::Linear {
        start: (x0, y0),
        end: (x1, y1),
        colors,
        positions,
        tile,
        local,
    };
    vm.ret_i64(ctx, store_shader(spec)?);
    Ok(())
}

fn rg_native_create(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let local = local_matrix(vm.arg_i64(ctx, 0))?;
    let focal = (vm.arg_f32(ctx, 1), vm.arg_f32(ctx, 2));
    let _focal_radius = vm.arg_f32(ctx, 3);
    let center = (vm.arg_f32(ctx, 4), vm.arg_f32(ctx, 5));
    let radius = vm.arg_f32(ctx, 6);
    let colors = color_array(vm, vm.arg_ref(ctx, 7))?;
    let positions = optional_float_array(vm, vm.arg_ref(ctx, 8))?;
    let tile = tile_mode(vm.arg_i32(ctx, 9));
    let spec = ShaderSpec::Radial {
        center,
        radius,
        colors,
        positions,
        tile,
        local,
        focal,
    };
    vm.ret_i64(ctx, store_shader(spec)?);
    Ok(())
}

fn sg_native_create(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let local = local_matrix(vm.arg_i64(ctx, 0))?;
    let cx = vm.arg_f32(ctx, 1);
    let cy = vm.arg_f32(ctx, 2);
    let colors = color_array(vm, vm.arg_ref(ctx, 3))?;
    let positions = optional_float_array(vm, vm.arg_ref(ctx, 4))?;
    let spec = ShaderSpec::Sweep {
        center: (cx, cy),
        colors,
        positions,
        local,
    };
    vm.ret_i64(ctx, store_shader(spec)?);
    Ok(())
}

fn bs_native_create(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let local = local_matrix(vm.arg_i64(ctx, 0))?;
    let bitmap = vm.arg_i64(ctx, 1);
    if !super::gfx_state::with_ctx(|c| c.bitmaps.contains(super::gfx_state::rh(bitmap))) {
        return Err(super::gfx_state::stale("BitmapShader bitmap", bitmap));
    }
    let tile_x = tile_mode(vm.arg_i32(ctx, 2));
    let tile_y = tile_mode(vm.arg_i32(ctx, 3));
    let spec = ShaderSpec::Bitmap {
        bitmap: super::gfx_state::rh(bitmap),
        tile_x,
        tile_y,
        local,
    };
    vm.ret_i64(ctx, store_shader(spec)?);
    Ok(())
}

fn bs_native_create_aniso(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    // `(local, bitmap, tileX, tileY, maxAniso, filtered)` — aniso is advisory.
    let local = local_matrix(vm.arg_i64(ctx, 0))?;
    let bitmap = vm.arg_i64(ctx, 1);
    if !super::gfx_state::with_ctx(|c| c.bitmaps.contains(super::gfx_state::rh(bitmap))) {
        return Err(super::gfx_state::stale("BitmapShader bitmap", bitmap));
    }
    let tile_x = tile_mode(vm.arg_i32(ctx, 2));
    let tile_y = tile_mode(vm.arg_i32(ctx, 3));
    let spec = ShaderSpec::Bitmap {
        bitmap: super::gfx_state::rh(bitmap),
        tile_x,
        tile_y,
        local,
    };
    vm.ret_i64(ctx, store_shader(spec)?);
    Ok(())
}

fn cs_native_create(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let _local = local_matrix(vm.arg_i64(ctx, 0))?;
    let a = vm.arg_i64(ctx, 1);
    let b = vm.arg_i64(ctx, 2);
    let mode = PorterDuffMode::from_ordinal(vm.arg_i32(ctx, 3))
        .ok_or_else(|| VmError::unsupported_api("ComposeShader mode", format!("ordinal {}", vm.arg_i32(ctx, 3))))?;
    let sa = shader_spec(a)
        .ok_or_else(|| VmError::unsupported_api("ComposeShader shaderA", "unresolved shader handle"))?;
    let sb = shader_spec(b)
        .ok_or_else(|| VmError::unsupported_api("ComposeShader shaderB", "unresolved shader handle"))?;
    let spec = ShaderSpec::Compose {
        a: Box::new(sa),
        b: Box::new(sb),
        mode,
    };
    vm.ret_i64(ctx, store_shader(spec)?);
    Ok(())
}

fn paint_set_mask_filter(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    take_handle(vm, ctx, |r| &mut r.mask_filter)
}

fn paint_set_path_effect(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    take_handle(vm, ctx, |r| &mut r.path_effect)
}

fn paint_set_color_filter(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let _ = filters();
    take_handle(vm, ctx, |r| &mut r.color_filter)
}

fn paint_set_shadow(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let radius = vm.arg_f32(ctx, 1);
    let dx = vm.arg_f32(ctx, 2);
    let dy = vm.arg_f32(ctx, 3);
    let color = vm.arg_i64(ctx, 4) as u32;
    with_paint(vm.arg_i64(ctx, 0), |r| r.shadow = Some((radius, dx, dy, color)))?;
    Ok(())
}

fn paint_has_shadow(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_bool(ctx, with_paint(vm.arg_i64(ctx, 0), |r| r.shadow.is_some())?);
    Ok(())
}

fn paint_get_underline_position(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    // Validate the handle, then report the renderer's single underline rule.
    let _ = paint_rec(vm.arg_i64(ctx, 0))?;
    vm.ret_f32(ctx, -2.0);
    Ok(())
}

fn paint_get_underline_thickness(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let _ = paint_rec(vm.arg_i64(ctx, 0))?;
    vm.ret_f32(ctx, 1.0);
    Ok(())
}

fn paint_get_strike_position(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let _ = paint_rec(vm.arg_i64(ctx, 0))?;
    vm.ret_f32(ctx, -3.0);
    Ok(())
}

fn paint_get_strike_thickness(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let _ = paint_rec(vm.arg_i64(ctx, 0))?;
    vm.ret_f32(ctx, 1.0);
    Ok(())
}

fn paint_set_text_locales(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let _ = vm.string_to_rust(vm.arg_ref(ctx, 1));
    with_paint(vm.arg_i64(ctx, 0), |r| r.text_locales = 0)?;
    vm.ret_i32(ctx, 0);
    Ok(())
}

fn paint_set_font_features(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let s = vm.string_to_rust(vm.arg_ref(ctx, 1));
    with_paint(vm.arg_i64(ctx, 0), |r| r.font_features = s)?;
    Ok(())
}

fn paint_set_font_variations(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let s = vm.string_to_rust(vm.arg_ref(ctx, 1));
    with_paint(vm.arg_i64(ctx, 0), |r| r.font_variations = s)?;
    Ok(())
}

static COLORSPACES: OnceLock<Mutex<HandleTable<()>>> = OnceLock::new();

fn colorspace_rgb_create(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let table = COLORSPACES.get_or_init(|| Mutex::new(HandleTable::new()));
    let h = table
        .lock()
        .map_err(|_| VmError::Internal("ColorSpace table poisoned".into()))?
        .insert(())
        .to_jlong();
    vm.ret_i64(ctx, h);
    Ok(())
}

fn filter_create(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let color = vm.arg_i32(ctx, 0) as u32;
    let mode = vm.arg_i32(ctx, 1);
    let spec = ColorFilterSpec::BlendMode {
        color,
        mode: acos_render::color::AndroidBlendMode::from_ordinal(mode).ok_or_else(|| {
            VmError::unsupported_api(
                format!("BlendMode ordinal {mode}"),
                "android.graphics.BlendModeColorFilter",
            )
        })?,
    };
    let h = filters()
        .lock()
        .map_err(|_| VmError::Internal("ColorFilter table poisoned".into()))?
        .insert(spec)
        .to_jlong();
    vm.ret_i64(ctx, h);
    Ok(())
}

// ---------------------------------------------------------------------------
// Text-measurement natives
// ---------------------------------------------------------------------------

fn attrs_of(vm: &Vm, ctx: NativeCtx) -> VmResult<TextAttrs> {
    Ok(paint_rec(vm.arg_i64(ctx, 0))?.text_attrs())
}

fn write_metrics_int(vm: &mut Vm, out: u32, m: acos_render::text::FontMetricsInt) {
    vm.set_instance_field_by_name(out, "top", m.top as u32);
    vm.set_instance_field_by_name(out, "ascent", m.ascent as u32);
    vm.set_instance_field_by_name(out, "descent", m.descent as u32);
    vm.set_instance_field_by_name(out, "bottom", m.bottom as u32);
    vm.set_instance_field_by_name(out, "leading", m.leading as u32);
}

fn paint_font_metrics_int(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let attrs = attrs_of(vm, ctx)?;
    let out = vm.arg_ref(ctx, 1);
    let m = gfx_text::metrics_int(&attrs)?;
    write_metrics_int(vm, out, m);
    vm.ret_i32(ctx, 5);
    Ok(())
}

fn paint_font_metrics(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let attrs = attrs_of(vm, ctx)?;
    let out = vm.arg_ref(ctx, 1);
    let m = gfx_text::metrics_int(&attrs)?;
    let (descent, ascent, leading) = (m.descent as f32, m.ascent as f32, m.leading as f32);
    for (name, v) in [
        ("top", m.top as f32),
        ("ascent", ascent),
        ("descent", descent),
        ("bottom", m.bottom as f32),
        ("leading", leading),
    ] {
        vm.set_instance_field_by_name(out, name, v.to_bits());
    }
    vm.ret_f32(ctx, descent - ascent + leading);
    Ok(())
}

fn paint_font_metrics_text_chars(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let attrs = attrs_of(vm, ctx)?;
    let out = vm.arg_ref(ctx, 7);
    write_metrics_int(vm, out, gfx_text::metrics_int(&attrs)?);
    Ok(())
}

fn paint_font_metrics_text_string(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let attrs = attrs_of(vm, ctx)?;
    let out = vm.arg_ref(ctx, 7);
    write_metrics_int(vm, out, gfx_text::metrics_int(&attrs)?);
    Ok(())
}

fn paint_ascent(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let attrs = attrs_of(vm, ctx)?;
    vm.ret_f32(ctx, gfx_text::metrics_int(&attrs)?.ascent as f32);
    Ok(())
}

fn paint_descent(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let attrs = attrs_of(vm, ctx)?;
    vm.ret_f32(ctx, gfx_text::metrics_int(&attrs)?.descent as f32);
    Ok(())
}

fn paint_text_advances_chars(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let attrs = attrs_of(vm, ctx)?;
    let text_h = vm.arg_ref(ctx, 1);
    let start = vm.arg_i32(ctx, 2).max(0) as usize;
    let end = vm.arg_i32(ctx, 3).max(0) as usize;
    let arr = vm.arg_ref(ctx, 7);
    let index = vm.arg_i32(ctx, 8).max(0) as usize;
    let all = gfx_text::char_array(vm, text_h)?;
    let end = end.min(all.len());
    let slice = if start <= end { &all[start..end] } else { &[][..] };
    let mut out = vec![0.0f32; slice.len()];
    let total = gfx_text::advances(slice, &attrs, &mut out)?;
    write_float_array(vm, arr, index, out.len(), &out)?;
    vm.ret_f32(ctx, total);
    Ok(())
}

fn paint_text_advances_string(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let attrs = attrs_of(vm, ctx)?;
    let s = vm.string_to_rust(vm.arg_ref(ctx, 1)).unwrap_or_default();
    let start = vm.arg_i32(ctx, 2).max(0) as usize;
    let end = vm.arg_i32(ctx, 3).max(0) as usize;
    let arr = vm.arg_ref(ctx, 7);
    let index = vm.arg_i32(ctx, 8).max(0) as usize;
    let units: Vec<u16> = s.encode_utf16().collect();
    let end = end.min(units.len());
    let slice = if start <= end { &units[start..end] } else { &[][..] };
    let mut out = vec![0.0f32; slice.len()];
    let total = gfx_text::advances(slice, &attrs, &mut out)?;
    write_float_array(vm, arr, index, out.len(), &out)?;
    vm.ret_f32(ctx, total);
    Ok(())
}

fn paint_run_char_advance(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let attrs = attrs_of(vm, ctx)?;
    let text_h = vm.arg_ref(ctx, 1);
    let start = vm.arg_i32(ctx, 2).max(0) as usize;
    let end = vm.arg_i32(ctx, 3).max(0) as usize;
    // `nGetRunCharacterAdvance(J [C I I I I Z I [F I)F`: the advances array and
    // its write index are arguments 8 and 9 (7 is the run offset).
    let arr = vm.arg_ref(ctx, 8);
    let index = vm.arg_i32(ctx, 9).max(0) as usize;
    let all = gfx_text::char_array(vm, text_h)?;
    let end = end.min(all.len());
    let slice = if start <= end { &all[start..end] } else { &[][..] };
    let mut out = vec![0.0f32; slice.len()];
    let total = gfx_text::advances(slice, &attrs, &mut out)?;
    write_float_array(vm, arr, index, slice.len(), &out)?;
    vm.ret_f32(ctx, total);
    Ok(())
}

fn paint_run_advance(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let attrs = attrs_of(vm, ctx)?;
    let all = gfx_text::char_array(vm, vm.arg_ref(ctx, 1))?;
    let index = vm.arg_i32(ctx, 7).max(0) as usize;
    vm.ret_f32(ctx, gfx_text::run_advance(&all, &attrs, index)?);
    Ok(())
}

fn paint_offset_for_advance(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let attrs = attrs_of(vm, ctx)?;
    let all = gfx_text::char_array(vm, vm.arg_ref(ctx, 1))?;
    let advance = vm.arg_f32(ctx, 7);
    let mut out = vec![0.0f32; all.len()];
    gfx_text::advances(&all, &attrs, &mut out)?;
    let mut acc = 0.0f32;
    let mut idx = out.len();
    for (i, a) in out.iter().enumerate() {
        if acc + a * 0.5 > advance {
            idx = i;
            break;
        }
        acc += a;
    }
    vm.ret_i32(ctx, idx as i32);
    Ok(())
}

fn paint_string_bounds(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let attrs = attrs_of(vm, ctx)?;
    let s = vm.string_to_rust(vm.arg_ref(ctx, 1)).unwrap_or_default();
    let out = vm.arg_ref(ctx, 5);
    let units: Vec<u16> = s.encode_utf16().collect();
    if let Some(r) = gfx_text::bounds(&units, &attrs)? {
        gfx_text::write_rect_int(vm, out, r)?;
    }
    Ok(())
}

fn paint_char_array_bounds(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let attrs = attrs_of(vm, ctx)?;
    let out = vm.arg_ref(ctx, 5);
    let units = gfx_text::char_array(vm, vm.arg_ref(ctx, 1))?;
    if let Some(r) = gfx_text::bounds(&units, &attrs)? {
        gfx_text::write_rect_int(vm, out, r)?;
    }
    Ok(())
}

fn break_count(units: &[u16], attrs: &TextAttrs, max_width: f32) -> VmResult<i32> {
    let mut out = vec![0.0f32; units.len()];
    gfx_text::advances(units, attrs, &mut out)?;
    let mut acc = 0.0f32;
    let mut n = 0usize;
    for a in &out {
        if acc + a > max_width {
            break;
        }
        acc += a;
        n += 1;
    }
    Ok(n as i32)
}

fn paint_break_text_chars(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let attrs = attrs_of(vm, ctx)?;
    let all = gfx_text::char_array(vm, vm.arg_ref(ctx, 1))?;
    let start = vm.arg_i32(ctx, 2).max(0) as usize;
    let max_width = vm.arg_f32(ctx, 4);
    let slice = &all[start.min(all.len())..];
    vm.ret_i32(ctx, break_count(slice, &attrs, max_width)?);
    Ok(())
}

fn paint_break_text_string(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let attrs = attrs_of(vm, ctx)?;
    let s = vm.string_to_rust(vm.arg_ref(ctx, 1)).unwrap_or_default();
    let max_width = vm.arg_f32(ctx, 3);
    let units: Vec<u16> = s.encode_utf16().collect();
    vm.ret_i32(ctx, break_count(&units, &attrs, max_width)?);
    Ok(())
}

fn paint_has_glyph(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let attrs = attrs_of(vm, ctx)?;
    let cp = vm.arg_i32(ctx, 1);
    let ch = char::from_u32(cp as u32).unwrap_or('\u{FFFD}');
    vm.ret_bool(ctx, gfx_text::has_glyph(ch, &attrs)?);
    Ok(())
}

fn paint_equals_text(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let a = paint_rec(vm.arg_i64(ctx, 0))?;
    let b = paint_rec(vm.arg_i64(ctx, 1))?;
    let same = a.text_attrs().typeface == b.text_attrs().typeface
        && a.text_size == b.text_size
        && a.text_scale_x == b.text_scale_x
        && a.font_features == b.font_features
        && a.font_variations == b.font_variations;
    vm.ret_bool(ctx, same);
    Ok(())
}

fn paint_text_path_chars(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let attrs = attrs_of(vm, ctx)?;
    let units = gfx_text::char_array(vm, vm.arg_ref(ctx, 2))?;
    let x = vm.arg_f32(ctx, 5);
    let y = vm.arg_f32(ctx, 6);
    let path_h = vm.arg_i64(ctx, 7);
    let path = gfx_text::text_path(&units, x, y, &attrs)?;
    crate::natives::gfx_path::replace_path(path_h, path)?;
    Ok(())
}

fn paint_text_path_string(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let attrs = attrs_of(vm, ctx)?;
    let s = vm.string_to_rust(vm.arg_ref(ctx, 2)).unwrap_or_default();
    let x = vm.arg_f32(ctx, 5);
    let y = vm.arg_f32(ctx, 6);
    let path_h = vm.arg_i64(ctx, 7);
    let units: Vec<u16> = s.encode_utf16().collect();
    let path = gfx_text::text_path(&units, x, y, &attrs)?;
    crate::natives::gfx_path::replace_path(path_h, path)?;
    Ok(())
}

fn unsupported_text_cursor(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Err(VmError::unsupported_api(
        "Paint.nGetTextRunCursor (bidi cursor movement)",
        "android.graphics.Paint",
    ))
}

/// `Paint.nGetFillPath(src, dst)`: apply stroking to `src` and write the
/// resulting fill outline to `dst`. Mirrors Skia's `SkPaint::getFillPath`:
/// `FILL` copies the source, `STROKE`/`FILL_AND_STROKE` converts the stroke to
/// its fill outline. Returns true when the resulting path is non-empty.
fn paint_get_fill_path(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let rec = paint_rec(vm.arg_i64(ctx, 0))?;
    let src = crate::natives::gfx_path::path_clone(vm.arg_i64(ctx, 1))?;
    let dst = vm.arg_i64(ctx, 2);
    let stroked = matches!(
        PaintStyle::from_ordinal(rec.style),
        Some(PaintStyle::Stroke) | Some(PaintStyle::FillAndStroke)
    );
    let result = if stroked {
        let cap = StrokeCap::from_ordinal(rec.stroke_cap).unwrap_or(StrokeCap::Butt);
        let join = StrokeJoin::from_ordinal(rec.stroke_join).unwrap_or(StrokeJoin::Miter);
        // A hairline (width 0) still outlines at 1px in Skia.
        let width = if rec.stroke_width > 0.0 {
            rec.stroke_width
        } else {
            1.0
        };
        src.stroke_to_fill(width, cap, join, rec.stroke_miter)
    } else {
        Some(src.clone())
    };
    match result {
        Some(p) => {
            let non_empty = !p.is_empty();
            crate::natives::gfx_path::replace_path(dst, p)?;
            vm.ret_bool(ctx, non_empty);
        }
        None => {
            crate::natives::gfx_path::replace_path(dst, Path::new())?;
            vm.ret_bool(ctx, false);
        }
    }
    Ok(())
}

fn write_float_array(
    vm: &mut Vm,
    arr: u32,
    index: usize,
    count: usize,
    src: &[f32],
) -> VmResult<()> {
    use crate::heap::{ArrayData, Object};
    if arr == 0 {
        return Ok(());
    }
    let Some(Object::Array(a)) = vm.heap.get_mut(arr) else {
        return Err(VmError::Internal("expected float[]".to_string()));
    };
    if let ArrayData::Float(v) = &mut a.data {
        for (i, value) in src.iter().enumerate().take(count) {
            if let Some(slot) = v.get_mut(index + i) {
                *slot = *value;
            }
        }
    }
    Ok(())
}
