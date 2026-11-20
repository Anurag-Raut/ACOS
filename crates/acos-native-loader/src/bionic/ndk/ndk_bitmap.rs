//! `libjnigraphics`: `AndroidBitmap_getInfo` / `lockPixels` / `unlockPixels`.
//!
//! The pixel buffer is the one `acos-render::Bitmap` owns for the Java
//! `android.graphics.Bitmap` object: `lockPixels` hands out the bitmap's own
//! RGBA_8888 storage, so writes made by native code are exactly what
//! `Bitmap.getPixel` (the `nativeGetPixel` slot) reads back, and `unlockPixels`
//! bumps the bitmap's generation id like every other mutation.
//!
//! Only the configs whose in-memory layout *is* the NDK format are lockable
//! (RGBA_8888 / RGBX_8888). A `RGB_565`/`ALPHA_8`/`RGBA_F16` bitmap has a
//! different physical layout in ACOS, so locking it returns
//! `ANDROID_BITMAP_RESULT_BAD_PARAMETER` with a structured diagnostic rather
//! than a pointer to bytes of the wrong format.

use std::os::raw::{c_int, c_void};

use super::{host_failed, with_host, BitmapInfo};

const LIB: &str = "libjnigraphics";

/// `ANDROID_BITMAP_RESULT_SUCCESS`.
pub const ANDROID_BITMAP_RESULT_SUCCESS: c_int = 0;
/// `ANDROID_BITMAP_RESULT_BAD_PARAMETER`.
pub const ANDROID_BITMAP_RESULT_BAD_PARAMETER: c_int = -1;

/// `AndroidBitmapInfo` (NDK layout: five `uint32_t`s).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct AndroidBitmapInfo {
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub format: u32,
    pub flags: u32,
}

fn fill(out: *mut AndroidBitmapInfo, info: &BitmapInfo) -> c_int {
    if out.is_null() {
        return ANDROID_BITMAP_RESULT_BAD_PARAMETER;
    }
    // SAFETY: the caller passed a writable AndroidBitmapInfo by contract.
    unsafe {
        out.write(AndroidBitmapInfo {
            width: info.width,
            height: info.height,
            stride: info.stride,
            format: info.format,
            flags: info.flags,
        });
    }
    ANDROID_BITMAP_RESULT_SUCCESS
}

/// `AndroidBitmap_getInfo(JNIEnv*, jobject bitmap, AndroidBitmapInfo* info)`.
///
/// # Safety
/// `bitmap` must be a JNI reference; `info` must be writable.
pub unsafe extern "C" fn androidbitmap_getinfo(
    env: *mut c_void,
    bitmap: *mut c_void,
    info: *mut AndroidBitmapInfo,
) -> c_int {
    match with_host(|h| h.bitmap_info(env as usize, bitmap as usize)) {
        Ok(i) => fill(info, &i),
        Err(e) => host_failed(
            LIB,
            "AndroidBitmap_getInfo",
            e,
            ANDROID_BITMAP_RESULT_BAD_PARAMETER,
        ),
    }
}

/// `AndroidBitmap_lockPixels(JNIEnv*, jobject bitmap, void** addr)`.
///
/// # Safety
/// `bitmap` must be a JNI reference; `addr` must be writable.
pub unsafe extern "C" fn androidbitmap_lockpixels(
    env: *mut c_void,
    bitmap: *mut c_void,
    addr: *mut *mut c_void,
) -> c_int {
    if addr.is_null() {
        return ANDROID_BITMAP_RESULT_BAD_PARAMETER;
    }
    match with_host(|h| h.bitmap_lock(env as usize, bitmap as usize)) {
        Ok(i) => {
            addr.write(i.pixels as *mut c_void);
            ANDROID_BITMAP_RESULT_SUCCESS
        }
        Err(e) => host_failed(
            LIB,
            "AndroidBitmap_lockPixels",
            e,
            ANDROID_BITMAP_RESULT_BAD_PARAMETER,
        ),
    }
}

/// `AndroidBitmap_unlockPixels(JNIEnv*, jobject bitmap)`.
///
/// # Safety
/// `bitmap` must be a JNI reference previously locked.
pub unsafe extern "C" fn androidbitmap_unlockpixels(
    env: *mut c_void,
    bitmap: *mut c_void,
) -> c_int {
    match with_host(|h| h.bitmap_unlock(env as usize, bitmap as usize)) {
        Ok(()) => ANDROID_BITMAP_RESULT_SUCCESS,
        Err(e) => host_failed(
            LIB,
            "AndroidBitmap_unlockPixels",
            e,
            ANDROID_BITMAP_RESULT_BAD_PARAMETER,
        ),
    }
}

/// Table entries for `libjnigraphics`.
pub fn table() -> Vec<(&'static str, *const c_void)> {
    vec![
        (
            "AndroidBitmap_getInfo",
            androidbitmap_getinfo as *const c_void,
        ),
        (
            "AndroidBitmap_lockPixels",
            androidbitmap_lockpixels as *const c_void,
        ),
        (
            "AndroidBitmap_unlockPixels",
            androidbitmap_unlockpixels as *const c_void,
        ),
    ]
}
