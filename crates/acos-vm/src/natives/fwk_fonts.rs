//! `android.graphics.fonts.Font`, `Font$Builder`, `FontFamily` and
//! `FontFamily$Builder` natives, plus the `Typeface` natives the font-config
//! path needs (`nativeCreateFromArray`, `nativeForceSetStaticFinalField`,
//! `nativeGetSupportedAxes`).
//!
//! Android's `Typeface.<clinit>` + `loadPreinstalledSystemFontMap` parse
//! `/system/etc/fonts.xml` into `android.text.FontConfig`, then
//! `SystemFonts.createFontFamily` mmaps every `/system/fonts/*.ttf|otf` through
//! `FileChannel.map` and builds one native `Font` per `<font>` entry, one
//! native `FontFamily` per `<family>`, and finally one native `Typeface` per
//! named family (`Typeface.createFromFamilies`). ACOS services that surface
//! here:
//!
//! * the font bytes are those already copied into the
//!   [`super::native_mem`] region by `FileChannelImpl.map0`;
//! * the resulting [`FontRec`] resolves the face against
//!   [`acos_render::text::fonts::FontCollection`] (the same real `fonts.xml`
//!   the renderer uses) by file basename, so `Paint.measureText` shapes with
//!   the exact `Roboto-Regular.ttf` face the framework selected;
//! * `nativeCreateFromArray` materialises a `TypefaceRec` carrying that
//!   concrete face in [`super::gfx_text`]'s handle table, so the existing
//!   `Paint`/text-measurement natives keep working unchanged.
//!
//! # Documented divergences from Minikin/FreeType
//!
//! * `Font.nNewByteBuffer` (a direct `ByteBuffer` view over the font bytes) is
//!   not implemented: ACOS's VM cannot synthesise a `java.nio.DirectByteBuffer`
//!   with an arbitrary address from a native. `Font.getBuffer()` therefore
//!   raises a structured `UNSUPPORTED_JAVA_API`. Nothing on the system-font
//!   construction or measurement path calls it.
//! * `nativeWriteTypefaces` / `nativeReadTypefaces` (serialised font-map
//!   transport) are also unsupported; the preinstalled path does not use them.
//! * Variable-font axes are stored and reported (`getAxes`,
//!   `getSupportedAxes`); shaping applies them through `acos-render`
//!   (`rustybuzz` variation coordinates), while the FreeType A8 mask path still
//!   renders the default instance (a pre-existing ADR-0019 limitation).

use std::path::Path;
use std::sync::{Mutex, OnceLock};

use acos_render::text::fonts::{FontCollection, FontRef};
use acos_render::text::{FontMetrics, TextEngine, TextStyle};

use crate::class::NativeCtx;
use crate::error::{VmError, VmResult};
use crate::heap::{ArrayData, ArrayObj, Handle, Object};
use crate::natives::gfx_handles::{Handle as GfxHandle, HandleTable};
use crate::natives::gfx_text::{self, TypefaceRec};
use crate::vm::Vm;

use super::native_mem;

/// A native `android.graphics.fonts.Font`.
#[derive(Clone, Debug)]
pub struct FontRec {
    /// Absolute path the framework recorded (`mFile.getAbsolutePath()`), or `""`.
    pub path: String,
    /// TrueType collection face index.
    pub ttc_index: u32,
    /// CSS weight 1..1000.
    pub weight: i32,
    pub italic: bool,
    /// Variation axes applied to the face, as `(OpenType tag, value)`.
    pub axes: Vec<(u32, f32)>,
    /// Resolved acos-render face, when the file is present in the collection.
    pub font: Option<FontRef>,
    /// Synthetic address of the font bytes in [`native_mem`], or 0.
    pub buffer_addr: i64,
    pub locale_list: String,
    pub source_id: i32,
}

/// A native `android.graphics.fonts.FontFamily`.
#[derive(Clone, Debug)]
pub struct FontFamilyRec {
    pub fonts: Vec<i64>,
    pub lang_tags: String,
    pub variant: i32,
}

#[derive(Default)]
struct FontBuilderRec {
    axes: Vec<(u32, f32)>,
}

#[derive(Default)]
struct FamilyBuilderRec {
    fonts: Vec<i64>,
}

struct FontShared {
    fonts: HandleTable<FontRec>,
    families: HandleTable<FontFamilyRec>,
    font_builders: HandleTable<FontBuilderRec>,
    family_builders: HandleTable<FamilyBuilderRec>,
}

impl FontShared {
    fn new() -> FontShared {
        FontShared {
            fonts: HandleTable::new(),
            families: HandleTable::new(),
            font_builders: HandleTable::new(),
            family_builders: HandleTable::new(),
        }
    }

    fn font(&self, h: i64) -> VmResult<FontRec> {
        self.fonts
            .get(GfxHandle::from_jlong(h))
            .cloned()
            .map_err(|e| font_error("Font", &e))
    }

    fn family(&self, h: i64) -> VmResult<FontFamilyRec> {
        self.families
            .get(GfxHandle::from_jlong(h))
            .cloned()
            .map_err(|e| font_error("FontFamily", &e))
    }
}

static FONTS: OnceLock<Mutex<FontShared>> = OnceLock::new();

fn with_fonts<R>(f: impl FnOnce(&mut FontShared) -> R) -> R {
    let m = FONTS.get_or_init(|| Mutex::new(FontShared::new()));
    match m.lock() {
        Ok(mut g) => f(&mut g),
        Err(poisoned) => f(&mut poisoned.into_inner()),
    }
}

fn font_error(kind: &str, e: &crate::natives::gfx_handles::HandleError) -> VmError {
    VmError::Internal(format!("{kind}: {e}"))
}

fn unsupported(what: &str) -> VmError {
    VmError::unsupported_api(what, "android.graphics.fonts")
}

// ---------------------------------------------------------------------------
// Native registration
// ---------------------------------------------------------------------------

pub(crate) fn install(vm: &mut Vm) -> VmResult<()> {
    let fb = "Landroid/graphics/fonts/Font$Builder;";
    vm.register_native(fb, "nInitBuilder", "()J", fb_init);
    vm.register_native(fb, "nAddAxis", "(JIF)V", fb_add_axis);
    vm.register_native(fb, "nBuild", "(JLjava/nio/ByteBuffer;Ljava/lang/String;Ljava/lang/String;IZI)J", fb_build);
    vm.register_native(fb, "nClone", "(JJIZI)J", fb_clone);

    let f = "Landroid/graphics/fonts/Font;";
    vm.register_native(f, "nCloneFont", "(J)J", font_clone);
    vm.register_native(f, "nGetAvailableFontSet", "()[J", font_available);
    vm.register_native(f, "nGetAxisCount", "(J)I", font_axis_count);
    vm.register_native(f, "nGetAxisInfo", "(JI)J", font_axis_info);
    vm.register_native(f, "nGetBufferAddress", "(J)J", font_buffer_address);
    vm.register_native(f, "nGetFontMetrics", "(JJLandroid/graphics/Paint$FontMetrics;)F", font_metrics);
    vm.register_native(f, "nGetFontPath", "(J)Ljava/lang/String;", font_path);
    vm.register_native(f, "nGetGlyphBounds", "(JIJLandroid/graphics/RectF;)F", font_glyph_bounds);
    vm.register_native(f, "nGetIndex", "(J)I", font_index);
    vm.register_native(f, "nGetLocaleList", "(J)Ljava/lang/String;", font_locale_list);
    vm.register_native(f, "nGetMinikinFontPtr", "(J)J", font_minikin_ptr);
    vm.register_native(f, "nGetPackedStyle", "(J)I", font_packed_style);
    vm.register_native(f, "nGetReleaseNativeFont", "()J", release_func);
    vm.register_native(f, "nGetSourceId", "(J)I", font_source_id);
    vm.register_native(f, "nNewByteBuffer", "(J)Ljava/nio/ByteBuffer;", font_new_byte_buffer);

    let family = "Landroid/graphics/fonts/FontFamily;";
    vm.register_native(family, "nGetFont", "(JI)J", family_get_font);
    vm.register_native(family, "nGetFontSize", "(J)I", family_font_size);
    vm.register_native(family, "nGetLangTags", "(J)Ljava/lang/String;", family_lang_tags);
    vm.register_native(family, "nGetVariant", "(J)I", family_variant);

    let fbb = "Landroid/graphics/fonts/FontFamily$Builder;";
    vm.register_native(fbb, "nInitBuilder", "()J", fbb_init);
    vm.register_native(fbb, "nAddFont", "(JJ)V", fbb_add_font);
    vm.register_native(fbb, "nBuild", "(JLjava/lang/String;IZZ)J", fbb_build);
    vm.register_native(fbb, "nGetReleaseNativeFamily", "()J", release_func);

    let t = "Landroid/graphics/Typeface;";
    // Overrides the W19-B placeholder so the real variable-font axes are
    // reported for typefaces built from a concrete face.
    vm.register_native(t, "nativeGetSupportedAxes", "(J)[I", typeface_supported_axes);
    // The two remaining Typeface natives the font-config path needs; the rest
    // stay owned by `gfx_text`.
    vm.register_native(t, "nativeCreateFromArray", "([JJII)J", typeface_create_from_array);
    vm.register_native(
        t,
        "nativeForceSetStaticFinalField",
        "(Ljava/lang/String;Landroid/graphics/Typeface;)V",
        typeface_force_static_final,
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Font$Builder
// ---------------------------------------------------------------------------

fn fb_init(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = with_fonts(|s| s.font_builders.insert(FontBuilderRec::default()).to_jlong());
    vm.ret_i64(ctx, h);
    Ok(())
}

fn fb_add_axis(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let builder = vm.arg_i64(ctx, 0);
    let tag = vm.arg_i32(ctx, 1) as u32;
    let value = vm.arg_f32(ctx, 2);
    with_fonts(|s| {
        if let Ok(b) = s.font_builders.get_mut(GfxHandle::from_jlong(builder)) {
            b.axes.push((tag, value));
        }
    });
    Ok(())
}

fn fb_build(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let builder = vm.arg_i64(ctx, 0);
    let buffer = vm.arg_ref(ctx, 1);
    let path = vm.string_to_rust(vm.arg_ref(ctx, 2)).unwrap_or_default();
    let locale = vm.string_to_rust(vm.arg_ref(ctx, 3)).unwrap_or_default();
    let weight = vm.arg_i32(ctx, 4);
    let italic = vm.arg_bool(ctx, 5);
    let ttc = vm.arg_i32(ctx, 6);
    let axes = with_fonts(|s| {
        s.font_builders
            .remove(GfxHandle::from_jlong(builder))
            .map(|b| b.axes)
            .unwrap_or_default()
    });
    let addr = font_buffer_addr(vm, buffer)?;
    let font = resolve_font_ref(&path, ttc as u32, weight, italic, &axes);
    let rec = FontRec {
        path,
        ttc_index: ttc.max(0) as u32,
        weight: weight.clamp(1, 1000),
        italic,
        axes,
        font,
        buffer_addr: addr,
        locale_list: locale,
        source_id: 0,
    };
    let h = with_fonts(|s| s.fonts.insert(rec).to_jlong());
    vm.ret_i64(ctx, h);
    Ok(())
}

fn fb_clone(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    // `nClone(baseFontPtr, builderPtr, weight, italic, ttcIndex)`.
    let base = vm.arg_i64(ctx, 0);
    let builder = vm.arg_i64(ctx, 1);
    let weight = vm.arg_i32(ctx, 2);
    let italic = vm.arg_bool(ctx, 3);
    let ttc = vm.arg_i32(ctx, 4);
    let mut rec = with_fonts(|s| s.font(base))?;
    let axes = with_fonts(|s| {
        s.font_builders
            .remove(GfxHandle::from_jlong(builder))
            .map(|b| b.axes)
            .unwrap_or_default()
    });
    rec.weight = weight.clamp(1, 1000);
    rec.italic = italic;
    rec.ttc_index = ttc.max(0) as u32;
    if !axes.is_empty() {
        rec.axes = axes;
    }
    let h = with_fonts(|s| s.fonts.insert(rec).to_jlong());
    vm.ret_i64(ctx, h);
    Ok(())
}

// ---------------------------------------------------------------------------
// Font
// ---------------------------------------------------------------------------

fn font_clone(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let rec = with_fonts(|s| s.font(vm.arg_i64(ctx, 0)))?;
    let h = with_fonts(|s| s.fonts.insert(rec).to_jlong());
    vm.ret_i64(ctx, h);
    Ok(())
}

fn font_available(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handles: Vec<i64> = with_fonts(|s| s.fonts.iter().map(|(h, _)| h.to_jlong()).collect());
    let arr = vm.alloc(Object::Array(ArrayObj {
        elem_desc: "J".to_string(),
        data: ArrayData::Long(handles),
    }))?;
    vm.ret_ref(ctx, arr);
    Ok(())
}

fn font_axis_count(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let rec = with_fonts(|s| s.font(vm.arg_i64(ctx, 0)))?;
    vm.ret_i32(ctx, rec.axes.len() as i32);
    Ok(())
}

fn font_axis_info(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let rec = with_fonts(|s| s.font(vm.arg_i64(ctx, 0)))?;
    let i = vm.arg_i32(ctx, 1).max(0) as usize;
    match rec.axes.get(i) {
        Some((tag, value)) => {
            // AOSP `pack`: low 32 bits = float bits, bits 56..63 = tag byte 0,
            // 48..55 = tag byte 1, 40..47 = tag byte 2, 32..39 = tag byte 3.
            let packed = ((*tag as u64) << 32) | value.to_bits() as u64;
            vm.ret_i64(ctx, packed as i64);
        }
        None => vm.ret_i64(ctx, 0),
    }
    Ok(())
}

fn font_buffer_address(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let rec = with_fonts(|s| s.font(vm.arg_i64(ctx, 0)))?;
    vm.ret_i64(ctx, rec.buffer_addr);
    Ok(())
}

fn font_metrics(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let rec = with_fonts(|s| s.font(vm.arg_i64(ctx, 0)))?;
    let paint_ptr = vm.arg_i64(ctx, 1);
    let out = vm.arg_ref(ctx, 2);
    let font = rec.font.ok_or_else(|| unsupported("Font.nGetFontMetrics without a resolved face"))?;
    let attrs = crate::natives::gfx_paint::paint_rec(paint_ptr)?.text_attrs();
    let m = metrics_for(&font, &attrs);
    write_font_metrics(vm, out, &m);
    vm.ret_f32(ctx, attrs.text_size);
    Ok(())
}

fn font_path(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let rec = with_fonts(|s| s.font(vm.arg_i64(ctx, 0)))?;
    let h = vm.new_string(&rec.path)?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn font_glyph_bounds(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let rec = with_fonts(|s| s.font(vm.arg_i64(ctx, 0)))?;
    let glyph_id = vm.arg_i32(ctx, 1).max(0) as u16;
    let paint_ptr = vm.arg_i64(ctx, 2);
    let out = vm.arg_ref(ctx, 3);
    let font = rec.font.ok_or_else(|| unsupported("Font.nGetGlyphBounds without a resolved face"))?;
    let attrs = crate::natives::gfx_paint::paint_rec(paint_ptr)?.text_attrs();
    let advance = glyph_bounds(&font, glyph_id, attrs.text_size, attrs.scale_x, vm, out)?;
    vm.ret_f32(ctx, advance);
    Ok(())
}

fn font_index(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let rec = with_fonts(|s| s.font(vm.arg_i64(ctx, 0)))?;
    vm.ret_i32(ctx, rec.ttc_index as i32);
    Ok(())
}

fn font_locale_list(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let rec = with_fonts(|s| s.font(vm.arg_i64(ctx, 0)))?;
    let h = vm.new_string(&rec.locale_list)?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn font_minikin_ptr(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let rec = with_fonts(|s| s.font(vm.arg_i64(ctx, 0)))?;
    // A stable identity used by `Font.equals`; the acos-render font id is
    // exactly one per mapped face.
    vm.ret_i64(ctx, rec.font.as_ref().map(|f| f.file.id as i64).unwrap_or(0));
    Ok(())
}

fn font_packed_style(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let rec = with_fonts(|s| s.font(vm.arg_i64(ctx, 0)))?;
    // `FontFileUtil.packWeightStyle`: weight in the low 16 bits, italic bit 16.
    let packed = (rec.weight & 0xffff) | if rec.italic { 0x1_0000 } else { 0 };
    vm.ret_i32(ctx, packed);
    Ok(())
}

fn font_source_id(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let rec = with_fonts(|s| s.font(vm.arg_i64(ctx, 0)))?;
    vm.ret_i32(ctx, rec.source_id);
    Ok(())
}

fn font_new_byte_buffer(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Err(unsupported("Font.nNewByteBuffer (direct ByteBuffer view)"))
}

// ---------------------------------------------------------------------------
// FontFamily
// ---------------------------------------------------------------------------

fn family_get_font(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fam = with_fonts(|s| s.family(vm.arg_i64(ctx, 0)))?;
    let i = vm.arg_i32(ctx, 1);
    if i < 0 || i as usize >= fam.fonts.len() {
        return Err(vm.throw_new("Ljava/lang/IndexOutOfBoundsException;", None));
    }
    vm.ret_i64(ctx, fam.fonts[i as usize]);
    Ok(())
}

fn family_font_size(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fam = with_fonts(|s| s.family(vm.arg_i64(ctx, 0)))?;
    vm.ret_i32(ctx, fam.fonts.len() as i32);
    Ok(())
}

fn family_lang_tags(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fam = with_fonts(|s| s.family(vm.arg_i64(ctx, 0)))?;
    let h = vm.new_string(&fam.lang_tags)?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn family_variant(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fam = with_fonts(|s| s.family(vm.arg_i64(ctx, 0)))?;
    vm.ret_i32(ctx, fam.variant);
    Ok(())
}

// ---------------------------------------------------------------------------
// FontFamily$Builder
// ---------------------------------------------------------------------------

fn fbb_init(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = with_fonts(|s| s.family_builders.insert(FamilyBuilderRec::default()).to_jlong());
    vm.ret_i64(ctx, h);
    Ok(())
}

fn fbb_add_font(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let builder = vm.arg_i64(ctx, 0);
    let font = vm.arg_i64(ctx, 1);
    // Resolve now so a bad font handle is a structured error, not a dangling
    // family entry.
    with_fonts(|s| {
        s.font(font)?;
        if let Ok(b) = s.family_builders.get_mut(GfxHandle::from_jlong(builder)) {
            b.fonts.push(font);
        }
        Ok::<(), VmError>(())
    })?;
    Ok(())
}

fn fbb_build(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let builder = vm.arg_i64(ctx, 0);
    let lang = vm.string_to_rust(vm.arg_ref(ctx, 1)).unwrap_or_default();
    let variant = vm.arg_i32(ctx, 2);
    // `nBuild` also carries custom/default-fallback flags; the Java `FontFamily`
    // surface never reads them back, so ACOS does not retain them.
    let _custom_fallback = vm.arg_bool(ctx, 3);
    let _default_fallback = vm.arg_bool(ctx, 4);
    let fonts = with_fonts(|s| {
        s.family_builders
            .remove(GfxHandle::from_jlong(builder))
            .map(|b| b.fonts)
            .unwrap_or_default()
    });
    let rec = FontFamilyRec {
        fonts,
        lang_tags: lang,
        variant,
    };
    let h = with_fonts(|s| s.families.insert(rec).to_jlong());
    vm.ret_i64(ctx, h);
    Ok(())
}

// ---------------------------------------------------------------------------
// Typeface
// ---------------------------------------------------------------------------

fn typeface_create_from_array(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let ptrs = long_array(vm, vm.arg_ref(ctx, 0))?;
    let weight = vm.arg_i32(ctx, 2);
    let italic = vm.arg_i32(ctx, 3);
    let mut chosen: Option<(Option<String>, i32, bool, FontRef)> = None;
    'outer: for ptr in ptrs {
        let fam = match with_fonts(|s| s.family(ptr)) {
            Ok(f) => f,
            Err(_) => continue,
        };
        for fh in &fam.fonts {
            let font = match with_fonts(|s| s.font(*fh)) {
                Ok(f) => f,
                Err(_) => continue,
            };
            if let Some(fr) = font.font.clone() {
                let w = if weight >= 0 { weight } else { font.weight };
                let it = if italic >= 0 { italic != 0 } else { font.italic };
                let family = family_name_of(&fr).or_else(|| ttf_family_name(&fr));
                chosen = Some((family, w, it, fr));
                break 'outer;
            }
        }
    }
    let rec = match chosen {
        Some((family, weight, italic, font)) => TypefaceRec {
            family,
            weight,
            italic,
            font: Some(font),
        },
        // No resolvable family (e.g. a font whose file is absent from the
        // extracted image): fall back to the default typeface like the
        // framework does for an unknown family name.
        None => gfx_text::typeface_rec(0)?,
    };
    let h = gfx_text::with_text(|t| t.typefaces.insert(rec).to_jlong());
    vm.ret_i64(ctx, h);
    Ok(())
}

fn typeface_force_static_final(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let name = vm.string_to_rust(vm.arg_ref(ctx, 0));
    let value = vm.arg_ref(ctx, 1);
    if let Some(name) = name {
        if let Some(cid) = vm.class_by_desc.get("Landroid/graphics/Typeface;").copied() {
            let off = vm.classes[cid as usize]
                .declared_static_fields
                .iter()
                .find(|f| f.name == name)
                .map(|f| f.offset as usize);
            if let Some(off) = off {
                if off < vm.classes[cid as usize].statics.len() {
                    vm.classes[cid as usize].statics[off] = value;
                }
            }
        }
    }
    Ok(())
}

fn typeface_supported_axes(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let rec = gfx_text::typeface_rec(vm.arg_i64(ctx, 0))?;
    let tags: Vec<i32> = rec
        .font
        .as_ref()
        .map(variable_axis_tags)
        .unwrap_or_default()
        .into_iter()
        .map(|t| t as i32)
        .collect();
    let arr = vm.alloc(Object::Array(ArrayObj {
        elem_desc: "I".to_string(),
        data: ArrayData::Int(tags),
    }))?;
    vm.ret_ref(ctx, arr);
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn release_func(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    // ACOS native objects are generation-checked handles, not raw pointers, so
    // there is no finalizer to register.
    vm.ret_i64(ctx, 0);
    Ok(())
}

/// Read a `long[]` reference. AOT-compiled framework code passes a real array.
fn long_array(vm: &Vm, h: Handle) -> VmResult<Vec<i64>> {
    match vm.heap.get(h) {
        Some(Object::Array(a)) => match &a.data {
            ArrayData::Long(v) => Ok(v.clone()),
            _ => Err(VmError::Internal("expected long[]".to_string())),
        },
        _ => Err(VmError::Internal("expected long[]".to_string())),
    }
}

/// Resolve a font `ByteBuffer`'s direct address without copying the font bytes.
///
/// `Font$Builder.nBuild` only stores the address for later rendering; it never
/// reads the buffer. Copying the whole (often multi-hundred-KB) font out of
/// native memory per font made booting the system font list a large native
/// cost of a launch, so validate the buffer shape and return the address only.
fn font_buffer_addr(vm: &mut Vm, buffer: Handle) -> VmResult<i64> {
    if buffer == crate::heap::NULL {
        return Err(unsupported("Font$Builder.nBuild without a font buffer"));
    }
    let addr = read_long_field(vm, buffer, "address")
        .ok_or_else(|| unsupported("Font$Builder.nBuild on a non-direct ByteBuffer"))?;
    // Touch the first byte so the mapped-region check the copying version
    // performed still runs, without materialising the region.
    let _ = native_mem::read_bytes_at(vm, addr, 1)?;
    Ok(addr)
}

/// Read a 64-bit instance field (two consecutive slots) by name.
fn read_long_field(vm: &Vm, obj: Handle, name: &str) -> Option<i64> {
    let class = vm.class_of_object(obj)?;
    let mut c = Some(class);
    while let Some(cur) = c {
        if let Some(f) = vm.classes[cur as usize]
            .declared_instance_fields
            .iter()
            .find(|f| f.name == name)
        {
            let off = f.offset as usize;
            if let Some(Object::Instance { fields, .. }) = vm.heap.get(obj) {
                let lo = *fields.get(off)?;
                let hi = *fields.get(off + 1)?;
                return Some((((hi as u64) << 32) | lo as u64) as i64);
            }
            return None;
        }
        c = vm.classes[cur as usize].super_class;
    }
    None
}

/// Resolve a framework font file to an acos-render face.
///
/// The framework's `filePath` is the guest path (`/system/fonts/Roboto-Regular.ttf`)
/// while the collection was loaded from the extracted host mirror, so the match
/// is on the file basename. A font built from a bare `ByteBuffer` (empty path)
/// falls back to the face's own family name.
fn resolve_font_ref(
    path: &str,
    ttc_index: u32,
    weight: i32,
    italic: bool,
    axes: &[(u32, f32)],
) -> Option<FontRef> {
    let fc = FontCollection::shared()?;
    let base = Path::new(path).file_name().and_then(|s| s.to_str());
    if let Some(base) = base {
        for (idx, fam) in fc.families.iter().enumerate() {
            if let Some(entry) = fam.fonts.iter().find(|e| {
                e.file
                    .path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .map(|n| n == base)
                    .unwrap_or(false)
            }) {
                return Some(make_font_ref(entry.file.clone(), idx, weight, italic, axes));
            }
        }
    }
    let _ = ttc_index;
    None
}

fn make_font_ref(
    file: std::sync::Arc<acos_render::text::fonts::FontFile>,
    family_key: usize,
    weight: i32,
    italic: bool,
    axes: &[(u32, f32)],
) -> FontRef {
    FontRef {
        file,
        weight: weight.clamp(1, 1000) as u16,
        italic,
        axes: axes
            .iter()
            .map(|(tag, value)| (tag_to_string(*tag), *value))
            .collect(),
        family_key,
    }
}

fn family_name_of(font: &FontRef) -> Option<String> {
    let fc = FontCollection::shared()?;
    fc.families.get(font.family_key).and_then(|f| f.name.clone())
}

fn ttf_family_name(font: &FontRef) -> Option<String> {
    let face = ttf_parser::Face::parse(&font.file.data, font.file.face_index).ok()?;
    let names = face.names();
    for want in [16u16, 1u16] {
        for i in 0..names.len() {
            if let Some(name) = names.get(i) {
                if name.name_id == want {
                    if let Some(s) = name.to_string() {
                        return Some(s);
                    }
                }
            }
        }
    }
    None
}

fn variable_axis_tags(font: &FontRef) -> Vec<u32> {
    let Ok(face) = ttf_parser::Face::parse(&font.file.data, font.file.face_index) else {
        return Vec::new();
    };
    face.variation_axes().into_iter().map(|a| a.tag.0).collect()
}

fn tag_to_string(tag: u32) -> String {
    tag.to_be_bytes().iter().map(|&b| b as char).collect()
}

/// `android.graphics.Paint.FontMetrics` (five float fields).
fn write_font_metrics(vm: &mut Vm, obj: Handle, m: &FontMetrics) {
    vm.set_instance_field_by_name(obj, "top", m.top.to_bits());
    vm.set_instance_field_by_name(obj, "ascent", m.ascent.to_bits());
    vm.set_instance_field_by_name(obj, "descent", m.descent.to_bits());
    vm.set_instance_field_by_name(obj, "bottom", m.bottom.to_bits());
    vm.set_instance_field_by_name(obj, "leading", m.leading.to_bits());
}

fn metrics_for(font: &FontRef, attrs: &gfx_text::TextAttrs) -> FontMetrics {
    let Some(fc) = FontCollection::shared() else {
        return FontMetrics::default();
    };
    let engine = TextEngine::new(fc);
    let style = TextStyle {
        font: font.clone(),
        family: None,
        size: attrs.text_size,
        scale_x: attrs.scale_x,
        skew_x: attrs.skew_x,
        letter_spacing: attrs.letter_spacing,
        fake_bold: attrs.fake_bold,
        features: Vec::new(),
        variations: Vec::new(),
    };
    engine.font_metrics(&style)
}

/// `Font.nGetGlyphBounds`: glyph outline bounds at the paint's text size, and
/// the horizontal advance (both scaled by `size / unitsPerEm * scaleX`).
fn glyph_bounds(
    font: &FontRef,
    glyph_id: u16,
    size: f32,
    scale_x: f32,
    vm: &mut Vm,
    out: Handle,
) -> VmResult<f32> {
    let Ok(face) = ttf_parser::Face::parse(&font.file.data, font.file.face_index) else {
        return Err(unsupported("Font.nGetGlyphBounds on an unparsable face"));
    };
    let upem = face.units_per_em() as f32;
    let scale = if upem > 0.0 { size / upem } else { 1.0 } * scale_x;
    let advance = face
        .glyph_hor_advance(ttf_parser::GlyphId(glyph_id))
        .map(|a| a as f32 * scale)
        .unwrap_or(0.0);
    if let Some(bbox) = face.glyph_bounding_box(ttf_parser::GlyphId(glyph_id)) {
        let left = bbox.x_min as f32 * scale;
        // Font coordinates are y-up; `RectF` is y-down.
        let top = -(bbox.y_max as f32) * scale;
        let right = bbox.x_max as f32 * scale;
        let bottom = -(bbox.y_min as f32) * scale;
        vm.set_instance_field_by_name(out, "left", left.to_bits());
        vm.set_instance_field_by_name(out, "top", top.to_bits());
        vm.set_instance_field_by_name(out, "right", right.to_bits());
        vm.set_instance_field_by_name(out, "bottom", bottom.to_bits());
    }
    Ok(advance)
}
