//! FreeType glyph rasterisation (ADR-0019).
//!
//! Android's text stack rasterises glyphs with FreeType, not by filling
//! `ttf-parser` outlines through a path rasteriser. This module reproduces that
//! backend: `rustybuzz` still decides *where* each glyph goes (advances,
//! kerning, GSUB/GPOS), while FreeType decides *what* each glyph looks like —
//! `FT_LOAD_NO_HINTING | FT_LOAD_RENDER` into an 8-bit grayscale (`GRAY`) A8
//! mask, exactly the configuration the render-fidelity PoC measured against the
//! real Android 14 screenshot (`research/render-fidelity/REPORT.md` §3, §6.2).
//!
//! # FFI confinement
//!
//! This is the only module in `acos-render` that calls into FreeType. Rust's
//! `#![forbid(unsafe_code)]` is not enabled crate-wide (the font mapper already
//! needs `unsafe` for `mmap`), so the unsafe FFI is kept here with per-block
//! `SAFETY` comments. The wrapper owns every raw pointer and never lets one
//! escape.
//!
//! # Build
//!
//! The dependency is `freetype-sys` with its `bundled` feature, which compiles
//! the pinned **FreeType 2.13.2** C sources shipped with the crate (AOSP's
//! `external/freetype` is 2.13.0 at `android-14.0.0_r1` and 2.13.2 at the QPR
//! the fixture came from). No host `libfreetype` is required, and if the
//! vendored C cannot compile the build fails loudly in `freetype-sys`'s
//! `build.rs`. Drop the `bundled` feature in `Cargo.toml` to link the host
//! library via `pkg-config` instead.

use std::collections::HashMap;
use std::ptr;
use std::sync::{Arc, Mutex, OnceLock};

use freetype_sys as ft;

use crate::text::fonts::{FontFile, FontRef};

/// Upper bound on cached A8 masks before the cache is dropped wholesale, the
/// same policy the shaping cache uses. Masks are tiny (a 14sp glyph is a few
/// hundred bytes), so this bounds worst-case memory without an LRU.
const MASK_CACHE_CAPACITY: usize = 4096;

/// A rasterised glyph: an 8-bit coverage (alpha) mask plus its placement.
///
/// The mask is tightly packed (`rows * width` bytes); FreeType's padded
/// `pitch` is stripped when it is copied out of the native bitmap.
#[derive(Clone, Debug)]
pub struct GlyphMask {
    pub width: u32,
    pub rows: u32,
    /// Horizontal bearing from the pen origin to the mask's left edge.
    pub left: i32,
    /// Vertical bearing from the baseline to the mask's top edge.
    pub top: i32,
    pub coverage: Vec<u8>,
    /// For a colour glyph (COLR/CPAL v0/v1, CBDT/CBLC, sbix) this is a
    /// **premultiplied RGBA8** bitmap (`width * rows * 4` bytes) that replaces
    /// `coverage` at blit time. `None` for an ordinary grayscale A8 mask.
    pub color: Option<Vec<u8>>,
}

impl GlyphMask {
    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.rows == 0
    }

    /// Whether this glyph carries its own colour (emoji) bitmap.
    pub fn is_color(&self) -> bool {
        self.color.is_some()
    }
}

/// Variable-font axis coordinates in a canonical, hashable form.
///
/// Each entry is an OpenType axis tag (`wght` = `0x77676874`) and the design
/// value in 16.16 fixed point, the unit `FT_Set_Var_Design_Coordinates`
/// expects. The list is sorted and deduplicated so two equivalent variation
/// sets (including `fonts.xml` axes overridden by `FontVariationSettings`)
/// produce the same cache key.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct VariationCoords {
    coords: Vec<(u32, i64)>,
}

impl VariationCoords {
    /// Normalise `(tag, value)` pairs into the sorted, deduplicated 16.16
    /// design-coordinate list FreeType wants.
    pub fn new(variations: &[(String, f32)]) -> Self {
        let mut coords: Vec<(u32, i64)> = variations
            .iter()
            .filter_map(|(tag, value)| {
                let b = tag.as_bytes();
                if b.len() != 4 || !value.is_finite() {
                    return None;
                }
                Some((
                    u32::from_be_bytes([b[0], b[1], b[2], b[3]]),
                    (*value as f64 * 65536.0).round() as i64,
                ))
            })
            .collect();
        coords.sort_unstable_by_key(|(tag, _)| *tag);
        coords.dedup_by_key(|(tag, _)| *tag);
        VariationCoords { coords }
    }

    pub fn is_empty(&self) -> bool {
        self.coords.is_empty()
    }
}

/// One open FreeType face plus the mmap that backs it.
struct OpenFace {
    raw: ft::FT_Face,
    /// Keeps the font bytes alive for as long as FreeType points into them.
    _file: Arc<FontFile>,
    /// The 26.6 character size FreeType is currently configured with, so a
    /// size change is only pushed down when it actually changes.
    char_size: i64,
    /// The variation coordinates currently pushed to this face, so a change
    /// is only applied when it differs (the same policy as `char_size`).
    variations: VariationCoords,
}

#[derive(PartialEq, Eq, Hash)]
struct MaskKey {
    font_id: u64,
    glyph_id: u16,
    char_size: i64,
    /// The variable-font instance, so masks for different weights/styles are
    /// cached separately (ADR-0019 `T-33`).
    variations: VariationCoords,
    /// Horizontal subpixel pen offset quantised to 1/64 px (`0..=63`), so a
    /// glyph rendered at a fractional pen position is cached separately from
    /// the same glyph at an integer position.
    subpixel: u8,
    /// FreeType load target (`0` = no hinting, `1` = `FT_LOAD_TARGET_LIGHT`).
    hint: u8,
}

/// FreeType load target selected by `ACOS_TEXT_HINT` (`none` default, `light`
/// for `FT_LOAD_TARGET_LIGHT`). Android's rasteriser is normally the light
/// target; the fidelity corpus measures both instead of assuming.
fn text_hint_target() -> u8 {
    static HINT: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *HINT.get_or_init(|| match std::env::var("ACOS_TEXT_HINT").as_deref() {
        Ok("light") | Ok("slight") => 1,
        _ => 0,
    })
}

struct Rasterizer {
    library: ft::FT_Library,
    faces: HashMap<u64, OpenFace>,
    masks: HashMap<MaskKey, Arc<GlyphMask>>,
}

// SAFETY: `Rasterizer` owns raw `FT_Library`/`FT_Face` pointers, which are not
// `Send` by auto-trait. Every access is serialised behind the `Mutex` in
// `rasterizer()`, and no pointer is ever copied out, so moving the whole struct
// to another thread is sound. FreeType is explicitly documented as safe to use
// from multiple threads as long as a single face/library object is not used
// concurrently; the mutex provides that.
unsafe impl Send for Rasterizer {}

impl Rasterizer {
    fn new() -> Option<Rasterizer> {
        let mut library: ft::FT_Library = ptr::null_mut();
        // SAFETY: `library` is a valid out-pointer; on success FreeType fills
        // it and owns all internal state. On failure we do not touch it.
        let err = unsafe { ft::FT_Init_FreeType(&mut library) };
        if err != 0 || library.is_null() {
            return None;
        }
        Some(Rasterizer {
            library,
            faces: HashMap::new(),
            masks: HashMap::new(),
        })
    }

    /// Open (or fetch) the FreeType face for a font file.
    fn face_for(&mut self, file: &Arc<FontFile>) -> Option<&mut OpenFace> {
        if !self.faces.contains_key(&file.id) {
            let mut raw: ft::FT_Face = ptr::null_mut();
            let len = file.data.len() as ft::FT_Long;
            // SAFETY: `file.data` is an mmap owned by the `Arc<FontFile>` we
            // clone into the map entry, so the pointer stays valid for the
            // lifetime of the face. `len` is the mapped length and
            // `face_index` selects the face inside a collection.
            let err = unsafe {
                ft::FT_New_Memory_Face(
                    self.library,
                    file.data.as_ptr(),
                    len,
                    file.face_index as ft::FT_Long,
                    &mut raw,
                )
            };
            if err != 0 || raw.is_null() {
                return None;
            }
            self.faces.insert(
                file.id,
                OpenFace {
                    raw,
                    _file: file.clone(),
                    char_size: 0,
                    variations: VariationCoords::default(),
                },
            );
        }
        self.faces.get_mut(&file.id)
    }

    /// Rasterise one glyph at `char_size` (26.6 px), or `None` when FreeType
    /// cannot produce a grayscale bitmap (caller falls back to outlines).
    fn rasterize_uncached(
        &mut self,
        file: &Arc<FontFile>,
        glyph_id: u16,
        char_size: i64,
        variations: &VariationCoords,
        subpixel: u8,
        hint: u8,
    ) -> Option<GlyphMask> {
        let library = self.library;
        let face = self.face_for(file)?;
        // A scalable face (outlines, COLRv0, sbix) is sized with
        // `FT_Set_Char_Size`. A bitmap-only face (CBDT/CBLC) has fixed strikes
        // and rejects an arbitrary pixel size, so pick the nearest strike and
        // resample the result to the requested size below.
        // SAFETY: `face.raw` is live; the face flags/size table are read-only.
        let scalable = unsafe { (*face.raw).face_flags } & ft::FT_FACE_FLAG_SCALABLE != 0;
        let mut bitmap_scale = 1.0f32;
        if scalable {
            if face.char_size != char_size {
                // SAFETY: `char_size` is a 26.6 (F26Dot6) pixel size.
                let err = unsafe {
                    ft::FT_Set_Char_Size(
                        face.raw,
                        char_size as ft::FT_F26Dot6,
                        char_size as ft::FT_F26Dot6,
                        0,
                        0,
                    )
                };
                if err != 0 {
                    return None;
                }
                face.char_size = char_size;
            }
        } else {
            // SAFETY: `available_sizes` has `num_fixed_sizes` entries.
            let num = unsafe { (*face.raw).num_fixed_sizes }.max(0);
            if num == 0 {
                return None;
            }
            let want_px = char_size as f32 / 64.0;
            let mut best = 0i32;
            let mut best_d = f32::MAX;
            for i in 0..num {
                // SAFETY: `i < num_fixed_sizes`.
                let s = unsafe { &*(*face.raw).available_sizes.add(i as usize) };
                let px = s.y_ppem as f32 / 64.0;
                let d = (px - want_px).abs();
                if d < best_d {
                    best_d = d;
                    best = i;
                }
            }
            if unsafe { ft::FT_Select_Size(face.raw, best) } != 0 {
                return None;
            }
            // SAFETY: `best < num_fixed_sizes`.
            let strike_px = unsafe { (*face.raw).available_sizes.add(best as usize) };
            let strike_px = unsafe { (*strike_px).y_ppem as f32 / 64.0 }.max(1.0);
            bitmap_scale = want_px / strike_px;
            // Force a size re-set if the next request is scalable/other.
            face.char_size = 0;
        }
        if face.variations != *variations && !apply_variations(library, face, variations) {
            return None;
        }

        // Subpixel positioning: translate the unhinted outline by the
        // fractional pen offset before rasterising, exactly the mechanism
        // Skia's FreeType scaler uses when `kSubpixelPositioning` is set. A
        // null matrix keeps the size; only the 26.6 delta is applied. The
        // transform is re-set on every load (including a zero delta) so it
        // never leaks between glyphs.
        // `subpixel` is a 1/64-px bucket in 26.6 units (1 unit = 1/64 px), so
        // the delta is the bucket value directly. Bitmap strikes keep an
        // integer position (a translate would be resampled away below).
        let mut delta = ft::FT_Vector {
            x: if scalable { subpixel as ft::FT_Pos } else { 0 },
            y: 0,
        };
        // SAFETY: `face.raw` is live; FT_Set_Transform only mutates the face's
        // transform and returns void.
        unsafe { ft::FT_Set_Transform(face.raw, ptr::null_mut(), &mut delta) };

        // SAFETY: `face.raw` is live. `FT_LOAD_RENDER` asks FreeType to render
        // the glyph into `glyph.bitmap` in the same call. `FT_LOAD_COLOR` asks
        // it to synthesise a premultiplied BGRA bitmap for a colour glyph
        // (COLRv0/CPAL, CBDT/CBLC, sbix); it is a no-op for ordinary outline
        // glyphs, which stay grayscale A8. `hint` selects the rasteriser:
        // unhinted (the historical / PoC-measured best) or `TARGET_LIGHT`
        // (vertical-only hinting, Android's `SkPaint` default).
        let target = if hint == 1 {
            ft::FT_LOAD_TARGET_LIGHT
        } else {
            ft::FT_LOAD_NO_HINTING
        };
        let flags = target | ft::FT_LOAD_RENDER | ft::FT_LOAD_COLOR;
        let err = unsafe { ft::FT_Load_Glyph(face.raw, glyph_id as ft::FT_UInt, flags) };
        if err != 0 {
            return None;
        }

        // SAFETY: after a successful `FT_Load_Glyph` the slot is populated.
        let slot = unsafe { (*face.raw).glyph };
        if slot.is_null() {
            return None;
        }
        // SAFETY: the slot outlives these reads; we only borrow it under the
        // same lock and copy the bitmap bytes out before returning.
        let bitmap = unsafe { &(*slot).bitmap };
        // Grayscale A8 masks and colour BGRA bitmaps are composited here;
        // anything else (monochrome, LCD, ...) falls back to the path renderer.
        let mode = bitmap.pixel_mode as i32;
        if mode != ft::FT_PIXEL_MODE_GRAY as i32 && mode != ft::FT_PIXEL_MODE_BGRA as i32 {
            return None;
        }
        let left = unsafe { (*slot).bitmap_left };
        let top = unsafe { (*slot).bitmap_top };
        let width = bitmap.width.max(0) as u32;
        let rows = bitmap.rows.max(0) as u32;
        if width == 0 || rows == 0 {
            return Some(GlyphMask {
                width,
                rows,
                left,
                top,
                coverage: Vec::new(),
                color: None,
            });
        }
        if bitmap.buffer.is_null() {
            return None;
        }

        let pitch = bitmap.pitch;
        let abs_pitch = pitch.unsigned_abs() as usize;
        let total = abs_pitch.checked_mul(rows as usize)?;
        // SAFETY: FreeType guarantees `rows * |pitch|` readable bytes at
        // `buffer` after a render, and the buffer stays valid until the next
        // load on this face (we copy out immediately under the same lock).
        let src = unsafe { std::slice::from_raw_parts(bitmap.buffer, total) };

        if bitmap.pixel_mode as i32 == ft::FT_PIXEL_MODE_BGRA as i32 {
            // A colour glyph: COLR/CPAL layers, a CBDT/CBLC PNG strike or an
            // sbix bitmap were composited by FreeType into premultiplied BGRA.
            // Reorder to premultiplied RGBA for tiny-skia.
            let mut rgba = vec![0u8; (width as usize) * (rows as usize) * 4];
            for y in 0..rows as usize {
                let src_row = if pitch >= 0 { y } else { rows as usize - 1 - y };
                let off = src_row * abs_pitch;
                let row_len = width as usize * 4;
                if off + row_len > src.len() {
                    return None;
                }
                for x in 0..width as usize {
                    let s = off + x * 4;
                    let d = (y * width as usize + x) * 4;
                    // BGRA -> RGBA (both premultiplied).
                    rgba[d] = src[s + 2];
                    rgba[d + 1] = src[s + 1];
                    rgba[d + 2] = src[s];
                    rgba[d + 3] = src[s + 3];
                }
            }
            let (rgba, width, rows, left, top) = if (bitmap_scale - 1.0).abs() > 0.01 {
                let dw = ((width as f32 * bitmap_scale).round() as u32).max(1);
                let dh = ((rows as f32 * bitmap_scale).round() as u32).max(1);
                match scale_rgba(&rgba, width, rows, dw, dh) {
                    Some((scaled, w, h)) => (
                        scaled,
                        w,
                        h,
                        (left as f32 * bitmap_scale).round() as i32,
                        (top as f32 * bitmap_scale).round() as i32,
                    ),
                    None => (rgba, width, rows, left, top),
                }
            } else {
                (rgba, width, rows, left, top)
            };
            return Some(GlyphMask {
                width,
                rows,
                left,
                top,
                coverage: Vec::new(),
                color: Some(rgba),
            });
        }

        let mut coverage = vec![0u8; (width as usize) * (rows as usize)];
        for y in 0..rows as usize {
            // Positive pitch is top-down; negative pitch is bottom-up.
            let src_row = if pitch >= 0 { y } else { rows as usize - 1 - y };
            let off = src_row * abs_pitch;
            let end = off + width as usize;
            if end > src.len() {
                return None;
            }
            coverage[y * width as usize..y * width as usize + width as usize]
                .copy_from_slice(&src[off..end]);
        }

        Some(GlyphMask {
            width,
            rows,
            left,
            top,
            coverage,
            color: None,
        })
    }
}

impl Drop for Rasterizer {
    fn drop(&mut self) {
        // SAFETY: every face was created by `FT_New_Memory_Face` on this
        // library and is dropped exactly once; the library is then freed.
        unsafe {
            for (_, face) in self.faces.drain() {
                ft::FT_Done_Face(face.raw);
            }
            ft::FT_Done_Library(self.library);
        }
    }
}

static RASTERIZER: OnceLock<Option<Mutex<Rasterizer>>> = OnceLock::new();

fn rasterizer() -> Option<&'static Mutex<Rasterizer>> {
    RASTERIZER
        .get_or_init(|| Rasterizer::new().map(Mutex::new))
        .as_ref()
}

/// Resample a premultiplied RGBA8 bitmap to `dw x dh` (used to bring a fixed
/// CBDT/CBLC strike to the requested text size). Bilinear via tiny-skia; the
/// result is premultiplied RGBA8, matching the input.
fn scale_rgba(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Option<(Vec<u8>, u32, u32)> {
    use tiny_skia::{IntSize, Pixmap, PixmapPaint, Transform};
    let size = IntSize::from_wh(sw, sh)?;
    let pm = Pixmap::from_vec(src.to_vec(), size)?;
    let mut dst = Pixmap::new(dw, dh)?;
    let paint = PixmapPaint {
        opacity: 1.0,
        blend_mode: tiny_skia::BlendMode::Source,
        quality: tiny_skia::FilterQuality::Bilinear,
    };
    dst.draw_pixmap(
        0,
        0,
        pm.as_ref(),
        &paint,
        Transform::from_scale(dw as f32 / sw as f32, dh as f32 / sh as f32),
        None,
    );
    Some((dst.take(), dw, dh))
}

/// Whether a FreeType rasteriser could be initialised at all.
pub fn available() -> bool {
    rasterizer().is_some()
}

/// Rasterise `glyph_id` from `font` at `size_px` pixels, caching the A8 mask.
///
/// Returns `None` if FreeType is unavailable, the font cannot be opened, or the
/// glyph has no grayscale outline — the caller must then use the outline path.
///
/// Variable-font axes are pushed to FreeType with
/// `FT_Set_Var_Design_Coordinates`, using the same design-space values
/// `rustybuzz` shapes with, so the rasterised instance and the advances agree.
/// The mask cache key includes the axis coordinates, so `wght=400` and
/// `wght=700` never collide.
pub fn rasterize(
    font: &FontRef,
    glyph_id: u16,
    size_px: f32,
    variations: &[(String, f32)],
) -> Option<Arc<GlyphMask>> {
    let coords = VariationCoords::new(variations);
    rasterize_coords(font, glyph_id, size_px, &coords)
}

/// [`rasterize`] with an already-normalised coordinate set. The hot text-draw
/// loop computes the coordinates once per shaped run instead of once per glyph.
pub fn rasterize_coords(
    font: &FontRef,
    glyph_id: u16,
    size_px: f32,
    variations: &VariationCoords,
) -> Option<Arc<GlyphMask>> {
    rasterize_subpixel_coords(font, glyph_id, size_px, variations, 0.0)
}

/// [`rasterize_coords`] with a horizontal subpixel pen offset in pixels
/// (`subpixel_x` in `0.0..1.0`). The offset is quantised to 1/64 px, applied to
/// the unhinted outline with `FT_Set_Transform`, and included in the mask cache
/// key — the FreeType analogue of Skia's `kSubpixelPositioning` grid.
/// `subpixel_x == 0.0` is exactly the integer-pen path [`rasterize_coords`]
/// uses.
pub fn rasterize_subpixel_coords(
    font: &FontRef,
    glyph_id: u16,
    size_px: f32,
    variations: &VariationCoords,
    subpixel_x: f32,
) -> Option<Arc<GlyphMask>> {
    if !size_px.is_finite() || size_px <= 0.0 {
        return None;
    }
    let char_size = (size_px as f64 * 64.0).round() as i64;
    if char_size <= 0 {
        return None;
    }
    let subpixel = if subpixel_x.is_finite() {
        ((subpixel_x.rem_euclid(1.0)) * 64.0)
            .round()
            .clamp(0.0, 63.0) as u8
    } else {
        0
    };
    let key = MaskKey {
        font_id: font.file.id,
        glyph_id,
        char_size,
        variations: variations.clone(),
        subpixel,
        hint: text_hint_target(),
    };
    let mut guard = rasterizer()?.lock().ok()?;
    if let Some(cached) = guard.masks.get(&key) {
        return Some(cached.clone());
    }
    let variation_coords = key.variations.clone();
    let mask = Arc::new(guard.rasterize_uncached(
        &font.file,
        glyph_id,
        char_size,
        &variation_coords,
        key.subpixel,
        key.hint,
    )?);
    if guard.masks.len() >= MASK_CACHE_CAPACITY {
        guard.masks.clear();
    }
    guard.masks.insert(key, mask.clone());
    Some(mask)
}

/// Push `variations` onto `face`, mapping design coordinates through the
/// face's `fvar` axis order. Returns `false` when FreeType rejects the
/// coordinates, which makes the caller fall back to the outline path.
///
/// `FT_Set_Var_Design_Coordinates` is used (not `FT_Set_Var_Blend_Coordinates`)
/// because `rustybuzz` shapes with design-space values and the same
/// `(tag, value)` pairs are handed to both. This is also the call Minikin's
/// `MinikinFontFreeType::setVariationAxes` makes on Android.
fn apply_variations(
    library: ft::FT_Library,
    face: &mut OpenFace,
    variations: &VariationCoords,
) -> bool {
    let mut mm: *mut ft::FT_MM_Var = ptr::null_mut();
    // SAFETY: `face.raw` is a live face and `mm` is a valid out-pointer; on
    // success FreeType allocates an `FT_MM_Var` owned by the caller.
    let err = unsafe { ft::FT_Get_MM_Var(face.raw, &mut mm) };
    if err != 0 || mm.is_null() {
        // A static face has no MM data: the (empty) request is satisfied.
        face.variations = variations.clone();
        return true;
    }
    // SAFETY: `mm` is the live FT_MM_Var returned above.
    let num_axis = unsafe { (*mm).num_axis };
    if num_axis == 0 {
        // SAFETY: `mm` was allocated by FT_Get_MM_Var on this library.
        unsafe { ft::FT_Done_MM_Var(library, mm) };
        face.variations = variations.clone();
        return true;
    }
    // Start from the face's own defaults, then override matching tags.
    let mut coords = vec![0i64; num_axis as usize];
    for (i, coord) in coords.iter_mut().enumerate() {
        // SAFETY: `i < num_axis`, so the axis pointer is in bounds.
        *coord = unsafe { (*(*mm).axis.add(i)).def };
    }
    for &(tag, value) in &variations.coords {
        for (i, coord) in coords.iter_mut().enumerate() {
            // SAFETY: `i < num_axis`.
            if unsafe { (*(*mm).axis.add(i)).tag } == tag as ft::FT_ULong {
                *coord = value;
                break;
            }
        }
    }
    // SAFETY: `face.raw` is live and `coords` holds `num_axis` entries.
    let err = unsafe { ft::FT_Set_Var_Design_Coordinates(face.raw, num_axis, coords.as_ptr()) };
    // SAFETY: `mm` is no longer used after this call; it must be freed with
    // the library that produced it.
    unsafe { ft::FT_Done_MM_Var(library, mm) };
    if err != 0 {
        return false;
    }
    face.variations = variations.clone();
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freetype_initialises_and_rasterises_a_glyph() {
        let Some(fonts) = acos_test_support::require_some(crate::FontCollection::shared(), "freetype_initialises_and_rasterises_a_glyph", "no fonts available") else {
            return;
        };
        assert!(available(), "bundled FreeType failed to initialise");
        let font = fonts
            .resolve_typeface(None, 400, false)
            .expect("default font");
        // 'H' is present in every Latin font.
        let glyph_id = {
            let face = ttf_parser::Face::parse(&font.file.data, font.file.face_index).unwrap();
            face.glyph_index('H').unwrap().0
        };
        let mask = rasterize(&font, glyph_id, 36.75, &[]).expect("FreeType mask");
        assert!(mask.width > 0 && mask.rows > 0);
        assert_eq!(mask.coverage.len(), (mask.width * mask.rows) as usize);
        // An 'H' stem must cover some pixels fully and some partially (AA).
        let max = mask.coverage.iter().copied().max().unwrap();
        assert_eq!(max, 255, "mask should have fully covered pixels");
        assert!(
            mask.coverage.iter().any(|&c| c > 0 && c < 255),
            "unhinted grayscale mask should contain partial coverage"
        );
        // Second call must hit the cache and return an equal mask.
        let again = rasterize(&font, glyph_id, 36.75, &[]).unwrap();
        assert_eq!(again.coverage, mask.coverage);
    }

    /// Total A8 coverage of a glyph mask (sum of every pixel's alpha).
    fn ink(mask: &GlyphMask) -> u64 {
        mask.coverage.iter().map(|&c| c as u64).sum()
    }

    fn glyph_h(font: &FontRef) -> u16 {
        let face = ttf_parser::Face::parse(&font.file.data, font.file.face_index).unwrap();
        face.glyph_index('H').unwrap().0
    }

    #[test]
    fn variable_weight_masks_differ_and_are_monotonic() {
        let Some(fonts) = acos_test_support::require_some(crate::FontCollection::shared(), "variable_weight_masks_differ_and_are_monotonic", "no fonts available") else {
            return;
        };
        assert!(available(), "bundled FreeType failed to initialise");
        // The Android 14 sans-serif family is the Roboto variable font; each
        // `<font>` entry carries the matching `wght` axis, so 100..900 selects
        // nine distinct instances of the same file.
        let mut last = 0u64;
        let mut seen = Vec::new();
        for weight in [100u16, 200, 300, 400, 500, 600, 700, 800, 900] {
            let font = fonts.resolve_typeface(None, weight, false).unwrap();
            let gid = glyph_h(&font);
            let mask = rasterize(&font, gid, 36.75, &font.axes).expect("mask");
            let total = ink(&mask);
            seen.push((weight, total));
            assert!(
                total > last,
                "wght {weight} coverage {total} did not exceed previous {last}"
            );
            last = total;
        }
        // Sanity: the lightest and heaviest instances must be clearly apart,
        // not a rounding-level difference.
        let light = seen.first().unwrap().1;
        let heavy = seen.last().unwrap().1;
        assert!(
            heavy > light * 2,
            "wght 900 ink {heavy} not > 2x wght 100 ink {light}: {seen:?}"
        );
    }

    #[test]
    fn variable_instance_cache_keys_do_not_collide() {
        let Some(fonts) = acos_test_support::require_some(crate::FontCollection::shared(), "variable_instance_cache_keys_do_not_collide", "no fonts available") else {
            return;
        };
        let font = fonts.resolve_typeface(None, 400, false).unwrap();
        let gid = glyph_h(&font);
        let light = rasterize(&font, gid, 36.75, &[("wght".into(), 200.0)]).unwrap();
        let heavy = rasterize(&font, gid, 36.75, &[("wght".into(), 800.0)]).unwrap();
        assert_ne!(light.coverage, heavy.coverage, "instances must differ");
        // The exact same coordinates must hit the cache and reproduce the
        // mask bit-for-bit.
        let light_again = rasterize(&font, gid, 36.75, &[("wght".into(), 200.0)]).unwrap();
        assert_eq!(light_again.coverage, light.coverage);
        // Out-of-order tag lists are canonicalised to the same key.
        let ordered = rasterize(
            &font,
            gid,
            36.75,
            &[("wght".into(), 200.0), ("wdth".into(), 100.0)],
        )
        .unwrap();
        let reordered = rasterize(
            &font,
            gid,
            36.75,
            &[("wdth".into(), 100.0), ("wght".into(), 200.0)],
        )
        .unwrap();
        assert_eq!(reordered.coverage, ordered.coverage);
    }

    #[test]
    fn colour_emoji_rasterises_to_an_rgba_bitmap() {
        let Some(fonts) = acos_test_support::require_some(crate::FontCollection::shared(), "colour_emoji_rasterises_to_an_rgba_bitmap", "no fonts available") else {
            return;
        };
        assert!(available(), "bundled FreeType failed to initialise");
        // Use the CBDT/CBLC strike font explicitly: it is bitmap-only (no
        // outlines), so it exercises the fixed-strike selection and resampling
        // path as well as the BGRA colour bitmap. Android marks it
        // `ignore="true"` when a COLR font is present, but the strike code is
        // the same one `NotoColorEmojiFlags.ttf` uses at runtime.
        let Some(entry) = fonts
            .families
            .iter()
            .flat_map(|f| &f.fonts)
            .find(|e| e.file.path.to_string_lossy().contains("Legacy"))
        else {
            acos_test_support::require_prereq(false, "colour_emoji_rasterises_to_an_rgba_bitmap", "no CBDT emoji font available");
            return;
        };
        let font = FontRef {
            file: entry.file.clone(),
            weight: entry.weight,
            italic: entry.italic,
            axes: entry.axes.clone(),
            family_key: 0,
        };
        let face = ttf_parser::Face::parse(&font.file.data, font.file.face_index).unwrap();
        let gid = face.glyph_index('\u{1F600}').expect("emoji glyph").0;
        let size_px = 36.75f32;
        let mask = rasterize(&font, gid, size_px, &font.axes).expect("colour mask");
        assert!(mask.is_color(), "emoji must take the colour path: {font:?}");
        let rgba = mask.color.as_ref().unwrap();
        assert_eq!(rgba.len(), (mask.width * mask.rows * 4) as usize);
        // The fixed strike was resampled to roughly the requested text size.
        assert!(
            mask.width as f32 >= size_px * 0.5 && mask.width as f32 <= size_px * 1.5,
            "CBDT strike was not resampled to the text size: {} vs {size_px}",
            mask.width
        );
        // At least one fully/partly opaque pixel and at least one non-gray
        // pixel: a colour emoji must not be a monochrome A8 mask.
        let mut opaque = 0usize;
        let mut colourful = false;
        for px in rgba.chunks_exact(4) {
            if px[3] == 255 {
                opaque += 1;
            }
            if px[0] != px[1] || px[1] != px[2] {
                colourful = true;
            }
        }
        assert!(opaque > 0, "emoji bitmap has no opaque pixels");
        assert!(colourful, "emoji bitmap is monochrome");
    }
}
