//! `android.graphics.Bitmap` natives over `acos-render`'s pixel store.
//!
//! `acos-render::Bitmap` already owns a width/height/config/alpha pixel buffer
//! (`bitmap.rs`); this module maps the framework's `jlong` native pointer onto
//! its generation-checked handle. Ashmem is not modelled — ACOS renders
//! offscreen (ADR-0011) — so the ashmem entry points are plain copies and
//! `nativeIsBackedByAshmem` is honestly `false`; the ones that need a real
//! `ashmem` fd or hardware buffer are left unregistered and surface as
//! `UNSUPPORTED_JAVA_API`.

use acos_render::BitmapConfig;

use crate::class::NativeCtx;
use crate::error::{VmError, VmResult};
use crate::heap::{ArrayData, Object};
use crate::natives::gfx_state::{gj, render_err, rh, stale, with_ctx};
use crate::vm::Vm;

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
