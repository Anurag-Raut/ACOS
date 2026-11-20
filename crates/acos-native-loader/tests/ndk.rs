//! NDK platform surface: `libandroid` / `libjnigraphics` / `liblog` / EGL.
//!
//! A real NDK-compiled fixture (`fixtures/jni/acos_ndk_surface.c`, checked in as
//! `tests/data/libacosndk.so`) imports every API through the loader's bionic
//! table. The tables are answered by the [`NdkHost`] seam; this test installs a
//! host backed by **real** ACOS substrates:
//!
//! * assets come from a real APK zip (`AAssetManager_open` reads the entry),
//! * bitmaps are real `acos_render::Bitmap` pixel stores — the same storage the
//!   Java `Bitmap.getPixel` native reads, so a C write is visible to `getPixel`,
//! * `ANativeWindow_lock` allocates a real RGBA_8888 buffer and
//!   `unlockAndPost` publishes it into a frame store,
//! * `AConfiguration` reports a real system configuration,
//! * `AChoreographer` callbacks are dispatched by the host's vsync seam.
//!
//! EGL/GLES resolve to reported traps: the fixture asserts
//! `EGL_NO_DISPLAY`/`EGL_NOT_INITIALIZED`/`GL_INVALID_OPERATION`, never success.

mod common;

use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::os::raw::{c_char, c_int, c_void};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use acos_native_loader::{
    clear_ndk_host, install_ndk_host, ARect, BitmapInfo, FrameCallback, Loader, LoaderConfig,
    NdkConfiguration, NdkHost, NdkResult, WindowBuffer, WindowInfo,
};
use acos_render::{Bitmap, BitmapConfig};

const BITMAP_KEY: usize = 0xB17;
const SURFACE_KEY: usize = 0x5F4;

#[derive(Default)]
struct Frame {
    width: i32,
    height: i32,
    format: i32,
    stride: i32,
    pixels: Vec<u8>,
}

struct Window {
    width: i32,
    height: i32,
    format: i32,
    stride: i32,
    buffer: Vec<u8>,
    locked: bool,
}

struct Asset {
    data: Box<[u8]>,
    pos: usize,
}

struct DirEntry {
    names: Vec<String>,
    next: usize,
}

struct TestHost {
    apk: std::path::PathBuf,
    next: Mutex<u64>,
    assets: Mutex<HashMap<u64, Asset>>,
    dirs: Mutex<HashMap<u64, DirEntry>>,
    bitmaps: Mutex<HashMap<usize, Bitmap>>,
    windows: Mutex<HashMap<u64, Window>>,
    frames: Mutex<Vec<Frame>>,
    frames_seen: AtomicUsize,
    config: NdkConfiguration,
}

impl TestHost {
    fn id(&self) -> u64 {
        let mut g = self.next.lock().unwrap();
        *g += 1;
        *g
    }

    fn read_entry(&self, name: &str) -> Option<Box<[u8]>> {
        let file = File::open(&self.apk).ok()?;
        let mut zip = zip::ZipArchive::new(file).ok()?;
        let mut entry = zip.by_name(name).ok()?;
        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut buf).ok()?;
        Some(buf.into_boxed_slice())
    }
}

impl NdkHost for TestHost {
    fn asset_manager_from_java(&self, _env: usize, _manager: usize) -> NdkResult<u64> {
        // The sentinel `jobject` names the installed APK; a real runtime resolves
        // `AssetManager.mNativePtr` instead.
        Ok(self.id())
    }

    fn asset_open(&self, _manager: u64, filename: &str, _mode: i32) -> NdkResult<u64> {
        let data = self
            .read_entry(filename)
            .ok_or_else(|| format!("asset not in APK: {filename}"))?;
        let id = self.id();
        self.assets
            .lock()
            .unwrap()
            .insert(id, Asset { data, pos: 0 });
        Ok(id)
    }

    fn asset_read(&self, asset: u64, buf: &mut [u8]) -> NdkResult<usize> {
        let mut g = self.assets.lock().unwrap();
        let a = g.get_mut(&asset).ok_or("unknown asset")?;
        let n = buf.len().min(a.data.len().saturating_sub(a.pos));
        buf[..n].copy_from_slice(&a.data[a.pos..a.pos + n]);
        a.pos += n;
        Ok(n)
    }

    fn asset_seek(&self, asset: u64, offset: i64, whence: i32) -> NdkResult<i64> {
        let mut g = self.assets.lock().unwrap();
        let a = g.get_mut(&asset).ok_or("unknown asset")?;
        let base = match whence {
            0 => 0,
            1 => a.pos as i64,
            2 => a.data.len() as i64,
            _ => return Err(format!("bad whence {whence}")),
        };
        let pos = (base + offset).clamp(0, a.data.len() as i64);
        a.pos = pos as usize;
        Ok(pos)
    }

    fn asset_length(&self, asset: u64) -> NdkResult<i64> {
        let g = self.assets.lock().unwrap();
        Ok(g.get(&asset).ok_or("unknown asset")?.data.len() as i64)
    }

    fn asset_remaining(&self, asset: u64) -> NdkResult<i64> {
        let g = self.assets.lock().unwrap();
        let a = g.get(&asset).ok_or("unknown asset")?;
        Ok((a.data.len() - a.pos) as i64)
    }

    fn asset_buffer(&self, asset: u64) -> NdkResult<*const c_void> {
        let g = self.assets.lock().unwrap();
        let a = g.get(&asset).ok_or("unknown asset")?;
        Ok(a.data.as_ptr() as *const c_void)
    }

    fn asset_is_allocated(&self, asset: u64) -> NdkResult<bool> {
        Ok(self.assets.lock().unwrap().contains_key(&asset))
    }

    fn asset_close(&self, asset: u64) -> NdkResult<()> {
        self.assets.lock().unwrap().remove(&asset);
        Ok(())
    }

    fn asset_dir_open(&self, _manager: u64, dir: &str) -> NdkResult<u64> {
        let file = File::open(&self.apk).map_err(|e| e.to_string())?;
        let mut zip = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
        let prefix = format!("{}/", dir.trim_end_matches('/'));
        let mut names = Vec::new();
        for i in 0..zip.len() {
            let entry = zip.by_index(i).map_err(|e| e.to_string())?;
            if let Some(rest) = entry.name().strip_prefix(&prefix) {
                if !rest.is_empty() && !rest.contains('/') {
                    names.push(rest.to_string());
                }
            }
        }
        if names.is_empty() {
            return Err(format!("asset dir not in APK: {dir}"));
        }
        let id = self.id();
        self.dirs
            .lock()
            .unwrap()
            .insert(id, DirEntry { names, next: 0 });
        Ok(id)
    }

    fn asset_dir_next(&self, dir: u64) -> NdkResult<Option<String>> {
        let mut g = self.dirs.lock().unwrap();
        let d = g.get_mut(&dir).ok_or("unknown asset dir")?;
        if d.next >= d.names.len() {
            return Ok(None);
        }
        let n = d.names[d.next].clone();
        d.next += 1;
        Ok(Some(n))
    }

    fn asset_dir_rewind(&self, dir: u64) -> NdkResult<()> {
        let mut g = self.dirs.lock().unwrap();
        let d = g.get_mut(&dir).ok_or("unknown asset dir")?;
        d.next = 0;
        Ok(())
    }

    fn asset_dir_close(&self, dir: u64) -> NdkResult<()> {
        self.dirs.lock().unwrap().remove(&dir);
        Ok(())
    }

    fn bitmap_info(&self, _env: usize, _bitmap: usize) -> NdkResult<BitmapInfo> {
        let g = self.bitmaps.lock().unwrap();
        let b = g.get(&BITMAP_KEY).ok_or("unknown bitmap")?;
        Ok(BitmapInfo {
            width: b.width(),
            height: b.height(),
            stride: b.width() * 4,
            format: 1, // ANDROID_BITMAP_FORMAT_RGBA_8888
            flags: 1,  // ANDROID_BITMAP_FLAGS_ALPHA_PREMUL
            pixels: std::ptr::null_mut(),
        })
    }

    fn bitmap_lock(&self, _env: usize, _bitmap: usize) -> NdkResult<BitmapInfo> {
        let mut g = self.bitmaps.lock().unwrap();
        let b = g.get_mut(&BITMAP_KEY).ok_or("unknown bitmap")?;
        if b.config() != BitmapConfig::Rgba8888 {
            return Err(format!(
                "bitmap config {:?} has no NDK-compatible layout",
                b.config()
            ));
        }
        let width = b.width();
        let height = b.height();
        let stride = width * 4;
        let pixels = b.pixels_mut();
        Ok(BitmapInfo {
            width,
            height,
            stride,
            format: 1,
            flags: 1,
            pixels: pixels.as_mut_ptr() as *mut u8,
        })
    }

    fn bitmap_unlock(&self, _env: usize, _bitmap: usize) -> NdkResult<()> {
        if self.bitmaps.lock().unwrap().contains_key(&BITMAP_KEY) {
            Ok(())
        } else {
            Err("unknown bitmap".into())
        }
    }

    fn window_from_surface(&self, _env: usize, _surface: usize) -> NdkResult<u64> {
        Ok(SURFACE_KEY as u64)
    }

    fn window_acquire(&self, _window: u64) -> NdkResult<()> {
        Ok(())
    }

    fn window_release(&self, _window: u64) -> NdkResult<()> {
        Ok(())
    }

    fn window_info(&self, window: u64) -> NdkResult<WindowInfo> {
        let g = self.windows.lock().unwrap();
        let w = g.get(&window).ok_or("unknown window")?;
        Ok(WindowInfo {
            width: w.width,
            height: w.height,
            format: w.format,
        })
    }

    fn window_set_geometry(
        &self,
        window: u64,
        width: i32,
        height: i32,
        format: i32,
    ) -> NdkResult<()> {
        let mut g = self.windows.lock().unwrap();
        let w = g.get_mut(&window).ok_or("unknown window")?;
        w.width = width;
        w.height = height;
        w.format = format;
        w.stride = width;
        w.buffer = vec![0u8; (width * height * 4) as usize];
        Ok(())
    }

    fn window_lock(&self, window: u64, _dirty: Option<ARect>) -> NdkResult<WindowBuffer> {
        let mut g = self.windows.lock().unwrap();
        let w = g.get_mut(&window).ok_or("unknown window")?;
        w.locked = true;
        Ok(WindowBuffer {
            width: w.width,
            height: w.height,
            stride: w.stride,
            format: w.format,
            bits: w.buffer.as_mut_ptr(),
        })
    }

    fn window_unlock_and_post(&self, window: u64) -> NdkResult<()> {
        let mut g = self.windows.lock().unwrap();
        let w = g.get_mut(&window).ok_or("unknown window")?;
        if !w.locked {
            return Err("unlockAndPost without lock".into());
        }
        w.locked = false;
        self.frames.lock().unwrap().push(Frame {
            width: w.width,
            height: w.height,
            format: w.format,
            stride: w.stride,
            pixels: w.buffer.clone(),
        });
        Ok(())
    }

    fn window_to_surface(&self, _env: usize, _window: u64) -> NdkResult<u64> {
        Ok(SURFACE_KEY as u64)
    }

    fn configuration(&self) -> NdkResult<NdkConfiguration> {
        Ok(self.config.clone())
    }

    fn choreographer_post(
        &self,
        _delay_nanos: i64,
        cb: FrameCallback,
        data: *mut c_void,
    ) -> NdkResult<u64> {
        if let Some(cb) = cb {
            // SAFETY: the fixture registered this C callback for the frame.
            unsafe { cb(1_000_000, data) };
        }
        self.frames_seen.fetch_add(1, Ordering::SeqCst);
        Ok(self.id())
    }
}

fn build_apk(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("app.apk");
    let file = File::create(&path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let opts = zip::write::SimpleFileOptions::default();
    zip.start_file("assets/hello.txt", opts).unwrap();
    zip.write_all(b"hello-acos!!").unwrap();
    zip.finish().unwrap();
    path
}

#[test]
fn ndk_surface_is_backed_by_real_acos_state() {
    let _g = common::serial();
    let scratch = std::env::temp_dir().join(format!("acos-ndk-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();
    let apk = build_apk(&scratch);

    let host = Arc::new(TestHost {
        apk,
        next: Mutex::new(0),
        assets: Mutex::new(HashMap::new()),
        dirs: Mutex::new(HashMap::new()),
        bitmaps: Mutex::new(HashMap::from([(
            BITMAP_KEY,
            Bitmap::new(4, 3, BitmapConfig::Rgba8888).unwrap(),
        )])),
        windows: Mutex::new(HashMap::from([(
            SURFACE_KEY as u64,
            Window {
                width: 8,
                height: 4,
                format: 1,
                stride: 8,
                buffer: vec![0u8; 8 * 4 * 4],
                locked: false,
            },
        )])),
        frames: Mutex::new(Vec::new()),
        frames_seen: AtomicUsize::new(0),
        config: NdkConfiguration {
            density: 420,
            orientation: 1,
            screen_width_dp: 480,
            screen_height_dp: 800,
            ..Default::default()
        },
    });
    install_ndk_host(host.clone());

    // Trusted checked-in test object in an unconfined test process; use the
    // explicit test-only confinement opt-out (ADR-0022).
    let loader = Loader::new(
        LoaderConfig::for_tests_without_confinement().with_search_path(common::data_dir()),
    );
    let h = loader.load_file(&common::data("libacosndk.so")).unwrap();

    // --- assets: bytes come from the APK entry ---------------------------
    let asset_test: extern "C" fn(*mut c_void, *const c_char, *const c_char) -> c_int =
        unsafe { common::sym(h.symbol("acos_ndk_asset_test").unwrap()) };
    let name = std::ffi::CString::new("assets/hello.txt").unwrap();
    let dir = std::ffi::CString::new("assets").unwrap();
    assert_eq!(
        asset_test(0xA5 as *mut c_void, name.as_ptr(), dir.as_ptr()),
        0,
        "AAsset* behaviour (bits 0..12)"
    );

    // --- bitmap: native writes are visible to Java Bitmap.getPixel -------
    let bitmap_test: extern "C" fn(*mut c_void) -> c_int =
        unsafe { common::sym(h.symbol("acos_ndk_bitmap_test").unwrap()) };
    assert_eq!(
        bitmap_test(BITMAP_KEY as *mut c_void),
        0,
        "AndroidBitmap_* behaviour"
    );
    {
        let g = host.bitmaps.lock().unwrap();
        let b = g.get(&BITMAP_KEY).unwrap();
        assert_eq!(b.get_pixel(0, 0), 0xFF11_2233, "native lock write visible");
        assert_eq!(
            b.get_pixel(2, 1),
            0xFFAA_BBCC,
            "second locked pixel visible"
        );
        assert_eq!(b.get_pixel(3, 2), 0, "untouched pixel stays zero");
    }

    // --- window: lock/post publishes a real RGBA_8888 frame --------------
    let window_test: extern "C" fn(*mut c_void) -> c_int =
        unsafe { common::sym(h.symbol("acos_ndk_window_test").unwrap()) };
    assert_eq!(
        window_test(SURFACE_KEY as *mut c_void),
        0,
        "ANativeWindow_* behaviour"
    );
    {
        let frames = host.frames.lock().unwrap();
        assert_eq!(frames.len(), 1, "unlockAndPost published exactly one frame");
        let f = &frames[0];
        assert_eq!((f.width, f.height, f.format), (8, 4, 1));
        assert_eq!(f.stride, 8);
        assert_eq!(f.pixels.len(), 8 * 4 * 4);
        // Pixel (3,2): R=48, G=64, B=0x7f, A=0xff (RGBA byte order).
        let off = (2 * f.stride + 3) as usize * 4;
        assert_eq!(&f.pixels[off..off + 4], &[48, 64, 0x7f, 0xff]);
    }

    // --- looper, configuration, choreographer, EGL traps -----------------
    let looper_test: extern "C" fn() -> c_int =
        unsafe { common::sym(h.symbol("acos_ndk_looper_test").unwrap()) };
    assert_eq!(looper_test(), 0, "ALooper_* behaviour");

    let config_test: extern "C" fn() -> c_int =
        unsafe { common::sym(h.symbol("acos_ndk_config_test").unwrap()) };
    assert_eq!(config_test(), 0, "AConfiguration_* behaviour");

    let frame_test: extern "C" fn() -> c_int =
        unsafe { common::sym(h.symbol("acos_ndk_choreographer_test").unwrap()) };
    assert_eq!(frame_test(), 0, "AChoreographer_* behaviour");
    assert_eq!(host.frames_seen.load(Ordering::SeqCst), 2);

    let egl_test: extern "C" fn() -> c_int =
        unsafe { common::sym(h.symbol("acos_ndk_egl_test").unwrap()) };
    assert_eq!(
        egl_test(),
        0,
        "EGL/GLES report EGL_NO_DISPLAY, never success"
    );

    loader.unload(&h).unwrap();
    clear_ndk_host();
    let _ = std::fs::remove_dir_all(&scratch);
}
