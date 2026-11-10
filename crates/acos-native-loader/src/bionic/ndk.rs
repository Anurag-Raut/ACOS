//! NDK platform surface (`libandroid`, `liblog`, `libjnigraphics`).
//!
//! Native engines (React Native modules, the Flutter engine, ML Kit, image
//! pipelines) import the public NDK C API directly. On a device those symbols
//! live in `libandroid.so` / `libjnigraphics.so` and are backed by platform
//! state: the APK asset manager, `Bitmap` pixel storage, window surfaces and
//! the vsync source.
//!
//! ACOS has that state, but it lives *outside* this crate (`acos-framework-
//! natives` assets, `acos-render` bitmaps, the WMS surface + completed-frame
//! store, the VM configuration). The loader therefore never fakes an NDK call:
//! every entry point in these modules forwards to a [`NdkHost`] installed by
//! the embedding runtime (`acos-jni` in production, a test host in the
//! integration tests). Without a host — or for behaviour a host cannot honour —
//! the call fails with a structured diagnostic naming the library and symbol
//! and returns the documented error value (`NULL`, `-1`, `BAD_PARAMETER`,
//! `EGL_NO_DISPLAY`). It never returns a plausible-looking success.

use std::collections::HashSet;
use std::os::raw::c_void;
use std::sync::{Arc, Mutex, OnceLock, RwLock};

pub mod ndk_asset;
pub mod ndk_bitmap;
pub mod ndk_gl;
pub mod ndk_looper;
pub mod ndk_misc;
pub mod ndk_window;

/// Result of a host-backed NDK operation. `Err` carries the structured reason
/// that is reported to the caller/log; it is never a silent success.
pub type NdkResult<T> = Result<T, String>;

/// `ANativeWindow_Buffer` metadata + pixel address. `repr(C)` is required: the
/// guest writes through this exact layout (NDK: 4×`int32_t`, `void*`, reserved).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct WindowBuffer {
    pub width: i32,
    pub height: i32,
    pub stride: i32,
    pub format: i32,
    pub bits: *mut u8,
}

/// `ANativeWindow` geometry (WINDOW_FORMAT_RGBA_8888 = 1, RGB_565 = 4).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct WindowInfo {
    pub width: i32,
    pub height: i32,
    pub format: i32,
}

/// `AndroidBitmapInfo` + the locked pixel address (set only by `lockPixels`).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct BitmapInfo {
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub format: u32,
    pub flags: u32,
    pub pixels: *mut u8,
}

/// `ARect` as used by `ANativeWindow_lock`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct ARect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

/// Configuration values the NDK `AConfiguration` surface reports.
#[derive(Clone, Debug, Default)]
pub struct NdkConfiguration {
    pub mcc: i32,
    pub mnc: i32,
    pub locale: String,
    pub orientation: i32,
    pub touchscreen: i32,
    pub density: i32,
    pub keyboard: i32,
    pub keyboard_hidden: i32,
    pub navigation: i32,
    pub screen_width: i32,
    pub screen_height: i32,
    pub screen_layout: i32,
    pub ui_mode: i32,
    pub smallest_screen_width_dp: i32,
    pub screen_width_dp: i32,
    pub screen_height_dp: i32,
    pub color_mode: i32,
}

/// A frame clock callback posted through [`AChoreographer_postFrameCallback`].
pub type FrameCallback = Option<unsafe extern "C" fn(i64, *mut c_void)>;

/// Host implementation of the NDK surfaces that need ACOS state.
///
/// Every method has a default that fails with a structured reason, so a host
/// only implements what it can genuinely honour. The loader logs that reason
/// and returns the documented error value; it never substitutes a fake success.
pub trait NdkHost: Send + Sync {
    // --- libandroid: AssetManager -----------------------------------------
    /// `AAssetManager_fromJava`: resolve the Java `AssetManager` object.
    fn asset_manager_from_java(&self, _env: usize, _manager: usize) -> NdkResult<u64> {
        Err("AAssetManager_fromJava: host does not expose the asset manager".into())
    }
    fn asset_open(&self, _manager: u64, _filename: &str, _mode: i32) -> NdkResult<u64> {
        Err("AAssetManager_open: host does not expose APK assets".into())
    }
    fn asset_read(&self, _asset: u64, _buf: &mut [u8]) -> NdkResult<usize> {
        Err("AAsset_read: host does not expose APK assets".into())
    }
    fn asset_seek(&self, _asset: u64, _offset: i64, _whence: i32) -> NdkResult<i64> {
        Err("AAsset_seek: host does not expose APK assets".into())
    }
    fn asset_length(&self, _asset: u64) -> NdkResult<i64> {
        Err("AAsset_getLength: host does not expose APK assets".into())
    }
    fn asset_is_allocated(&self, _asset: u64) -> NdkResult<bool> {
        Err("AAsset_isAllocated: host does not expose APK assets".into())
    }
    /// `AAsset_getBuffer`: a pointer that stays valid until `AAsset_close`.
    fn asset_buffer(&self, _asset: u64) -> NdkResult<*const c_void> {
        Err("AAsset_getBuffer: host does not expose APK assets".into())
    }
    fn asset_remaining(&self, _asset: u64) -> NdkResult<i64> {
        Err("AAsset_getRemainingLength: host does not expose APK assets".into())
    }
    fn asset_close(&self, _asset: u64) -> NdkResult<()> {
        Err("AAsset_close: host does not expose APK assets".into())
    }
    fn asset_dir_open(&self, _manager: u64, _dir: &str) -> NdkResult<u64> {
        Err("AAssetManager_openDir: host does not expose APK assets".into())
    }
    fn asset_dir_next(&self, _dir: u64) -> NdkResult<Option<String>> {
        Err("AAssetDir_getNextFileName: host does not expose APK assets".into())
    }
    fn asset_dir_rewind(&self, _dir: u64) -> NdkResult<()> {
        Err("AAssetDir_rewind: host does not expose APK assets".into())
    }
    fn asset_dir_close(&self, _dir: u64) -> NdkResult<()> {
        Err("AAssetDir_close: host does not expose APK assets".into())
    }

    // --- libjnigraphics ----------------------------------------------------
    fn bitmap_info(&self, _env: usize, _bitmap: usize) -> NdkResult<BitmapInfo> {
        Err("AndroidBitmap_getInfo: host does not expose Bitmap storage".into())
    }
    fn bitmap_lock(&self, _env: usize, _bitmap: usize) -> NdkResult<BitmapInfo> {
        Err("AndroidBitmap_lockPixels: host does not expose Bitmap storage".into())
    }
    fn bitmap_unlock(&self, _env: usize, _bitmap: usize) -> NdkResult<()> {
        Err("AndroidBitmap_unlockPixels: host does not expose Bitmap storage".into())
    }

    // --- libandroid: ANativeWindow ----------------------------------------
    fn window_from_surface(&self, _env: usize, _surface: usize) -> NdkResult<u64> {
        Err("ANativeWindow_fromSurface: host does not expose WMS surfaces".into())
    }
    fn window_acquire(&self, _window: u64) -> NdkResult<()> {
        Err("ANativeWindow_acquire: host does not expose WMS surfaces".into())
    }
    fn window_release(&self, _window: u64) -> NdkResult<()> {
        Err("ANativeWindow_release: host does not expose WMS surfaces".into())
    }
    fn window_info(&self, _window: u64) -> NdkResult<WindowInfo> {
        Err("ANativeWindow_getWidth/Height/Format: unknown window".into())
    }
    fn window_set_geometry(
        &self,
        _window: u64,
        _width: i32,
        _height: i32,
        _format: i32,
    ) -> NdkResult<()> {
        Err("ANativeWindow_setBuffersGeometry: host does not expose WMS surfaces".into())
    }
    fn window_lock(&self, _window: u64, _dirty: Option<ARect>) -> NdkResult<WindowBuffer> {
        Err("ANativeWindow_lock: host does not expose WMS surfaces".into())
    }
    fn window_unlock_and_post(&self, _window: u64) -> NdkResult<()> {
        Err("ANativeWindow_unlockAndPost: host does not expose WMS surfaces".into())
    }
    fn window_to_surface(&self, _env: usize, _window: u64) -> NdkResult<u64> {
        Err("ANativeWindow_toSurface: host does not expose WMS surfaces".into())
    }

    // --- libandroid: configuration / vsync ---------------------------------
    fn configuration(&self) -> NdkResult<NdkConfiguration> {
        Err("AConfiguration_*: host does not expose the system Configuration".into())
    }
    /// Arm a frame-clock callback; the host invokes `cb` on its next vsync.
    fn choreographer_post(
        &self,
        _delay_nanos: i64,
        _cb: FrameCallback,
        _data: *mut c_void,
    ) -> NdkResult<u64> {
        Err("AChoreographer_postFrameCallback: host has no vsync source".into())
    }
    fn choreographer_remove(&self, _token: u64) -> NdkResult<()> {
        Err("AChoreographer_removeFrameCallback: host has no vsync source".into())
    }
}

static HOST: OnceLock<RwLock<Option<Arc<dyn NdkHost>>>> = OnceLock::new();

fn host_slot() -> &'static RwLock<Option<Arc<dyn NdkHost>>> {
    HOST.get_or_init(|| RwLock::new(None))
}

/// Install the process-wide NDK host (the embedding runtime's bridge).
pub fn install_host(host: Arc<dyn NdkHost>) {
    *host_slot().write().unwrap_or_else(|e| e.into_inner()) = Some(host);
}

/// Remove the installed host (test teardown).
pub fn clear_host() {
    *host_slot().write().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Run `f` against the installed host, or fail with a structured reason.
pub fn with_host<T>(f: impl FnOnce(&dyn NdkHost) -> NdkResult<T>) -> NdkResult<T> {
    let guard = host_slot().read().unwrap_or_else(|e| e.into_inner());
    match guard.as_ref() {
        Some(host) => f(host.as_ref()),
        None => Err("no NDK host installed (runtime did not install one)".into()),
    }
}

fn reported() -> &'static Mutex<HashSet<(String, String)>> {
    static REPORTED: OnceLock<Mutex<HashSet<(String, String)>>> = OnceLock::new();
    REPORTED.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Report a structured `UNSUPPORTED_NATIVE_SYMBOL` for `library::symbol` and
/// return the API's documented error value. Reported once per symbol so a hot
/// loop cannot flood the log.
pub fn unsupported<T>(library: &str, symbol: &str, reason: &str, fail: T) -> T {
    let key = (library.to_string(), symbol.to_string());
    let first = reported()
        .lock()
        .map(|mut set| set.insert(key))
        .unwrap_or(true);
    if first {
        eprintln!(
            "ACOS UNSUPPORTED_NATIVE_SYMBOL: {library}::{symbol} — {reason} \
             (returning the documented error value; no fake success)"
        );
    }
    fail
}

/// Like [`unsupported`] but for a host that answered with `Err`.
pub fn host_failed<T>(library: &str, symbol: &str, err: String, fail: T) -> T {
    unsupported(library, symbol, &err, fail)
}

/// Convert an opaque host id into a C handle (0 is never a valid id).
pub fn handle(id: u64) -> *mut c_void {
    id as usize as *mut c_void
}

/// Convert a C handle back to a host id (`None` for NULL).
pub fn handle_id(p: *const c_void) -> Option<u64> {
    if p.is_null() {
        None
    } else {
        Some(p as usize as u64)
    }
}

/// Read a NUL-terminated guest string.
///
/// # Safety
/// `s` must be NULL or point at a readable NUL-terminated C string.
pub unsafe fn c_string(s: *const std::os::raw::c_char) -> Option<String> {
    if s.is_null() {
        return None;
    }
    Some(std::ffi::CStr::from_ptr(s).to_string_lossy().into_owned())
}
