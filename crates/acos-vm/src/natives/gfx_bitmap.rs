//! `android.graphics.Bitmap` natives over `acos-render`'s pixel store.
//!
//! `acos-render::Bitmap` already owns a width/height/config/alpha pixel buffer
//! (`bitmap.rs`); this module maps the framework's `jlong` native pointer onto
//! its generation-checked handle. Ashmem is not modelled — ACOS renders
//! offscreen (ADR-0011) — so the ashmem entry points are plain copies and
//! `nativeIsBackedByAshmem` is honestly `false`; the ones that need a real
//! `ashmem` fd or hardware buffer are left unregistered and surface as
//! `UNSUPPORTED_JAVA_API`.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use acos_render::image::ImageFormat;
use acos_render::BitmapConfig;

use crate::class::NativeCtx;
use crate::error::{VmError, VmResult};
use crate::heap::{ArrayData, ArrayObj, Object};
use crate::javabridge::JavaValue;
use crate::natives::gfx_state::{gj, render_err, rh, stale, with_ctx};
use crate::vm::Vm;

/// A decoded image held alive between `ImageDecoder.nCreate` and
/// `nDecodeBitmap`/`nClose`. Android keeps the encoded stream in native memory
/// and decodes lazily; ACOS decodes the header eagerly (for the sampled size
/// the `ImageDecoder` constructor needs) and keeps the encoded bytes so each
/// `nDecodeBitmap` can produce a fresh `Bitmap`.
struct DecoderState {
    bytes: Vec<u8>,
    format: ImageFormat,
    width: i32,
    height: i32,
    animated: bool,
}

thread_local! {
    static DECODERS: RefCell<HashMap<u64, DecoderState>> = RefCell::new(HashMap::new());
    static NEXT_DECODER: Cell<u64> = const { Cell::new(1) };
}

fn mime_for(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Png => "image/png",
        ImageFormat::Jpeg => "image/jpeg",
        ImageFormat::WebP => "image/webp",
        ImageFormat::Unknown => "image/*",
    }
}

fn bitmap(vm: &Vm, ctx: NativeCtx, n: usize) -> VmResult<acos_render::Handle> {
    let j = vm.arg_i64(ctx, n);
    if with_ctx(|c| c.bitmaps.contains(rh(j))) {
        Ok(rh(j))
    } else {
        Err(stale("Bitmap", j))
    }
}

fn int_array(vm: &Vm, h: u32) -> Option<Vec<i32>> {
    match vm.heap.get(h) {
        Some(Object::Array(a)) => match &a.data {
            ArrayData::Int(v) => Some(v.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// Register the `Bitmap` natives ACOS can honour.
pub(crate) fn install(vm: &mut Vm) -> VmResult<()> {
    const BM: &str = "Landroid/graphics/Bitmap;";
    vm.register_native(BM, "nativeCreate", "([IIIIIIZJ)Landroid/graphics/Bitmap;", b_create);
    vm.register_native(BM, "nativeGetNativeFinalizer", "()J", b_zero_long);
    vm.register_native(BM, "nativeCopy", "(JIZ)Landroid/graphics/Bitmap;", b_copy);
    vm.register_native(BM, "nativeCopyAshmem", "(J)Landroid/graphics/Bitmap;", b_copy_same);
    vm.register_native(BM, "nativeCopyAshmemConfig", "(JI)Landroid/graphics/Bitmap;", b_copy_config);
    vm.register_native(BM, "nativeCopyPreserveInternalConfig", "(J)Landroid/graphics/Bitmap;", b_copy_same);
    vm.register_native(BM, "nativeConfig", "(J)I", b_config);
    vm.register_native(BM, "nativeRowBytes", "(J)I", b_row_bytes);
    vm.register_native(BM, "nativeGetAllocationByteCount", "(J)I", b_alloc_bytes);
    vm.register_native(BM, "nativeGenerationId", "(J)I", b_generation);
    vm.register_native(BM, "nativeHasAlpha", "(J)Z", b_has_alpha);
    vm.register_native(BM, "nativeSetHasAlpha", "(JZZ)V", b_set_has_alpha);
    vm.register_native(BM, "nativeIsPremultiplied", "(J)Z", b_premultiplied);
    vm.register_native(BM, "nativeSetPremultiplied", "(JZ)V", b_set_premultiplied);
    vm.register_native(BM, "nativeIsImmutable", "(J)Z", b_immutable);
    vm.register_native(BM, "nativeSetImmutable", "(J)V", b_set_immutable);
    vm.register_native(BM, "nativeErase", "(JI)V", b_erase);
    vm.register_native(BM, "nativeErase", "(JJJ)V", b_erase_long);
    vm.register_native(BM, "nativeGetPixel", "(JII)I", b_get_pixel);
    vm.register_native(BM, "nativeSetPixel", "(JIII)V", b_set_pixel);
    vm.register_native(BM, "nativeGetPixels", "(J[IIIIIII)V", b_get_pixels);
    vm.register_native(BM, "nativeSetPixels", "(J[IIIIIII)V", b_set_pixels);
    vm.register_native(BM, "nativeRecycle", "(J)V", b_recycle);
    vm.register_native(BM, "nativePrepareToDraw", "(J)V", b_noop);
    vm.register_native(BM, "nativeSameAs", "(JJ)Z", b_same_as);
    vm.register_native(BM, "nativeIsBackedByAshmem", "(J)Z", b_false);
    vm.register_native(BM, "nativeIsSRGB", "(J)Z", b_true);
    vm.register_native(BM, "nativeIsSRGBLinear", "(J)Z", b_false);
    vm.register_native(BM, "nativeComputeColorSpace", "(J)Landroid/graphics/ColorSpace;", b_null_ref);
    vm.register_native(BM, "nativeSetColorSpace", "(JJ)V", b_noop);
    vm.register_native(BM, "nativeGetAshmemFD", "(J)I", b_minus_one);
    vm.register_native(BM, "nativeHasMipMap", "(J)Z", b_false);
    vm.register_native(BM, "nativeSetHasMipMap", "(JZ)V", b_noop);
    vm.register_native(BM, "nativeExtractAlpha", "(JJ[I)Landroid/graphics/Bitmap;", b_extract_alpha);
    vm.register_native(BM, "nativeGetColor", "(JII)J", b_get_color);

    const BF: &str = "Landroid/graphics/BitmapFactory;";
    vm.register_native(
        BF,
        "nativeDecodeStream",
        "(Ljava/io/InputStream;[BLandroid/graphics/Rect;Landroid/graphics/BitmapFactory$Options;JJ)Landroid/graphics/Bitmap;",
        bf_decode_stream,
    );
    vm.register_native(
        BF,
        "nativeDecodeByteArray",
        "([BIILandroid/graphics/BitmapFactory$Options;JJ)Landroid/graphics/Bitmap;",
        bf_decode_byte_array,
    );

    const ID: &str = "Landroid/graphics/ImageDecoder;";
    vm.register_native(ID, "nCreate", "(Ljava/io/InputStream;[BZLandroid/graphics/ImageDecoder$Source;)Landroid/graphics/ImageDecoder;", id_create_stream);
    vm.register_native(ID, "nCreate", "([BIIZLandroid/graphics/ImageDecoder$Source;)Landroid/graphics/ImageDecoder;", id_create_bytes);
    vm.register_native(ID, "nCreate", "(Ljava/nio/ByteBuffer;IIZLandroid/graphics/ImageDecoder$Source;)Landroid/graphics/ImageDecoder;", id_create_unsupported);
    vm.register_native(ID, "nCreate", "(JZLandroid/graphics/ImageDecoder$Source;)Landroid/graphics/ImageDecoder;", id_create_unsupported);
    vm.register_native(ID, "nCreate", "(Ljava/io/FileDescriptor;JZLandroid/graphics/ImageDecoder$Source;)Landroid/graphics/ImageDecoder;", id_create_unsupported);
    vm.register_native(
        ID,
        "nDecodeBitmap",
        "(JLandroid/graphics/ImageDecoder;ZIILandroid/graphics/Rect;ZIZZZJZ)Landroid/graphics/Bitmap;",
        id_decode_bitmap,
    );
    vm.register_native(ID, "nGetSampledSize", "(JI)Landroid/util/Size;", id_get_sampled_size);
    vm.register_native(ID, "nGetMimeType", "(J)Ljava/lang/String;", id_get_mime_type);
    vm.register_native(ID, "nGetColorSpace", "(J)Landroid/graphics/ColorSpace;", b_null_ref);
    vm.register_native(ID, "nGetPadding", "(JLandroid/graphics/Rect;)V", id_get_padding);
    vm.register_native(ID, "nClose", "(J)V", id_close);
    Ok(())
}

fn b_zero_long(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i64(ctx, 0);
    Ok(())
}
fn b_noop(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Ok(())
}
fn b_false(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_bool(ctx, false);
    Ok(())
}
fn b_true(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_bool(ctx, true);
    Ok(())
}
fn b_null_ref(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_ref(ctx, 0);
    Ok(())
}
fn b_minus_one(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i32(ctx, -1);
    Ok(())
}

fn b_create(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let colors = vm.arg_ref(ctx, 0);
    let offset = vm.arg_i32(ctx, 1);
    let _stride = vm.arg_i32(ctx, 2);
    let width = vm.arg_i32(ctx, 3);
    let height = vm.arg_i32(ctx, 4);
    let config = BitmapConfig::from_ordinal(vm.arg_i32(ctx, 5))
        .ok_or_else(|| VmError::unsupported_api("Bitmap.Config", "android.graphics.Bitmap"))?;
    let has_alpha = vm.arg_bool(ctx, 6);
    if width <= 0 || height <= 0 {
        return Err(VmError::unsupported_api(
            "Bitmap.createBitmap with non-positive dimensions",
            "android.graphics.Bitmap",
        ));
    }
    let src = int_array(vm, colors);
    let h = with_ctx(|c| {
        let handle = c
            .bitmap_create(width as u32, height as u32, config)
            .map_err(|e| render_err("Bitmap.createBitmap", e))?;
        if let Some(colors) = src {
            let off = offset.max(0) as usize;
            if let Some(bm) = c.bitmaps.get_mut(handle) {
                let px = bm.pixels_mut();
                let n = px.len().min(colors.len().saturating_sub(off));
                for i in 0..n {
                    px[i] = colors[off + i] as u32;
                }
                bm.set_has_alpha(has_alpha);
            }
        }
        Ok::<_, VmError>(handle)
    })?;
    // API 34 `nativeCreate` returns a real `android.graphics.Bitmap`; Java
    // reads `mNativePtr` for `getPixel`/`getPixels` and the NDK host reads it
    // for `AndroidBitmap_*`. ACOS stores the render handle in that long field.
    let obj = vm
        .new_instance("Landroid/graphics/Bitmap;")
        .map_err(|e| VmError::Internal(format!("Bitmap.createBitmap: {e}")))?;
    vm.set_field(obj, "mNativePtr", crate::javabridge::JavaValue::Long(gj(h)))
        .map_err(|e| VmError::Internal(format!("Bitmap.mNativePtr: {e}")))?;
    let _ = vm.set_field(obj, "mWidth", crate::javabridge::JavaValue::Int(width));
    let _ = vm.set_field(obj, "mHeight", crate::javabridge::JavaValue::Int(height));
    let _ = vm.set_field(obj, "mDensity", crate::javabridge::JavaValue::Int(0));
    vm.ret_ref(ctx, obj);
    Ok(())
}

fn b_copy(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let src = bitmap(vm, ctx, 0)?;
    let config = BitmapConfig::from_ordinal(vm.arg_i32(ctx, 1));
    let h = with_ctx(|c| c.bitmap_copy(src, config).map_err(|e| render_err("Bitmap.copy", e)))?;
    vm.ret_i64(ctx, gj(h));
    Ok(())
}

fn b_copy_same(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let src = bitmap(vm, ctx, 0)?;
    let h = with_ctx(|c| c.bitmap_copy(src, None).map_err(|e| render_err("Bitmap.copy", e)))?;
    vm.ret_i64(ctx, gj(h));
    Ok(())
}

fn b_copy_config(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let src = bitmap(vm, ctx, 0)?;
    let config = BitmapConfig::from_ordinal(vm.arg_i32(ctx, 1));
    let h = with_ctx(|c| c.bitmap_copy(src, config).map_err(|e| render_err("Bitmap.copy", e)))?;
    vm.ret_i64(ctx, gj(h));
    Ok(())
}

fn b_config(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let b = bitmap(vm, ctx, 0)?;
    let v = with_ctx(|c| c.bitmap_config(b).map_err(|e| render_err("Bitmap.getConfig", e)))?;
    vm.ret_i32(ctx, v);
    Ok(())
}

fn b_row_bytes(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let b = bitmap(vm, ctx, 0)?;
    let v = with_ctx(|c| c.bitmaps.get(b).map(|x| x.row_bytes() as i32)).unwrap_or(0);
    vm.ret_i32(ctx, v);
    Ok(())
}

fn b_alloc_bytes(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let b = bitmap(vm, ctx, 0)?;
    let v = with_ctx(|c| c.bitmaps.get(b).map(|x| x.allocation_byte_count() as i32)).unwrap_or(0);
    vm.ret_i32(ctx, v);
    Ok(())
}

fn b_generation(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let b = bitmap(vm, ctx, 0)?;
    let v = with_ctx(|c| c.bitmaps.get(b).map(|x| x.generation_id())).unwrap_or(0);
    vm.ret_i32(ctx, v);
    Ok(())
}

fn b_has_alpha(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let b = bitmap(vm, ctx, 0)?;
    let v = with_ctx(|c| c.bitmaps.get(b).map(|x| x.has_alpha())).unwrap_or(true);
    vm.ret_bool(ctx, v);
    Ok(())
}

fn b_set_has_alpha(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let b = bitmap(vm, ctx, 0)?;
    let v = vm.arg_bool(ctx, 1);
    with_ctx(|c| {
        if let Some(x) = c.bitmaps.get_mut(b) {
            x.set_has_alpha(v);
        }
    });
    Ok(())
}

fn b_premultiplied(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let b = bitmap(vm, ctx, 0)?;
    let v = with_ctx(|c| c.bitmaps.get(b).map(|x| x.is_premultiplied())).unwrap_or(true);
    vm.ret_bool(ctx, v);
    Ok(())
}

fn b_set_premultiplied(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let b = bitmap(vm, ctx, 0)?;
    let v = vm.arg_bool(ctx, 1);
    with_ctx(|c| {
        if let Some(x) = c.bitmaps.get_mut(b) {
            x.set_premultiplied(v);
        }
    });
    Ok(())
}

fn b_immutable(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let b = bitmap(vm, ctx, 0)?;
    let v = with_ctx(|c| c.bitmaps.get(b).map(|x| x.is_immutable())).unwrap_or(false);
    vm.ret_bool(ctx, v);
    Ok(())
}

fn b_set_immutable(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let b = bitmap(vm, ctx, 0)?;
    with_ctx(|c| {
        if let Some(x) = c.bitmaps.get_mut(b) {
            x.set_immutable();
        }
    });
    Ok(())
}

fn b_erase(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let b = bitmap(vm, ctx, 0)?;
    let c = vm.arg_i32(ctx, 1) as u32;
    with_ctx(|ctx| ctx.bitmap_erase(b, c).map_err(|e| render_err("Bitmap.eraseColor", e)))?;
    Ok(())
}

fn b_erase_long(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let b = bitmap(vm, ctx, 0)?;
    let c = vm.arg_i64(ctx, 1) as u32;
    with_ctx(|ctx| ctx.bitmap_erase(b, c).map_err(|e| render_err("Bitmap.eraseColor", e)))?;
    Ok(())
}

fn b_get_pixel(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let b = bitmap(vm, ctx, 0)?;
    let (x, y) = (vm.arg_i32(ctx, 1), vm.arg_i32(ctx, 2));
    let c = with_ctx(|c| c.bitmap_get_pixel(b, x, y).map_err(|e| render_err("Bitmap.getPixel", e)))?;
    vm.ret_i32(ctx, c as i32);
    Ok(())
}

fn b_set_pixel(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let b = bitmap(vm, ctx, 0)?;
    let (x, y, c) = (vm.arg_i32(ctx, 1), vm.arg_i32(ctx, 2), vm.arg_i32(ctx, 3) as u32);
    with_ctx(|ctx| ctx.bitmap_set_pixel(b, x, y, c).map_err(|e| render_err("Bitmap.setPixel", e)))?;
    Ok(())
}

fn b_get_pixels(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let b = bitmap(vm, ctx, 0)?;
    let arr = vm.arg_ref(ctx, 1);
    let (offset, _stride, x, y, w, h) = (
        vm.arg_i32(ctx, 2),
        vm.arg_i32(ctx, 3),
        vm.arg_i32(ctx, 4),
        vm.arg_i32(ctx, 5),
        vm.arg_i32(ctx, 6),
        vm.arg_i32(ctx, 7),
    );
    let pixels = with_ctx(|c| c.bitmap_pixels(b).map_err(|e| render_err("Bitmap.getPixels", e)))?;
    let bw = with_ctx(|c| c.bitmaps.get(b).map(|x| x.width())).unwrap_or(0) as i32;
    if let Some(Object::Array(a)) = vm.heap.get_mut(arr) {
        if let ArrayData::Int(dst) = &mut a.data {
            let mut idx = offset.max(0) as usize;
            for row in 0..h.max(0) {
                for col in 0..w.max(0) {
                    let sx = x + col;
                    let sy = y + row;
                    if sx >= 0 && sy >= 0 && sx < bw {
                        let si = (sy * bw + sx) as usize;
                        if si < pixels.len() && idx < dst.len() {
                            dst[idx] = pixels[si] as i32;
                        }
                    }
                    idx += 1;
                }
            }
        }
    }
    Ok(())
}

fn b_set_pixels(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let b = bitmap(vm, ctx, 0)?;
    let arr = vm.arg_ref(ctx, 1);
    let (offset, _stride, x, y, w, h) = (
        vm.arg_i32(ctx, 2),
        vm.arg_i32(ctx, 3),
        vm.arg_i32(ctx, 4),
        vm.arg_i32(ctx, 5),
        vm.arg_i32(ctx, 6),
        vm.arg_i32(ctx, 7),
    );
    let src = int_array(vm, arr).unwrap_or_default();
    let bw = with_ctx(|c| c.bitmaps.get(b).map(|x| x.width())).unwrap_or(0) as i32;
    let bh = with_ctx(|c| c.bitmaps.get(b).map(|x| x.height())).unwrap_or(0) as i32;
    with_ctx(|c| {
        if let Some(x_bm) = c.bitmaps.get_mut(b) {
            let px = x_bm.pixels_mut();
            let mut idx = offset.max(0) as usize;
            for row in 0..h.max(0) {
                for col in 0..w.max(0) {
                    let dx = x + col;
                    let dy = y + row;
                    if dx >= 0 && dy >= 0 && dx < bw && dy < bh && idx < src.len() {
                        let di = (dy * bw + dx) as usize;
                        if di < px.len() {
                            px[di] = src[idx] as u32;
                        }
                    }
                    idx += 1;
                }
            }
        }
    });
    Ok(())
}

fn b_recycle(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let b = bitmap(vm, ctx, 0)?;
    with_ctx(|c| {
        let _ = c.bitmaps.remove(b);
    });
    Ok(())
}

fn b_same_as(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let a = bitmap(vm, ctx, 0)?;
    let b = bitmap(vm, ctx, 1)?;
    let same = with_ctx(|c| match (c.bitmaps.get(a), c.bitmaps.get(b)) {
        (Some(x), Some(y)) => x.width() == y.width() && x.height() == y.height() && x.pixels() == y.pixels(),
        _ => false,
    });
    vm.ret_bool(ctx, same);
    Ok(())
}

fn b_extract_alpha(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let src = bitmap(vm, ctx, 0)?;
    let h = with_ctx(|c| c.bitmap_extract_alpha(src).map_err(|e| render_err("Bitmap.extractAlpha", e)))?;
    vm.ret_i64(ctx, gj(h));
    Ok(())
}

fn b_get_color(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    // `Bitmap.getColor(x,y)` returns a packed color long; ACOS surfaces the
    // sRGB ARGB in the low 32 bits (the high bits are the colorspace encoding).
    let b = bitmap(vm, ctx, 0)?;
    let (x, y) = (vm.arg_i32(ctx, 1), vm.arg_i32(ctx, 2));
    let c = with_ctx(|c| c.bitmap_get_pixel(b, x, y).map_err(|e| render_err("Bitmap.getColor", e)))?;
    vm.ret_i64(ctx, c as i64);
    Ok(())
}

// ---------------------------------------------------------------------------
// ImageDecoder / BitmapFactory decode natives
// ---------------------------------------------------------------------------

fn bridge(e: crate::javabridge::BridgeError) -> VmError {
    match e {
        crate::javabridge::BridgeError::Vm(e) => e,
        other => VmError::Internal(other.to_string()),
    }
}

/// Wrap an `acos-render` bitmap handle in an `android.graphics.Bitmap` object.
fn wrap_bitmap(
    vm: &mut Vm,
    h: acos_render::Handle,
    nine_patch: Option<&[u8]>,
) -> VmResult<u32> {
    let (w, ht) = with_ctx(|c| {
        c.bitmaps
            .get(h)
            .map(|b| (b.width() as i32, b.height() as i32))
            .unwrap_or((0, 0))
    });
    let obj = vm.new_instance("Landroid/graphics/Bitmap;").map_err(bridge)?;
    vm.set_field(obj, "mNativePtr", JavaValue::Long(gj(h))).map_err(bridge)?;
    let _ = vm.set_field(obj, "mWidth", JavaValue::Int(w));
    let _ = vm.set_field(obj, "mHeight", JavaValue::Int(ht));
    let _ = vm.set_field(obj, "mDensity", JavaValue::Int(0));
    if let Some(chunk) = nine_patch {
        let arr = vm.alloc(Object::Array(ArrayObj {
            elem_desc: "B".to_string(),
            data: ArrayData::Byte(chunk.iter().map(|&b| b as i8).collect()),
        }))?;
        let _ = vm.set_field(obj, "mNinePatchChunk", JavaValue::Object(arr));
    }
    Ok(obj)
}

/// Extract the `npTc` (nine-patch) payload from a PNG byte stream.
///
/// aapt/aapt2 store the serialized `Res_png_9patch` in a PNG ancillary chunk
/// named `npTc`. `BitmapFactory`'s native decoder copies that payload into
/// `Bitmap.mNinePatchChunk`, and `Drawable.createFromResourceStream` uses it to
/// build a `NinePatchDrawable`. Without it an XML `<nine-patch>` drawable
/// throws `<nine-patch> requires a valid 9-patch source image`.
fn png_nine_patch_chunk(bytes: &[u8]) -> Option<Vec<u8>> {
    const SIG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    if bytes.len() < 8 || bytes[..8] != SIG {
        return None;
    }
    let mut pos = 8usize;
    while pos + 8 <= bytes.len() {
        let len = u32::from_be_bytes([
            bytes[pos],
            bytes[pos + 1],
            bytes[pos + 2],
            bytes[pos + 3],
        ]) as usize;
        let ty = &bytes[pos + 4..pos + 8];
        let data_start = pos + 8;
        let Some(data_end) = data_start.checked_add(len) else {
            break;
        };
        if data_end + 4 > bytes.len() {
            break;
        }
        if ty == b"npTc" {
            return Some(bytes[data_start..data_end].to_vec());
        }
        if ty == b"IEND" {
            break;
        }
        pos = data_end + 4;
    }
    None
}

fn byte_array_prefix(vm: &Vm, h: u32, n: usize) -> Vec<u8> {
    match vm.heap.get(h) {
        Some(Object::Array(a)) => match &a.data {
            ArrayData::Byte(v) => v.iter().take(n).map(|b| *b as u8).collect(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

/// Read a Java `InputStream` to EOF using the caller's scratch byte array.
fn read_stream(vm: &mut Vm, stream: u32, storage: u32) -> VmResult<Vec<u8>> {
    let buf = if storage != 0 && vm.heap.get(storage).is_some() {
        storage
    } else {
        vm.new_array_empty("B", 16384).map_err(bridge)?
    };
    let cap = match vm.heap.get(buf) {
        Some(Object::Array(a)) => a.len(),
        _ => 0,
    };
    if cap == 0 {
        return Err(VmError::Internal("ImageDecoder: empty scratch buffer".into()));
    }
    let mut out = Vec::new();
    loop {
        let n = vm
            .call_method(stream, "read", "([B)I", &[JavaValue::Object(buf)])
            .map_err(bridge)?;
        let n = match n {
            JavaValue::Int(n) => n,
            _ => return Err(VmError::Internal("InputStream.read returned non-int".into())),
        };
        if n <= 0 {
            break;
        }
        out.extend_from_slice(&byte_array_prefix(vm, buf, n as usize));
        if out.len() > 512 * 1024 * 1024 {
            return Err(VmError::unsupported_api(
                "ImageDecoder: stream larger than 512 MiB",
                "android.graphics.ImageDecoder",
            ));
        }
    }
    Ok(out)
}

fn register_decoder(bytes: Vec<u8>, format: ImageFormat, width: i32, height: i32, animated: bool) -> u64 {
    let handle = NEXT_DECODER.with(|c| {
        let h = c.get();
        c.set(h + 1);
        h
    });
    DECODERS.with(|d| {
        d.borrow_mut().insert(
            handle,
            DecoderState {
                bytes,
                format,
                width,
                height,
                animated,
            },
        );
    });
    handle
}

fn decoder_state(handle: u64) -> Option<(Vec<u8>, ImageFormat, i32, i32, bool)> {
    DECODERS.with(|d| {
        d.borrow()
            .get(&handle)
            .map(|s| (s.bytes.clone(), s.format, s.width, s.height, s.animated))
    })
}

fn new_decoder_object(vm: &mut Vm, handle: u64, width: i32, height: i32, animated: bool) -> VmResult<u32> {
    vm.new_instance_ctor(
        "Landroid/graphics/ImageDecoder;",
        "(JIIZZ)V",
        &[
            JavaValue::Long(handle as i64),
            JavaValue::Int(width),
            JavaValue::Int(height),
            JavaValue::Boolean(false),
            JavaValue::Boolean(animated),
        ],
    )
    .map_err(bridge)
}

fn decode_bytes(bytes: &[u8]) -> VmResult<(ImageFormat, i32, i32)> {
    let img = acos_render::image::decode(bytes).map_err(|e| {
        VmError::unsupported_api(format!("ImageDecoder decode: {e}"), "acos-render")
    })?;
    Ok((acos_render::image::sniff(bytes), img.width as i32, img.height as i32))
}

fn id_create_stream(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let stream = vm.arg_ref(ctx, 0);
    let storage = vm.arg_ref(ctx, 1);
    let bytes = read_stream(vm, stream, storage)?;
    let (format, width, height) = decode_bytes(&bytes)?;
    let handle = register_decoder(bytes, format, width, height, false);
    let obj = new_decoder_object(vm, handle, width, height, false)?;
    vm.ret_ref(ctx, obj);
    Ok(())
}

fn id_create_bytes(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let arr = vm.arg_ref(ctx, 0);
    let offset = vm.arg_i32(ctx, 1).max(0) as usize;
    let length = vm.arg_i32(ctx, 2).max(0) as usize;
    let all = byte_array_prefix(vm, arr, usize::MAX);
    let end = (offset + length).min(all.len());
    let slice = if offset <= all.len() {
        all[offset..end].to_vec()
    } else {
        Vec::new()
    };
    let (format, width, height) = decode_bytes(&slice)?;
    let handle = register_decoder(slice, format, width, height, false);
    let obj = new_decoder_object(vm, handle, width, height, false)?;
    vm.ret_ref(ctx, obj);
    Ok(())
}

fn id_create_unsupported(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_ref(ctx, 0);
    Ok(())
}

fn id_decode_bitmap(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 0) as u64;
    let (bytes, _format, _w, _h, _animated) = decoder_state(handle).ok_or_else(|| {
        VmError::StaleHandle {
            what: format!("ImageDecoder handle {handle:#x}"),
        }
    })?;
    let h = with_ctx(|c| c.bitmap_create_from_bytes(&bytes))
        .map_err(|e| render_err("ImageDecoder.decodeBitmap", e))?;
    let chunk = png_nine_patch_chunk(&bytes);
    let obj = wrap_bitmap(vm, h, chunk.as_deref())?;
    vm.ret_ref(ctx, obj);
    Ok(())
}

fn id_get_sampled_size(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 0) as u64;
    let sample = vm.arg_i32(ctx, 1).max(1);
    let (_bytes, _format, w, h, _animated) = decoder_state(handle).ok_or_else(|| {
        VmError::StaleHandle {
            what: format!("ImageDecoder handle {handle:#x}"),
        }
    })?;
    let sw = (w / sample).max(1);
    let sh = (h / sample).max(1);
    let size = vm
        .new_instance_ctor(
            "Landroid/util/Size;",
            "(II)V",
            &[JavaValue::Int(sw), JavaValue::Int(sh)],
        )
        .map_err(bridge)?;
    vm.ret_ref(ctx, size);
    Ok(())
}

fn id_get_mime_type(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 0) as u64;
    let mime = decoder_state(handle)
        .map(|(_, f, _, _, _)| mime_for(f))
        .unwrap_or("image/*");
    let s = vm.new_string(mime)?;
    vm.ret_ref(ctx, s);
    Ok(())
}

fn id_get_padding(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let rect = vm.arg_ref(ctx, 1);
    if rect != 0 {
        for f in ["left", "top", "right", "bottom"] {
            let _ = vm.set_field(rect, f, JavaValue::Int(0));
        }
    }
    Ok(())
}

fn id_close(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 0) as u64;
    DECODERS.with(|d| {
        d.borrow_mut().remove(&handle);
    });
    Ok(())
}

// BitmapFactory
fn options_just_bounds(vm: &mut Vm, opts: u32) -> bool {
    if opts == 0 {
        return false;
    }
    matches!(vm.get_field(opts, "inJustDecodeBounds"), Ok(JavaValue::Boolean(true)))
}

fn set_options_outputs(vm: &mut Vm, opts: u32, w: i32, h: i32, format: ImageFormat) {
    if opts == 0 {
        return;
    }
    let _ = vm.set_field(opts, "outWidth", JavaValue::Int(w));
    let _ = vm.set_field(opts, "outHeight", JavaValue::Int(h));
    let _ = vm.set_field(opts, "outConfig", JavaValue::Int(BitmapConfig::Rgba8888 as i32));
    if let Ok(mime) = vm.new_string(mime_for(format)) {
        let _ = vm.set_field(opts, "outMimeType", JavaValue::Object(mime));
    }
}

fn bf_decode_bytes(vm: &mut Vm, ctx: NativeCtx, bytes: Vec<u8>) -> VmResult<()> {
    let opts = vm.arg_ref(ctx, 3);
    let (format, w, h) = decode_bytes(&bytes)?;
    set_options_outputs(vm, opts, w, h, format);
    if options_just_bounds(vm, opts) {
        vm.ret_ref(ctx, 0);
        return Ok(());
    }
    let hnd = with_ctx(|c| c.bitmap_create_from_bytes(&bytes))
        .map_err(|e| render_err("BitmapFactory.decode", e))?;
    let chunk = png_nine_patch_chunk(&bytes);
    let obj = wrap_bitmap(vm, hnd, chunk.as_deref())?;
    vm.ret_ref(ctx, obj);
    Ok(())
}

fn bf_decode_byte_array(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let arr = vm.arg_ref(ctx, 0);
    let offset = vm.arg_i32(ctx, 1).max(0) as usize;
    let length = vm.arg_i32(ctx, 2).max(0) as usize;
    let all = byte_array_prefix(vm, arr, usize::MAX);
    let end = (offset + length).min(all.len());
    let slice = if offset <= all.len() {
        all[offset..end].to_vec()
    } else {
        Vec::new()
    };
    bf_decode_bytes(vm, ctx, slice)
}

fn bf_decode_stream(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let stream = vm.arg_ref(ctx, 0);
    let storage = vm.arg_ref(ctx, 1);
    let bytes = read_stream(vm, stream, storage)?;
    bf_decode_bytes(vm, ctx, bytes)
}
