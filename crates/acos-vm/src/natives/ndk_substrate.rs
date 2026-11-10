//! Safe substrate accessors for the production NDK host (`acos-jni`).
//!
//! The loader's NDK shims answer from an installed [`NdkHost`]; in production
//! that host lives in `acos-jni` (it already depends on `acos-native-loader`
//! for the trait and on `acos-vm` for the substrates). `acos-vm` forbids
//! `unsafe`, so this module exposes the real ACOS state — the asset engine,
//! `Bitmap` pixel stores, the system `Configuration`, the vsync clock and the
//! compositor frame sink — behind safe functions that the host forwards to.
//!
//! Nothing here fabricates a value: every function reports a structured `Err`
//! (or `None`) when the substrate is absent or holds a format ACOS cannot
//! honour. A stale handle never silently reads another object.
//!
//! All state is per-OS-thread, exactly like the interpreters and native tables
//! it serves (`Vm` is `!Send`), so the host carries no substrate state itself.

use std::cell::RefCell;
use std::collections::HashMap;

use acos_render::BitmapConfig;

use crate::natives::gfx_state::{self, rh};
use crate::natives::fwk_res;
use crate::natives::fwk_surface as surface;
use crate::natives::gfx_hwui;

/// `AndroidBitmapInfo` values backed by a real `acos-render::Bitmap`.
///
/// `pixels` is non-null only while the bitmap is locked (mirrors the NDK: the
/// address is valid between `AndroidBitmap_lockPixels`/`unlockPixels`), but the
/// underlying storage is always the bitmap's own buffer, so a guest write is
/// immediately visible to `Bitmap.getPixel`.
#[derive(Clone, Copy, Debug)]
pub struct BitmapInfo {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Row stride in bytes (`ANDROID_BITMAP_FORMAT_RGBA_8888` = width * 4).
    pub stride: u32,
    /// `ANDROID_BITMAP_FORMAT_*` (1 = RGBA_8888).
    pub format: u32,
    /// `ANDROID_BITMAP_FLAGS_*` (1 = alpha premultiplied).
    pub flags: u32,
    /// Locked pixel address, or null for `getInfo`.
    pub pixels: *mut u8,
}

/// The real system configuration the NDK `AConfiguration` surface reports.
#[derive(Clone, Debug, Default)]
pub struct Configuration {
    /// Mobile country code.
    pub mcc: i32,
    /// Mobile network code.
    pub mnc: i32,
    /// BCP-47-ish locale tag (`en-US`).
    pub locale: String,
    /// `ACONFIGURATION_ORIENTATION_*`.
    pub orientation: i32,
    /// `ACONFIGURATION_TOUCHSCREEN_*`.
    pub touchscreen: i32,
    /// Density in dpi.
    pub density: i32,
    /// `ACONFIGURATION_KEYBOARD_*`.
    pub keyboard: i32,
    /// `ACONFIGURATION_KEYSHIDDEN_*`.
    pub keyboard_hidden: i32,
    /// `ACONFIGURATION_NAVIGATION_*`.
    pub navigation: i32,
    /// Screen width in physical pixels.
    pub screen_width: i32,
    /// Screen height in physical pixels.
    pub screen_height: i32,
    /// `ACONFIGURATION_SCREENLAYOUT_*`.
    pub screen_layout: i32,
    /// `ACONFIGURATION_UI_MODE_*`.
    pub ui_mode: i32,
    /// Smallest screen width in dp.
    pub smallest_screen_width_dp: i32,
    /// Screen width in dp.
    pub screen_width_dp: i32,
    /// Screen height in dp.
    pub screen_height_dp: i32,
    /// `ACONFIGURATION_COLOR_MODE_*`.
    pub color_mode: i32,
}

thread_local! {
    /// NDK asset handle -> (engine manager, engine asset). The engine's own
    /// handles collide across managers, so the host keeps a global namespace.
    static ASSETS: RefCell<HashMap<u64, (u64, u64)>> = RefCell::new(HashMap::new());
    /// Registry-local asset-directory cursor ids are reused verbatim, so no
    /// extra table is needed for directories.
    static NEXT: RefCell<u64> = const { RefCell::new(0) };
    /// The last `AAssetManager` seen, so `AConfiguration` reports its config.
    static LAST_MANAGER: RefCell<Option<u64>> = const { RefCell::new(None) };
}

fn alloc() -> u64 {
    NEXT.with(|n| {
        let mut n = n.borrow_mut();
        *n += 1;
        *n
    })
}

// ---------------------------------------------------------------------------
// Assets
// ---------------------------------------------------------------------------

/// Validate a Java `AssetManager.mNativePtr` against the engine.
pub fn asset_manager_exists(manager: u64) -> bool {
    let ok = fwk_res::ndk_manager_exists(manager);
    if ok {
        LAST_MANAGER.with(|m| *m.borrow_mut() = Some(manager));
    }
    ok
}

/// `AAssetManager_open`.
pub fn asset_open(manager: u64, filename: &str, mode: i32) -> Result<u64, String> {
    let engine = fwk_res::ndk_asset_open(manager, filename, mode)?;
    let id = alloc();
    ASSETS.with(|a| a.borrow_mut().insert(id, (manager, engine)));
    Ok(id)
}

fn asset_lookup(id: u64) -> Result<(u64, u64), String> {
    ASSETS
        .with(|a| a.borrow().get(&id).copied())
        .ok_or_else(|| format!("unknown AAsset handle {id:#x}"))
}

/// `AAsset_read`.
pub fn asset_read(id: u64, out: &mut [u8]) -> Result<usize, String> {
    let (manager, engine) = asset_lookup(id)?;
    fwk_res::ndk_asset_read(manager, engine, out)
}

/// `AAsset_seek` (64-bit).
pub fn asset_seek(id: u64, offset: i64, whence: i32) -> Result<i64, String> {
    let (manager, engine) = asset_lookup(id)?;
    fwk_res::ndk_asset_seek(manager, engine, offset, whence)
}

/// `AAsset_getLength`.
pub fn asset_length(id: u64) -> Result<i64, String> {
    let (manager, engine) = asset_lookup(id)?;
    fwk_res::ndk_asset_length(manager, engine).map(|n| n as i64)
}

/// `AAsset_getRemainingLength`.
pub fn asset_remaining(id: u64) -> Result<i64, String> {
    let (manager, engine) = asset_lookup(id)?;
    fwk_res::ndk_asset_remaining(manager, engine).map(|n| n as i64)
}

/// `AAsset_getBuffer` — valid until `AAsset_close`.
pub fn asset_buffer(id: u64) -> Result<*const std::os::raw::c_void, String> {
    let (manager, engine) = asset_lookup(id)?;
    fwk_res::ndk_asset_buffer(manager, engine).map(|p| p.cast())
}

/// `AAsset_isAllocated`.
pub fn asset_is_allocated(id: u64) -> bool {
    let Ok((manager, engine)) = asset_lookup(id) else {
        return false;
    };
    fwk_res::ndk_asset_is_allocated(manager, engine)
}

/// `AAsset_close`.
pub fn asset_close(id: u64) {
    if let Some((manager, engine)) = ASSETS.with(|a| a.borrow_mut().remove(&id)) {
        fwk_res::ndk_asset_close(manager, engine);
    }
}

/// `AAssetManager_openDir` — capture the direct children of `dir`.
pub fn asset_dir_open(manager: u64, dir: &str) -> Result<u64, String> {
    let names = fwk_res::ndk_asset_dir(manager, dir)?;
    Ok(fwk_res::ndk_dir_store(names))
}

/// `AAssetDir_getNextFileName`.
pub fn asset_dir_next(id: u64) -> Option<String> {
    fwk_res::ndk_dir_next(id)
}

/// `AAssetDir_rewind`.
pub fn asset_dir_rewind(id: u64) {
    fwk_res::ndk_dir_rewind(id);
}

/// `AAssetDir_close`.
pub fn asset_dir_close(id: u64) {
    fwk_res::ndk_dir_close(id);
}

// ---------------------------------------------------------------------------
// Bitmaps (`libjnigraphics`)
// ---------------------------------------------------------------------------

fn bitmap_meta(handle: i64) -> Result<BitmapInfo, String> {
    gfx_state::with_ctx(|c| {
        let b = c
            .bitmaps
            .get(rh(handle))
            .ok_or_else(|| format!("stale Bitmap handle {handle:#x}"))?;
        if b.config() != BitmapConfig::Rgba8888 {
            return Err(format!(
                "Bitmap config {:?} has no NDK-compatible layout (only RGBA_8888)",
                b.config()
            ));
        }
        Ok(BitmapInfo {
            width: b.width(),
            height: b.height(),
            stride: b.row_bytes() as u32,
            format: 1, // ANDROID_BITMAP_FORMAT_RGBA_8888
            flags: 1,  // ANDROID_BITMAP_FLAGS_ALPHA_PREMUL
            pixels: std::ptr::null_mut(),
        })
    })
}

/// `AndroidBitmap_getInfo`.
pub fn bitmap_info(handle: i64) -> Result<BitmapInfo, String> {
    bitmap_meta(handle)
}

/// `AndroidBitmap_lockPixels` — returns the bitmap's own pixel storage.
pub fn bitmap_lock(handle: i64) -> Result<BitmapInfo, String> {
    gfx_state::with_ctx(|c| {
        let b = c
            .bitmaps
            .get_mut(rh(handle))
            .ok_or_else(|| format!("stale Bitmap handle {handle:#x}"))?;
        if b.config() != BitmapConfig::Rgba8888 {
            return Err(format!(
                "Bitmap config {:?} has no NDK-compatible layout (only RGBA_8888)",
                b.config()
            ));
        }
        let width = b.width();
        let height = b.height();
        let stride = b.row_bytes() as u32;
        let pixels = b.pixels_mut().as_mut_ptr() as *mut u8;
        Ok(BitmapInfo {
            width,
            height,
            stride,
            format: 1,
            flags: 1,
            pixels,
        })
    })
}

/// `AndroidBitmap_unlockPixels`.
pub fn bitmap_unlock(handle: i64) -> Result<(), String> {
    gfx_state::with_ctx(|c| {
        if c.bitmaps.get(rh(handle)).is_some() {
            Ok(())
        } else {
            Err(format!("stale Bitmap handle {handle:#x}"))
        }
    })
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// The real system configuration (from the live asset manager, else default).
pub fn configuration() -> Configuration {
    let manager = LAST_MANAGER.with(|m| *m.borrow()).unwrap_or(0);
    let Some(c) = fwk_res::ndk_manager_config(manager) else {
        return Configuration::default();
    };
    let lang = String::from_utf8_lossy(&c.language).trim_matches('\0').to_string();
    let country = String::from_utf8_lossy(&c.country).trim_matches('\0').to_string();
    let locale = if country.is_empty() {
        lang
    } else {
        format!("{lang}-{country}")
    };
    let density = c.density as i32;
    let (screen_width, screen_height) = if c.screen_width > 0 && c.screen_height > 0 {
        (c.screen_width as i32, c.screen_height as i32)
    } else {
        // AOSP `getScreenSize` derives the physical size from the screen dp
        // size and the density bucket.
        let scale = density as f32 / 160.0;
        (
            (c.screen_width_dp as f32 * scale) as i32,
            (c.screen_height_dp as f32 * scale) as i32,
        )
    };
    Configuration {
        mcc: c.mcc as i32,
        mnc: c.mnc as i32,
        locale,
        orientation: c.orientation as i32,
        touchscreen: c.touchscreen as i32,
        density,
        keyboard: c.keyboard as i32,
        keyboard_hidden: (c.input_flags & 0x03) as i32,
        navigation: c.navigation as i32,
        screen_width,
        screen_height,
        screen_layout: c.screen_layout as i32,
        ui_mode: c.ui_mode as i32,
        smallest_screen_width_dp: c.smallest_screen_width_dp as i32,
        screen_width_dp: c.screen_width_dp as i32,
        screen_height_dp: c.screen_height_dp as i32,
        color_mode: c.color_mode as i32,
    }
}

// ---------------------------------------------------------------------------
// Window / vsync / frames
// ---------------------------------------------------------------------------

/// The `SurfaceControl` layer a Java `Surface.mNativeObject` wraps, if live.
pub fn surface_layer(surface_handle: u64) -> Option<u64> {
    surface::surface_layer(surface_handle)
}

/// The buffer size of the layer a Java `Surface` wraps, if live.
pub fn surface_size(surface_handle: u64) -> Option<(i32, i32)> {
    surface::surface_size(surface_handle)
}

/// Submit a completed software buffer to the installed frame sink (the
/// runtime's `WmSurfaceBackend` in production, publishing into the
/// `FrameStore`; a `MemorySink` when no compositor is installed).
pub fn submit_frame(window: u64, width: u32, height: u32, rgba: &[u8], cause_id: i64) -> u64 {
    gfx_hwui::submit_frame(window, width, height, rgba, cause_id)
}

/// The virtual vsync interval in nanoseconds (`fwk_vsync`).
pub fn vsync_interval_nanos() -> i64 {
    crate::natives::fwk_vsync::interval_nanos()
}

#[cfg(test)]
mod tests {
    use super::*;
    use acos_framework_natives::res::assets::ApkAssets;
    use acos_framework_natives::res::manager::AssetManager;
    use acos_render::BitmapConfig;
    use crate::natives::{fwk_res, gfx_hwui, gfx_state};

    fn apk_bytes() -> Vec<u8> {
        use std::io::Write;
        let mut buf = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            zip.start_file(
                "assets/hello.txt",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
            zip.write_all(b"hello-acos!!").unwrap();
            zip.finish().unwrap();
        }
        buf
    }

    #[test]
    fn assets_bitmap_and_frame_come_from_real_substrates() {
        // --- assets: a real in-memory APK through the engine -----------------
        let mut mgr = AssetManager::new();
        mgr.add_apk_assets(ApkAssets::from_bytes(apk_bytes()).unwrap());
        let ptr = 0x00A5_5E70;
        fwk_res::test_install_manager(ptr, mgr);
        assert!(asset_manager_exists(ptr), "manager is live");

        let asset = asset_open(ptr, "assets/hello.txt", 3).unwrap();
        assert_eq!(asset_length(asset).unwrap(), 12);
        assert_eq!(asset_remaining(asset).unwrap(), 12);
        assert!(asset_is_allocated(asset));
        let mut out = [0u8; 16];
        assert_eq!(asset_read(asset, &mut out).unwrap(), 12);
        assert_eq!(&out[..12], b"hello-acos!!");
        assert_eq!(asset_seek(asset, 0, 0).unwrap(), 0);
        assert_eq!(asset_remaining(asset).unwrap(), 12);
        // `getBuffer` is non-null and the payload is the entry's bytes.
        let whole = asset_buffer(asset).unwrap();
        assert!(!whole.is_null(), "getBuffer yields the payload address");
        assert_eq!(asset_seek(asset, 0, 0).unwrap(), 0);
        let mut buf = [0u8; 12];
        assert_eq!(asset_read(asset, &mut buf).unwrap(), 12);
        assert_eq!(&buf, b"hello-acos!!");
        asset_close(asset);
        assert!(!asset_is_allocated(asset), "closed handle is dead");

        let dir = asset_dir_open(ptr, "assets").unwrap();
        assert_eq!(asset_dir_next(dir).as_deref(), Some("hello.txt"));
        assert_eq!(asset_dir_next(dir), None);
        asset_dir_rewind(dir);
        assert_eq!(asset_dir_next(dir).as_deref(), Some("hello.txt"));
        asset_dir_close(dir);

        // --- bitmap: lock writes the real pixel store ------------------------
        let handle = gfx_state::with_ctx(|c| {
            gfx_state::gj(
                c.bitmaps
                    .insert(acos_render::Bitmap::new(4, 3, BitmapConfig::Rgba8888).unwrap()),
            )
        });
        let info = bitmap_lock(handle).unwrap();
        assert_eq!((info.width, info.height, info.stride, info.format), (4, 3, 16, 1));
        // The locked address is the bitmap's own pixel store (`acos-vm`
        // forbids `unsafe`, so compare pointers rather than dereferencing).
        let owned = gfx_state::with_ctx(|c| {
            c.bitmaps
                .get_mut(gfx_state::rh(handle))
                .unwrap()
                .pixels_mut()
                .as_mut_ptr() as *mut u8
        });
        assert_eq!(info.pixels, owned, "lock exposes the real pixel store");
        bitmap_unlock(handle).unwrap();
        // A write to the same store is visible through `getPixel`.
        gfx_state::with_ctx(|c| {
            c.bitmaps
                .get_mut(gfx_state::rh(handle))
                .unwrap()
                .set_pixel(0, 0, 0xFF11_2233);
        });
        gfx_state::with_ctx(|c| {
            assert_eq!(
                c.bitmaps.get(gfx_state::rh(handle)).unwrap().get_pixel(0, 0),
                0xFF11_2233,
                "the store write is visible to Bitmap.getPixel"
            );
        });
        // A non-RGBA format is a structured error, never a silent success.
        let rgb565 = gfx_state::with_ctx(|c| {
            gfx_state::gj(
                c.bitmaps
                    .insert(acos_render::Bitmap::new(2, 2, BitmapConfig::Rgb565).unwrap()),
            )
        });
        assert!(bitmap_lock(rgb565).is_err(), "RGB_565 has no NDK layout");

        // --- frame: a real buffer submission reaches the sink ----------------
        #[derive(Default)]
        struct Shared {
            calls: std::sync::Mutex<Vec<(u64, u32, u32, Vec<u8>, i64)>>,
            seq: std::sync::atomic::AtomicU64,
        }
        #[derive(Clone)]
        struct RecordingSink(std::sync::Arc<Shared>);
        impl gfx_hwui::FrameSink for RecordingSink {
            fn submit_frame(
                &mut self,
                window_id: u64,
                width: u32,
                height: u32,
                rgba: &[u8],
                cause_id: i64,
            ) -> u64 {
                self.0
                    .calls
                    .lock()
                    .unwrap()
                    .push((window_id, width, height, rgba.to_vec(), cause_id));
                self.0.seq.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1
            }
        }
        let shared = std::sync::Arc::new(Shared::default());
        gfx_hwui::install_sink(Box::new(RecordingSink(std::sync::Arc::clone(&shared))));
        let rgba = [1u8, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 10, 11, 12, 255];
        let frame = submit_frame(0x1234, 2, 2, &rgba, 7);
        assert_eq!(frame, 1, "the sink accepted the frame");
        let calls = shared.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], (0x1234, 2, 2, rgba.to_vec(), 7));
    }
}
