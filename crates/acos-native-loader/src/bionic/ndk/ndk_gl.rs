//! EGL / GLES entry points: **resolved, but structured traps**.
//!
//! ACOS has no GL implementation yet (software GL, i.e. a GLES-on-`acos-render`
//! translation layer, is a later, separately-scoped task — see the note in
//! `results/native-loader/inventory/README.md`). Binding these symbols to
//! `dlsym`-NULL would make a guest crash on a wild call; binding them to
//! success would be a lie. Instead each entry point resolves to a function
//! that reports `UNSUPPORTED_NATIVE_SYMBOL` exactly once and returns the
//! EGL/GLES "no object" value the caller must handle:
//!
//! * `eglGetDisplay` / `eglGetCurrentDisplay` → `EGL_NO_DISPLAY` (0)
//! * every other EGL entry → `EGL_FALSE`/`EGL_NO_*` (0)
//! * `eglGetError` → `EGL_NOT_INITIALIZED` (0x3001), so an app that checks the
//!   error sees "no display", never a false `EGL_SUCCESS`
//! * `glGetError` → `GL_INVALID_OPERATION` (0x0502), never `GL_NO_ERROR`
//! * `glGetString`/`glGetStringi` → `NULL`
//!
//! The inventory classifies every one of them as **partial** (resolved to a
//! reported trap), not as provided.

use std::os::raw::c_void;

/// Whether `name` is an EGL/GLES entry that resolves to a reported trap.
pub fn is_trapped(name: &str) -> bool {
    full_table().iter().any(|(n, _)| *n == name)
}

/// `EGL_NOT_INITIALIZED`.
pub const EGL_NOT_INITIALIZED: u32 = 0x3001;
/// `EGL_NO_DISPLAY`.
pub const EGL_NO_DISPLAY: usize = 0;
/// `GL_INVALID_OPERATION`.
pub const GL_INVALID_OPERATION: u32 = 0x0502;

/// Marker for a GL entry point no ACOS renderer backs yet.
fn gl_trap(symbol: &'static str) -> usize {
    super::unsupported(
        "libEGL/libGLESv2",
        symbol,
        "no GL implementation in ACOS; software GL is a later task",
        0,
    )
}

macro_rules! gl_entries {
    ($($fn_name:ident => $symbol:literal),+ $(,)?) => {
        $(
            #[doc = concat!("`", $symbol, "` — structured unsupported trap.")]
            ///
            /// # Safety
            /// Reports and returns the documented error value.
            pub unsafe extern "C" fn $fn_name() -> usize {
                gl_trap($symbol)
            }
        )+

        /// Table entries for the trapped EGL/GLES surface.
        pub fn table() -> Vec<(&'static str, *const c_void)> {
            vec![$(($symbol, $fn_name as *const c_void)),+]
        }
    };
}

/// `eglGetError` — reports "not initialized", never a false success.
///
/// # Safety
/// Always safe.
pub unsafe extern "C" fn egl_get_error() -> u32 {
    gl_trap("eglGetError");
    EGL_NOT_INITIALIZED
}

/// `glGetError` — reports `GL_INVALID_OPERATION`, never `GL_NO_ERROR`.
///
/// # Safety
/// Always safe.
pub unsafe extern "C" fn gl_get_error() -> u32 {
    gl_trap("glGetError");
    GL_INVALID_OPERATION
}

/// `eglGetDisplay` — consistently `EGL_NO_DISPLAY`.
///
/// # Safety
/// Always safe.
pub unsafe extern "C" fn egl_get_display() -> usize {
    gl_trap("eglGetDisplay");
    EGL_NO_DISPLAY
}

/// `eglGetCurrentDisplay` — consistently `EGL_NO_DISPLAY`.
///
/// # Safety
/// Always safe.
pub unsafe extern "C" fn egl_get_current_display() -> usize {
    gl_trap("eglGetCurrentDisplay");
    EGL_NO_DISPLAY
}

/// Table entries for EGL + GLES (structured traps).
pub fn full_table() -> Vec<(&'static str, *const c_void)> {
    let mut t = table();
    for (name, ptr) in [
        ("eglGetError", egl_get_error as *const c_void),
        ("glGetError", gl_get_error as *const c_void),
        ("eglGetDisplay", egl_get_display as *const c_void),
        (
            "eglGetCurrentDisplay",
            egl_get_current_display as *const c_void,
        ),
    ] {
        if !t.iter().any(|(n, _)| *n == name) {
            t.push((name, ptr));
        }
    }
    t
}

gl_entries!(
    egl_bind_api => "eglBindAPI",
    egl_choose_config => "eglChooseConfig",
    egl_create_context => "eglCreateContext",
    egl_create_pbuffer_surface => "eglCreatePbufferSurface",
    egl_create_window_surface => "eglCreateWindowSurface",
    egl_destroy_context => "eglDestroyContext",
    egl_destroy_surface => "eglDestroySurface",
    egl_get_current_context => "eglGetCurrentContext",
    egl_get_current_surface => "eglGetCurrentSurface",
    egl_get_proc_address => "eglGetProcAddress",
    egl_initialize => "eglInitialize",
    egl_make_current => "eglMakeCurrent",
    egl_query_string => "eglQueryString",
    egl_query_surface => "eglQuerySurface",
    egl_release_thread => "eglReleaseThread",
    egl_swap_buffers => "eglSwapBuffers",
    egl_swap_interval => "eglSwapInterval",
    egl_terminate => "eglTerminate",
    gl_bind_framebuffer => "glBindFramebuffer",
    gl_bind_texture => "glBindTexture",
    gl_clear => "glClear",
    gl_clear_color => "glClearColor",
    gl_client_wait_sync => "glClientWaitSync",
    gl_delete_framebuffers => "glDeleteFramebuffers",
    gl_delete_queries => "glDeleteQueries",
    gl_delete_sync => "glDeleteSync",
    gl_delete_textures => "glDeleteTextures",
    gl_draw_buffers => "glDrawBuffers",
    gl_enable => "glEnable",
    gl_disable => "glDisable",
    gl_fence_sync => "glFenceSync",
    gl_finish => "glFinish",
    gl_flush => "glFlush",
    gl_framebuffer_texture2d => "glFramebufferTexture2D",
    gl_gen_framebuffers => "glGenFramebuffers",
    gl_gen_queries => "glGenQueries",
    gl_gen_textures => "glGenTextures",
    gl_get_framebuffer_attachment_parameteriv => "glGetFramebufferAttachmentParameteriv",
    gl_get_integeri_v => "glGetIntegeri_v",
    gl_get_integerv => "glGetIntegerv",
    gl_get_query_objectuiv => "glGetQueryObjectuiv",
    gl_get_string => "glGetString",
    gl_get_stringi => "glGetStringi",
    gl_pixel_storei => "glPixelStorei",
    gl_read_pixels => "glReadPixels",
    gl_tex_image2d => "glTexImage2D",
    gl_tex_parameteri => "glTexParameteri",
    gl_tex_storage2d => "glTexStorage2D",
    gl_use_program => "glUseProgram",
    gl_viewport => "glViewport",
    gl_wait_sync => "glWaitSync",
);
