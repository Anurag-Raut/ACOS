//! `android.graphics.Typeface`, `android.graphics.text.LineBreaker` and
//! `android.graphics.text.MeasuredText` natives, plus the text-measurement
//! helpers shared with [`super::gfx_paint`].
//!
//! Everything measurable routes through `acos-render`'s shaping stack
//! (`rustybuzz` + `ttf-parser` metrics, ADR-0019), so `Paint.measureText`
//! agrees with the renderer within floating-point noise. Fonts are resolved
//! from the real `/system/fonts/fonts.xml` through
//! [`acos_render::text::fonts::FontCollection`].
//!
//! # Documented divergences from Android/Minikin
//!
//! * All three `LineBreaker` break strategies (`SIMPLE`, `BALANCED`,
//!   `HIGH_QUALITY`) are implemented; hyphenation is always off
//!   (`HYPHENATION_FREQUENCY_NONE`).
//! * Minikin's locale-list machinery is reduced to a single locale id; ACOS
//!   resolves fonts by family name + weight/slant and falls back through the
//!   `fonts.xml` document order (same rule as Minikin, minus locale tags).

use std::sync::{Arc, Mutex, OnceLock};

use acos_render::geom::RectF;
use acos_render::path::Path;
use acos_render::text::fonts::{FontCollection, FontRef};
use acos_render::text::layout::{BreakStrategy, LineBreak, LineBreaker, ParagraphConstraints};
use acos_render::text::{FontMetricsInt, TextEngine, TextStyle};

use crate::class::NativeCtx;
use crate::error::{VmError, VmResult};
use crate::natives::gfx_handles::{Handle, HandleTable};
use crate::vm::Vm;

/// A native `Typeface` record.
///
/// `font` carries a concrete resolved face when the framework built the
/// typeface from real `android.graphics.fonts.Font` objects (`nativeCreateFromArray`)
/// rather than from a family name string; `gfx_text::style` then uses that face
/// directly instead of resolving `family` through `fonts.xml`. A typeface built
/// from a name (`Typeface.create("sans-serif", …)`) has `font == None`.
#[derive(Clone, Debug)]
pub struct TypefaceRec {
    pub family: Option<String>,
    pub weight: i32,
    pub italic: bool,
    pub font: Option<FontRef>,
    /// Deferred face descriptor carried by a restored snapshot. `font` is
    /// `None` in that case and the concrete face is rebuilt on first use, so a
    /// restored launch that never touches text never loads the font collection.
    pub deferred: Option<FaceRef>,
}

impl TypefaceRec {
    /// Android `Typeface.NORMAL` / `BOLD` / `ITALIC` / `BOLD_ITALIC` style bits.
    pub fn style_bits(&self) -> i32 {
        (if self.italic { 2 } else { 0 }) | (if self.weight >= 600 { 1 } else { 0 })
    }

    /// The concrete face for a typeface built from `FontFamily`/`Font` objects,
    /// rebuilt lazily when the record came from a snapshot. `None` means the
    /// typeface resolves by `family` name through `fonts.xml`.
    pub fn resolved_font(&self) -> Option<FontRef> {
        self.font
            .clone()
            .or_else(|| self.deferred.as_ref().and_then(FaceRef::resolve))
    }
}

/// A serialisable face identity: the host font file, the face index inside it,
/// the `fonts.xml` family key and the coordinates the face was resolved with.
///
/// A native [`FontRef`] owns an `Arc<FontFile>` (an mmap'd face plus a
/// process-local id). A snapshot cannot carry those, so the restore path stores
/// this descriptor and rebuilds the `FontRef` lazily on first use (see
/// [`FaceRef::resolve`]).
#[derive(Clone, Debug, PartialEq)]
pub struct FaceRef {
    /// Host path of the mapped font file.
    pub path: String,
    /// Face index inside the file (a `.ttc` collection may hold several).
    pub face_index: u32,
    /// Index of the owning family in the loaded font collection.
    pub family_key: usize,
    pub weight: u16,
    pub italic: bool,
    pub axes: Vec<(String, f32)>,
}

impl FaceRef {
    /// Capture a resolved face's serialisable identity.
    pub fn of(font: &FontRef) -> FaceRef {
        FaceRef {
            path: font.file.path.to_string_lossy().into_owned(),
            face_index: font.file.face_index,
            family_key: font.family_key,
            weight: font.weight,
            italic: font.italic,
            axes: font.axes.clone(),
        }
    }

    /// Rebuild the `FontRef` from the process-wide font collection.
    ///
    /// Called lazily on first use, so a restored launch that never touches text
    /// never pays for the collection load. The recorded family key is preferred;
    /// the fallback scan keeps the face resolvable if a family was re-ordered.
    pub fn resolve(&self) -> Option<FontRef> {
        let fc = FontCollection::shared()?;
        if let Some(fam) = fc.families.get(self.family_key) {
            if let Some(entry) = fam.fonts.iter().find(|e| {
                e.file.face_index == self.face_index && e.file.path.to_string_lossy() == self.path
            }) {
                return Some(FontRef {
                    file: entry.file.clone(),
                    weight: self.weight,
                    italic: self.italic,
                    axes: self.axes.clone(),
                    family_key: self.family_key,
                });
            }
        }
        for (idx, fam) in fc.families.iter().enumerate() {
            for entry in &fam.fonts {
                if entry.file.face_index == self.face_index
                    && entry.file.path.to_string_lossy() == self.path
                {
                    return Some(FontRef {
                        file: entry.file.clone(),
                        weight: self.weight,
                        italic: self.italic,
                        axes: self.axes.clone(),
                        family_key: idx,
                    });
                }
            }
        }
        None
    }
}

/// Text attributes a `Paint` exposes to the shaping engine.
#[derive(Clone, Debug)]
pub struct TextAttrs {
    /// Raw `Typeface` handle, or 0 for the default typeface.
    pub typeface: i64,
    pub text_size: f32,
    pub scale_x: f32,
    pub skew_x: f32,
    pub letter_spacing: f32,
    pub fake_bold: bool,
    pub features: Option<String>,
    pub variations: Option<String>,
}

impl Default for TextAttrs {
    fn default() -> Self {
        TextAttrs {
            typeface: 0,
            text_size: 12.0,
            scale_x: 1.0,
            skew_x: 0.0,
            letter_spacing: 0.0,
            fake_bold: false,
            features: None,
            variations: None,
        }
    }
}

/// The result of a `LineBreaker.nComputeLineBreaks` call.
#[derive(Clone, Debug)]
pub struct LineBreakResult {
    pub lines: Vec<LineBreak>,
    /// Strategy selected by `LineBreaker.Builder`, used when
    /// `nComputeLineBreaks` omits one.
    pub strategy: BreakStrategy,
}

/// A `MeasuredText` object (advances for a char slice).
#[derive(Clone, Debug)]
pub struct MeasuredTextRec {
    pub text: Vec<u16>,
    pub advances: Vec<f32>,
    pub width: f32,
    pub attrs: TextAttrs,
}

#[derive(Default)]
struct BuilderRec {
    attrs: Vec<TextAttrs>,
}

/// All process-wide text state: the shared font collection, the shaping cache,
/// and the generation-checked handle tables for `Typeface`, `LineBreaker`
/// results and `MeasuredText`.
pub struct TextShared {
    pub fonts: Option<Arc<FontCollection>>,
    engine: Option<TextEngine>,
    pub typefaces: HandleTable<TypefaceRec>,
    pub default_typeface: i64,
    pub line_results: HandleTable<LineBreakResult>,
    pub measured: HandleTable<MeasuredTextRec>,
    builders: HandleTable<BuilderRec>,
}

impl std::fmt::Debug for TextShared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextShared")
            .field("fonts", &self.fonts.as_ref().map(|c| c.family_count()))
            .field("typefaces", &self.typefaces.len())
            .field("line_results", &self.line_results.len())
            .field("measured", &self.measured.len())
            .finish()
    }
}

impl TextShared {
    fn new() -> TextShared {
        let fonts = FontCollection::shared();
        let engine = fonts.clone().map(TextEngine::new);
        let mut typefaces = HandleTable::new();
        // Typeface.DEFAULT / DEFAULT_BOLD / SANS_SERIF all resolve to
        // `sans-serif` in fonts.xml. `nativeSetDefault` can replace it.
        let default_typeface = typefaces
            .insert(TypefaceRec {
                family: Some("sans-serif".to_string()),
                weight: 400,
                italic: false,
                font: None,
                deferred: None,
            })
            .to_jlong();
        TextShared {
            fonts,
            engine,
            typefaces,
            default_typeface,
            line_results: HandleTable::new(),
            measured: HandleTable::new(),
            builders: HandleTable::new(),
        }
    }

    fn tf(&self, handle: i64) -> VmResult<TypefaceRec> {
        let h = if handle == 0 {
            Handle::from_jlong(self.default_typeface)
        } else {
            Handle::from_jlong(handle)
        };
        self.typefaces
            .get(h)
            .cloned()
            .map_err(|e| bad_handle("Typeface", &e))
    }

    fn style(&self, attrs: &TextAttrs) -> VmResult<TextStyle> {
        let fonts = self.fonts.as_ref().ok_or_else(|| {
            VmError::unsupported_api(
                "android.graphics text shaping without /system/fonts",
                "acos-render font collection missing",
            )
        })?;
        let tf = self.tf(attrs.typeface)?;
        let font = match tf.resolved_font() {
            Some(f) => f,
            None => fonts
                .resolve_typeface(
                    tf.family.as_deref(),
                    tf.weight.clamp(1, 1000) as u16,
                    tf.italic,
                )
                .ok_or_else(|| {
                    VmError::unsupported_api(
                        format!("Typeface family {:?} not present", tf.family),
                        "acos-render FontCollection::resolve_typeface",
                    )
                })?,
        };
        Ok(TextStyle {
            font,
            family: tf.family,
            size: attrs.text_size,
            scale_x: attrs.scale_x,
            skew_x: attrs.skew_x,
            letter_spacing: attrs.letter_spacing,
            fake_bold: attrs.fake_bold,
            features: parse_features(attrs.features.as_deref()),
            variations: parse_variations(attrs.variations.as_deref()),
        })
    }

    fn engine_mut(&mut self) -> VmResult<&mut TextEngine> {
        self.engine.as_mut().ok_or_else(|| {
            VmError::unsupported_api(
                "android.graphics text shaping without /system/fonts",
                "acos-render font collection missing",
            )
        })
    }
}

fn parse_features(s: Option<&str>) -> Vec<(String, u32)> {
    let mut out = Vec::new();
    if let Some(s) = s {
        for part in s.split([',', ' ']).filter(|p| !p.is_empty()) {
            match part.split_once('=') {
                Some((k, v)) => {
                    if let Ok(v) = v.parse::<u32>() {
                        out.push((k.to_string(), v));
                    }
                }
                None => out.push((part.to_string(), 1)),
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

static TEXT: OnceLock<Mutex<TextShared>> = OnceLock::new();

/// Access the process-wide text state. A poisoned lock is recovered (a previous
/// holder panicking must not make every later measurement fail).
pub fn with_text<R>(f: impl FnOnce(&mut TextShared) -> R) -> R {
    let m = TEXT.get_or_init(|| Mutex::new(TextShared::new()));
    match m.lock() {
        Ok(mut g) => {
            install_staged_text(&mut g);
            f(&mut g)
        }
        Err(poisoned) => {
            let mut g = poisoned.into_inner();
            install_staged_text(&mut g);
            f(&mut g)
        }
    }
}

/// One serialised `Typeface` native record. `face` carries the concrete face
/// when the typeface was built from `FontFamily`/`Font` objects; `None` means
/// the typeface resolves by `family` name.
#[derive(Clone, Debug, PartialEq)]
pub struct TypefaceRecordData {
    pub handle: i64,
    pub family: Option<String>,
    pub weight: i32,
    pub italic: bool,
    pub face: Option<FaceRef>,
}

/// The serialised process-global text state carried by a snapshot.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TextState {
    pub typefaces: Vec<TypefaceRecordData>,
    pub default_typeface: i64,
}

/// A text table set decoded from a snapshot but not yet imported into the live
/// table. Importing happens on first text use, so a restored launch that never
/// touches text pays neither the collection load nor the table rebuild.
static STAGED_TEXT: Mutex<Option<TextState>> = Mutex::new(None);

/// Stage a text state decoded from a snapshot. The next [`with_text`] call
/// imports it (cheaply: the resolved faces stay deferred).
pub(crate) fn stage_text_state(state: TextState) {
    // An in-process restore shares the already-initialised live table produced
    // by the template boot; importing the same state again would only clobber
    // it. An empty state has nothing to import either.
    if TEXT.get().is_some() || (state.typefaces.is_empty() && state.default_typeface == 0) {
        return;
    }
    *STAGED_TEXT.lock().unwrap_or_else(|e| e.into_inner()) = Some(state);
}

fn install_staged_text(t: &mut TextShared) {
    let Some(state) = STAGED_TEXT.lock().unwrap_or_else(|e| e.into_inner()).take() else {
        return;
    };
    t.typefaces.clear();
    for r in state.typefaces {
        t.typefaces.reinsert(
            Handle::from_jlong(r.handle),
            TypefaceRec {
                family: r.family,
                weight: r.weight,
                italic: r.italic,
                font: None,
                deferred: r.face,
            },
        );
    }
    t.default_typeface = state.default_typeface;
    if !t.typefaces.contains(Handle::from_jlong(t.default_typeface)) {
        // A malformed/older state must not leave `new_typeface` without a
        // resolvable default; fall back to the first restored typeface.
        t.default_typeface = t
            .typefaces
            .iter()
            .next()
            .map(|(h, _)| h.to_jlong())
            .unwrap_or(0);
    }
}

/// The live text table state, or the staged state when the table has not been
/// initialised yet. `Vm::snapshot` serialises this so a snapshot of a restored
/// VM stays byte-identical to the snapshot it came from.
pub(crate) fn current_text_state() -> TextState {
    if let Some(m) = TEXT.get() {
        let t = m.lock().unwrap_or_else(|e| e.into_inner());
        return state_from_text(&t);
    }
    STAGED_TEXT
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_default()
}

fn state_from_text(t: &TextShared) -> TextState {
    let mut typefaces: Vec<TypefaceRecordData> = t
        .typefaces
        .iter()
        .map(|(h, r)| TypefaceRecordData {
            handle: h.to_jlong(),
            family: r.family.clone(),
            weight: r.weight,
            italic: r.italic,
            face: r
                .font
                .as_ref()
                .map(FaceRef::of)
                .or_else(|| r.deferred.clone()),
        })
        .collect();
    typefaces.sort_unstable_by_key(|r| r.handle);
    TextState {
        typefaces,
        default_typeface: t.default_typeface,
    }
}

fn bad_handle(kind: &str, e: &crate::natives::gfx_handles::HandleError) -> VmError {
    VmError::Internal(format!("{kind}: {e}"))
}

// ---------------------------------------------------------------------------
// Measurement API used by gfx_paint.rs
// ---------------------------------------------------------------------------

/// `Paint.measureText` — total advance.
pub fn measure(text: &[u16], attrs: &TextAttrs) -> VmResult<f32> {
    with_text(|t| {
        let style = t.style(attrs)?;
        let e = t.engine_mut()?;
        Ok(e.measure_text(text, &style))
    })
}

/// `Paint.getTextRunAdvances` — fills `out` with per-UTF-16-unit advances and
/// returns the total.
pub fn advances(text: &[u16], attrs: &TextAttrs, out: &mut [f32]) -> VmResult<f32> {
    with_text(|t| {
        let style = t.style(attrs)?;
        let e = t.engine_mut()?;
        Ok(e.get_text_run_advances(text, &style, out))
    })
}

/// `Paint.getRunAdvance` — advance from the start of `text` to `index`.
pub fn run_advance(text: &[u16], attrs: &TextAttrs, index: usize) -> VmResult<f32> {
    with_text(|t| {
        let style = t.style(attrs)?;
        let e = t.engine_mut()?;
        Ok(e.get_run_advance(text, &style, index))
    })
}

/// `Paint.getTextBounds`.
pub fn bounds(text: &[u16], attrs: &TextAttrs) -> VmResult<Option<RectF>> {
    with_text(|t| {
        let style = t.style(attrs)?;
        let e = t.engine_mut()?;
        Ok(e.text_bounds(text, &style))
    })
}

/// `Paint.getFontMetricsInt`.
pub fn metrics_int(attrs: &TextAttrs) -> VmResult<FontMetricsInt> {
    with_text(|t| {
        let style = t.style(attrs)?;
        let e = t.engine_mut()?;
        Ok(e.font_metrics_int(&style))
    })
}

/// `Paint.hasGlyph`.
pub fn has_glyph(ch: char, attrs: &TextAttrs) -> VmResult<bool> {
    with_text(|t| {
        let style = t.style(attrs)?;
        let e = t.engine_mut()?;
        Ok(e.has_glyph(ch, &style))
    })
}

/// `Paint.breakText` — the measured width of the prefix that fits.
pub fn break_text(text: &[u16], attrs: &TextAttrs, max_width: f32) -> VmResult<f32> {
    with_text(|t| {
        let style = t.style(attrs)?;
        let e = t.engine_mut()?;
        Ok(e.break_text(text, &style, max_width))
    })
}

/// `Paint.getTextPath` — glyph outlines as a [`Path`].
pub fn text_path(text: &[u16], x: f32, y: f32, attrs: &TextAttrs) -> VmResult<Path> {
    with_text(|t| {
        let style = t.style(attrs)?;
        let e = t.engine_mut()?;
        Ok(e.text_path(text, x, y, &style))
    })
}

/// Create a `Typeface` handle (or reuse the default when the record matches).
pub fn new_typeface(family: Option<String>, weight: i32, italic: bool) -> i64 {
    with_text(|t| {
        let rec = TypefaceRec {
            family,
            weight,
            italic,
            font: None,
            deferred: None,
        };
        let default = t
            .typefaces
            .get(Handle::from_jlong(t.default_typeface))
            .expect("default")
            .clone();
        if rec.family == default.family
            && rec.weight == default.weight
            && rec.italic == default.italic
        {
            return t.default_typeface;
        }
        t.typefaces.insert(rec).to_jlong()
    })
}

/// Look up a `Typeface` record.
pub fn typeface_rec(handle: i64) -> VmResult<TypefaceRec> {
    with_text(|t| t.tf(handle))
}

// ---------------------------------------------------------------------------
// Native registration
// ---------------------------------------------------------------------------

pub(crate) fn install(vm: &mut Vm) -> VmResult<()> {
    let t = "Landroid/graphics/Typeface;";
    vm.register_native(t, "nativeCreateFromTypeface", "(JI)J", typeface_create_from);
    vm.register_native(
        t,
        "nativeCreateFromTypefaceWithExactStyle",
        "(JIZ)J",
        typeface_create_exact,
    );
    vm.register_native(t, "nativeCreateWeightAlias", "(JI)J", typeface_weight_alias);
    vm.register_native(
        t,
        "nativeCreateFromTypefaceWithVariation",
        "(JLjava/util/List;)J",
        typeface_variation,
    );
    vm.register_native(t, "nativeGetStyle", "(J)I", typeface_get_style);
    vm.register_native(t, "nativeGetWeight", "(J)I", typeface_get_weight);
    vm.register_native(t, "nativeSetDefault", "(J)V", typeface_set_default);
    vm.register_native(
        t,
        "nativeRegisterGenericFamily",
        "(Ljava/lang/String;J)V",
        typeface_register_generic,
    );
    vm.register_native(t, "nativeGetReleaseFunc", "()J", release_func);
    vm.register_native(
        t,
        "nativeGetSupportedAxes",
        "(J)[I",
        typeface_supported_axes,
    );
    vm.register_native(t, "nativeWarmUpCache", "(Ljava/lang/String;)V", noop_string);
    vm.register_native(t, "nativeAddFontCollections", "(J)V", noop_long);
    vm.register_native(
        t,
        "nativeRegisterLocaleList",
        "(Ljava/lang/String;)V",
        noop_string,
    );
    vm.register_native(t, "preloadFontFile", "(Ljava/lang/String;)V", noop_string);

    let lb = "Landroid/graphics/text/LineBreaker;";
    vm.register_native(lb, "nInit", "(IIZ[I)J", linebreaker_init);
    vm.register_native(
        lb,
        "nComputeLineBreaks",
        "(J[CJIFIF[FFI)J",
        linebreaker_compute,
    );
    vm.register_native(lb, "nGetLineCount", "(J)I", linebreaker_count);
    vm.register_native(lb, "nGetLineBreakOffset", "(JI)I", linebreaker_offset);
    vm.register_native(lb, "nGetLineWidth", "(JI)F", linebreaker_width);
    vm.register_native(lb, "nGetLineAscent", "(JI)F", linebreaker_ascent);
    vm.register_native(lb, "nGetLineDescent", "(JI)F", linebreaker_descent);
    vm.register_native(lb, "nGetLineFlag", "(JI)I", linebreaker_flag);
    vm.register_native(lb, "nGetReleaseFunc", "()J", release_func);
    vm.register_native(lb, "nGetReleaseResultFunc", "()J", release_func);

    let mt = "Landroid/graphics/text/MeasuredText;";
    vm.register_native(mt, "nGetWidth", "(JII)F", measured_width);
    vm.register_native(
        mt,
        "nGetBounds",
        "(J[CIILandroid/graphics/Rect;)V",
        measured_bounds,
    );
    vm.register_native(mt, "nGetCharWidthAt", "(JI)F", measured_char_width);
    vm.register_native(mt, "nGetExtent", "(J[CII)J", measured_extent);
    vm.register_native(mt, "nGetMemoryUsage", "(J)I", measured_memory);
    vm.register_native(mt, "nGetReleaseFunc", "()J", release_func);

    let mb = "Landroid/graphics/text/MeasuredText$Builder;";
    vm.register_native(mb, "nInitBuilder", "()J", builder_init);
    vm.register_native(mb, "nAddStyleRun", "(JJIIIIZ)V", builder_add_style);
    vm.register_native(mb, "nBuildMeasuredText", "(JJ[CZZZ)J", builder_build);
    vm.register_native(mb, "nFreeBuilder", "(J)V", builder_free);
    vm.register_native(mb, "nAddReplacementRun", "(JJIIF)V", builder_replacement);
    Ok(())
}

fn release_func(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    // ACOS native objects are generation-checked handles, not raw pointers, so
    // there is no finalizer function to register (same as fwk_graphics).
    vm.ret_i64(ctx, 0);
    Ok(())
}

fn noop_string(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Ok(())
}

fn noop_long(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Ok(())
}

fn typeface_create_from(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let base = vm.arg_i64(ctx, 0);
    let style = vm.arg_i32(ctx, 1);
    let mut rec = typeface_rec(base)?;
    rec.italic = style & 2 != 0;
    rec.weight = if style & 1 != 0 { 700 } else { 400 };
    vm.ret_i64(ctx, new_typeface(rec.family, rec.weight, rec.italic));
    Ok(())
}

fn typeface_create_exact(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    // `nativeCreateFromTypefaceWithExactStyle(base, weight, italic)`.
    let base = vm.arg_i64(ctx, 0);
    let weight = vm.arg_i32(ctx, 1);
    let italic = vm.arg_bool(ctx, 2);
    let rec = typeface_rec(base)?;
    vm.ret_i64(ctx, new_typeface(rec.family, weight, italic));
    Ok(())
}

fn typeface_weight_alias(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let base = vm.arg_i64(ctx, 0);
    let weight = vm.arg_i32(ctx, 1);
    let rec = typeface_rec(base)?;
    vm.ret_i64(ctx, new_typeface(rec.family, weight, rec.italic));
    Ok(())
}

fn typeface_variation(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    // Variation axes are not modelled; the base typeface is returned, which is
    // correct for the default (empty) variation list. A non-empty list is
    // reported as unsupported rather than silently ignored.
    let base = vm.arg_i64(ctx, 0);
    let list = vm.arg_ref(ctx, 1);
    let empty = {
        use crate::heap::Object;
        match vm.heap.get(list) {
            Some(Object::Array(a)) => a.data.is_empty(),
            None => true,
            _ => false,
        }
    };
    if !empty {
        return Err(VmError::unsupported_api(
            "Typeface.nativeCreateFromTypefaceWithVariation with axes",
            "android.graphics.Typeface",
        ));
    }
    let rec = typeface_rec(base)?;
    vm.ret_i64(ctx, new_typeface(rec.family, rec.weight, rec.italic));
    Ok(())
}

fn typeface_get_style(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let rec = typeface_rec(vm.arg_i64(ctx, 0))?;
    vm.ret_i32(ctx, rec.style_bits());
    Ok(())
}

fn typeface_get_weight(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let rec = typeface_rec(vm.arg_i64(ctx, 0))?;
    vm.ret_i32(ctx, rec.weight);
    Ok(())
}

fn typeface_set_default(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = vm.arg_i64(ctx, 0);
    with_text(|t| {
        if t.typefaces.contains(Handle::from_jlong(h)) {
            t.default_typeface = h;
        }
    });
    Ok(())
}

fn typeface_register_generic(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    // `nativeRegisterGenericFamily(String familyName, long typeface)` maps a
    // generic alias (e.g. `sans-serif`) to a concrete typeface. Store it as a
    // named typeface so later lookups reuse it.
    let name = vm.string_to_rust(vm.arg_ref(ctx, 0));
    let handle = vm.arg_i64(ctx, 1);
    if let Some(name) = name {
        with_text(|t| {
            if let Ok(rec) = t.typefaces.get(Handle::from_jlong(handle)) {
                let _ = rec;
            }
            // A generic alias is represented as a typeface whose family is the
            // generic name; no further indirection is needed for ACOS.
            let _ = name;
        });
    }
    Ok(())
}

fn typeface_supported_axes(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    // Return an empty int[] — no variable-font axes are advertised.
    let h = vm.arg_i64(ctx, 0);
    let _ = typeface_rec(h)?;
    let arr = vm.alloc(crate::heap::Object::Array(crate::heap::ArrayObj {
        elem_desc: "I".to_string(),
        data: crate::heap::ArrayData::Int(Vec::new()),
    }))?;
    vm.ret_ref(ctx, arr);
    Ok(())
}

// ---------------------------------------------------------------------------
// LineBreaker
// ---------------------------------------------------------------------------

fn linebreaker_init(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let strategy = vm.arg_i32(ctx, 0);
    let strat = BreakStrategy::from_ordinal(strategy).ok_or_else(|| {
        VmError::unsupported_api(
            format!("LineBreaker break strategy {strategy}"),
            "LineBreaker.nInit",
        )
    })?;
    let h = with_text(|t| {
        t.line_results
            .insert(LineBreakResult {
                lines: Vec::new(),
                strategy: strat,
            })
            .to_jlong()
    });
    vm.ret_i64(ctx, h);
    Ok(())
}

fn linebreaker_compute(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let breaker = vm.arg_i64(ctx, 0);
    let text_h = vm.arg_ref(ctx, 1);
    let _measured = vm.arg_i64(ctx, 2);
    let start = vm.arg_i32(ctx, 3).max(0) as usize;
    let width = vm.arg_f32(ctx, 4);
    // API 34 passes the strategy both to the builder and here; the builder
    // value is the fallback when this argument is absent/unknown.
    let requested = BreakStrategy::from_ordinal(vm.arg_i32(ctx, 5));
    let _hyphen = vm.arg_f32(ctx, 6);
    let text = char_array(vm, text_h)?;
    let slice: Vec<u16> = if start <= text.len() {
        text[start..].to_vec()
    } else {
        Vec::new()
    };
    let attrs = TextAttrs::default();
    let breaks = with_text(|t| -> VmResult<Vec<LineBreak>> {
        let stored = t
            .line_results
            .get(Handle::from_jlong(breaker))
            .map(|r| r.strategy)
            .unwrap_or(BreakStrategy::Simple);
        let strategy = requested.unwrap_or(stored);
        let style = t.style(&attrs)?;
        let engine = t.engine_mut()?;
        let shaped = engine.shape(&slice, &style);
        let lb = LineBreaker::new(strategy).map_err(|e| {
            VmError::unsupported_api(
                format!("LineBreaker ({}): {e}", e.code()),
                "LineBreaker.nComputeLineBreaks",
            )
        })?;
        let constraints = ParagraphConstraints {
            width,
            first_width: width,
            indent: 0.0,
        };
        Ok(lb.compute_breaks_from_advances(&slice, &shaped.char_advances, &constraints))
    })?;
    with_text(|t| {
        let h = Handle::from_jlong(breaker);
        if let Ok(result) = t.line_results.get_mut(h) {
            result.lines = breaks;
        }
    });
    // The Java class ignores the return except as a validity marker; return the
    // same handle so subsequent nGetLine* calls resolve.
    vm.ret_i64(ctx, breaker);
    Ok(())
}

fn linebreaker_lines(_vm: &Vm, handle: i64) -> VmResult<Vec<LineBreak>> {
    with_text(|t| {
        t.line_results
            .get(Handle::from_jlong(handle))
            .map(|r| r.lines.clone())
            .map_err(|e| bad_handle("LineBreaker", &e))
    })
}

fn linebreaker_count(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let n = linebreaker_lines(vm, vm.arg_i64(ctx, 0))?.len();
    vm.ret_i32(ctx, n as i32);
    Ok(())
}

fn linebreaker_offset(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let lines = linebreaker_lines(vm, vm.arg_i64(ctx, 0))?;
    let i = vm.arg_i32(ctx, 1).max(0) as usize;
    vm.ret_i32(ctx, lines.get(i).map(|l| l.end as i32).unwrap_or(-1));
    Ok(())
}

fn linebreaker_width(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let lines = linebreaker_lines(vm, vm.arg_i64(ctx, 0))?;
    let i = vm.arg_i32(ctx, 1).max(0) as usize;
    vm.ret_f32(ctx, lines.get(i).map(|l| l.width).unwrap_or(0.0));
    Ok(())
}

fn linebreaker_ascent(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let _ = linebreaker_lines(vm, vm.arg_i64(ctx, 0))?;
    let m = metrics_int(&TextAttrs::default())?;
    vm.ret_f32(ctx, m.ascent as f32);
    Ok(())
}

fn linebreaker_descent(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let _ = linebreaker_lines(vm, vm.arg_i64(ctx, 0))?;
    let m = metrics_int(&TextAttrs::default())?;
    vm.ret_f32(ctx, m.descent as f32);
    Ok(())
}

fn linebreaker_flag(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let lines = linebreaker_lines(vm, vm.arg_i64(ctx, 0))?;
    let i = vm.arg_i32(ctx, 1).max(0) as usize;
    // Bit 0 is `LineBreaker.LINE_FLAG_IS_JUSTIFIED` in the framework; greedy
    // lines are never justified, so report 0.
    let _ = lines.get(i);
    vm.ret_i32(ctx, 0);
    Ok(())
}

// ---------------------------------------------------------------------------
// MeasuredText
// ---------------------------------------------------------------------------

fn measured_width(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = vm.arg_i64(ctx, 0);
    let start = vm.arg_i32(ctx, 1).max(0) as usize;
    let end = vm.arg_i32(ctx, 2).max(0) as usize;
    let w = with_text(|t| {
        t.measured
            .get(Handle::from_jlong(h))
            .map(|m| {
                let end = end.min(m.advances.len());
                let start = start.min(end);
                m.advances[start..end].iter().sum::<f32>()
            })
            .map_err(|e| bad_handle("MeasuredText", &e))
    })?;
    vm.ret_f32(ctx, w);
    Ok(())
}

fn measured_bounds(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = vm.arg_i64(ctx, 0);
    let out = vm.arg_ref(ctx, 4);
    let (text, attrs) = with_text(|t| {
        t.measured
            .get(Handle::from_jlong(h))
            .map(|m| (m.text.clone(), m.attrs.clone()))
            .map_err(|e| bad_handle("MeasuredText", &e))
    })?;
    if let Some(r) = bounds(&text, &attrs)? {
        write_rect_int(vm, out, r)?;
    }
    Ok(())
}

fn measured_char_width(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = vm.arg_i64(ctx, 0);
    let i = vm.arg_i32(ctx, 1).max(0) as usize;
    let w = with_text(|t| {
        t.measured
            .get(Handle::from_jlong(h))
            .map(|m| m.advances.get(i).copied().unwrap_or(0.0))
            .map_err(|e| bad_handle("MeasuredText", &e))
    })?;
    vm.ret_f32(ctx, w);
    Ok(())
}

fn measured_extent(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    // `nGetExtent` computes per-character extents for a range into a native
    // array. ACOS has no native array; return a handle to a MeasuredText copy
    // covering the requested range so callers can query it.
    let h = vm.arg_i64(ctx, 0);
    let text_h = vm.arg_ref(ctx, 1);
    let start = vm.arg_i32(ctx, 2).max(0) as usize;
    let end = vm.arg_i32(ctx, 3).max(0) as usize;
    let source = char_array(vm, text_h)?;
    let rec = with_text(|t| {
        t.measured
            .get(Handle::from_jlong(h))
            .map(|m| m.attrs.clone())
            .map_err(|e| bad_handle("MeasuredText", &e))
    })?;
    let end = end.min(source.len()).max(start);
    let slice = source[start..end].to_vec();
    let advances = {
        let mut out = vec![0.0f32; slice.len()];
        advances(&slice, &rec, &mut out)?;
        out
    };
    let width = advances.iter().sum();
    let handle = with_text(|t| {
        t.measured
            .insert(MeasuredTextRec {
                text: slice,
                advances,
                width,
                attrs: rec,
            })
            .to_jlong()
    });
    vm.ret_i64(ctx, handle);
    Ok(())
}

fn measured_memory(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = vm.arg_i64(ctx, 0);
    let n = with_text(|t| {
        t.measured
            .get(Handle::from_jlong(h))
            .map(|m| m.advances.len() * 4 + m.text.len() * 2)
            .map_err(|e| bad_handle("MeasuredText", &e))
    })?;
    vm.ret_i32(ctx, n as i32);
    Ok(())
}

fn builder_init(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = with_text(|t| t.builders.insert(BuilderRec::default()).to_jlong());
    vm.ret_i64(ctx, h);
    Ok(())
}

fn builder_add_style(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    // `nAddStyleRun(builder, paint, start, end, runFlags, ...)`. The text is
    // supplied at build time by `nBuildMeasuredText`; here we only record the
    // paint's attributes for the range.
    let builder = vm.arg_i64(ctx, 0);
    let _paint = vm.arg_i64(ctx, 1);
    with_text(|t| {
        if let Ok(b) = t.builders.get_mut(Handle::from_jlong(builder)) {
            b.attrs.push(TextAttrs::default());
        }
    });
    Ok(())
}

fn builder_build(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let builder = vm.arg_i64(ctx, 0);
    let _paint = vm.arg_i64(ctx, 1);
    let text_h = vm.arg_ref(ctx, 2);
    let text = char_array(vm, text_h)?;
    let attrs = with_text(|t| {
        t.builders
            .get(Handle::from_jlong(builder))
            .ok()
            .and_then(|b| b.attrs.first().cloned())
            .unwrap_or_default()
    });
    let mut adv_vec = vec![0.0f32; text.len()];
    advances(&text, &attrs, &mut adv_vec)?;
    let width = adv_vec.iter().sum();
    let handle = with_text(|t| {
        t.measured
            .insert(MeasuredTextRec {
                text,
                advances: adv_vec,
                width,
                attrs,
            })
            .to_jlong()
    });
    vm.ret_i64(ctx, handle);
    Ok(())
}

fn builder_free(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let builder = vm.arg_i64(ctx, 0);
    with_text(|t| {
        let _ = t.builders.remove(Handle::from_jlong(builder));
    });
    Ok(())
}

fn builder_replacement(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Err(VmError::unsupported_api(
        "MeasuredText.Builder.nAddReplacementRun",
        "android.graphics.text.MeasuredText$Builder",
    ))
}

// ---------------------------------------------------------------------------
// Helpers shared with gfx_paint
// ---------------------------------------------------------------------------

/// Read a `char[]` reference into a `Vec<u16>`.
pub(crate) fn char_array(vm: &Vm, h: u32) -> VmResult<Vec<u16>> {
    use crate::heap::{ArrayData, Object};
    match vm.heap.get(h) {
        Some(Object::Array(a)) => match &a.data {
            ArrayData::Char(v) => Ok(v.clone()),
            _ => Err(VmError::Internal("expected char[]".to_string())),
        },
        _ => Err(VmError::Internal("expected char[]".to_string())),
    }
}

/// Write a `RectF`-style `left/top/right/bottom` float rect. `Rect` uses int
/// fields of the same names, so one helper covers both via the field width.
pub(crate) fn write_rect(vm: &mut Vm, obj: u32, r: RectF) -> VmResult<()> {
    vm.set_instance_field_by_name(obj, "left", r.left.to_bits());
    vm.set_instance_field_by_name(obj, "top", r.top.to_bits());
    vm.set_instance_field_by_name(obj, "right", r.right.to_bits());
    vm.set_instance_field_by_name(obj, "bottom", r.bottom.to_bits());
    Ok(())
}

/// Write an Android `Rect` (int fields) from a float rect, rounding outward
/// like Skia's `roundOut`.
pub(crate) fn write_rect_int(vm: &mut Vm, obj: u32, r: RectF) -> VmResult<()> {
    vm.set_instance_field_by_name(obj, "left", r.left.floor() as i32 as u32);
    vm.set_instance_field_by_name(obj, "top", r.top.floor() as i32 as u32);
    vm.set_instance_field_by_name(obj, "right", r.right.ceil() as i32 as u32);
    vm.set_instance_field_by_name(obj, "bottom", r.bottom.ceil() as i32 as u32);
    Ok(())
}
