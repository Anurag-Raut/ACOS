//! The `ANativeWindow` seam for the software GLES substrate (ADR-0023).
//!
//! `acos-gles` owns the pixels; this host is what makes an `eglCreateWindowSurface`
//! on a real `ANativeWindow` hand its swapped colour buffer to the ACOS
//! compositor. The present path is exactly the NDK path
//! (`ANativeWindow_unlockAndPost`): write the surface's software buffer, then
//! submit to the installed frame sink (`WMS` → `FrameStore`). A swap therefore
//! produces the same `CompletedFrame` the View path produces, so `frame-diff`
//! and screenshots work unchanged.
//!
//! The host carries no state of its own: the window registry lives in
//! [`crate::ndk_host`] thread-locals, so the value is `Send + Sync` and can be
//! installed process-wide.

use std::sync::Arc;

use acos_gles::egl::{self, WindowHost};

use crate::ndk_host;

/// `WindowHost` backed by the runtime's real `ANativeWindow` registry.
#[derive(Debug)]
pub struct AnativeWindowHost;

impl WindowHost for AnativeWindowHost {
    fn geometry(&self, window: u64) -> Option<(u32, u32)> {
        ndk_host::window_geometry(window)
    }

    fn present(&self, window: u64, width: u32, height: u32, rgba: &[u8]) -> u64 {
        ndk_host::window_present(window, width, height, rgba)
    }
}

/// Install the substrate's loader table and the window host.
///
/// Called from the native load path only (`crate::ndk_host::install`), which the
/// CLI gates behind `--native`. Until this runs, `acos-native-loader` keeps its
/// fail-closed EGL/GLES traps.
pub fn install() {
    acos_gles::install_loader_table();
    egl::install_window_host(Arc::new(AnativeWindowHost));
}

/// Remove the loader table and window host (tests).
pub fn uninstall() {
    acos_native_loader::bionic::gles::clear_table();
    egl::clear_window_host();
    egl::reset();
}
