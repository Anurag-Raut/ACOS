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
}

impl GlyphMask {
    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.rows == 0
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
}

#[derive(PartialEq, Eq, Hash)]
struct MaskKey {
    font_id: u64,
    glyph_id: u16,
    char_size: i64,
    // NOTE: variable-font coordinates are intentionally absent; see
    // `rasterize` (masks use the default instance, as the outline path did).
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
    ) -> Option<GlyphMask> {
        let face = self.face_for(file)?;
        if face.char_size != char_size {
            // SAFETY: `face.raw` is a live face owned by `self`; `char_size`
            // is a 26.6 (F26Dot6) pixel size. A non-scalable face returns an
            // error and we fall back to outlines.
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

        // SAFETY: `face.raw` is live. `FT_LOAD_NO_HINTING` matches Android's
        // unhinted grayscale rasterisation; `FT_LOAD_RENDER` asks FreeType to
        // render the glyph into `glyph.bitmap` in the same call.
        let flags = ft::FT_LOAD_NO_HINTING | ft::FT_LOAD_RENDER;
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
        // Only grayscale masks are composited here; anything else (monochrome,
        // LCD, bitmap strike, colour) falls back to the path renderer.
        if bitmap.pixel_mode as i32 != ft::FT_PIXEL_MODE_GRAY as i32 {
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

/// Whether a FreeType rasteriser could be initialised at all.
pub fn available() -> bool {
    rasterizer().is_some()
}

/// Rasterise `glyph_id` from `font` at `size_px` pixels, caching the A8 mask.
///
/// Returns `None` if FreeType is unavailable, the font cannot be opened, or the
/// glyph has no grayscale outline — the caller must then use the outline path.
///
/// Variable-font axes (`FontRef::axes` / `TextStyle::variations`) are *not*
/// pushed to FreeType here. The outline path did not apply them either, so this
/// preserves the existing rendering contract: the default instance is used for
/// the mask, while `rustybuzz` still applies the axes to shaping/advances.
/// Enabling `FT_Set_Var_Design_Coordinates` is a fidelity improvement tracked
/// separately (T-33).
pub fn rasterize(font: &FontRef, glyph_id: u16, size_px: f32) -> Option<Arc<GlyphMask>> {
    if !size_px.is_finite() || size_px <= 0.0 {
        return None;
    }
    let char_size = (size_px as f64 * 64.0).round() as i64;
    if char_size <= 0 {
        return None;
    }
    let key = MaskKey {
        font_id: font.file.id,
        glyph_id,
        char_size,
    };
    let mut guard = rasterizer()?.lock().ok()?;
    if let Some(cached) = guard.masks.get(&key) {
        return Some(cached.clone());
    }
    let mask = Arc::new(guard.rasterize_uncached(&font.file, glyph_id, char_size)?);
    if guard.masks.len() >= MASK_CACHE_CAPACITY {
        guard.masks.clear();
    }
    guard.masks.insert(key, mask.clone());
    Some(mask)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freetype_initialises_and_rasterises_a_glyph() {
        let Some(fonts) = crate::FontCollection::shared() else {
            eprintln!("SKIP: no fonts available");
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
        let mask = rasterize(&font, glyph_id, 36.75).expect("FreeType mask");
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
        let again = rasterize(&font, glyph_id, 36.75).unwrap();
        assert_eq!(again.coverage, mask.coverage);
    }
}
