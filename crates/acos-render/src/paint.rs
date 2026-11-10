//! `android.graphics.Paint` and its sub-objects (shaders, colour filters,
//! mask filters, path effects, text attributes).
//!
//! Paint is a *description*; it deliberately stores no borrowed rasteriser
//! objects, so it can live in the [`crate::RenderContext`] handle table. The
//! conversion to a `tiny_skia::Paint` happens at draw time (see
//! `context.rs`/`canvas.rs`).

use crate::color::{AndroidBlendMode, PorterDuffMode};
use crate::handle::Handle;
use crate::matrix::Matrix3x3;

/// Android `Paint.Style`.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum PaintStyle {
    Fill = 0,
    Stroke = 1,
    FillAndStroke = 2,
}

impl PaintStyle {
    pub fn from_ordinal(o: i32) -> Option<Self> {
        Some(match o {
            0 => PaintStyle::Fill,
            1 => PaintStyle::Stroke,
            2 => PaintStyle::FillAndStroke,
            _ => return None,
        })
    }
}

/// Android `Paint.Cap`.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum StrokeCap {
    Butt = 0,
    Round = 1,
    Square = 2,
}

impl StrokeCap {
    pub fn from_ordinal(o: i32) -> Option<Self> {
        Some(match o {
            0 => StrokeCap::Butt,
            1 => StrokeCap::Round,
            2 => StrokeCap::Square,
            _ => return None,
        })
    }

    pub fn to_skia(self) -> tiny_skia::LineCap {
        match self {
            StrokeCap::Butt => tiny_skia::LineCap::Butt,
            StrokeCap::Round => tiny_skia::LineCap::Round,
            StrokeCap::Square => tiny_skia::LineCap::Square,
        }
    }
}

/// Android `Paint.Join`.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum StrokeJoin {
    Miter = 0,
    Round = 1,
    Bevel = 2,
}

impl StrokeJoin {
    pub fn from_ordinal(o: i32) -> Option<Self> {
        Some(match o {
            0 => StrokeJoin::Miter,
            1 => StrokeJoin::Round,
            2 => StrokeJoin::Bevel,
            _ => return None,
        })
    }

    pub fn to_skia(self) -> tiny_skia::LineJoin {
        match self {
            StrokeJoin::Miter => tiny_skia::LineJoin::Miter,
            StrokeJoin::Round => tiny_skia::LineJoin::Round,
            StrokeJoin::Bevel => tiny_skia::LineJoin::Bevel,
        }
    }
}

/// Android `Paint.Align`.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum TextAlign {
    Left = 0,
    Center = 1,
    Right = 2,
}

impl TextAlign {
    pub fn from_ordinal(o: i32) -> Option<Self> {
        Some(match o {
            0 => TextAlign::Left,
            1 => TextAlign::Center,
            2 => TextAlign::Right,
            _ => return None,
        })
    }
}

/// Android `Shader.TileMode`.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum TileMode {
    Clamp = 0,
    Repeat = 1,
    Mirror = 2,
}

impl TileMode {
    pub fn from_ordinal(o: i32) -> Option<Self> {
        Some(match o {
            0 => TileMode::Clamp,
            1 => TileMode::Repeat,
            2 => TileMode::Mirror,
            _ => return None,
        })
    }

    pub fn to_skia(self) -> tiny_skia::SpreadMode {
        match self {
            TileMode::Clamp => tiny_skia::SpreadMode::Pad,
            TileMode::Repeat => tiny_skia::SpreadMode::Repeat,
            TileMode::Mirror => tiny_skia::SpreadMode::Reflect,
        }
    }
}

/// A shader description (no borrowed pixmaps).
#[derive(Clone, PartialEq, Debug)]
pub enum ShaderSpec {
    Linear {
        start: (f32, f32),
        end: (f32, f32),
        colors: Vec<u32>,
        positions: Option<Vec<f32>>,
        tile: TileMode,
        local: Matrix3x3,
    },
    Radial {
        center: (f32, f32),
        radius: f32,
        colors: Vec<u32>,
        positions: Option<Vec<f32>>,
        tile: TileMode,
        local: Matrix3x3,
    },
    Sweep {
        center: (f32, f32),
        colors: Vec<u32>,
        positions: Option<Vec<f32>>,
        local: Matrix3x3,
    },
    Bitmap {
        bitmap: Handle,
        tile_x: TileMode,
        tile_y: TileMode,
        local: Matrix3x3,
    },
}

/// Skia's `SkBlurMask::ConvertRadiusToSigma` / `BlurMaskFilter` semantics:
/// the Java `blurRadius` is *not* the Gaussian standard deviation.
///
/// `sigma = radius * 0.57735f + 0.5f`. ACOS applies this in
/// [`crate::canvas`] before approximating the Gaussian with box passes, so a
/// `BlurMaskFilter(radius, ...)` or a `RenderEffect` blur has the same spread
/// as Android's Skia rasteriser.
pub fn radius_to_sigma(radius: f32) -> f32 {
    radius * 0.57735 + 0.5
}

/// Android `MaskFilter.Blur` style.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum BlurStyle {
    Normal = 0,
    Solid = 1,
    Outer = 2,
    Inner = 3,
}

impl BlurStyle {
    pub fn from_ordinal(o: i32) -> Option<Self> {
        Some(match o {
            0 => BlurStyle::Normal,
            1 => BlurStyle::Solid,
            2 => BlurStyle::Outer,
            3 => BlurStyle::Inner,
            _ => return None,
        })
    }
}

/// A mask filter description.
#[derive(Clone, PartialEq, Debug)]
pub enum MaskFilterSpec {
    Blur { radius: f32, style: BlurStyle },
}

/// A path effect description.
#[derive(Clone, PartialEq, Debug)]
pub enum PathEffectSpec {
    Dash { intervals: Vec<f32>, phase: f32 },
    Corner { radius: f32 },
}

/// A colour filter description.
#[derive(Clone, PartialEq, Debug)]
pub enum ColorFilterSpec {
    PorterDuff { color: u32, mode: PorterDuffMode },
    BlendMode { color: u32, mode: AndroidBlendMode },
    /// 4x5 row-major colour matrix (RGBA + translation), like
    /// `android.graphics.ColorMatrix`.
    ColorMatrix { matrix: [f32; 20] },
    /// Identity colour filter — used by `PorterDuffColorFilter` with SRC_OVER
    /// as a fast path? Kept explicit so callers can see the intent.
    None,
}

/// Android `Paint` (a description).
#[derive(Clone, Debug)]
pub struct Paint {
    pub color: u32,
    pub alpha: u32,
    pub style: PaintStyle,
    pub stroke_width: f32,
    pub stroke_miter: f32,
    pub stroke_cap: StrokeCap,
    pub stroke_join: StrokeJoin,
    pub anti_alias: bool,
    /// `dither` is accepted and ignored: ACOS always rasterises in RGBA8888,
    /// so there is no banding to dither away. Documented, not silently wrong.
    pub dither: bool,
    pub filter_bitmap: bool,
    pub blend_mode: tiny_skia::BlendMode,
    /// The original Android blend mode, for fidelity reporting.
    pub blend_mode_android: Option<AndroidBlendMode>,
    pub shader: Option<ShaderSpec>,
    pub color_filter: Option<ColorFilterSpec>,
    pub mask_filter: Option<MaskFilterSpec>,
    pub path_effect: Option<PathEffectSpec>,

    // Text attributes.
    pub typeface: Option<Handle>,
    pub text_size: f32,
    pub text_scale_x: f32,
    pub text_skew_x: f32,
    pub letter_spacing: f32,
    pub word_spacing: f32,
    pub fake_bold: bool,
    pub underline: bool,
    pub strike_thru: bool,
    pub text_align: TextAlign,
    pub subpixel_text: bool,
    pub linear_text: bool,
    pub hinting: bool,
    pub font_feature_settings: Option<String>,
    pub font_variation_settings: Option<String>,
    pub elegant_text_height: bool,
    pub shadow_layer: Option<ShadowLayer>,
}

impl Default for Paint {
    fn default() -> Self {
        Paint {
            color: 0xff00_0000,
            alpha: 255,
            style: PaintStyle::Fill,
            stroke_width: 0.0,
            stroke_miter: 4.0,
            stroke_cap: StrokeCap::Butt,
            stroke_join: StrokeJoin::Miter,
            // `android.graphics.Paint()` calls `this(ANTI_ALIAS_FLAG)`, so
            // anti-aliasing is ON by default (frameworks/base Paint.java @
            // android-14.0.0_r1). The previous `false` rendered fixture text
            // with only two grey levels (ADR-0019 / REPORT.md §3.4).
            anti_alias: true,
            dither: true,
            filter_bitmap: true,
            blend_mode: tiny_skia::BlendMode::SourceOver,
            blend_mode_android: Some(AndroidBlendMode::SrcOver),
            shader: None,
            color_filter: None,
            mask_filter: None,
            path_effect: None,
            typeface: None,
            text_size: 12.0,
            text_scale_x: 1.0,
            text_skew_x: 0.0,
            letter_spacing: 0.0,
            word_spacing: 0.0,
            fake_bold: false,
            underline: false,
            strike_thru: false,
            text_align: TextAlign::Left,
            subpixel_text: false,
            linear_text: false,
            hinting: true,
            font_feature_settings: None,
            font_variation_settings: None,
            elegant_text_height: false,
            shadow_layer: None,
        }
    }
}

/// Android `Paint.setShadowLayer`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ShadowLayer {
    pub radius: f32,
    pub dx: f32,
    pub dy: f32,
    pub color: u32,
}

impl Paint {
    /// Effective ARGB colour after applying the paint alpha.
    pub fn effective_argb(&self) -> u32 {
        let a = ((self.color >> 24) & 0xff) * self.alpha / 255;
        (self.color & 0x00ff_ffff) | (a << 24)
    }

    /// Android `Paint.getStrokeWidth`.
    pub fn stroke_width(&self) -> f32 {
        self.stroke_width
    }

    /// Build a tiny-skia stroke description from this paint.
    pub fn skia_stroke(&self) -> tiny_skia::Stroke {
        let mut stroke = tiny_skia::Stroke {
            width: self.stroke_width,
            miter_limit: self.stroke_miter.max(0.0),
            line_cap: self.stroke_cap.to_skia(),
            line_join: self.stroke_join.to_skia(),
            ..Default::default()
        };
        if let Some(PathEffectSpec::Dash { intervals, phase }) = &self.path_effect {
            stroke.dash = tiny_skia::StrokeDash::new(intervals.clone(), *phase);
        }
        stroke
    }

    /// Skia fill rule for text/path fills (the path carries the authoritative
    /// fill type; this is only for shader-only draws).
    pub fn alpha_is_opaque(&self) -> bool {
        self.alpha >= 255
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_paint_is_opaque_black_fill() {
        let p = Paint::default();
        assert_eq!(p.effective_argb(), 0xff00_0000);
        assert_eq!(p.style, PaintStyle::Fill);
    }

    #[test]
    fn default_paint_antialiases_like_android() {
        // `new Paint()` == `new Paint(ANTI_ALIAS_FLAG)` on Android S+.
        let p = Paint::default();
        assert!(p.anti_alias, "Paint() must default anti_alias to true");
        // The other Paint() defaults that the rasteriser consumes.
        assert_eq!(p.color, 0xff00_0000);
        assert_eq!(p.alpha, 255);
        assert_eq!(p.stroke_width, 0.0);
        assert_eq!(p.stroke_miter, 4.0);
        assert_eq!(p.stroke_cap, StrokeCap::Butt);
        assert_eq!(p.stroke_join, StrokeJoin::Miter);
        assert_eq!(p.text_align, TextAlign::Left);
        assert!(p.filter_bitmap);
        assert!(!p.subpixel_text);
    }

    #[test]
    fn alpha_modulates_colour() {
        let p = Paint {
            color: 0xff112233,
            alpha: 128,
            ..Default::default()
        };
        let a = (p.effective_argb() >> 24) & 0xff;
        assert_eq!(a, 128);
        assert_eq!(p.effective_argb() & 0x00ff_ffff, 0x00112233);
    }

    #[test]
    fn blur_radius_converts_to_skia_sigma() {
        // Skia's ConvertRadiusToSigma: radius * 0.57735 + 0.5.
        assert!((radius_to_sigma(0.0) - 0.5).abs() < 1e-6);
        assert!((radius_to_sigma(1.0) - 1.07735).abs() < 1e-5);
        assert!((radius_to_sigma(8.0) - 5.1188).abs() < 1e-4);
        // Monotonic, so a larger radius always blurs more.
        assert!(radius_to_sigma(2.0) > radius_to_sigma(1.0));
    }

    #[test]
    fn ordinals_round_trip() {
        assert_eq!(PaintStyle::from_ordinal(2), Some(PaintStyle::FillAndStroke));
        assert_eq!(StrokeCap::from_ordinal(1), Some(StrokeCap::Round));
        assert_eq!(StrokeJoin::from_ordinal(2), Some(StrokeJoin::Bevel));
        assert_eq!(TextAlign::from_ordinal(1), Some(TextAlign::Center));
        assert_eq!(TileMode::from_ordinal(2), Some(TileMode::Mirror));
        assert!(PaintStyle::from_ordinal(9).is_none());
    }
}
