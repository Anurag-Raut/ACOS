//! Production NDK host: answers `libandroid` / `libjnigraphics` calls from the
//! real ACOS substrates.
//!
//! The loader's NDK shims ([`acos_native_loader::NdkHost`]) never fake a call;
//! without a host they report a structured `UNSUPPORTED_NATIVE_SYMBOL`. This
//! module is the production host installed by the JNI loader binding
//! ([`crate::load::load_named`]) when a guest library is loaded, so a native
//! engine that imports `AAssetManager_*`, `AndroidBitmap_*`,
//! `ANativeWindow_*`, `AChoreographer_*` or `AConfiguration_*` gets the real
//! thing:
//!
//! * `AAssetManager_fromJava` reads the Java `AssetManager.mNativePtr` and
//!   resolves it against the real `acos-framework-natives` asset engine;
//! * `AndroidBitmap_*` reads the Java `Bitmap.mNativePtr` and locks the real
//!   `acos-render::Bitmap` pixel store (so a native write is visible to Java
//!   `Bitmap.getPixel`);
//! * `ANativeWindow_fromSurface` reads the Java `Surface.mNativeObject`,
//!   allocates a real software buffer of the surface's geometry, and
//!   `unlockAndPost` submits it to the compositor's frame sink (`WMS` →
//!   `FrameStore` in production), preserving stride/format;
//! * `AChoreographer` dispatches the frame callback on the calling thread
//!   against the virtual vsync clock; `AConfiguration` reports the live system
//!   `Configuration`.
//!
//! Unsupported behaviour (a stale handle, a non-RGBA bitmap, an unknown sensor)
//! returns the API's documented error and is reported — never a success stub.
//! All substrate state lives in `acos-vm` thread-locals; the host carries none,
//! so it can be a `Send + Sync` `Arc` shared process-wide.

use std::collections::HashMap;
use std::os::raw::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use acos_native_loader::{
    install_ndk_host, ARect, BitmapInfo, FrameCallback, NdkConfiguration, NdkHost, NdkResult,
    WindowBuffer, WindowInfo,
};
use acos_vm::natives::ndk_substrate as substrate;
use acos_vm::{JavaValue, Vm};

/// `mNativePtr` on `android.graphics.Bitmap`.
const NATIVE_PTR_FIELD: &str = "mNativePtr";
/// `mObject` on `android.content.res.AssetManager` (the API 34 field holding
/// the `nativeCreate` handle).
const ASSET_MANAGER_FIELD: &str = "mObject";
/// `mNativeObject` on `android.view.Surface` (and `SurfaceControl`).
const NATIVE_OBJECT_FIELD: &str = "mNativeObject";

/// `WINDOW_FORMAT_RGBA_8888`.
const WINDOW_FORMAT_RGBA_8888: i32 = 1;

/// Absolute per-dimension ceiling for a software NDK buffer. Anything above
/// this is a guest bug or an attempt to drive a huge host allocation (W67-A
/// #6).
const MAX_WINDOW_DIM: i32 = 16_384;

/// Absolute pixel ceiling for one RGBA8888 buffer (16 Mi pixels = 64 MiB).
/// The dimension ceiling alone is not enough: `16384x16384` would be 1 GiB.
const MAX_WINDOW_PIXELS: u64 = 16_777_216;

/// Bytes per pixel for the only software format the host supports.
const RGBA_BYTES_PER_PIXEL: u64 = 4;

/// Checked byte length of an RGBA8888 buffer with the given geometry.
///
/// `stride` is in pixels and must be at least `width` (a guest that asks for a
/// smaller stride would have the buffer read/written out of bounds). Every
/// multiplication is checked; the result is bounded by [`MAX_WINDOW_PIXELS`] and
/// [`MAX_WINDOW_DIM`], so guest geometry can never size an unbounded host
/// allocation.
fn checked_buffer_len(width: i32, height: i32, stride: i32) -> NdkResult<usize> {
    if width <= 0 || height <= 0 {
        return Err(format!("bad geometry {width}x{height}"));
    }
    if width > MAX_WINDOW_DIM || height > MAX_WINDOW_DIM || stride > MAX_WINDOW_DIM {
        return Err(format!(
            "geometry {width}x{height} stride {stride} exceeds the {MAX_WINDOW_DIM}px dimension \
             ceiling"
        ));
    }
    if stride < width {
        return Err(format!("stride {stride} is smaller than width {width}"));
    }
    let rows = (height as u64)
        .checked_mul(stride as u64)
        .ok_or_else(|| format!("geometry {width}x{height} stride {stride} overflows"))?;
    if rows > MAX_WINDOW_PIXELS {
        return Err(format!(
            "geometry {width}x{height} stride {stride} is {rows} px, over the \
             {MAX_WINDOW_PIXELS}px buffer ceiling"
        ));
    }
    let bytes = rows
        .checked_mul(RGBA_BYTES_PER_PIXEL)
        .ok_or_else(|| format!("geometry {width}x{height} stride {stride} overflows bytes"))?;
    usize::try_from(bytes).map_err(|_| format!("geometry {width}x{height} does not fit usize"))
}

/// Validate a window geometry/format and return its checked RGBA8888 size.
fn checked_window_geometry(width: i32, height: i32, format: i32) -> NdkResult<usize> {
    if format != WINDOW_FORMAT_RGBA_8888 {
        return Err(format!(
            "format {format} has no software layout (only WINDOW_FORMAT_RGBA_8888)"
        ));
    }
    checked_buffer_len(width, height, width)
}

/// The largest window dimension accepted on a display of `configured` pixels.
///
/// A window buffer may legitimately be larger than the panel (a scaled surface),
/// so the display bound is twice the physical size, but never above the fixed
/// absolute ceiling. A zero/unknown configuration grants only the absolute
/// ceiling, so the cap never disappears.
fn display_dimension_limit(configured: i32) -> i32 {
    if configured > 0 {
        configured.saturating_mul(2).clamp(1, MAX_WINDOW_DIM)
    } else {
        MAX_WINDOW_DIM
    }
}

/// Validate the layout of a `BitmapInfo` handed back by the substrate. The
/// pixel count is bounded by [`MAX_WINDOW_PIXELS`] and every product is checked.
fn checked_bitmap_layout(width: u32, height: u32, stride: u32) -> NdkResult<()> {
    if width == 0 || height == 0 {
        return Err(format!("bitmap geometry {width}x{height} is empty"));
    }
    if width > MAX_WINDOW_DIM as u32 || height > MAX_WINDOW_DIM as u32 {
        return Err(format!(
            "bitmap geometry {width}x{height} exceeds the {MAX_WINDOW_DIM}px dimension ceiling"
        ));
    }
    let min_stride = (width as u64)
        .checked_mul(RGBA_BYTES_PER_PIXEL)
        .ok_or_else(|| format!("bitmap width {width} overflows its row stride"))?;
    if (stride as u64) < min_stride {
        return Err(format!(
            "bitmap stride {stride} is smaller than width {width} x {RGBA_BYTES_PER_PIXEL} bytes"
        ));
    }
    let pixels = (width as u64)
        .checked_mul(height as u64)
        .ok_or_else(|| format!("bitmap geometry {width}x{height} overflows"))?;
    if pixels > MAX_WINDOW_PIXELS {
        return Err(format!(
            "bitmap geometry {width}x{height} is {pixels} px, over the {MAX_WINDOW_PIXELS}px \
             ceiling"
        ));
    }
    Ok(())
}

/// Run `f` with the VM bound to this host thread by the JNI runtime.
fn with_vm<R>(f: impl FnOnce(&mut Vm) -> R) -> Option<R> {
    let p = crate::state::current_vm();
    if p.is_null() {
        return None;
    }
    // SAFETY: `JniRuntime::bind`/native dispatch published this VM pointer for
    // the calling host thread and it stays valid for the operation.
    Some(f(unsafe { &mut *p }))
}

/// Read a `long` (or `int`) native-handle field from a Java object handle.
fn read_native_handle(object: usize, field: &str) -> Option<u64> {
    if object == 0 {
        return None;
    }
    with_vm(|vm| match vm.get_field(object as acos_vm::Handle, field) {
        Ok(JavaValue::Long(v)) => Some(v as u64),
        Ok(JavaValue::Int(v)) => Some(v as u32 as u64),
        _ => None,
    })
    .flatten()
}

fn config_from(sub: substrate::Configuration) -> NdkConfiguration {
    NdkConfiguration {
        mcc: sub.mcc,
        mnc: sub.mnc,
        locale: sub.locale,
        orientation: sub.orientation,
        touchscreen: sub.touchscreen,
        density: sub.density,
        keyboard: sub.keyboard,
        keyboard_hidden: sub.keyboard_hidden,
        navigation: sub.navigation,
        screen_width: sub.screen_width,
        screen_height: sub.screen_height,
        screen_layout: sub.screen_layout,
        ui_mode: sub.ui_mode,
        smallest_screen_width_dp: sub.smallest_screen_width_dp,
        screen_width_dp: sub.screen_width_dp,
        screen_height_dp: sub.screen_height_dp,
        color_mode: sub.color_mode,
    }
}

/// One `ANativeWindow` over a Java `Surface`'s layer.
struct Window {
    surface: u64,
    layer: u64,
    width: i32,
    height: i32,
    format: i32,
    /// Row stride in pixels (the `ANativeWindow_Buffer` unit).
    stride: i32,
    /// Software pixel buffer (`height * stride * 4` bytes).
    buffer: Vec<u8>,
    locked: bool,
}

/// Process-wide window registry.
///
/// Not thread-local on purpose: an engine creates its `ANativeWindow` on the
/// platform thread (`ANativeWindow_fromSurface`, from `nativeSurfaceCreated`)
/// but locks/posts it from the **raster thread**. A per-thread map would make
/// `ANativeWindow_lock` report "unknown window" on that thread and no frame
/// would ever reach the compositor. The map is only ever touched through this
/// mutex; a `Window` owns its pixel buffer, so it is `Send`.
static WINDOWS: std::sync::OnceLock<std::sync::Mutex<HashMap<u64, Window>>> =
    std::sync::OnceLock::new();

fn windows() -> &'static std::sync::Mutex<HashMap<u64, Window>> {
    WINDOWS.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

static NEXT_WINDOW: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0x4e57_0000);
static NEXT_CHOREO: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The last buffer a window presented, process-wide.
///
/// The engine's raster thread is the one that publishes (it owns the software
/// renderer), while the harness observes from the platform thread. A process
/// global capture is therefore the only way to observe a real present without
/// reaching into the engine's threads.
static PRESENTED: std::sync::OnceLock<std::sync::Mutex<Vec<PresentedFrame>>> =
    std::sync::OnceLock::new();

/// Upper bound on retained presented frames (the harness only inspects the
/// first few; an unbounded history would hold every 1 MiB+ buffer forever).
const MAX_PRESENTED: usize = 16;

/// One completed frame published through an `ANativeWindow`.
#[derive(Clone, Debug)]
pub struct PresentedFrame {
    /// The window id (`ANativeWindow*`) that published it.
    pub window: u64,
    /// The compositor layer the window is bound to.
    pub layer: u64,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// RGBA8888 pixels, `width * height * 4` bytes.
    pub rgba: Vec<u8>,
    /// Sink frame sequence (0 when the sink rejected it).
    pub seq: u64,
    /// The host thread that published the frame (`{:?}` of its id).
    pub thread: String,
}

fn record_presented(frame: PresentedFrame) {
    let slot = PRESENTED.get_or_init(|| std::sync::Mutex::new(Vec::new()));
    if let Ok(mut frames) = slot.lock() {
        if frames.len() < MAX_PRESENTED {
            frames.push(frame);
        } else {
            // Keep the newest bounded window rather than silently dropping.
            frames.remove(0);
            frames.push(frame);
        }
    }
}

/// Every buffer presented through an `ANativeWindow` on any thread (bounded).
#[must_use]
pub fn presented_frames() -> Vec<PresentedFrame> {
    PRESENTED
        .get()
        .and_then(|slot| slot.lock().ok().map(|frames| frames.clone()))
        .unwrap_or_default()
}

/// The most recent buffer presented through an `ANativeWindow` on any thread.
#[must_use]
pub fn last_presented_frame() -> Option<PresentedFrame> {
    presented_frames().pop()
}

/// A monotonic nanosecond clock (host `CLOCK_MONOTONIC`, the `nanoTime` family).
fn monotonic_nanos() -> i64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    let start = START.get_or_init(Instant::now);
    i64::try_from(start.elapsed().as_nanos()).unwrap_or(i64::MAX)
}

fn alloc_window_id() -> u64 {
    NEXT_WINDOW.fetch_add(1, Ordering::SeqCst).max(1)
}

fn alloc_choreographer_id() -> u64 {
    NEXT_CHOREO.fetch_add(1, Ordering::SeqCst) + 1
}

/// Invoke a guest frame callback. Kept out of the public surface so the raw
/// `data` pointer is only dereferenced behind an explicit `unsafe` call.
///
/// # Safety
/// `cb` must be a valid C function pointer registered by the guest for the
/// duration of the frame; `data` must be the opaque argument it supplied.
unsafe fn invoke_frame_callback(
    cb: unsafe extern "C" fn(i64, *mut c_void),
    frame_time_nanos: i64,
    data: *mut c_void,
) {
    // SAFETY: forwarded from the caller's contract.
    unsafe { cb(frame_time_nanos, data) };
}

/// The production NDK host. Stateless: every substrate lives in `acos-vm`
/// thread-locals, so one `Arc` serves the whole process.
#[derive(Debug, Default)]
pub struct ProductionNdkHost;

static INSTALLED: AtomicBool = AtomicBool::new(false);

/// Install the production NDK host once per process.
///
/// The loader's host slot is process-wide. Calling this before the first guest
/// library loads makes every NDK import answer from real state; a later call is
/// a no-op so a test that installed its own host is not silently replaced.
pub fn install() {
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    install_ndk_host(Arc::new(ProductionNdkHost));
    // The software GLES substrate (ADR-0025) replaces the loader's EGL/GLES
    // traps and connects `eglSwapBuffers` to the same compositor sink the NDK
    // window path uses. This runs only on the native load path, i.e. only when
    // `--native` (or `ACOS_NATIVE=1`) enabled native code.
    crate::gles_host::install();
}

/// Re-install unconditionally (test helper: overrides a previous install).
pub fn install_forced() {
    INSTALLED.store(true, Ordering::SeqCst);
    install_ndk_host(Arc::new(ProductionNdkHost));
    crate::gles_host::install();
}

/// Geometry of a live `ANativeWindow` (used by the EGL window-surface host).
#[must_use]
pub fn window_geometry(window: u64) -> Option<(u32, u32)> {
    let g = windows().lock().ok()?;
    let rec = g.get(&window)?;
    if rec.width <= 0 || rec.height <= 0 {
        return None;
    }
    Some((rec.width as u32, rec.height as u32))
}

/// Publish `rgba` as the window's new contents, exactly like
/// `ANativeWindow_unlockAndPost`: copy into the window's software buffer, then
/// submit it to the compositor sink. Returns the compositor frame id (0 when
/// the submission was rejected or the window is unknown).
pub fn window_present(window: u64, width: u32, height: u32, rgba: &[u8]) -> u64 {
    let Some((layer, pixels)) = (|| {
        let mut g = windows().lock().ok()?;
        let rec = g.get_mut(&window)?;
        let len = checked_buffer_len(width as i32, height as i32, width as i32).ok()?;
        if rgba.len() < len {
            return None;
        }
        rec.width = width as i32;
        rec.height = height as i32;
        rec.stride = width as i32;
        rec.buffer.clear();
        rec.buffer.extend_from_slice(&rgba[..len]);
        Some((rec.layer, rec.buffer.clone()))
    })()
    else {
        eprintln!(
            "UNSUPPORTED_GLES call=eglSwapBuffers reason=unknown or unallocatable ANativeWindow \
             {window:#x}"
        );
        return 0;
    };
    let seq = substrate::submit_frame(layer, width, height, &pixels, 0);
    if seq != 0 {
        record_presented(PresentedFrame {
            window,
            layer,
            width,
            height,
            rgba: pixels,
            seq,
            thread: format!("{:?}", std::thread::current().id()),
        });
    }
    seq
}

/// Register a native window without a Java `Surface` (integration tests).
///
/// The production path is `ANativeWindow_fromSurface`, which resolves a live
/// `SurfaceControl` layer; this helper lets a test drive the EGL swap → frame
/// sink path with an explicit layer handle. Returns the window id.
#[doc(hidden)]
pub fn register_native_window(width: i32, height: i32, layer: u64, surface: u64) -> u64 {
    let id = alloc_window_id();
    if let Ok(mut g) = windows().lock() {
        g.insert(
            id,
            Window {
                surface,
                layer,
                width,
                height,
                format: WINDOW_FORMAT_RGBA_8888,
                stride: width.max(0),
                buffer: vec![0u8; (width.max(0) * height.max(0) * 4) as usize],
                locked: false,
            },
        );
    }
    id
}

impl NdkHost for ProductionNdkHost {
    // --- assets ----------------------------------------------------------
    fn asset_manager_from_java(&self, _env: usize, manager: usize) -> NdkResult<u64> {
        // API 34 moved the `nativeCreate` handle to `mObject`; accept both so a
        // framework build using either name resolves. The manager registry is
        // the real `fwk_res` engine table, so the handle must be live.
        let ptr = read_native_handle(manager, ASSET_MANAGER_FIELD)
            .or_else(|| read_native_handle(manager, NATIVE_PTR_FIELD))
            .ok_or("AAssetManager_fromJava: object has no readable native handle")?;
        if !substrate::asset_manager_exists(ptr) {
            return Err(format!(
                "AAssetManager_fromJava: mNativePtr {ptr:#x} is not a live AssetManager"
            ));
        }
        Ok(ptr)
    }

    fn asset_open(&self, manager: u64, filename: &str, mode: i32) -> NdkResult<u64> {
        match substrate::asset_open(manager, filename, mode) {
            Ok(handle) => Ok(handle),
            Err(first) => {
                // Android's asset namespace root is the APK's `assets/`
                // directory: `AAssetManager_open("flutter_assets/x")` reads the
                // ZIP entry `assets/flutter_assets/x`. The ACOS asset engine
                // matches entry names verbatim, so retry with the prefix (the
                // exact AAPT mapping) before returning the documented error.
                if !filename.starts_with("assets/") {
                    let prefixed = format!("assets/{filename}");
                    if let Ok(handle) = substrate::asset_open(manager, &prefixed, mode) {
                        return Ok(handle);
                    }
                }
                Err(first)
            }
        }
    }

    fn asset_read(&self, asset: u64, buf: &mut [u8]) -> NdkResult<usize> {
        substrate::asset_read(asset, buf)
    }

    fn asset_seek(&self, asset: u64, offset: i64, whence: i32) -> NdkResult<i64> {
        substrate::asset_seek(asset, offset, whence)
    }

    fn asset_length(&self, asset: u64) -> NdkResult<i64> {
        substrate::asset_length(asset)
    }

    fn asset_is_allocated(&self, asset: u64) -> NdkResult<bool> {
        Ok(substrate::asset_is_allocated(asset))
    }

    fn asset_buffer(&self, asset: u64) -> NdkResult<*const c_void> {
        substrate::asset_buffer(asset)
    }

    fn asset_remaining(&self, asset: u64) -> NdkResult<i64> {
        substrate::asset_remaining(asset)
    }

    fn asset_close(&self, asset: u64) -> NdkResult<()> {
        substrate::asset_close(asset);
        Ok(())
    }

    fn asset_dir_open(&self, manager: u64, dir: &str) -> NdkResult<u64> {
        match substrate::asset_dir_open(manager, dir) {
            Ok(handle) => Ok(handle),
            Err(first) => {
                // Same `assets/` namespace mapping as `AAssetManager_open`.
                if !dir.starts_with("assets/") {
                    let prefixed = format!("assets/{dir}");
                    if let Ok(handle) = substrate::asset_dir_open(manager, &prefixed) {
                        return Ok(handle);
                    }
                }
                Err(first)
            }
        }
    }

    fn asset_dir_next(&self, dir: u64) -> NdkResult<Option<String>> {
        Ok(substrate::asset_dir_next(dir))
    }

    fn asset_dir_rewind(&self, dir: u64) -> NdkResult<()> {
        substrate::asset_dir_rewind(dir);
        Ok(())
    }

    fn asset_dir_close(&self, dir: u64) -> NdkResult<()> {
        substrate::asset_dir_close(dir);
        Ok(())
    }

    // --- bitmaps ---------------------------------------------------------
    fn bitmap_info(&self, _env: usize, bitmap: usize) -> NdkResult<BitmapInfo> {
        let handle = read_native_handle(bitmap, NATIVE_PTR_FIELD)
            .ok_or("AndroidBitmap_getInfo: object has no readable mNativePtr")?;
        let info = substrate::bitmap_info(handle as i64)?;
        checked_bitmap_layout(info.width, info.height, info.stride)
            .map_err(|e| format!("AndroidBitmap_getInfo: {e}"))?;
        Ok(BitmapInfo {
            width: info.width,
            height: info.height,
            stride: info.stride,
            format: info.format,
            flags: info.flags,
            pixels: std::ptr::null_mut(),
        })
    }

    fn bitmap_lock(&self, _env: usize, bitmap: usize) -> NdkResult<BitmapInfo> {
        let handle = read_native_handle(bitmap, NATIVE_PTR_FIELD)
            .ok_or("AndroidBitmap_lockPixels: object has no readable mNativePtr")?;
        let info = substrate::bitmap_lock(handle as i64)?;
        checked_bitmap_layout(info.width, info.height, info.stride)
            .map_err(|e| format!("AndroidBitmap_lockPixels: {e}"))?;
        Ok(BitmapInfo {
            width: info.width,
            height: info.height,
            stride: info.stride,
            format: info.format,
            flags: info.flags,
            pixels: info.pixels,
        })
    }

    fn bitmap_unlock(&self, _env: usize, bitmap: usize) -> NdkResult<()> {
        let handle = read_native_handle(bitmap, NATIVE_PTR_FIELD)
            .ok_or("AndroidBitmap_unlockPixels: object has no readable mNativePtr")?;
        substrate::bitmap_unlock(handle as i64)
    }

    // --- windows ---------------------------------------------------------
    fn window_from_surface(&self, _env: usize, surface: usize) -> NdkResult<u64> {
        let handle = read_native_handle(surface, NATIVE_OBJECT_FIELD)
            .ok_or("ANativeWindow_fromSurface: Surface has no readable mNativeObject")?;
        let layer = substrate::surface_layer(handle)
            .ok_or_else(|| format!("ANativeWindow_fromSurface: no live layer for {handle:#x}"))?;
        let (width, height) = substrate::surface_size(handle).unwrap_or((0, 0));
        let id = alloc_window_id();
        if let Ok(mut g) = windows().lock() {
            g.insert(
                id,
                Window {
                    surface: handle,
                    layer,
                    width,
                    height,
                    format: WINDOW_FORMAT_RGBA_8888,
                    stride: width.max(0),
                    buffer: Vec::new(),
                    locked: false,
                },
            );
        }
        Ok(id)
    }

    fn window_acquire(&self, _window: u64) -> NdkResult<()> {
        Ok(())
    }

    fn window_release(&self, window: u64) -> NdkResult<()> {
        if let Ok(mut g) = windows().lock() {
            g.remove(&window);
        }
        Ok(())
    }

    fn window_info(&self, window: u64) -> NdkResult<WindowInfo> {
        let g = windows()
            .lock()
            .map_err(|_| "ANativeWindow_query: window table poisoned".to_string())?;
        let rec = g
            .get(&window)
            .ok_or_else(|| format!("unknown ANativeWindow {window:#x}"))?;
        Ok(WindowInfo {
            width: rec.width,
            height: rec.height,
            format: rec.format,
        })
    }

    fn window_set_geometry(
        &self,
        window: u64,
        width: i32,
        height: i32,
        format: i32,
    ) -> NdkResult<()> {
        let bytes = checked_window_geometry(width, height, format)
            .map_err(|e| format!("ANativeWindow_setBuffersGeometry: {e}"))?;
        // Bound by the live display configuration as well as the fixed ceiling.
        let cfg = substrate::configuration();
        let max_w = display_dimension_limit(cfg.screen_width);
        let max_h = display_dimension_limit(cfg.screen_height);
        if width > max_w || height > max_h {
            return Err(format!(
                "ANativeWindow_setBuffersGeometry: geometry {width}x{height} exceeds the display \
                 bound {max_w}x{max_h}"
            ));
        }
        let mut g = windows()
            .lock()
            .map_err(|_| "ANativeWindow_setBuffersGeometry: window table poisoned".to_string())?;
        let rec = g
            .get_mut(&window)
            .ok_or_else(|| format!("unknown ANativeWindow {window:#x}"))?;
        rec.width = width;
        rec.height = height;
        rec.format = format;
        rec.stride = width;
        rec.buffer = vec![0u8; bytes];
        Ok(())
    }

    fn window_lock(&self, window: u64, _dirty: Option<ARect>) -> NdkResult<WindowBuffer> {
        let mut g = windows().lock().map_err(|_| {
            "ANativeWindow_lock: window table poisoned".to_string()
        })?;
        let rec = g
            .get_mut(&window)
            .ok_or_else(|| format!("unknown ANativeWindow {window:#x}"))?;
        let needed = checked_buffer_len(rec.width, rec.height, rec.stride)
            .map_err(|e| format!("ANativeWindow_lock: {e}"))?;
        if rec.buffer.len() < needed {
            rec.buffer.resize(needed, 0);
        }
        rec.locked = true;
        // SAFETY: `rec.buffer` stays alive and is not reallocated while the
        // caller fills the locked region; `window_unlock_and_post` re-checks
        // the length before using it.
        Ok(WindowBuffer {
            width: rec.width,
            height: rec.height,
            stride: rec.stride,
            format: rec.format,
            bits: rec.buffer.as_mut_ptr(),
        })
    }

    fn window_unlock_and_post(&self, window: u64) -> NdkResult<()> {
        let (layer, width, height, stride, buffer) = {
            let mut g = windows().lock().map_err(|_| {
                "ANativeWindow_unlockAndPost: window table poisoned".to_string()
            })?;
            let rec = g
                .get_mut(&window)
                .ok_or_else(|| format!("unknown ANativeWindow {window:#x}"))?;
            if !rec.locked {
                return Err("ANativeWindow_unlockAndPost: window was not locked".to_string());
            }
            rec.locked = false;
            (
                rec.layer,
                rec.width as u32,
                rec.height as u32,
                rec.stride,
                rec.buffer.clone(),
            )
        };
        let expected = checked_buffer_len(width as i32, height as i32, stride)
            .map_err(|e| format!("ANativeWindow_unlockAndPost: {e}"))?;
        if buffer.len() < expected {
            return Err(format!(
                "ANativeWindow_unlockAndPost: buffer {} bytes, expected {expected}",
                buffer.len()
            ));
        }
        // A real buffer submission is the only frame-publish signal: hand the
        // pixels to the installed compositor sink (WMS → FrameStore).
        let frame = substrate::submit_frame(layer, width, height, &buffer, 0);
        if frame == 0 {
            return Err(
                "ANativeWindow_unlockAndPost: compositor rejected the frame submission".into(),
            );
        }
        record_presented(PresentedFrame {
            window,
            layer,
            width,
            height,
            rgba: buffer,
            seq: frame,
            thread: format!("{:?}", std::thread::current().id()),
        });
        Ok(())
    }

    fn window_to_surface(&self, _env: usize, window: u64) -> NdkResult<u64> {
        let g = windows()
            .lock()
            .map_err(|_| "ANativeWindow_getSurface: window table poisoned".to_string())?;
        g.get(&window)
            .map(|r| r.surface)
            .ok_or_else(|| format!("unknown ANativeWindow {window:#x}"))
    }

    // --- configuration / vsync ------------------------------------------
    fn configuration(&self) -> NdkResult<NdkConfiguration> {
        Ok(config_from(substrate::configuration()))
    }

    // The NDK trait mandates a safe signature with an opaque raw-pointer
    // argument; the pointer is only ever handed to the guest callback.
    #[allow(clippy::not_unsafe_ptr_arg_deref)]
    fn choreographer_post(
        &self,
        delay_nanos: i64,
        cb: FrameCallback,
        data: *mut c_void,
    ) -> NdkResult<u64> {
        // ACOS's frame clock is the virtual vsync timeline; the callback runs on
        // the calling thread's looper, exactly as the NDK contract requires.
        //
        // `frameTimeNanos` must be a **monotonic, increasing** timestamp: the
        // engine derives the frame's `TimePoint` from it and animators/vsync
        // bookkeeping assume successive frames advance. Use the host monotonic
        // clock (same epoch family as `System.nanoTime`) instead of a fixed
        // interval, which would make every frame look like the same instant.
        let interval = substrate::vsync_interval_nanos();
        let now = monotonic_nanos();
        let frame_time = now.saturating_add(delay_nanos.max(0)).saturating_add(interval);
        if let Some(cb) = cb {
            // Real Android never runs the frame callback inside the post: the
            // Choreographer queues it on the calling thread's looper and the
            // looper delivers it at the next poll with the frame time. Running
            // it synchronously here re-enters the engine's `RequestFrame`
            // mid-task (the callback consumes the waiter state before the
            // posting task returns), which leaves a mismatched frame callback
            // and no frame. Queue it on the loader's `ALooper` instead.
            // SAFETY: the guest registered this C callback for the frame and
            // supplied `data`; the NDK contract ties both to the looper.
            let queued = unsafe {
                acos_native_loader::bionic::ndk::ndk_looper::post_frame_callback(
                    frame_time, cb, data,
                )
            };
            if !queued {
                // No prepared looper on this thread: fall back to a direct
                // invocation so a caller without a looper still advances.
                // SAFETY: forwarded from the caller's contract.
                unsafe { invoke_frame_callback(cb, frame_time, data) };
            }
        }
        Ok(alloc_choreographer_id())
    }

    fn choreographer_remove(&self, _token: u64) -> NdkResult<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn insert_window(id: u64, width: i32, height: i32) {
        if let Ok(mut g) = windows().lock() {
            g.insert(
                id,
                Window {
                    surface: 1,
                    layer: 1,
                    width,
                    height,
                    format: WINDOW_FORMAT_RGBA_8888,
                    stride: width,
                    buffer: vec![0u8; (width * height * 4) as usize],
                    locked: false,
                },
            );
        }
    }

    fn remove_window(id: u64) {
        if let Ok(mut g) = windows().lock() {
            g.remove(&id);
        }
    }

    #[test]
    fn checked_geometry_accepts_reasonable_sizes() {
        let bytes =
            checked_window_geometry(1920, 1080, WINDOW_FORMAT_RGBA_8888).unwrap();
        assert_eq!(bytes, 1920 * 1080 * 4);
        assert_eq!(
            checked_window_geometry(1, 1, WINDOW_FORMAT_RGBA_8888).unwrap(),
            4
        );
    }

    #[test]
    fn checked_geometry_rejects_bad_format_and_empty_size() {
        // Only RGBA_8888 has a software layout.
        assert!(checked_window_geometry(16, 16, 4).is_err());
        assert!(checked_window_geometry(0, 16, WINDOW_FORMAT_RGBA_8888).is_err());
        assert!(checked_window_geometry(16, -1, WINDOW_FORMAT_RGBA_8888).is_err());
    }

    #[test]
    fn checked_geometry_rejects_unbounded_guest_sizes() {
        // The W67-A #6 example: 200000x200000 must be refused before a ~160 GB
        // allocation is attempted.
        let err = checked_window_geometry(200_000, 200_000, WINDOW_FORMAT_RGBA_8888)
            .expect_err("200000x200000 must be rejected");
        assert!(err.contains("ceiling"), "unexpected error: {err}");

        // Arithmetic overflow on both i32 dimensions.
        assert!(checked_window_geometry(i32::MAX, i32::MAX, WINDOW_FORMAT_RGBA_8888).is_err());

        // Under the dimension ceiling but over the pixel ceiling.
        assert!(checked_window_geometry(4097, 4096, WINDOW_FORMAT_RGBA_8888).is_err());

        // The exact ceiling is accepted (4096 x 4096 x 4 = 64 MiB).
        assert_eq!(
            checked_window_geometry(4096, 4096, WINDOW_FORMAT_RGBA_8888).unwrap(),
            4096 * 4096 * 4
        );
    }

    #[test]
    fn buffer_len_requires_stride_at_least_width() {
        assert!(checked_buffer_len(16, 16, 15).is_err());
        assert_eq!(checked_buffer_len(16, 16, 16).unwrap(), 16 * 16 * 4);
        assert_eq!(checked_buffer_len(16, 16, 20).unwrap(), 16 * 20 * 4);
        assert!(checked_buffer_len(16, 16, MAX_WINDOW_DIM + 1).is_err());
    }

    #[test]
    fn bitmap_layout_is_bounded() {
        assert!(checked_bitmap_layout(16, 16, 64).is_ok());
        // stride is bytes: it may not be smaller than width * 4.
        assert!(checked_bitmap_layout(16, 16, 32).is_err());
        assert!(checked_bitmap_layout(0, 16, 64).is_err());
        // A guest-sized bitmap is refused.
        assert!(checked_bitmap_layout(200_000, 200_000, 800_000).is_err());
    }

    #[test]
    fn window_set_geometry_caps_host_allocation() {
        let host = ProductionNdkHost;
        let id = alloc_window_id();
        insert_window(id, 2, 2);

        // Huge geometry returns a structured error instead of allocating.
        let err = host
            .window_set_geometry(id, 200_000, 200_000, WINDOW_FORMAT_RGBA_8888)
            .expect_err("huge geometry must be rejected");
        assert!(err.contains("ceiling"), "unexpected error: {err}");

        // Unknown window handles are still an error.
        assert!(host
            .window_set_geometry(0xdead, 16, 16, WINDOW_FORMAT_RGBA_8888)
            .is_err());

        // A valid geometry allocates exactly width*height*4 and locks with a
        // stride that is not smaller than the width.
        host.window_set_geometry(id, 16, 16, WINDOW_FORMAT_RGBA_8888)
            .unwrap();
        {
            let g = windows().lock().unwrap();
            let rec = g.get(&id).unwrap();
            assert_eq!(rec.buffer.len(), 16 * 16 * 4);
            assert_eq!(rec.stride, 16);
        }
        let buf = host.window_lock(id, None).unwrap();
        assert_eq!((buf.width, buf.height, buf.stride), (16, 16, 16));
        assert!(host.window_lock(0xdead, None).is_err());

        remove_window(id);
    }
}
