//! `libandroid` native window API (`ANativeWindow_*`).
//!
//! On Android an `ANativeWindow` is the producer side of a `Surface`: the
//! native code dequeues a software buffer, draws into it and posts it to
//! SurfaceFlinger. ACOS has no SurfaceFlinger (ADR-0008); the producer side is
//! the WMS `Surface` record plus the runtime's completed-frame store. The
//! loader keeps only the opaque NDK handle and forwards every operation to the
//! host, which backs `lock` with a real software buffer of the requested
//! geometry and `unlockAndPost` with a frame publication.
//!
//! Formats: `WINDOW_FORMAT_RGBA_8888` (1) and `WINDOW_FORMAT_RGB_565` (4) are
//! the only software formats the NDK defines. The host reports the stride it
//! actually allocated, so a guest that honours `ANativeWindow_Buffer.stride`
//! sees the same layout the frame store publishes.

use std::os::raw::{c_char, c_int, c_void};

use super::{handle, handle_id, host_failed, with_host, ARect, WindowBuffer};

const LIB: &str = "libandroid";

/// `WINDOW_FORMAT_RGBA_8888`.
pub const WINDOW_FORMAT_RGBA_8888: c_int = 1;
/// `WINDOW_FORMAT_RGBX_8888`.
pub const WINDOW_FORMAT_RGBX_8888: c_int = 2;
/// `WINDOW_FORMAT_RGB_565`.
pub const WINDOW_FORMAT_RGB_565: c_int = 4;

fn rect(p: *const ARect) -> Option<ARect> {
    if p.is_null() {
        None
    } else {
        // SAFETY: the NDK contract is a readable ARect or NULL.
        Some(unsafe { *p })
    }
}

/// `ANativeWindow_fromSurface(JNIEnv*, jobject surface)`.
///
/// # Safety
/// `surface` must be a JNI reference to an `android.view.Surface`.
pub unsafe extern "C" fn anativewindow_fromsurface(
    env: *mut c_void,
    surface: *mut c_void,
) -> *mut c_void {
    match with_host(|h| h.window_from_surface(env as usize, surface as usize)) {
        Ok(id) => handle(id),
        Err(e) => host_failed(LIB, "ANativeWindow_fromSurface", e, std::ptr::null_mut()),
    }
}

/// `ANativeWindow_toSurface(JNIEnv*, ANativeWindow*)`.
///
/// # Safety
/// `window` must be a live window handle.
pub unsafe extern "C" fn anativewindow_tosurface(
    env: *mut c_void,
    window: *mut c_void,
) -> *mut c_void {
    let Some(id) = handle_id(window) else { return std::ptr::null_mut() };
    match with_host(|h| h.window_to_surface(env as usize, id)) {
        Ok(sid) => handle(sid),
        Err(e) => host_failed(LIB, "ANativeWindow_toSurface", e, std::ptr::null_mut()),
    }
}

/// `ANativeWindow_acquire(ANativeWindow*)`.
///
/// # Safety
/// `window` must be a live window handle.
pub unsafe extern "C" fn anativewindow_acquire(window: *mut c_void) {
    let Some(id) = handle_id(window) else { return };
    if let Err(e) = with_host(|h| h.window_acquire(id)) {
        host_failed::<()>(LIB, "ANativeWindow_acquire", e, ());
    }
}

/// `ANativeWindow_release(ANativeWindow*)`.
///
/// # Safety
/// `window` must be a live window handle; it may be invalid afterwards.
pub unsafe extern "C" fn anativewindow_release(window: *mut c_void) {
    let Some(id) = handle_id(window) else { return };
    if let Err(e) = with_host(|h| h.window_release(id)) {
        host_failed::<()>(LIB, "ANativeWindow_release", e, ());
    }
}

fn info(window: *mut c_void, what: &str, pick: impl Fn(&super::WindowInfo) -> c_int) -> c_int {
    let Some(id) = handle_id(window) else { return 0 };
    match with_host(|h| h.window_info(id)) {
        Ok(i) => pick(&i),
        Err(e) => host_failed(LIB, what, e, 0),
    }
}

/// `ANativeWindow_getWidth(ANativeWindow*)`.
///
/// # Safety
/// `window` must be a live window handle.
pub unsafe extern "C" fn anativewindow_getwidth(window: *mut c_void) -> c_int {
    info(window, "ANativeWindow_getWidth", |i| i.width)
}

/// `ANativeWindow_getHeight(ANativeWindow*)`.
///
/// # Safety
/// `window` must be a live window handle.
pub unsafe extern "C" fn anativewindow_getheight(window: *mut c_void) -> c_int {
    info(window, "ANativeWindow_getHeight", |i| i.height)
}

/// `ANativeWindow_getFormat(ANativeWindow*)`.
///
/// # Safety
/// `window` must be a live window handle.
pub unsafe extern "C" fn anativewindow_getformat(window: *mut c_void) -> c_int {
    info(window, "ANativeWindow_getFormat", |i| i.format)
}

/// `ANativeWindow_setBuffersGeometry(ANativeWindow*, int w, int h, int format)`.
///
/// # Safety
/// `window` must be a live window handle.
pub unsafe extern "C" fn anativewindow_setbuffersgeometry(
    window: *mut c_void,
    width: c_int,
    height: c_int,
    format: c_int,
) -> c_int {
    let Some(id) = handle_id(window) else { return -1 };
    match with_host(|h| h.window_set_geometry(id, width, height, format)) {
        Ok(()) => 0,
        Err(e) => host_failed(LIB, "ANativeWindow_setBuffersGeometry", e, -1),
    }
}

/// `ANativeWindow_setBuffersDataSpace(ANativeWindow*, int32_t)`.
///
/// ACOS's software buffers are always sRGB-ish RGBA/565; a non-default data
/// space cannot be honoured and is reported instead of silently ignored.
///
/// # Safety
/// `window` must be a live window handle.
pub unsafe extern "C" fn anativewindow_setbuffersdataspace(
    window: *mut c_void,
    _dataspace: i32,
) -> c_int {
    let Some(id) = handle_id(window) else { return -1 };
    match with_host(|h| h.window_info(id)) {
        Ok(_) => super::unsupported(
            LIB,
            "ANativeWindow_setBuffersDataSpace",
            "ACOS software buffers are always sRGB; no data-space conversion",
            -1,
        ),
        Err(e) => host_failed(LIB, "ANativeWindow_setBuffersDataSpace", e, -1),
    }
}

/// `ANativeWindow_lock(ANativeWindow*, ANativeWindow_Buffer* out, ARect* dirty)`.
///
/// # Safety
/// `out` must be writable; `dirty` is an optional readable `ARect`.
pub unsafe extern "C" fn anativewindow_lock(
    window: *mut c_void,
    out: *mut WindowBuffer,
    dirty: *const ARect,
) -> c_int {
    if out.is_null() {
        return -1;
    }
    let Some(id) = handle_id(window) else { return -1 };
    match with_host(|h| h.window_lock(id, rect(dirty))) {
        Ok(buf) => {
            out.write(buf);
            0
        }
        Err(e) => host_failed(LIB, "ANativeWindow_lock", e, -1),
    }
}

/// `ANativeWindow_unlockAndPost(ANativeWindow*)`.
///
/// # Safety
/// `window` must be locked.
pub unsafe extern "C" fn anativewindow_unlockandpost(window: *mut c_void) -> c_int {
    let Some(id) = handle_id(window) else { return -1 };
    match with_host(|h| h.window_unlock_and_post(id)) {
        Ok(()) => 0,
        Err(e) => host_failed(LIB, "ANativeWindow_unlockAndPost", e, -1),
    }
}

/// Unimplemented hardware-buffer path (`ANativeWindow_setBuffersTransform`).
///
/// # Safety
/// Always safe; reports and returns the documented error.
pub unsafe extern "C" fn anativewindow_setbufferstransform(
    _window: *mut c_void,
    _transform: i32,
) -> c_int {
    super::unsupported(
        LIB,
        "ANativeWindow_setBuffersTransform",
        "no hardware composer; software buffers have no transform",
        -1,
    )
}

/// Table entries for the window API.
pub fn table() -> Vec<(&'static str, *const c_void)> {
    let _ = WINDOW_FORMAT_RGBA_8888;
    let _ = WINDOW_FORMAT_RGBX_8888;
    let _ = WINDOW_FORMAT_RGB_565;
    let _ = std::mem::size_of::<c_char>();
    vec![
        (
            "ANativeWindow_fromSurface",
            anativewindow_fromsurface as *const c_void,
        ),
        (
            "ANativeWindow_toSurface",
            anativewindow_tosurface as *const c_void,
        ),
        ("ANativeWindow_acquire", anativewindow_acquire as *const c_void),
        ("ANativeWindow_release", anativewindow_release as *const c_void),
        ("ANativeWindow_getWidth", anativewindow_getwidth as *const c_void),
        (
            "ANativeWindow_getHeight",
            anativewindow_getheight as *const c_void,
        ),
        (
            "ANativeWindow_getFormat",
            anativewindow_getformat as *const c_void,
        ),
        (
            "ANativeWindow_setBuffersGeometry",
            anativewindow_setbuffersgeometry as *const c_void,
        ),
        (
            "ANativeWindow_setBuffersDataSpace",
            anativewindow_setbuffersdataspace as *const c_void,
        ),
        (
            "ANativeWindow_setBuffersTransform",
            anativewindow_setbufferstransform as *const c_void,
        ),
        ("ANativeWindow_lock", anativewindow_lock as *const c_void),
        (
            "ANativeWindow_unlockAndPost",
            anativewindow_unlockandpost as *const c_void,
        ),
    ]
}
