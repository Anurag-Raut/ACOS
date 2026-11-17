//! EGL display/config/context/surface objects over the software rasteriser.
//!
//! An EGL **window** surface is backed by an `ANativeWindow` handle the host
//! resolves; `eglSwapBuffers` hands the colour buffer to the installed
//! [`WindowHost`], which is the ACOS compositor seam (`ANativeWindow` →
//! `FrameStore`). An EGL **pbuffer** surface is the off-screen equivalent and
//! never publishes a frame.
//!
//! Fail-closed rules (ADR-0025 / ADR-0018):
//!
//! * `eglCreateWindowSurface` on a native window the host cannot resolve is
//!   `EGL_BAD_NATIVE_WINDOW` — never a black surface.
//! * `eglSwapBuffers` with no window host installed is `EGL_BAD_SURFACE` plus a
//!   structured `UNSUPPORTED_GLES` line — never a silently-dropped frame.
//! * Every other unsupported entry point returns its documented `EGL_*` error
//!   and logs once.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use crate::error::{diagnostic, unsupported};
use crate::gl::Gl;
use crate::raster::Framebuffer;

/// `EGL_SUCCESS`.
pub const EGL_SUCCESS: i32 = 0x3000;
/// `EGL_NOT_INITIALIZED`.
pub const EGL_NOT_INITIALIZED: i32 = 0x3001;
/// `EGL_BAD_ACCESS`.
pub const EGL_BAD_ACCESS: i32 = 0x3002;
/// `EGL_BAD_ALLOC`.
pub const EGL_BAD_ALLOC: i32 = 0x3003;
/// `EGL_BAD_ATTRIBUTE`.
pub const EGL_BAD_ATTRIBUTE: i32 = 0x3004;
/// `EGL_BAD_CONFIG`.
pub const EGL_BAD_CONFIG: i32 = 0x3005;
/// `EGL_BAD_CONTEXT`.
pub const EGL_BAD_CONTEXT: i32 = 0x3006;
/// `EGL_BAD_DISPLAY`.
pub const EGL_BAD_DISPLAY: i32 = 0x3008;
/// `EGL_BAD_MATCH`.
pub const EGL_BAD_MATCH: i32 = 0x3009;
/// `EGL_BAD_NATIVE_WINDOW`.
pub const EGL_BAD_NATIVE_WINDOW: i32 = 0x300B;
/// `EGL_BAD_PARAMETER`.
pub const EGL_BAD_PARAMETER: i32 = 0x300C;
/// `EGL_BAD_SURFACE`.
pub const EGL_BAD_SURFACE: i32 = 0x300D;
/// `EGL_BAD_CURRENT_SURFACE`.
pub const EGL_BAD_CURRENT_SURFACE: i32 = 0x3007;

/// `EGL_NO_DISPLAY` / `EGL_NO_CONTEXT` / `EGL_NO_SURFACE`.
pub const EGL_NO_HANDLE: u64 = 0;
/// `EGL_TRUE`.
pub const EGL_TRUE: i32 = 1;
/// `EGL_FALSE`.
pub const EGL_FALSE: i32 = 0;

#[allow(missing_docs)]
pub mod attrib {
    pub const ALPHA_SIZE: i32 = 0x3021;
    pub const BLUE_SIZE: i32 = 0x3022;
    pub const GREEN_SIZE: i32 = 0x3023;
    pub const RED_SIZE: i32 = 0x3024;
    pub const DEPTH_SIZE: i32 = 0x3025;
    pub const STENCIL_SIZE: i32 = 0x3026;
    pub const CONFIG_CAVEAT: i32 = 0x3027;
    pub const CONFIG_ID: i32 = 0x3028;
    pub const LEVEL: i32 = 0x3029;
    pub const MAX_PBUFFER_HEIGHT: i32 = 0x302F;
    pub const MAX_PBUFFER_PIXELS: i32 = 0x3030;
    pub const MAX_PBUFFER_WIDTH: i32 = 0x302E;
    pub const NATIVE_RENDERABLE: i32 = 0x302D;
    pub const SAMPLES: i32 = 0x3032;
    pub const SURFACE_TYPE: i32 = 0x3033;
    pub const TRANSPARENT_TYPE: i32 = 0x3034;
    pub const COLOR_BUFFER_TYPE: i32 = 0x303F;
    pub const RENDERABLE_TYPE: i32 = 0x3040;
    pub const MATCH_NATIVE_PIXMAP: i32 = 0x3041;
    pub const NONE: i32 = 0x3038;
    pub const PBUFFER_BIT: i32 = 0x0001;
    pub const PIXMAP_BIT: i32 = 0x0002;
    pub const WINDOW_BIT: i32 = 0x0004;
    pub const OPENGL_ES_BIT: i32 = 0x0001;
    pub const OPENGL_ES2_BIT: i32 = 0x0004;
    pub const OPENGL_ES3_BIT: i32 = 0x0040;
    pub const DEFAULT_DISPLAY: u64 = 0;
    pub const WIDTH: i32 = 0x3057;
    pub const HEIGHT: i32 = 0x3056;
    pub const CONTEXT_CLIENT_VERSION: i32 = 0x3098;
    pub const OPENGL_ES_API: i32 = 0x30A0;
    pub const DRAW: i32 = 0x3059;
    pub const READ: i32 = 0x305A;
    pub const VENDOR: i32 = 0x3053;
    pub const VERSION: i32 = 0x3054;
    pub const EXTENSIONS: i32 = 0x3055;
    pub const CLIENT_APIS: i32 = 0x308D;
    pub const SWAP_BEHAVIOR: i32 = 0x3093;
    pub const BUFFER_PRESERVED: i32 = 0x3094;
}

/// The display token returned by `eglGetDisplay(EGL_DEFAULT_DISPLAY)`.
///
/// A non-zero value so `EGL_NO_DISPLAY` remains distinguishable; the value is
/// opaque to the guest.
pub const DISPLAY: u64 = 0x4547_4c31;

/// A config the display advertises.
#[derive(Clone, Copy, Debug)]
#[allow(missing_docs)]
pub struct Config {
    pub id: u64,
    pub red: i32,
    pub green: i32,
    pub blue: i32,
    pub alpha: i32,
    pub depth: i32,
    pub stencil: i32,
    pub surface_type: i32,
    pub renderable_type: i32,
    pub samples: i32,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            id: 1,
            red: 8,
            green: 8,
            blue: 8,
            alpha: 8,
            depth: 24,
            stencil: 8,
            surface_type: attrib::WINDOW_BIT | attrib::PBUFFER_BIT | attrib::PIXMAP_BIT,
            renderable_type: attrib::OPENGL_ES_BIT | attrib::OPENGL_ES2_BIT,
            samples: 0,
        }
    }
}

/// The host seam for `ANativeWindow`-backed EGL surfaces.
///
/// `geometry` resolves the native window the guest passed to
/// `eglCreateWindowSurface`; `present` publishes the swapped colour buffer and
/// returns the compositor's frame id (`0` = rejected).
pub trait WindowHost: Send + Sync {
    /// Geometry of the native window, or `None` if the handle is unknown.
    fn geometry(&self, window: u64) -> Option<(u32, u32)>;
    /// Publish one frame; returns the frame id or 0 when the sink rejected it.
    fn present(&self, window: u64, width: u32, height: u32, rgba: &[u8]) -> u64;
}

/// A recording [`WindowHost`] for tests (models the compositor sink).
#[derive(Default)]
pub struct RecordedWindowHost {
    windows: Mutex<HashMap<u64, (u32, u32)>>,
    frames: Mutex<Vec<RecordedFrame>>,
}

/// One frame a [`RecordedWindowHost`] received.
#[derive(Clone, Debug)]
#[allow(missing_docs)]
pub struct RecordedFrame {
    pub window: u64,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl RecordedWindowHost {
    /// Register a native window with a geometry.
    pub fn add_window(&self, window: u64, width: u32, height: u32) {
        if let Ok(mut g) = self.windows.lock() {
            g.insert(window, (width, height));
        }
    }

    /// Frames received so far.
    #[must_use]
    pub fn frames(&self) -> Vec<RecordedFrame> {
        self.frames.lock().map(|g| g.clone()).unwrap_or_default()
    }

    /// The most recent frame.
    #[must_use]
    pub fn last_frame(&self) -> Option<RecordedFrame> {
        self.frames.lock().ok().and_then(|g| g.last().cloned())
    }
}

impl WindowHost for RecordedWindowHost {
    fn geometry(&self, window: u64) -> Option<(u32, u32)> {
        self.windows
            .lock()
            .ok()
            .and_then(|g| g.get(&window).copied())
    }

    fn present(&self, window: u64, width: u32, height: u32, rgba: &[u8]) -> u64 {
        let Ok(mut g) = self.frames.lock() else {
            return 0;
        };
        g.push(RecordedFrame {
            window,
            width,
            height,
            rgba: rgba.to_vec(),
        });
        g.len() as u64
    }
}

fn host_slot() -> &'static RwLock<Option<Arc<dyn WindowHost>>> {
    static HOST: OnceLock<RwLock<Option<Arc<dyn WindowHost>>>> = OnceLock::new();
    HOST.get_or_init(|| RwLock::new(None))
}

/// Install the window host used by `eglCreateWindowSurface`/`eglSwapBuffers`.
pub fn install_window_host(host: Arc<dyn WindowHost>) {
    if let Ok(mut g) = host_slot().write() {
        *g = Some(host);
    }
}

/// Remove the installed window host (test teardown).
pub fn clear_window_host() {
    if let Ok(mut g) = host_slot().write() {
        *g = None;
    }
}

fn window_host() -> Option<Arc<dyn WindowHost>> {
    host_slot().read().ok().and_then(|g| g.clone())
}

/// Surface flavour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum SurfaceKind {
    Window {
        /// The guest's `ANativeWindow*`.
        native: u64,
    },
    Pbuffer,
}

#[allow(missing_docs)]
struct Surface {
    config: u64,
    kind: SurfaceKind,
    fb: Framebuffer,
}

#[derive(Default)]
struct Registry {
    next: u64,
    configs: HashMap<u64, Config>,
    surfaces: HashMap<u64, Surface>,
    contexts: HashMap<u64, Gl>,
    initialized: bool,
}

fn registry() -> &'static Mutex<Registry> {
    static REG: OnceLock<Mutex<Registry>> = OnceLock::new();
    REG.get_or_init(|| {
        let mut reg = Registry {
            next: 1,
            ..Registry::default()
        };
        let cfg = Config::default();
        reg.configs.insert(cfg.id, cfg);
        Mutex::new(reg)
    })
}

#[derive(Clone, Copy, Debug, Default)]
struct Current {
    display: u64,
    draw: u64,
    read: u64,
    context: u64,
}

thread_local! {
    static CURRENT: RefCell<Current> = const { RefCell::new(Current { display: 0, draw: 0, read: 0, context: 0 }) };
    static ERROR: RefCell<i32> = const { RefCell::new(EGL_SUCCESS) };
    static NULL_TARGET: RefCell<Framebuffer> = RefCell::new(Framebuffer::new(0, 0));
}

/// Record an `eglGetError` value (first error wins).
pub fn set_error_pub(code: i32) {
    set_error(code);
}

fn set_error(code: i32) {
    ERROR.with(|e| {
        let mut g = e.borrow_mut();
        if *g == EGL_SUCCESS {
            *g = code;
        }
    });
}

/// `eglGetError`.
pub fn get_error() -> i32 {
    ERROR.with(|e| std::mem::replace(&mut *e.borrow_mut(), EGL_SUCCESS))
}

fn next_id(reg: &mut Registry) -> u64 {
    let id = reg.next;
    reg.next += 1;
    id
}

/// Whether `display` is the one display this substrate provides.
fn check_display(display: u64) -> bool {
    if display != DISPLAY {
        set_error(EGL_BAD_DISPLAY);
        return false;
    }
    let initialized = registry().lock().map(|r| r.initialized).unwrap_or(false);
    if !initialized {
        set_error(EGL_NOT_INITIALIZED);
        return false;
    }
    true
}

/// `eglGetDisplay`.
pub fn get_display(native_display: u64) -> u64 {
    if native_display != attrib::DEFAULT_DISPLAY {
        unsupported(
            "eglGetDisplay",
            format!("native display {native_display:#x} (only EGL_DEFAULT_DISPLAY)"),
        );
        set_error(EGL_BAD_PARAMETER);
        return EGL_NO_HANDLE;
    }
    DISPLAY
}

/// `eglInitialize` — returns `(major, minor)` or `None` on failure.
pub fn initialize(display: u64) -> Option<(i32, i32)> {
    if display != DISPLAY {
        set_error(EGL_BAD_DISPLAY);
        return None;
    }
    match registry().lock() {
        Ok(mut reg) => {
            reg.initialized = true;
            Some((1, 4))
        }
        Err(_) => {
            set_error(EGL_NOT_INITIALIZED);
            None
        }
    }
}

/// `eglTerminate`.
pub fn terminate(display: u64) -> i32 {
    if display != DISPLAY {
        set_error(EGL_BAD_DISPLAY);
        return EGL_FALSE;
    }
    if let Ok(mut reg) = registry().lock() {
        reg.initialized = false;
        reg.surfaces.clear();
        reg.contexts.clear();
    }
    CURRENT.with(|c| *c.borrow_mut() = Current::default());
    EGL_TRUE
}

/// `eglChooseConfig` over the single advertised RGBA8/depth24 config.
///
/// Attribute lists are honoured for the values the substrate can actually
/// provide; a request it cannot satisfy yields zero matching configs, which is
/// what EGL specifies (and what a caller must handle).
pub fn choose_config(display: u64, attribs: &[i32]) -> Vec<u64> {
    if !check_display(display) {
        return Vec::new();
    }
    let Ok(reg) = registry().lock() else {
        set_error(EGL_NOT_INITIALIZED);
        return Vec::new();
    };
    let mut want_min: Vec<(i32, i32)> = Vec::new();
    let mut want_exact: Vec<(i32, i32)> = Vec::new();
    let mut i = 0;
    while i + 1 < attribs.len() {
        let name = attribs[i];
        let value = attribs[i + 1];
        if name == attrib::NONE {
            break;
        }
        match name {
            attrib::RED_SIZE
            | attrib::GREEN_SIZE
            | attrib::BLUE_SIZE
            | attrib::ALPHA_SIZE
            | attrib::DEPTH_SIZE
            | attrib::STENCIL_SIZE
            | attrib::SAMPLES => want_min.push((name, value)),
            attrib::SURFACE_TYPE | attrib::RENDERABLE_TYPE => want_min.push((name, value)),
            attrib::CONFIG_ID | attrib::COLOR_BUFFER_TYPE => want_exact.push((name, value)),
            other => {
                unsupported("eglChooseConfig", format!("attribute {other:#x}"));
                return Vec::new();
            }
        }
        i += 2;
    }
    let mut out = Vec::new();
    for cfg in reg.configs.values() {
        let ok = want_min.iter().all(|(name, v)| match name {
            a if *a == attrib::RED_SIZE => cfg.red >= *v,
            a if *a == attrib::GREEN_SIZE => cfg.green >= *v,
            a if *a == attrib::BLUE_SIZE => cfg.blue >= *v,
            a if *a == attrib::ALPHA_SIZE => cfg.alpha >= *v,
            a if *a == attrib::DEPTH_SIZE => cfg.depth >= *v,
            a if *a == attrib::STENCIL_SIZE => cfg.stencil >= *v,
            a if *a == attrib::SAMPLES => cfg.samples >= *v,
            a if *a == attrib::SURFACE_TYPE => cfg.surface_type & *v != 0,
            a if *a == attrib::RENDERABLE_TYPE => cfg.renderable_type & *v != 0,
            _ => true,
        }) && want_exact.iter().all(|(name, v)| match name {
            a if *a == attrib::CONFIG_ID => cfg.id as i32 == *v,
            a if *a == attrib::COLOR_BUFFER_TYPE => *v == 0x308E,
            _ => true,
        });
        if ok {
            out.push(cfg.id);
        }
    }
    out.sort_unstable();
    out
}

/// `eglGetConfigAttrib`.
pub fn get_config_attrib(display: u64, config: u64, name: i32) -> Option<i32> {
    if !check_display(display) {
        return None;
    }
    let Ok(reg) = registry().lock() else {
        return None;
    };
    let Some(cfg) = reg.configs.get(&config) else {
        set_error(EGL_BAD_CONFIG);
        return None;
    };
    let v = match name {
        attrib::RED_SIZE => cfg.red,
        attrib::GREEN_SIZE => cfg.green,
        attrib::BLUE_SIZE => cfg.blue,
        attrib::ALPHA_SIZE => cfg.alpha,
        attrib::DEPTH_SIZE => cfg.depth,
        attrib::STENCIL_SIZE => cfg.stencil,
        attrib::SURFACE_TYPE => cfg.surface_type,
        attrib::RENDERABLE_TYPE => cfg.renderable_type,
        attrib::SAMPLES => cfg.samples,
        attrib::CONFIG_ID => cfg.id as i32,
        attrib::CONFIG_CAVEAT => 0x3038,
        attrib::NATIVE_RENDERABLE => 0,
        attrib::COLOR_BUFFER_TYPE => 0x308E,
        attrib::TRANSPARENT_TYPE => 0x3038,
        attrib::LEVEL => 0,
        attrib::MAX_PBUFFER_WIDTH | attrib::MAX_PBUFFER_HEIGHT => {
            crate::error::consts::MAX_DIM as i32
        }
        attrib::MAX_PBUFFER_PIXELS => {
            (crate::error::consts::MAX_DIM * crate::error::consts::MAX_DIM) as i32
        }
        other => {
            unsupported("eglGetConfigAttrib", format!("attribute {other:#x}"));
            set_error(EGL_BAD_ATTRIBUTE);
            return None;
        }
    };
    Some(v)
}

/// `eglCreateWindowSurface`.
pub fn create_window_surface(display: u64, config: u64, native_window: u64) -> u64 {
    if !check_display(display) {
        return EGL_NO_HANDLE;
    }
    let Some(host) = window_host() else {
        unsupported(
            "eglCreateWindowSurface",
            "no ANativeWindow host installed; a window surface cannot be created",
        );
        set_error(EGL_BAD_NATIVE_WINDOW);
        return EGL_NO_HANDLE;
    };
    let Some((width, height)) = host.geometry(native_window) else {
        unsupported(
            "eglCreateWindowSurface",
            format!("unknown ANativeWindow {native_window:#x}"),
        );
        set_error(EGL_BAD_NATIVE_WINDOW);
        return EGL_NO_HANDLE;
    };
    if width == 0
        || height == 0
        || width > crate::error::consts::MAX_DIM
        || height > crate::error::consts::MAX_DIM
    {
        set_error(EGL_BAD_ALLOC);
        return EGL_NO_HANDLE;
    }
    let Ok(mut reg) = registry().lock() else {
        set_error(EGL_NOT_INITIALIZED);
        return EGL_NO_HANDLE;
    };
    if !reg.configs.contains_key(&config) {
        set_error(EGL_BAD_CONFIG);
        return EGL_NO_HANDLE;
    }
    let id = next_id(&mut reg);
    reg.surfaces.insert(
        id,
        Surface {
            config,
            kind: SurfaceKind::Window {
                native: native_window,
            },
            fb: Framebuffer::new(width, height),
        },
    );
    id
}

/// `eglCreatePbufferSurface`.
pub fn create_pbuffer_surface(display: u64, config: u64, attribs: &[i32]) -> u64 {
    if !check_display(display) {
        return EGL_NO_HANDLE;
    }
    let mut width = 0i32;
    let mut height = 0i32;
    let mut i = 0;
    while i + 1 < attribs.len() {
        match attribs[i] {
            attrib::NONE => break,
            attrib::WIDTH => width = attribs[i + 1],
            attrib::HEIGHT => height = attribs[i + 1],
            other => {
                unsupported("eglCreatePbufferSurface", format!("attribute {other:#x}"));
                set_error(EGL_BAD_ATTRIBUTE);
                return EGL_NO_HANDLE;
            }
        }
        i += 2;
    }
    if width <= 0
        || height <= 0
        || width as u32 > crate::error::consts::MAX_DIM
        || height as u32 > crate::error::consts::MAX_DIM
    {
        set_error(EGL_BAD_ATTRIBUTE);
        return EGL_NO_HANDLE;
    }
    let Ok(mut reg) = registry().lock() else {
        set_error(EGL_NOT_INITIALIZED);
        return EGL_NO_HANDLE;
    };
    if !reg.configs.contains_key(&config) {
        set_error(EGL_BAD_CONFIG);
        return EGL_NO_HANDLE;
    }
    let id = next_id(&mut reg);
    reg.surfaces.insert(
        id,
        Surface {
            config,
            kind: SurfaceKind::Pbuffer,
            fb: Framebuffer::new(width as u32, height as u32),
        },
    );
    id
}

/// `eglDestroySurface`.
pub fn destroy_surface(display: u64, surface: u64) -> i32 {
    if !check_display(display) {
        return EGL_FALSE;
    }
    let Ok(mut reg) = registry().lock() else {
        return EGL_FALSE;
    };
    if reg.surfaces.remove(&surface).is_none() {
        set_error(EGL_BAD_SURFACE);
        return EGL_FALSE;
    }
    CURRENT.with(|c| {
        let mut cur = c.borrow_mut();
        if cur.draw == surface {
            cur.draw = 0;
        }
        if cur.read == surface {
            cur.read = 0;
        }
    });
    EGL_TRUE
}

/// `eglCreateContext` — `client_version` must be 2 (GLES2).
pub fn create_context(display: u64, config: u64, share: u64, attribs: &[i32]) -> u64 {
    if !check_display(display) {
        return EGL_NO_HANDLE;
    }
    let mut version = 2;
    let mut i = 0;
    while i + 1 < attribs.len() {
        match attribs[i] {
            attrib::NONE => break,
            attrib::CONTEXT_CLIENT_VERSION => version = attribs[i + 1],
            other => {
                unsupported("eglCreateContext", format!("attribute {other:#x}"));
                set_error(EGL_BAD_ATTRIBUTE);
                return EGL_NO_HANDLE;
            }
        }
        i += 2;
    }
    if version != 2 {
        unsupported(
            "eglCreateContext",
            format!("EGL_CONTEXT_CLIENT_VERSION {version} (only GLES2 is provided)"),
        );
        set_error(EGL_BAD_ATTRIBUTE);
        return EGL_NO_HANDLE;
    }
    let Ok(mut reg) = registry().lock() else {
        set_error(EGL_NOT_INITIALIZED);
        return EGL_NO_HANDLE;
    };
    if !reg.configs.contains_key(&config) {
        set_error(EGL_BAD_CONFIG);
        return EGL_NO_HANDLE;
    }
    if share != 0 && !reg.contexts.contains_key(&share) {
        set_error(EGL_BAD_CONTEXT);
        return EGL_NO_HANDLE;
    }
    let id = next_id(&mut reg);
    reg.contexts.insert(id, Gl::new());
    if share != 0 {
        diagnostic(
            "eglCreateContext",
            "share contexts share no objects in this substrate (per-context GLES2 objects)",
        );
    }
    id
}

/// `eglDestroyContext`.
pub fn destroy_context(display: u64, context: u64) -> i32 {
    if !check_display(display) {
        return EGL_FALSE;
    }
    let Ok(mut reg) = registry().lock() else {
        return EGL_FALSE;
    };
    if context != 0 && reg.contexts.remove(&context).is_none() {
        set_error(EGL_BAD_CONTEXT);
        return EGL_FALSE;
    }
    CURRENT.with(|c| {
        let mut cur = c.borrow_mut();
        if cur.context == context {
            cur.context = 0;
        }
    });
    EGL_TRUE
}

/// `eglMakeCurrent`.
pub fn make_current(display: u64, draw: u64, read: u64, context: u64) -> i32 {
    if !check_display(display) {
        return EGL_FALSE;
    }
    {
        let Ok(reg) = registry().lock() else {
            return EGL_FALSE;
        };
        if draw != 0 && !reg.surfaces.contains_key(&draw) {
            set_error(EGL_BAD_SURFACE);
            return EGL_FALSE;
        }
        if read != 0 && !reg.surfaces.contains_key(&read) {
            set_error(EGL_BAD_SURFACE);
            return EGL_FALSE;
        }
        if context != 0 && !reg.contexts.contains_key(&context) {
            set_error(EGL_BAD_CONTEXT);
            return EGL_FALSE;
        }
        if context != 0 && draw == 0 {
            unsupported(
                "eglMakeCurrent",
                "a context requires a draw surface (no surfaceless contexts)",
            );
            set_error(EGL_BAD_MATCH);
            return EGL_FALSE;
        }
    }
    CURRENT.with(|c| {
        *c.borrow_mut() = Current {
            display,
            draw,
            read,
            context,
        }
    });
    EGL_TRUE
}

/// `eglSwapBuffers` — publish the colour buffer through the window host.
pub fn swap_buffers(display: u64, surface: u64) -> i32 {
    if !check_display(display) {
        return EGL_FALSE;
    }
    let snapshot = {
        let Ok(mut reg) = registry().lock() else {
            return EGL_FALSE;
        };
        let Some(s) = reg.surfaces.get_mut(&surface) else {
            set_error(EGL_BAD_SURFACE);
            return EGL_FALSE;
        };
        match s.kind {
            SurfaceKind::Pbuffer => return EGL_TRUE,
            SurfaceKind::Window { native } => {
                let (w, h, pixels) = (s.fb.width, s.fb.height, s.fb.color.clone());
                Some((native, w, h, pixels))
            }
        }
    };
    let Some((native, width, height, pixels)) = snapshot else {
        return EGL_FALSE;
    };
    let Some(host) = window_host() else {
        unsupported(
            "eglSwapBuffers",
            "no ANativeWindow host installed; the frame cannot be published",
        );
        set_error(EGL_BAD_SURFACE);
        return EGL_FALSE;
    };
    // Re-read the geometry: a window may have been resized since creation.
    if let Some((w, h)) = host.geometry(native) {
        if w != width || h != height {
            if let Ok(mut reg) = registry().lock() {
                if let Some(s) = reg.surfaces.get_mut(&surface) {
                    s.fb.resize(w, h);
                }
            }
            let Ok(reg) = registry().lock() else {
                return EGL_FALSE;
            };
            let Some(s) = reg.surfaces.get(&surface) else {
                return EGL_FALSE;
            };
            let (w2, h2, px2) = (s.fb.width, s.fb.height, s.fb.color.clone());
            let frame = host.present(native, w2, h2, &px2);
            return if frame == 0 {
                set_error(EGL_BAD_SURFACE);
                EGL_FALSE
            } else {
                EGL_TRUE
            };
        }
    }
    let frame = host.present(native, width, height, &pixels);
    if frame == 0 {
        set_error(EGL_BAD_SURFACE);
        return EGL_FALSE;
    }
    EGL_TRUE
}

/// `eglQuerySurface` — `(width, height)` or a config/swap-behaviour value.
pub fn query_surface(display: u64, surface: u64, name: i32) -> Option<i32> {
    if !check_display(display) {
        return None;
    }
    let Ok(reg) = registry().lock() else {
        return None;
    };
    let Some(s) = reg.surfaces.get(&surface) else {
        set_error(EGL_BAD_SURFACE);
        return None;
    };
    Some(match name {
        attrib::WIDTH => s.fb.width as i32,
        attrib::HEIGHT => s.fb.height as i32,
        attrib::CONFIG_ID => s.config as i32,
        attrib::SWAP_BEHAVIOR => attrib::BUFFER_PRESERVED,
        other => {
            unsupported("eglQuerySurface", format!("attribute {other:#x}"));
            set_error(EGL_BAD_ATTRIBUTE);
            return None;
        }
    })
}

/// `eglGetCurrentDisplay` / `eglGetCurrentContext` / `eglGetCurrentSurface`.
#[must_use]
pub fn current_display() -> u64 {
    CURRENT.with(|c| c.borrow().display)
}

/// Current context handle (0 = none).
#[must_use]
pub fn current_context() -> u64 {
    CURRENT.with(|c| c.borrow().context)
}

/// Current draw/read surface handle.
#[must_use]
pub fn current_surface(read: bool) -> u64 {
    CURRENT.with(|c| {
        let cur = c.borrow();
        if read {
            cur.read
        } else {
            cur.draw
        }
    })
}

/// `eglQueryString`.
#[must_use]
pub fn query_string(display: u64, name: i32) -> Option<&'static str> {
    if display != DISPLAY {
        return None;
    }
    match name {
        attrib::VENDOR => Some("ACOS"),
        attrib::VERSION => Some("1.4 ACOS-1"),
        // Only what is implemented: no surfaceless contexts, no EGL images,
        // no KHR fence sync. Advertising otherwise would be dishonest.
        attrib::EXTENSIONS => Some(""),
        attrib::CLIENT_APIS => Some("OpenGL_ES"),
        _ => None,
    }
}

/// `eglBindAPI` — OpenGL ES only.
pub fn bind_api(api: i32) -> i32 {
    if api == attrib::OPENGL_ES_API {
        return EGL_TRUE;
    }
    unsupported(
        "eglBindAPI",
        format!("api {api:#x} (only EGL_OPENGL_ES_API)"),
    );
    set_error(EGL_BAD_PARAMETER);
    EGL_FALSE
}

/// `eglReleaseThread`, `eglWaitClient`, `eglSwapInterval`.
pub fn release_thread() -> i32 {
    CURRENT.with(|c| *c.borrow_mut() = Current::default());
    EGL_TRUE
}

/// `eglSwapInterval` — the substrate presents synchronously.
pub fn swap_interval(_display: u64, _interval: i32) -> i32 {
    EGL_TRUE
}

/// Run `f` against the current context and its draw-surface framebuffer.
///
/// Returns `None` when no context (or no draw surface) is current; the caller
/// must then report `GL_INVALID_OPERATION` — there is no implicit context.
pub fn with_current<R>(f: impl FnOnce(&mut Gl, &mut Framebuffer) -> R) -> Option<R> {
    let cur = CURRENT.with(|c| *c.borrow());
    if cur.context == 0 || cur.draw == 0 {
        return None;
    }
    let mut reg = registry().lock().ok()?;
    let Registry {
        contexts, surfaces, ..
    } = &mut *reg;
    let gl = contexts.get_mut(&cur.context)?;
    let fb = &mut surfaces.get_mut(&cur.draw)?.fb;
    Some(f(gl, fb))
}

/// Run `f` against the current read-surface framebuffer (no GL state needed).
pub fn with_read<R>(f: impl FnOnce(&Framebuffer) -> R) -> Option<R> {
    let cur = CURRENT.with(|c| *c.borrow());
    let surface = if cur.read != 0 { cur.read } else { cur.draw };
    let reg = registry().lock().ok()?;
    reg.surfaces.get(&surface).map(|s| f(&s.fb))
}

/// Read-only access to the current context's GL state.
pub fn with_current_state<R>(f: impl FnOnce(&Gl) -> R) -> Option<R> {
    let cur = CURRENT.with(|c| *c.borrow());
    if cur.context == 0 {
        return None;
    }
    let reg = registry().lock().ok()?;
    reg.contexts.get(&cur.context).map(f)
}

/// Run `f` against the current context even without a draw surface.
///
/// Used by entry points that only touch object/state space (`glGenBuffers`,
/// `glCompileShader`, ...): those are legal with a current context and are not
/// allowed to fabricate one.
pub fn with_gl_no_surface<R>(f: impl FnOnce(&mut Gl) -> R) -> Option<R> {
    let cur = CURRENT.with(|c| *c.borrow());
    if cur.context == 0 {
        return None;
    }
    let mut reg = registry().lock().ok()?;
    reg.contexts.get_mut(&cur.context).map(f)
}

/// Run `f` against the current context, using a 0x0 framebuffer when no draw
/// surface is current (so the call reports `GL_INVALID_FRAMEBUFFER_OPERATION`
/// instead of panicking).
pub fn with_gl_or_null<R>(f: impl FnOnce(&mut Gl, &mut Framebuffer) -> R) -> Option<R> {
    if current_context() != 0 && current_surface(false) != 0 {
        return with_current(f);
    }
    NULL_TARGET.with(|fb| with_gl_no_surface(|gl| f(gl, &mut fb.borrow_mut())))
}

/// Reset all EGL state (tests).
pub fn reset() {
    if let Ok(mut reg) = registry().lock() {
        let cfg = Config::default();
        *reg = Registry {
            next: 1,
            ..Registry::default()
        };
        reg.configs.insert(cfg.id, cfg);
    }
    CURRENT.with(|c| *c.borrow_mut() = Current::default());
    ERROR.with(|e| *e.borrow_mut() = EGL_SUCCESS);
    clear_window_host();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The EGL registry and the current-context slot are process-wide; the
    /// tests below mutate them, so they must not interleave.
    fn lock() -> std::sync::MutexGuard<'static, ()> {
        static GUARD: OnceLock<Mutex<()>> = OnceLock::new();
        GUARD
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[test]
    fn display_lifecycle_is_fail_closed() {
        let _g = lock();
        reset();
        // Before `eglInitialize`, every display-dependent call fails closed.
        assert!(choose_config(DISPLAY, &[attrib::RED_SIZE, 8, attrib::NONE]).is_empty());
        assert_eq!(get_error(), EGL_NOT_INITIALIZED);
        assert_eq!(
            create_context(DISPLAY, 1, 0, &[attrib::NONE]),
            EGL_NO_HANDLE
        );
        assert_eq!(get_error(), EGL_NOT_INITIALIZED);
        let d = get_display(attrib::DEFAULT_DISPLAY);
        assert_eq!(d, DISPLAY);
        assert_eq!(initialize(d), Some((1, 4)));
        assert!(choose_config(d, &[attrib::RED_SIZE, 8, attrib::NONE]).contains(&1));
        assert!(choose_config(d, &[attrib::RED_SIZE, 16, attrib::NONE]).is_empty());
        assert_eq!(terminate(d), EGL_TRUE);
        reset();
    }

    #[test]
    fn a_window_surface_requires_a_known_native_window() {
        let _g = lock();
        reset();
        let d = get_display(0);
        initialize(d);
        let host = Arc::new(RecordedWindowHost::default());
        install_window_host(host.clone());
        assert_eq!(
            create_window_surface(d, 1, 0x5EED),
            EGL_NO_HANDLE,
            "unknown window is refused"
        );
        assert_eq!(get_error(), EGL_BAD_NATIVE_WINDOW);
        host.add_window(0x5EED, 4, 3);
        let s = create_window_surface(d, 1, 0x5EED);
        assert_ne!(s, EGL_NO_HANDLE);
        assert_eq!(query_surface(d, s, attrib::WIDTH), Some(4));
        assert_eq!(query_surface(d, s, attrib::HEIGHT), Some(3));
        reset();
    }
}
