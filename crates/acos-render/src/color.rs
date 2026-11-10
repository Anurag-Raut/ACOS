//! Colours and PorterDuff/BlendMode tables.
//!
//! Android's `android.graphics.Color` packs ARGB into a signed `int`
//! (`0xAARRGGBB`). ACOS stores *bitmap* pixels as premultiplied RGBA8 (what
//! Skia/tiny-skia rasterise with) and converts at the API edge, exactly like
//! Android's `Bitmap.getPixel`/`setPixel` do against a premultiplied store.

use tiny_skia::BlendMode;

/// Android `Color` int (`0xAARRGGBB`) → un-premultiplied `(r, g, b, a)`.
#[inline]
pub const fn argb_to_rgba(argb: u32) -> (u8, u8, u8, u8) {
    (
        ((argb >> 16) & 0xff) as u8,
        ((argb >> 8) & 0xff) as u8,
        (argb & 0xff) as u8,
        ((argb >> 24) & 0xff) as u8,
    )
}

/// Un-premultiplied `(r, g, b, a)` → Android `Color` int.
#[inline]
pub const fn rgba_to_argb(r: u8, g: u8, b: u8, a: u8) -> u32 {
    ((a as u32) << 24) | ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
}

/// Premultiply a single 8-bit channel by an 8-bit alpha (round-to-nearest,
/// matching Skia's `SkAlphaMul`/`premultiply_u8`).
#[inline]
pub fn premultiply_channel(c: u8, a: u8) -> u8 {
    ((c as u16 * a as u16 + 127) / 255) as u8
}

/// Un-premultiply a channel by alpha.
#[inline]
pub fn unpremultiply_channel(c: u8, a: u8) -> u8 {
    if a == 0 {
        0
    } else {
        (((c as u32 * 255 + (a as u32 / 2)) / a as u32).min(255)) as u8
    }
}

/// Pack un-premultiplied RGBA into a premultiplied RGBA word whose in-memory
/// byte order is `[r, g, b, a]` (tiny-skia's layout).
#[inline]
pub fn pack_premul(r: u8, g: u8, b: u8, a: u8) -> u32 {
    let (r, g, b) = (
        premultiply_channel(r, a),
        premultiply_channel(g, a),
        premultiply_channel(b, a),
    );
    (r as u32) | ((g as u32) << 8) | ((b as u32) << 16) | ((a as u32) << 24)
}

/// Unpack a premultiplied RGBA word into un-premultiplied `(r, g, b, a)`.
#[inline]
pub fn unpack_unpremul(word: u32) -> (u8, u8, u8, u8) {
    let r = (word & 0xff) as u8;
    let g = ((word >> 8) & 0xff) as u8;
    let b = ((word >> 16) & 0xff) as u8;
    let a = ((word >> 24) & 0xff) as u8;
    (
        unpremultiply_channel(r, a),
        unpremultiply_channel(g, a),
        unpremultiply_channel(b, a),
        a,
    )
}

/// Android ARGB int → tiny-skia colour (premultiplied internally).
#[inline]
pub fn argb_to_sk(argb: u32) -> tiny_skia::Color {
    let (r, g, b, a) = argb_to_rgba(argb);
    tiny_skia::Color::from_rgba8(r, g, b, a)
}

/// Premultiplied RGBA8 word → tiny-skia colour.
#[inline]
pub fn premul_word_to_sk(word: u32) -> tiny_skia::Color {
    tiny_skia::Color::from_rgba8(
        (word & 0xff) as u8,
        ((word >> 8) & 0xff) as u8,
        ((word >> 16) & 0xff) as u8,
        ((word >> 24) & 0xff) as u8,
    )
}

/// Android `PorterDuff.Mode` ordinals (as passed to `nSetXfermode` /
/// `PorterDuffXfermode`).
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum PorterDuffMode {
    Clear,
    Src,
    Dst,
    #[default]
    SrcOver,
    DstOver,
    SrcIn,
    DstIn,
    SrcOut,
    DstOut,
    SrcAtop,
    DstAtop,
    Xor,
    Darken,
    Lighten,
    Multiply,
    Screen,
    Add,
    Overlay,
}

impl PorterDuffMode {
    /// Decode the `PorterDuff.Mode.ordinal()` used by `nSetXfermode`.
    pub fn from_ordinal(ordinal: i32) -> Option<Self> {
        Some(match ordinal {
            0 => PorterDuffMode::Clear,
            1 => PorterDuffMode::Src,
            2 => PorterDuffMode::Dst,
            3 => PorterDuffMode::SrcOver,
            4 => PorterDuffMode::DstOver,
            5 => PorterDuffMode::SrcIn,
            6 => PorterDuffMode::DstIn,
            7 => PorterDuffMode::SrcOut,
            8 => PorterDuffMode::DstOut,
            9 => PorterDuffMode::SrcAtop,
            10 => PorterDuffMode::DstAtop,
            11 => PorterDuffMode::Xor,
            12 => PorterDuffMode::Darken,
            13 => PorterDuffMode::Lighten,
            14 => PorterDuffMode::Multiply,
            15 => PorterDuffMode::Screen,
            16 => PorterDuffMode::Add,
            17 => PorterDuffMode::Overlay,
            _ => return None,
        })
    }

    pub fn to_blend(self) -> BlendMode {
        match self {
            PorterDuffMode::Clear => BlendMode::Clear,
            PorterDuffMode::Src => BlendMode::Source,
            PorterDuffMode::Dst => BlendMode::Destination,
            PorterDuffMode::SrcOver => BlendMode::SourceOver,
            PorterDuffMode::DstOver => BlendMode::DestinationOver,
            PorterDuffMode::SrcIn => BlendMode::SourceIn,
            PorterDuffMode::DstIn => BlendMode::DestinationIn,
            PorterDuffMode::SrcOut => BlendMode::SourceOut,
            PorterDuffMode::DstOut => BlendMode::DestinationOut,
            PorterDuffMode::SrcAtop => BlendMode::SourceAtop,
            PorterDuffMode::DstAtop => BlendMode::DestinationAtop,
            PorterDuffMode::Xor => BlendMode::Xor,
            PorterDuffMode::Darken => BlendMode::Darken,
            PorterDuffMode::Lighten => BlendMode::Lighten,
            PorterDuffMode::Multiply => BlendMode::Multiply,
            PorterDuffMode::Screen => BlendMode::Screen,
            PorterDuffMode::Add => BlendMode::Plus,
            PorterDuffMode::Overlay => BlendMode::Overlay,
        }
    }
}

/// Android `android.graphics.BlendMode` enum ordinals (API 29+), used by
/// `ColorFilter`/`Xfermode` replacements. Maps the full set onto tiny-skia's
/// Skia port blend modes.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum AndroidBlendMode {
    Clear = 0,
    Src = 1,
    Dst = 2,
    SrcOver = 3,
    DstOver = 4,
    SrcIn = 5,
    DstIn = 6,
    SrcOut = 7,
    DstOut = 8,
    SrcAtop = 9,
    DstAtop = 10,
    Xor = 11,
    Plus = 12,
    Modulate = 13,
    Screen = 14,
    Overlay = 15,
    Darken = 16,
    Lighten = 17,
    ColorDodge = 18,
    ColorBurn = 19,
    HardLight = 20,
    SoftLight = 21,
    Difference = 22,
    Exclusion = 23,
    Multiply = 24,
    Hue = 25,
    Saturation = 26,
    Color = 27,
    Luminosity = 28,
}

impl AndroidBlendMode {
    pub fn from_ordinal(ordinal: i32) -> Option<Self> {
        Some(match ordinal {
            0 => AndroidBlendMode::Clear,
            1 => AndroidBlendMode::Src,
            2 => AndroidBlendMode::Dst,
            3 => AndroidBlendMode::SrcOver,
            4 => AndroidBlendMode::DstOver,
            5 => AndroidBlendMode::SrcIn,
            6 => AndroidBlendMode::DstIn,
            7 => AndroidBlendMode::SrcOut,
            8 => AndroidBlendMode::DstOut,
            9 => AndroidBlendMode::SrcAtop,
            10 => AndroidBlendMode::DstAtop,
            11 => AndroidBlendMode::Xor,
            12 => AndroidBlendMode::Plus,
            13 => AndroidBlendMode::Modulate,
            14 => AndroidBlendMode::Screen,
            15 => AndroidBlendMode::Overlay,
            16 => AndroidBlendMode::Darken,
            17 => AndroidBlendMode::Lighten,
            18 => AndroidBlendMode::ColorDodge,
            19 => AndroidBlendMode::ColorBurn,
            20 => AndroidBlendMode::HardLight,
            21 => AndroidBlendMode::SoftLight,
            22 => AndroidBlendMode::Difference,
            23 => AndroidBlendMode::Exclusion,
            24 => AndroidBlendMode::Multiply,
            25 => AndroidBlendMode::Hue,
            26 => AndroidBlendMode::Saturation,
            27 => AndroidBlendMode::Color,
            28 => AndroidBlendMode::Luminosity,
            _ => return None,
        })
    }

    pub fn to_blend(self) -> BlendMode {
        match self {
            AndroidBlendMode::Clear => BlendMode::Clear,
            AndroidBlendMode::Src => BlendMode::Source,
            AndroidBlendMode::Dst => BlendMode::Destination,
            AndroidBlendMode::SrcOver => BlendMode::SourceOver,
            AndroidBlendMode::DstOver => BlendMode::DestinationOver,
            AndroidBlendMode::SrcIn => BlendMode::SourceIn,
            AndroidBlendMode::DstIn => BlendMode::DestinationIn,
            AndroidBlendMode::SrcOut => BlendMode::SourceOut,
            AndroidBlendMode::DstOut => BlendMode::DestinationOut,
            AndroidBlendMode::SrcAtop => BlendMode::SourceAtop,
            AndroidBlendMode::DstAtop => BlendMode::DestinationAtop,
            AndroidBlendMode::Xor => BlendMode::Xor,
            AndroidBlendMode::Plus => BlendMode::Plus,
            AndroidBlendMode::Modulate => BlendMode::Modulate,
            AndroidBlendMode::Screen => BlendMode::Screen,
            AndroidBlendMode::Overlay => BlendMode::Overlay,
            AndroidBlendMode::Darken => BlendMode::Darken,
            AndroidBlendMode::Lighten => BlendMode::Lighten,
            AndroidBlendMode::ColorDodge => BlendMode::ColorDodge,
            AndroidBlendMode::ColorBurn => BlendMode::ColorBurn,
            AndroidBlendMode::HardLight => BlendMode::HardLight,
            AndroidBlendMode::SoftLight => BlendMode::SoftLight,
            AndroidBlendMode::Difference => BlendMode::Difference,
            AndroidBlendMode::Exclusion => BlendMode::Exclusion,
            AndroidBlendMode::Multiply => BlendMode::Multiply,
            AndroidBlendMode::Hue => BlendMode::Hue,
            AndroidBlendMode::Saturation => BlendMode::Saturation,
            AndroidBlendMode::Color => BlendMode::Color,
            AndroidBlendMode::Luminosity => BlendMode::Luminosity,
        }
    }
}

/// Reference implementation of each PorterDuff mode on *un-premultiplied*
/// RGBA (used by tests and by the software `ColorFilter` path). `sa`/`da` are
/// alpha in `0..=1`.
pub fn porter_duff_reference(mode: PorterDuffMode, src: [f32; 4], dst: [f32; 4]) -> [f32; 4] {
    let (sr, sg, sb, sa) = (src[0], src[1], src[2], src[3]);
    let (dr, dg, db, da) = (dst[0], dst[1], dst[2], dst[3]);
    let (ra, rr, rg, rb);
    match mode {
        PorterDuffMode::Clear => return [0.0; 4],
        PorterDuffMode::Src => return src,
        PorterDuffMode::Dst => return dst,
        PorterDuffMode::SrcOver => {
            ra = sa + da * (1.0 - sa);
            let f = |s: f32, d: f32| s * sa + d * da * (1.0 - sa);
            return [f(sr, dr), f(sg, dg), f(sb, db), ra].map(|v| {
                if ra > 0.0 {
                    v / ra
                } else {
                    0.0
                }
            });
        }
        PorterDuffMode::DstOver => {
            return porter_duff_reference(PorterDuffMode::SrcOver, dst, src);
        }
        PorterDuffMode::SrcIn => {
            ra = sa * da;
            rr = sr * da;
            rg = sg * da;
            rb = sb * da;
        }
        PorterDuffMode::DstIn => {
            ra = da * sa;
            rr = dr * sa;
            rg = dg * sa;
            rb = db * sa;
        }
        PorterDuffMode::SrcOut => {
            ra = sa * (1.0 - da);
            rr = sr * (1.0 - da);
            rg = sg * (1.0 - da);
            rb = sb * (1.0 - da);
        }
        PorterDuffMode::DstOut => {
            ra = da * (1.0 - sa);
            rr = dr * (1.0 - sa);
            rg = dg * (1.0 - sa);
            rb = db * (1.0 - sa);
        }
        PorterDuffMode::SrcAtop => {
            ra = da;
            rr = sr * da + dr * (1.0 - sa);
            rg = sg * da + dg * (1.0 - sa);
            rb = sb * da + db * (1.0 - sa);
        }
        PorterDuffMode::DstAtop => {
            ra = sa;
            rr = dr * sa + sr * (1.0 - da);
            rg = dg * sa + sg * (1.0 - da);
            rb = db * sa + sb * (1.0 - da);
        }
        PorterDuffMode::Xor => {
            ra = sa * (1.0 - da) + da * (1.0 - sa);
            rr = sr * (1.0 - da) + dr * (1.0 - sa);
            rg = sg * (1.0 - da) + dg * (1.0 - sa);
            rb = sb * (1.0 - da) + db * (1.0 - sa);
        }
        PorterDuffMode::Darken => {
            return blend_separable(src, dst, f32::min);
        }
        PorterDuffMode::Lighten => {
            return blend_separable(src, dst, f32::max);
        }
        PorterDuffMode::Multiply => {
            return blend_separable(src, dst, |s, d| s * d);
        }
        PorterDuffMode::Screen => {
            return blend_separable(src, dst, |s, d| s + d - s * d);
        }
        PorterDuffMode::Add => {
            ra = (sa + da).min(1.0);
            rr = (sr + dr).min(1.0);
            rg = (sg + dg).min(1.0);
            rb = (sb + db).min(1.0);
        }
        PorterDuffMode::Overlay => {
            return blend_separable(src, dst, |s, d| {
                if d <= 0.5 {
                    2.0 * s * d
                } else {
                    1.0 - 2.0 * (1.0 - s) * (1.0 - d)
                }
            });
        }
    }
    // Premultiplied results above; un-premultiply.
    if ra > 0.0 {
        [rr / ra, rg / ra, rb / ra, ra]
    } else {
        [0.0, 0.0, 0.0, 0.0]
    }
}

fn blend_separable(src: [f32; 4], dst: [f32; 4], f: impl Fn(f32, f32) -> f32) -> [f32; 4] {
    let (sa, da) = (src[3], dst[3]);
    let ra = sa + da * (1.0 - sa);
    let mut out = [0.0f32; 4];
    for i in 0..3 {
        let blended = f(src[i], dst[i]);
        // Porter-Duff source-over with the blend function applied to colours,
        // both premultiplied by their alphas.
        let v = src[i] * (1.0 - da) + dst[i] * (1.0 - sa) + blended * sa * da;
        out[i] = v;
    }
    out[3] = ra;
    if ra > 0.0 {
        [
            (out[0] / ra).min(1.0),
            (out[1] / ra).min(1.0),
            (out[2] / ra).min(1.0),
            ra,
        ]
    } else {
        [0.0; 4]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argb_round_trip() {
        for &argb in &[0x00000000u32, 0xffffffff, 0x80ff0000, 0x12345678] {
            let (r, g, b, a) = argb_to_rgba(argb);
            assert_eq!(rgba_to_argb(r, g, b, a), argb);
        }
    }

    #[test]
    fn premultiply_round_trip_opaque() {
        let w = pack_premul(200, 100, 50, 255);
        assert_eq!(unpack_unpremul(w), (200, 100, 50, 255));
    }

    #[test]
    fn premultiply_round_trip_translucent() {
        let w = pack_premul(200, 100, 50, 128);
        let (r, g, b, a) = unpack_unpremul(w);
        assert_eq!(a, 128);
        assert!((r as i32 - 200).abs() <= 1, "r={r}");
        assert!((g as i32 - 100).abs() <= 1, "g={g}");
        assert!((b as i32 - 50).abs() <= 1, "b={b}");
    }

    #[test]
    fn porter_duff_src_over_reference() {
        let out = porter_duff_reference(
            PorterDuffMode::SrcOver,
            [1.0, 0.0, 0.0, 0.5],
            [0.0, 0.0, 1.0, 1.0],
        );
        // 0.5 red over opaque blue -> ~ (0.5, 0, 0.5, 1)
        assert!((out[0] - 0.5).abs() < 1e-5);
        assert!((out[2] - 0.5).abs() < 1e-5);
        assert!((out[3] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn blend_ordinals_unique() {
        let mut seen = std::collections::HashSet::new();
        for i in 0..29 {
            let m = AndroidBlendMode::from_ordinal(i).unwrap();
            assert!(seen.insert(m));
        }
        assert!(AndroidBlendMode::from_ordinal(29).is_none());
    }
}
