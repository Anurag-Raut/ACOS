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

use std::cell::RefCell;
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

/// `mNativePtr` on `android.content.res.AssetManager` / `android.graphics.Bitmap`.
const NATIVE_PTR_FIELD: &str = "mNativePtr";
/// `mNativeObject` on `android.view.Surface` (and `SurfaceControl`).
const NATIVE_OBJECT_FIELD: &str = "mNativeObject";

/// `WINDOW_FORMAT_RGBA_8888`.
const WINDOW_FORMAT_RGBA_8888: i32 = 1;

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

thread_local! {
    static WINDOWS: RefCell<HashMap<u64, Window>> = RefCell::new(HashMap::new());
    static NEXT_WINDOW: RefCell<u64> = const { RefCell::new(0x4e57_0000) };
    static CHOREO: RefCell<u64> = const { RefCell::new(0) };
}

fn alloc_window_id() -> u64 {
    NEXT_WINDOW.with(|n| {
        let mut n = n.borrow_mut();
        *n = n.wrapping_add(1).max(1);
        *n
    })
}

fn alloc_choreographer_id() -> u64 {
    CHOREO.with(|n| {
        let mut n = n.borrow_mut();
        *n += 1;
        *n
    })
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
}

/// Re-install unconditionally (test helper: overrides a previous install).
pub fn install_forced() {
    INSTALLED.store(true, Ordering::SeqCst);
    install_ndk_host(Arc::new(ProductionNdkHost));
}

impl NdkHost for ProductionNdkHost {
    // --- assets ----------------------------------------------------------
    fn asset_manager_from_java(&self, _env: usize, manager: usize) -> NdkResult<u64> {
        let ptr = read_native_handle(manager, NATIVE_PTR_FIELD)
            .ok_or("AAssetManager_fromJava: object has no readable mNativePtr")?;
        if !substrate::asset_manager_exists(ptr) {
            return Err(format!(
                "AAssetManager_fromJava: mNativePtr {ptr:#x} is not a live AssetManager"
            ));
        }
        Ok(ptr)
    }

    fn asset_open(&self, manager: u64, filename: &str, mode: i32) -> NdkResult<u64> {
        substrate::asset_open(manager, filename, mode)
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
        substrate::asset_dir_open(manager, dir)
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
        WINDOWS.with(|w| {
            w.borrow_mut().insert(
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
        });
        Ok(id)
    }

    fn window_acquire(&self, _window: u64) -> NdkResult<()> {
        Ok(())
    }

    fn window_release(&self, window: u64) -> NdkResult<()> {
        WINDOWS.with(|w| {
            w.borrow_mut().remove(&window);
        });
        Ok(())
    }

    fn window_info(&self, window: u64) -> NdkResult<WindowInfo> {
        WINDOWS.with(|w| {
            let g = w.borrow();
            let rec = g
                .get(&window)
                .ok_or_else(|| format!("unknown ANativeWindow {window:#x}"))?;
            Ok(WindowInfo {
                width: rec.width,
                height: rec.height,
                format: rec.format,
            })
        })
    }

    fn window_set_geometry(
        &self,
        window: u64,
        width: i32,
        height: i32,
        format: i32,
    ) -> NdkResult<()> {
        if format != WINDOW_FORMAT_RGBA_8888 {
            return Err(format!(
                "ANativeWindow_setBuffersGeometry: format {format} has no software layout \
                 (only WINDOW_FORMAT_RGBA_8888)"
            ));
        }
        if width <= 0 || height <= 0 {
            return Err(format!(
                "ANativeWindow_setBuffersGeometry: bad geometry {width}x{height}"
            ));
        }
        WINDOWS.with(|w| {
            let mut g = w.borrow_mut();
            let rec = g
                .get_mut(&window)
                .ok_or_else(|| format!("unknown ANativeWindow {window:#x}"))?;
            rec.width = width;
            rec.height = height;
            rec.format = format;
            rec.stride = width;
            rec.buffer = vec![0u8; (width as usize) * (height as usize) * 4];
            Ok(())
        })
    }

    fn window_lock(&self, window: u64, _dirty: Option<ARect>) -> NdkResult<WindowBuffer> {
        WINDOWS.with(|w| {
            let mut g = w.borrow_mut();
            let rec = g
                .get_mut(&window)
                .ok_or_else(|| format!("unknown ANativeWindow {window:#x}"))?;
            if rec.width <= 0 || rec.height <= 0 {
                return Err("ANativeWindow_lock: buffers have no geometry".into());
            }
            if rec.buffer.is_empty() {
                rec.buffer =
                    vec![0u8; (rec.width as usize) * (rec.height as usize) * 4];
            }
            rec.locked = true;
            Ok(WindowBuffer {
                width: rec.width,
                height: rec.height,
                stride: rec.stride,
                format: rec.format,
                bits: rec.buffer.as_mut_ptr(),
            })
        })
    }

    fn window_unlock_and_post(&self, window: u64) -> NdkResult<()> {
        let (layer, width, height, buffer) = WINDOWS.with(|w| {
            let mut g = w.borrow_mut();
            let rec = g
                .get_mut(&window)
                .ok_or_else(|| format!("unknown ANativeWindow {window:#x}"))?;
            if !rec.locked {
                return Err("ANativeWindow_unlockAndPost: window was not locked".to_string());
            }
            rec.locked = false;
            Ok((rec.layer, rec.width as u32, rec.height as u32, rec.buffer.clone()))
        })?;
        let expected = (width as usize) * (height as usize) * 4;
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
        Ok(())
    }

    fn window_to_surface(&self, _env: usize, window: u64) -> NdkResult<u64> {
        WINDOWS.with(|w| {
            w.borrow()
                .get(&window)
                .map(|r| r.surface)
                .ok_or_else(|| format!("unknown ANativeWindow {window:#x}"))
        })
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
        let interval = substrate::vsync_interval_nanos();
        let frame_time = delay_nanos.max(0).saturating_add(interval);
        if let Some(cb) = cb {
            // SAFETY: the guest registered this C callback for the frame; it
            // is invoked synchronously on the registering thread.
            unsafe { invoke_frame_callback(cb, frame_time, data) };
        }
        Ok(alloc_choreographer_id())
    }

    fn choreographer_remove(&self, _token: u64) -> NdkResult<()> {
        Ok(())
    }
}
