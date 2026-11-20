//! Text shaping, measurement and layout: an Android `minikin` replacement.
//! Shaping/metrics use `rustybuzz` (HarfBuzz) + `ttf-parser`; glyph
//! rasterisation uses FreeType (see [`freetype`], ADR-0019).
//!
//! Input is UTF-16 (`Java char`), exactly as the framework passes it to the
//! `Paint`/`LineBreaker`/`MeasuredText` natives. Output is advances, metrics,
//! bounds, glyph runs (renderable outlines) and greedy line breaks.

pub mod fonts;
pub mod freetype;
pub mod layout;

pub use fonts::{FontCollection, FontRef};
pub use layout::{LineBreak, LineBreaker, ParagraphConstraints};

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

use crate::geom::RectF;
use crate::paint::Paint;

/// Android `Paint.FontMetrics` (float), all in pixels relative to the baseline.
#[derive(Copy, Clone, PartialEq, Debug, Default)]
pub struct FontMetrics {
    pub top: f32,
    pub ascent: f32,
    pub descent: f32,
    pub bottom: f32,
    pub leading: f32,
}

/// Android `Paint.FontMetricsInt`.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub struct FontMetricsInt {
    pub top: i32,
    pub ascent: i32,
    pub descent: i32,
    pub bottom: i32,
    pub leading: i32,
}

/// `SkScalarRoundToInt` (`floor(x + 0.5)`), as used by
/// `GraphicsJNI::set_metrics_int` for the round-half-up Ascent/Descent/Leading
/// fields.
fn scalar_round_to_int(x: f32) -> i32 {
    (x + 0.5).floor() as i32
}

/// The exact `GraphicsJNI::set_metrics_int` conversion
/// (`frameworks/base/libs/hwui/jni/Graphics.cpp`): `top` floors, `bottom`
/// ceils, and `ascent`/`descent`/`leading` round half up.
impl From<FontMetrics> for FontMetricsInt {
    fn from(m: FontMetrics) -> Self {
        FontMetricsInt {
            top: m.top.floor() as i32,
            ascent: scalar_round_to_int(m.ascent),
            descent: scalar_round_to_int(m.descent),
            bottom: m.bottom.ceil() as i32,
            leading: scalar_round_to_int(m.leading),
        }
    }
}

/// A shaped glyph.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct ShapedGlyph {
    pub glyph_id: u16,
    /// UTF-16 index relative to the start of the shaped slice.
    pub cluster: usize,
    pub advance: f32,
    pub x_offset: f32,
    pub y_offset: f32,
    /// Index of the font run this glyph belongs to.
    pub font_slot: usize,
}

/// A run of glyphs all shaped with one font.
#[derive(Clone, Debug)]
pub struct FontRun {
    pub font: FontRef,
    pub start: usize,
    pub end: usize,
    pub rtl: bool,
    pub glyphs: Vec<ShapedGlyph>,
    pub advance: f32,
}

/// Shaped text: glyph runs (visual order) + per-UTF-16-char advances + metrics.
#[derive(Clone, Debug)]
pub struct ShapedText {
    pub runs: Vec<FontRun>,
    pub fonts: Vec<FontRef>,
    pub char_advances: Vec<f32>,
    pub total_advance: f32,
    pub metrics: FontMetrics,
    pub rtl: bool,
    pub missing_glyphs: usize,
}

/// Text attributes derived from a Paint.
#[derive(Clone, Debug)]
pub struct TextStyle {
    pub font: FontRef,
    /// Requested family name (for fallback ordering).
    pub family: Option<String>,
    pub size: f32,
    pub scale_x: f32,
    pub skew_x: f32,
    pub letter_spacing: f32,
    pub fake_bold: bool,
    pub features: Vec<(String, u32)>,
    pub variations: Vec<(String, f32)>,
}

impl TextStyle {
    pub fn from_paint(paint: &Paint, font: FontRef) -> Self {
        TextStyle {
            font,
            family: None,
            size: paint.text_size,
            scale_x: paint.text_scale_x,
            skew_x: paint.text_skew_x,
            letter_spacing: paint.letter_spacing,
            fake_bold: paint.fake_bold,
            features: parse_features(paint.font_feature_settings.as_deref()),
            variations: parse_variations(paint.font_variation_settings.as_deref()),
        }
    }
}

fn parse_features(s: Option<&str>) -> Vec<(String, u32)> {
    let mut out = Vec::new();
    if let Some(s) = s {
        for part in s.split([',', ' ']).filter(|p| !p.is_empty()) {
            if let Some((k, v)) = part.split_once('=') {
                if let Ok(v) = v.parse::<u32>() {
                    out.push((k.to_string(), v));
                }
            } else {
                out.push((part.to_string(), 1));
            }
        }
    }
    out
}

fn parse_variations(s: Option<&str>) -> Vec<(String, f32)> {
    let mut out = Vec::new();
    if let Some(s) = s {
        for part in s.split([',', ' ']).filter(|p| !p.is_empty()) {
            if let Some((k, v)) = part.split_once('=') {
                if let Ok(v) = v.parse::<f32>() {
                    out.push((k.to_string(), v));
                }
            }
        }
    }
    out
}

/// The effective variable-font design coordinates for a run: the resolved
/// face's `fonts.xml` axes overridden by the paint's `FontVariationSettings`.
/// Shaping (`rustybuzz`) and FreeType rasterisation must be handed the same
/// list so the instance that is shaped is the instance that is drawn.
pub fn effective_variations(
    font: &FontRef,
    style_variations: &[(String, f32)],
) -> Vec<(String, f32)> {
    let mut out = font.axes.clone();
    for (tag, value) in style_variations {
        if let Some(slot) = out.iter_mut().find(|(t, _)| t == tag) {
            slot.1 = *value;
        } else {
            out.push((tag.clone(), *value));
        }
    }
    out
}

#[derive(PartialEq, Eq, Hash)]
struct ShapeKey {
    font_id: u64,
    size_bits: u32,
    scale_bits: u32,
    spacing_bits: u32,
    fake_bold: bool,
    features: Vec<(String, u32)>,
    /// Effective variable-font design coordinates (f32 bits). Without this a
    /// `wght=400` shape would be served for a `wght=700` paint.
    variations: Vec<(String, u32)>,
    text: Vec<u16>,
}

/// Shaping-cache statistics (minikin `LayoutCache` analogue).
#[derive(Copy, Clone, Default, Debug)]
pub struct ShapeCacheStats {
    pub hits: u64,
    pub misses: u64,
}

impl ShapeCacheStats {
    pub fn hit_rate(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            0.0
        } else {
            self.hits as f64 / total as f64
        }
    }
}

/// The text engine: owns the font collection and a shaping/advance cache.
pub struct TextEngine {
    pub fonts: Arc<FontCollection>,
    cache: HashMap<ShapeKey, Arc<ShapedText>>,
    stats: ShapeCacheStats,
    cache_capacity: usize,
}

impl TextEngine {
    pub fn new(fonts: Arc<FontCollection>) -> Self {
        TextEngine {
            fonts,
            cache: HashMap::new(),
            stats: ShapeCacheStats::default(),
            cache_capacity: 4096,
        }
    }

    pub fn cache_stats(&self) -> ShapeCacheStats {
        self.stats
    }

    pub fn reset_cache_stats(&mut self) {
        self.stats = ShapeCacheStats::default();
    }

    /// Shape the UTF-16 slice with bidi runs and per-glyph font fallback.
    pub fn shape(&mut self, text: &[u16], style: &TextStyle) -> Arc<ShapedText> {
        if text.is_empty() {
            return Arc::new(empty_shaped(style));
        }
        let key = ShapeKey {
            font_id: style.font.file.id,
            size_bits: style.size.to_bits(),
            scale_bits: style.scale_x.to_bits(),
            spacing_bits: style.letter_spacing.to_bits(),
            fake_bold: style.fake_bold,
            features: style.features.clone(),
            variations: effective_variations(&style.font, &style.variations)
                .into_iter()
                .map(|(tag, value)| (tag, value.to_bits()))
                .collect(),
            text: text.to_vec(),
        };
        if let Some(v) = self.cache.get(&key) {
            self.stats.hits += 1;
            return v.clone();
        }
        self.stats.misses += 1;
        let shaped = Arc::new(self.shape_uncached(text, style));
        if self.cache.len() >= self.cache_capacity {
            self.cache.clear();
        }
        self.cache.insert(key, shaped.clone());
        shaped
    }

    fn shape_uncached(&self, text: &[u16], style: &TextStyle) -> ShapedText {
        let (s, byte_to_utf16) = decode_utf16(text);
        let total_utf16 = text.len();

        // Bidi analysis. A single paragraph is enough for a text run; the line
        // breaker splits paragraphs before calling shape.
        let bidi = unicode_bidi::BidiInfo::new(&s, None);
        let mut visual_ranges: Vec<(usize, usize, bool)> = Vec::new();
        if let Some(para) = bidi.paragraphs.first() {
            let (_levels, runs) = bidi.visual_runs(para, para.range.clone());
            for run in runs {
                let level = bidi
                    .levels
                    .get(run.start)
                    .copied()
                    .unwrap_or_else(|| unicode_bidi::Level::ltr());
                visual_ranges.push((run.start, run.end, level.is_rtl()));
            }
        }
        if visual_ranges.is_empty() {
            visual_ranges.push((0, s.len(), false));
        }

        let chain = self.fonts.fallback_chain(style.family.as_deref());
        let mut runs: Vec<FontRun> = Vec::new();
        let mut fonts: Vec<FontRef> = Vec::new();
        let mut missing_glyphs = 0usize;

        for (byte_start, byte_end, rtl) in visual_ranges {
            // Break the bidi run into font runs by coverage.
            let mut seg_start = byte_start;
            let mut current_font: Option<FontRef> = None;
            let mut seg_chars: Vec<(usize, usize)> = Vec::new(); // (byte_start, byte_end)
            for (b, ch) in s[byte_start..byte_end].char_indices() {
                let abs = byte_start + b;
                let font = self.pick_font(&chain, ch, style);
                let same = match (&current_font, &font) {
                    (Some(a), Some(b)) => a.file.id == b.file.id && a.axes == b.axes,
                    (None, None) => true,
                    _ => false,
                };
                if !same && current_font.is_some() {
                    let _ = seg_start;
                    runs.push(self.shape_font_run(
                        &s,
                        &byte_to_utf16,
                        &seg_chars,
                        current_font.take().unwrap(),
                        rtl,
                        style,
                        &mut fonts,
                        &mut missing_glyphs,
                    ));
                    seg_chars.clear();
                }
                current_font = font;
                seg_chars.push((abs, abs + ch.len_utf8()));
                seg_start = abs;
            }
            if !seg_chars.is_empty() {
                if let Some(font) = current_font.take() {
                    runs.push(self.shape_font_run(
                        &s,
                        &byte_to_utf16,
                        &seg_chars,
                        font,
                        rtl,
                        style,
                        &mut fonts,
                        &mut missing_glyphs,
                    ));
                }
            }
        }

        let metrics = font_metrics_for(&style.font, style.size);
        let total_advance: f32 = runs.iter().map(|r| r.advance).sum();

        // Per-UTF-16-char advances.
        let mut char_advances = vec![0.0f32; total_utf16];
        for run in &runs {
            let mut last_cluster = None;
            for g in &run.glyphs {
                if g.cluster < char_advances.len() {
                    char_advances[g.cluster] += g.advance;
                    last_cluster = Some(g.cluster);
                }
            }
            let _ = last_cluster;
        }
        // Letter spacing is added between characters.
        if style.letter_spacing != 0.0 {
            for a in char_advances.iter_mut() {
                *a += style.letter_spacing;
            }
        }

        let rtl = runs.iter().any(|r| r.rtl);
        ShapedText {
            runs,
            fonts,
            char_advances,
            total_advance,
            metrics,
            rtl,
            missing_glyphs,
        }
    }

    fn pick_font(&self, chain: &[usize], ch: char, style: &TextStyle) -> Option<FontRef> {
        // Fast path: the primary font covers the char.
        if face_has_glyph(&style.font, ch) {
            return Some(style.font.clone());
        }
        self.fonts
            .font_for_char(chain, ch as u32, style.font.weight, style.font.italic)
            .or_else(|| Some(style.font.clone()))
    }

    #[allow(clippy::too_many_arguments)]
    fn shape_font_run(
        &self,
        s: &str,
        byte_to_utf16: &[usize],
        seg_chars: &[(usize, usize)],
        font: FontRef,
        rtl: bool,
        style: &TextStyle,
        fonts: &mut Vec<FontRef>,
        missing_glyphs: &mut usize,
    ) -> FontRun {
        let seg_start = seg_chars.first().map(|c| c.0).unwrap_or(0);
        let seg_end = seg_chars.last().map(|c| c.1).unwrap_or(seg_start);
        let start_u16 = byte_to_utf16.get(seg_start).copied().unwrap_or(0);
        let end_u16 = byte_to_utf16.get(seg_end).copied().unwrap_or(start_u16);
        let sub = &s[seg_start..seg_end];

        let slot = fonts
            .iter()
            .position(|f| f.file.id == font.file.id && f.axes == font.axes)
            .unwrap_or_else(|| {
                fonts.push(font.clone());
                fonts.len() - 1
            });

        let upem = face_units_per_em(&font);
        let scale = if upem > 0 {
            style.size / upem as f32
        } else {
            1.0
        } * style.scale_x;

        let mut glyphs = Vec::new();
        let mut advance_px = 0.0f32;

        let Some(mut rb_face) = rustybuzz::Face::from_slice(&font.file.data, font.file.face_index)
        else {
            return FontRun {
                font,
                start: start_u16,
                end: end_u16,
                rtl,
                glyphs,
                advance: 0.0,
            };
        };
        let effective = effective_variations(&font, &style.variations);
        let variations: Vec<rustybuzz::Variation> = effective
            .iter()
            .filter_map(|(tag, value)| tag_to_variation(tag, *value))
            .collect();
        if !variations.is_empty() {
            rb_face.set_variations(&variations);
        }

        let features: Vec<rustybuzz::Feature> = style
            .features
            .iter()
            .filter_map(|(tag, v)| {
                let bytes = tag.as_bytes();
                if bytes.len() == 4 {
                    Some(rustybuzz::Feature::new(
                        rustybuzz::ttf_parser::Tag::from_bytes(&[
                            bytes[0], bytes[1], bytes[2], bytes[3],
                        ]),
                        *v,
                        ..,
                    ))
                } else {
                    None
                }
            })
            .collect();

        let mut buffer = rustybuzz::UnicodeBuffer::new();
        buffer.push_str(sub);
        buffer.set_direction(if rtl {
            rustybuzz::Direction::RightToLeft
        } else {
            rustybuzz::Direction::LeftToRight
        });
        if let Some(script) = script_of(sub) {
            buffer.set_script(script);
        }
        let output = rustybuzz::shape(&rb_face, &features, buffer);
        let positions = output.glyph_positions().to_vec();
        let infos = output.glyph_infos().to_vec();
        for (info, pos) in infos.iter().zip(positions.iter()) {
            let cluster_byte = info.cluster as usize + seg_start;
            // Advance relative to the slice start (the slice is exactly the
            // requested UTF-16 range, so index 0 is the range start).
            let cluster = byte_to_utf16
                .get(cluster_byte)
                .copied()
                .unwrap_or(start_u16);
            let adv = pos.x_advance as f32 * scale;
            if info.glyph_id == 0 {
                *missing_glyphs += 1;
            }
            glyphs.push(ShapedGlyph {
                glyph_id: info.glyph_id as u16,
                cluster,
                advance: adv,
                x_offset: pos.x_offset as f32 * scale,
                y_offset: pos.y_offset as f32 * scale,
                font_slot: slot,
            });
            advance_px += adv;
        }

        FontRun {
            font,
            start: start_u16,
            end: end_u16,
            rtl,
            glyphs,
            advance: advance_px,
        }
    }

    /// Android `Paint.getFontMetrics`.
    pub fn font_metrics(&self, style: &TextStyle) -> FontMetrics {
        font_metrics_for(&style.font, style.size)
    }

    pub fn font_metrics_int(&self, style: &TextStyle) -> FontMetricsInt {
        self.font_metrics(style).into()
    }

    /// Android `Paint.measureText`.
    pub fn measure_text(&mut self, text: &[u16], style: &TextStyle) -> f32 {
        self.shape(text, style).total_advance
    }

    /// Android `Paint.getTextRunAdvances`: writes per-char advances and returns
    /// the total.
    pub fn get_text_run_advances(
        &mut self,
        text: &[u16],
        style: &TextStyle,
        out: &mut [f32],
    ) -> f32 {
        let shaped = self.shape(text, style);
        for (i, slot) in out.iter_mut().enumerate().take(shaped.char_advances.len()) {
            *slot = shaped.char_advances[i];
        }
        shaped.total_advance
    }

    /// Android `Paint.getTextBounds`.
    pub fn text_bounds(&mut self, text: &[u16], style: &TextStyle) -> Option<RectF> {
        let shaped = self.shape(text, style);
        if text.is_empty() {
            return None;
        }
        let mut min_x = f32::INFINITY;
        let mut min_y = f32::INFINITY;
        let mut max_x = f32::NEG_INFINITY;
        let mut max_y = f32::NEG_INFINITY;
        let mut any = false;
        // Pen positions are computed in visual order.
        for run in &shaped.runs {
            let upem = face_units_per_em(&run.font);
            let scale = if upem > 0 {
                style.size / upem as f32
            } else {
                1.0
            };
            let face = parse_face(&run.font);
            let mut pen = 0.0f32;
            for g in &run.glyphs {
                if let Some(bbox) = face.glyph_bounding_box(ttf_parser::GlyphId(g.glyph_id)) {
                    let x0 = pen + g.x_offset + bbox.x_min as f32 * scale;
                    let x1 = pen + g.x_offset + bbox.x_max as f32 * scale;
                    let y0 = g.y_offset - bbox.y_max as f32 * scale;
                    let y1 = g.y_offset - bbox.y_min as f32 * scale;
                    min_x = min_x.min(x0);
                    min_y = min_y.min(y0);
                    max_x = max_x.max(x1);
                    max_y = max_y.max(y1);
                    any = true;
                }
                pen += g.advance;
            }
        }
        if any {
            Some(RectF::new(min_x, min_y, max_x, max_y))
        } else {
            None
        }
    }

    /// Android `Paint.breakText(..., maxWidth, measuredWidth)` — greedy.
    /// Returns the number of UTF-16 units that fit.
    pub fn break_text(&mut self, text: &[u16], style: &TextStyle, max_width: f32) -> f32 {
        let shaped = self.shape(text, style);
        let mut acc = 0.0f32;
        let mut count = 0usize;
        for a in &shaped.char_advances {
            if acc + a > max_width {
                break;
            }
            acc += a;
            count += 1;
        }
        let _ = count;
        acc
    }

    /// Android `Paint.getRunAdvance`: advance from `start` to `index`.
    pub fn get_run_advance(&mut self, text: &[u16], style: &TextStyle, index: usize) -> f32 {
        let shaped = self.shape(text, style);
        shaped.char_advances.iter().take(index).sum::<f32>()
    }

    /// Android `Paint.getOffsetForAdvance` — the char boundary closest to an
    /// advance.
    pub fn get_offset_for_advance(
        &mut self,
        text: &[u16],
        style: &TextStyle,
        advance: f32,
    ) -> usize {
        let shaped = self.shape(text, style);
        let mut acc = 0.0f32;
        for (i, a) in shaped.char_advances.iter().enumerate() {
            if acc + a * 0.5 > advance {
                return i;
            }
            acc += a;
        }
        shaped.char_advances.len()
    }

    /// Android `Paint.hasGlyph`.
    pub fn has_glyph(&self, ch: char, style: &TextStyle) -> bool {
        if face_has_glyph(&style.font, ch) {
            return true;
        }
        let chain = self.fonts.fallback_chain(style.family.as_deref());
        self.fonts
            .font_for_char(&chain, ch as u32, style.font.weight, style.font.italic)
            .is_some()
    }

    /// Android `Paint.getTextPath`.
    pub fn text_path(
        &mut self,
        text: &[u16],
        x: f32,
        y: f32,
        style: &TextStyle,
    ) -> crate::path::Path {
        let shaped = self.shape(text, style);
        let mut out = crate::path::Path::new();
        // Alignment is applied by the caller (it owns the paint).
        let mut pen_x = x;
        for run in &shaped.runs {
            let upem = face_units_per_em(&run.font);
            let scale = if upem > 0 {
                style.size / upem as f32
            } else {
                1.0
            };
            let face = parse_face(&run.font);
            for g in &run.glyphs {
                append_glyph(
                    &face,
                    g.glyph_id,
                    pen_x + g.x_offset,
                    y + g.y_offset - style.skew_x * pen_x,
                    scale,
                    &mut out,
                );
                pen_x += g.advance;
            }
        }
        out
    }

    /// Font collection accessor.
    pub fn fonts(&self) -> &FontCollection {
        &self.fonts
    }
}

fn face_units_per_em(font: &FontRef) -> u16 {
    ttf_parser::Face::parse(&font.file.data, font.file.face_index)
        .map(|f| f.units_per_em())
        .unwrap_or(1000)
}

fn parse_face(font: &FontRef) -> ttf_parser::Face<'_> {
    ttf_parser::Face::parse(&font.file.data, font.file.face_index).expect("font validated at load")
}

fn face_has_glyph(font: &FontRef, ch: char) -> bool {
    parse_face(font).glyph_index(ch).is_some()
}

/// `SkFontMetrics` for `font` at `size` px, matching Skia's FreeType backend
/// (`SkFontHost_FreeType.cpp`, `generateFontMetrics`): `ascent`/`descent` come
/// from `hhea`, `top`/`bottom` from the face bounding box (`head` yMax/yMin),
/// and `leading` from the hhea line gap (clamped to 0).
///
/// `top`/`bottom` are what `TextView`/`BoringLayout` use for a `wrap_content`
/// height when `includeFontPadding` is on (the default), so this is the pixel
/// difference W179-A observed (`count` 64 px vs ART 51 px) at 14sp.
fn font_metrics_for(font: &FontRef, size: f32) -> FontMetrics {
    let face = parse_face(font);
    let upem = face.units_per_em() as f32;
    let scale = if upem > 0.0 { size / upem } else { 1.0 };
    let ascent = -face.ascender() as f32 * scale;
    let descent = -face.descender() as f32 * scale;
    let bbox = face.global_bounding_box();
    let top = -bbox.y_max as f32 * scale;
    let bottom = -bbox.y_min as f32 * scale;
    let leading = (face.line_gap() as f32 * scale).max(0.0);
    FontMetrics {
        top,
        ascent,
        descent,
        bottom,
        leading,
    }
}

fn script_of(s: &str) -> Option<rustybuzz::Script> {
    let ch = s.chars().next()?;
    let script = unicode_script::UnicodeScript::script(&ch);
    rustybuzz::Script::from_str(script.short_name()).ok()
}

fn tag_to_variation(tag: &str, value: f32) -> Option<rustybuzz::Variation> {
    let b = tag.as_bytes();
    if b.len() != 4 {
        return None;
    }
    Some(rustybuzz::Variation {
        tag: rustybuzz::ttf_parser::Tag::from_bytes(&[b[0], b[1], b[2], b[3]]),
        value,
    })
}

fn empty_shaped(style: &TextStyle) -> ShapedText {
    ShapedText {
        runs: Vec::new(),
        fonts: Vec::new(),
        char_advances: Vec::new(),
        total_advance: 0.0,
        metrics: font_metrics_for(&style.font, style.size),
        rtl: false,
        missing_glyphs: 0,
    }
}

/// Decode a UTF-16 slice to a `String` plus a byte→UTF-16-index map.
fn decode_utf16(text: &[u16]) -> (String, Vec<usize>) {
    let mut s = String::with_capacity(text.len());
    // `map` is indexed by UTF-8 *byte* offset, so it must grow with the
    // encoded string (which can be longer than the UTF-16 input).
    let mut map: Vec<usize> = Vec::new();
    let mut i = 0usize;
    while i < text.len() {
        let unit = text[i];
        let (ch, consumed) = if (0xD800..0xDC00).contains(&unit)
            && i + 1 < text.len()
            && (0xDC00..0xE000).contains(&text[i + 1])
        {
            let low = text[i + 1];
            let cp = 0x10000 + (((unit as u32 - 0xD800) << 10) | (low as u32 - 0xDC00));
            (char::from_u32(cp).unwrap_or('\u{FFFD}'), 2)
        } else {
            (char::from_u32(unit as u32).unwrap_or('\u{FFFD}'), 1)
        };
        s.push(ch);
        for _ in 0..ch.len_utf8() {
            map.push(i);
        }
        i += consumed;
    }
    map.push(text.len());
    (s, map)
}

/// Append a glyph outline to `out`, scaled and offset.
pub fn append_glyph(
    face: &ttf_parser::Face,
    glyph_id: u16,
    x: f32,
    y: f32,
    scale: f32,
    out: &mut crate::path::Path,
) {
    struct Builder<'a> {
        path: &'a mut crate::path::Path,
        x: f32,
        y: f32,
        scale: f32,
    }
    impl ttf_parser::OutlineBuilder for Builder<'_> {
        fn move_to(&mut self, x: f32, y: f32) {
            self.path
                .move_to(self.x + x * self.scale, self.y - y * self.scale);
        }
        fn line_to(&mut self, x: f32, y: f32) {
            self.path
                .line_to(self.x + x * self.scale, self.y - y * self.scale);
        }
        fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
            self.path.quad_to(
                self.x + x1 * self.scale,
                self.y - y1 * self.scale,
                self.x + x * self.scale,
                self.y - y * self.scale,
            );
        }
        fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
            self.path.cubic_to(
                self.x + x1 * self.scale,
                self.y - y1 * self.scale,
                self.x + x2 * self.scale,
                self.y - y2 * self.scale,
                self.x + x * self.scale,
                self.y - y * self.scale,
            );
        }
        fn close(&mut self) {
            self.path.close();
        }
    }
    let mut b = Builder {
        path: out,
        x,
        y,
        scale,
    };
    let _ = face.outline_glyph(ttf_parser::GlyphId(glyph_id), &mut b);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine_or_skip() -> Option<TextEngine> {
        let fc = FontCollection::load()?;
        Some(TextEngine::new(Arc::new(fc)))
    }

    fn utf16(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    fn style(e: &TextEngine) -> TextStyle {
        let font = e
            .fonts
            .resolve_typeface(None, 400, false)
            .expect("default font");
        TextStyle {
            font,
            family: None,
            size: 16.0,
            scale_x: 1.0,
            skew_x: 0.0,
            letter_spacing: 0.0,
            fake_bold: false,
            features: Vec::new(),
            variations: Vec::new(),
        }
    }

    /// `GraphicsJNI::set_metrics_int`: `top` floors, `bottom` ceils, and
    /// `ascent`/`descent`/`leading` round half up. Pins the conversion itself
    /// so a species change in one field is caught without needing fonts.
    #[test]
    fn font_metrics_int_matches_graphics_jni_rounding() {
        let m = FontMetrics {
            top: -38.8136,
            ascent: -34.09424,
            descent: 8.972168,
            bottom: 9.959106,
            leading: 0.0,
        };
        let i = FontMetricsInt::from(m);
        assert_eq!(i.top, -39, "top floors");
        assert_eq!(i.ascent, -34, "ascent rounds half up");
        assert_eq!(i.descent, 9, "descent rounds half up");
        assert_eq!(i.bottom, 10, "bottom ceils");
        assert_eq!(i.leading, 0);
        assert_eq!(i.bottom - i.top, 49);
    }

    /// Ground truth from a real `art-api34-x86_64` emulator (1080x2400 @420):
    /// `Paint.FontMetricsInt` at 14sp (36.75 px) is
    /// `top=-39 asc=-34 desc=9 bot=10`, so a `wrap_content` TextView is 49 px;
    /// the `Widget.DeviceDefault.TextView` default text size resolves to the
    /// rounded 37 px, whose `top=-40 bot=11` gives the ART reference's 51 px.
    #[test]
    fn skia_metrics_match_art_api34_emulator() {
        let Some(e) = acos_test_support::require_some(engine_or_skip(), "skia_metrics_match_art_api34_emulator", "no fonts available") else {
            return;
        };
        // The host font set is not necessarily Roboto; only assert on Roboto.
        let at = |size: f32| {
            let mut st = style(&e);
            st.size = size;
            e.font_metrics_int(&st)
        };
        let check = at(36.75);
        if !(check.ascent == -34 && check.descent == 9) {
            acos_test_support::require_prereq(false, "skia_metrics_match_art_api34_emulator", &format!("default font is not the ART Roboto ({check:?})"));
            return;
        }
        assert_eq!(check.top, -39, "14sp/36.75px top");
        assert_eq!(check.bottom, 10, "14sp/36.75px bottom");
        assert_eq!(check.bottom - check.top, 49, "14sp wrap_content height");

        let check37 = at(37.0);
        assert_eq!(check37.top, -40, "theme default 37px top");
        assert_eq!(check37.ascent, -34, "theme default 37px ascent");
        assert_eq!(check37.descent, 9, "theme default 37px descent");
        assert_eq!(check37.bottom, 11, "theme default 37px bottom");
        assert_eq!(
            check37.bottom - check37.top,
            51,
            "ART counter `count` wrap_content height"
        );
    }

    #[test]
    fn advances_sum_equals_measure_text() {
        let Some(mut e) = acos_test_support::require_some(engine_or_skip(), "advances_sum_equals_measure_text", "no fonts available") else {
            return;
        };
        let st = style(&e);
        let text = utf16("Hello ACOS");
        let mut advances = vec![0.0f32; text.len()];
        let total = e.get_text_run_advances(&text, &st, &mut advances);
        let sum: f32 = advances.iter().sum();
        let measured = e.measure_text(&text, &st);
        assert!(
            (sum - measured).abs() < 0.01,
            "sum={sum} measured={measured}"
        );
        assert!((total - measured).abs() < 0.01);
    }

    #[test]
    fn non_ascii_text_shapes_without_panic() {
        let Some(mut e) = acos_test_support::require_some(engine_or_skip(), "non_ascii_text_shapes_without_panic", "no fonts available") else {
            return;
        };
        let st = style(&e);
        // Em dash (3 UTF-8 bytes / 1 UTF-16 unit), accented latin and CJK.
        let text = utf16("Hello — café 漢字 مرحبا");
        let mut advances = vec![0.0f32; text.len()];
        let total = e.get_text_run_advances(&text, &st, &mut advances);
        assert!(total > 0.0);
        assert!((advances.iter().sum::<f32>() - total).abs() < 0.01);
        let p = e.text_path(&text, 0.0, 0.0, &st);
        assert!(!p.is_empty());
    }

    #[test]
    fn shaping_cache_reports_hit_rate() {
        let Some(mut e) = acos_test_support::require_some(engine_or_skip(), "shaping_cache_reports_hit_rate", "no fonts available") else {
            return;
        };
        let st = style(&e);
        let text = utf16("repeat me");
        for _ in 0..10 {
            e.shape(&text, &st);
        }
        let stats = e.cache_stats();
        assert!(stats.hits >= 9, "{stats:?}");
        assert!(stats.hit_rate() > 0.8);
    }

    #[test]
    fn empty_text_is_zero_width() {
        let Some(mut e) = acos_test_support::require_some(engine_or_skip(), "empty_text_is_zero_width", "no fonts available") else {
            return;
        };
        let st = style(&e);
        assert_eq!(e.measure_text(&[], &st), 0.0);
    }

    #[test]
    fn font_metrics_have_positive_descent() {
        let Some(e) = acos_test_support::require_some(engine_or_skip(), "font_metrics_have_positive_descent", "no fonts available") else {
            return;
        };
        let st = style(&e);
        let m = e.font_metrics(&st);
        assert!(m.ascent <= 0.0, "{m:?}");
        assert!(m.descent >= 0.0, "{m:?}");
    }

    #[test]
    fn kerning_is_applied_for_av() {
        let Some(mut e) = acos_test_support::require_some(engine_or_skip(), "kerning_is_applied_for_av", "no fonts available") else {
            return;
        };
        let st = style(&e);
        let av = e.measure_text(&utf16("AV"), &st);
        let a = e.measure_text(&utf16("A"), &st);
        let v = e.measure_text(&utf16("V"), &st);
        assert!(av < a + v + 0.001, "AV={av} A+V={}", a + v);
    }

    #[test]
    fn text_path_is_non_empty() {
        let Some(mut e) = acos_test_support::require_some(engine_or_skip(), "text_path_is_non_empty", "no fonts available") else {
            return;
        };
        let st = style(&e);
        let p = e.text_path(&utf16("Hi"), 0.0, 0.0, &st);
        assert!(!p.is_empty());
    }
}
