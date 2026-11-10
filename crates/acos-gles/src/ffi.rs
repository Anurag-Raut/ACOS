//! `libEGL` / `libGLESv2` C entry points.
//!
//! This is the only module allowed to use `unsafe`: it unpacks C arguments
//! (pointers into guest memory, EGLint arrays) into the typed calls in
//! [`crate::gl`] / [`crate::egl`]. Nothing here allocates guest memory, JITs, or
//! executes guest code.
//!
//! Every entry point the two hosted engines import is present in [`table`].
//! Entry points the substrate cannot honour emit `UNSUPPORTED_GLES` and return
//! the documented error/zero value — they are never silently accepted.
// The C-ABI contract for every entry point below is the Khronos signature: the
// caller (a guest `.so`) guarantees that a pointer argument is either null or a
// valid range for the documented length, exactly as in C. That contract is
// stated once here and in the module header rather than 150 times, so the
// per-item `# Safety`/`SAFETY` lints are allowed in this module only; the rest
// of the crate keeps both lints denied.
#![allow(unsafe_code)]
#![allow(clippy::missing_safety_doc, clippy::undocumented_unsafe_blocks)]

use std::sync::OnceLock;

use crate::egl;
use crate::error::{consts as c, unsupported};
use crate::gl::Gl;
use crate::glsl::Value;

/// Ceiling for a single guest-supplied buffer/image upload (64 MiB).
const MAX_UPLOAD: usize = 64 * 1024 * 1024;
/// Ceiling for a shader source string.
const MAX_SHADER_SOURCE: usize = 1024 * 1024;

// ---------------------------------------------------------------------------
// Pointer helpers
// ---------------------------------------------------------------------------

/// Borrow `len` bytes of guest memory, or `None` when the pointer is null or
/// the length is out of bounds.
///
/// # Safety
/// `ptr` must be either 0 or a readable guest address range of `len` bytes.
unsafe fn bytes<'a>(ptr: usize, len: usize) -> Option<&'a [u8]> {
    if ptr == 0 || len > MAX_UPLOAD {
        return None;
    }
    // SAFETY: the caller guarantees a readable range (guest image mapped into
    // this process by `acos-native-loader`).
    Some(unsafe { std::slice::from_raw_parts(ptr as *const u8, len) })
}

/// Read a NUL-terminated C string.
///
/// # Safety
/// `ptr` must be 0 or a readable NUL-terminated byte string.
unsafe fn cstr(ptr: usize) -> Option<String> {
    if ptr == 0 {
        return None;
    }
    let mut len = 0usize;
    // SAFETY: the caller guarantees a NUL-terminated string.
    while unsafe { *(ptr as *const u8).add(len) } != 0 {
        len += 1;
        if len > 4096 {
            return None;
        }
    }
    // SAFETY: `len` bytes are readable, ASCII-or-UTF8 shader/identifier text.
    let s = unsafe { std::slice::from_raw_parts(ptr as *const u8, len) };
    Some(String::from_utf8_lossy(s).into_owned())
}

/// # Safety
/// `ptr` must be 0 or writable for `len` bytes.
unsafe fn write_bytes(ptr: usize, data: &[u8]) {
    if ptr == 0 {
        return;
    }
    // SAFETY: caller guarantees the destination is writable for `data.len()`.
    unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), ptr as *mut u8, data.len()) };
}

/// # Safety
/// `ptr` must be 0 or writable for one `i32`.
unsafe fn write_i32(ptr: usize, value: i32) {
    if ptr == 0 {
        return;
    }
    // SAFETY: caller guarantees a writable i32.
    unsafe { (ptr as *mut i32).write_unaligned(value) };
}

/// Read `count` `f32` values from guest memory.
///
/// # Safety
/// `ptr` must be 0 or readable for `count * 4` bytes.
unsafe fn floats(ptr: usize, count: usize) -> Vec<f32> {
    if ptr == 0 || count > 4096 {
        return Vec::new();
    }
    // SAFETY: caller guarantees the range; the guest's `float[]` may be only
    // 4-byte aligned, so read unaligned.
    (0..count)
        .map(|i| unsafe { (ptr as *const f32).add(i).read_unaligned() })
        .collect()
}

/// Run `f` against the current context; log when there is none.
fn gl_ctx<R>(call: &'static str, default: R, f: impl FnOnce(&mut Gl) -> R) -> R {
    match egl::with_gl_no_surface(f) {
        Some(v) => v,
        None => {
            unsupported(call, "no current EGL context");
            default
        }
    }
}

/// Run `f` against the current context + draw target; log when there is none.
fn gl_draw<R>(call: &'static str, default: R, f: impl FnOnce(&mut Gl, &mut crate::raster::Framebuffer) -> R) -> R {
    match egl::with_gl_or_null(f) {
        Some(v) => v,
        None => {
            unsupported(call, "no current EGL context");
            default
        }
    }
}

/// Read the `EGLint` attribute list at `ptr` (NUL/`EGL_NONE` terminated).
///
/// # Safety
/// `ptr` must be 0 or a readable, `EGL_NONE`-terminated `EGLint` array.
unsafe fn attrib_list(ptr: usize) -> Vec<i32> {
    let mut out = Vec::new();
    if ptr == 0 {
        return out;
    }
    let mut i = 0usize;
    while i < 64 {
        // SAFETY: the caller guarantees a terminated attribute list.
        let name = unsafe { (ptr as *const i32).add(i * 2).read_unaligned() };
        // SAFETY: the value follows the name.
        let value = unsafe { (ptr as *const i32).add(i * 2 + 1).read_unaligned() };
        out.push(name);
        out.push(value);
        if name == egl::attrib::NONE {
            break;
        }
        i += 1;
    }
    out
}

// ---------------------------------------------------------------------------
// EGL
// ---------------------------------------------------------------------------

/// `eglGetDisplay`.
pub unsafe extern "C" fn egl_get_display(native_display: u64) -> u64 {
    egl::get_display(native_display)
}

/// `eglGetError`.
pub unsafe extern "C" fn egl_get_error() -> i32 {
    egl::get_error()
}

/// `eglInitialize`.
pub unsafe extern "C" fn egl_initialize(display: u64, major: usize, minor: usize) -> i32 {
    match egl::initialize(display) {
        Some((maj, min)) => {
            unsafe {
                write_i32(major, maj);
                write_i32(minor, min);
            }
            egl::EGL_TRUE
        }
        None => egl::EGL_FALSE,
    }
}

/// `eglTerminate`.
pub unsafe extern "C" fn egl_terminate(display: u64) -> i32 {
    egl::terminate(display)
}

/// `eglChooseConfig`.
pub unsafe extern "C" fn egl_choose_config(
    display: u64,
    attribs: usize,
    configs: usize,
    config_size: i32,
    num_config: usize,
) -> i32 {
    if num_config == 0 {
        egl::set_error_pub(egl::EGL_BAD_PARAMETER);
        return egl::EGL_FALSE;
    }
    // SAFETY: the guest passes an EGL_NONE-terminated list or null.
    let attrs = unsafe { attrib_list(attribs) };
    let matching = egl::choose_config(display, &attrs);
    unsafe { write_i32(num_config, matching.len() as i32) };
    if configs != 0 && config_size > 0 {
        for (i, cfg) in matching.iter().take(config_size as usize).enumerate() {
            // SAFETY: caller-provided array of at least config_size entries.
            unsafe { (configs as *mut u64).add(i).write_unaligned(*cfg) };
        }
    }
    egl::EGL_TRUE
}

/// `eglGetConfigAttrib`.
pub unsafe extern "C" fn egl_get_config_attrib(
    display: u64,
    config: u64,
    name: i32,
    value: usize,
) -> i32 {
    match egl::get_config_attrib(display, config, name) {
        Some(v) => {
            unsafe { write_i32(value, v) };
            egl::EGL_TRUE
        }
        None => egl::EGL_FALSE,
    }
}

/// `eglCreateWindowSurface`.
pub unsafe extern "C" fn egl_create_window_surface(
    display: u64,
    config: u64,
    native_window: usize,
    _attribs: usize,
) -> u64 {
    if native_window == 0 {
        egl::set_error_pub(egl::EGL_BAD_NATIVE_WINDOW);
        return egl::EGL_NO_HANDLE;
    }
    egl::create_window_surface(display, config, native_window as u64)
}

/// `eglCreatePbufferSurface`.
pub unsafe extern "C" fn egl_create_pbuffer_surface(
    display: u64,
    config: u64,
    attribs: usize,
) -> u64 {
    // SAFETY: EGL_NONE-terminated attribute list or null.
    let attrs = unsafe { attrib_list(attribs) };
    egl::create_pbuffer_surface(display, config, &attrs)
}

/// `eglDestroySurface`.
pub unsafe extern "C" fn egl_destroy_surface(display: u64, surface: u64) -> i32 {
    egl::destroy_surface(display, surface)
}

/// `eglCreateContext`.
pub unsafe extern "C" fn egl_create_context(
    display: u64,
    config: u64,
    share: u64,
    attribs: usize,
) -> u64 {
    // SAFETY: EGL_NONE-terminated attribute list or null.
    let attrs = unsafe { attrib_list(attribs) };
    egl::create_context(display, config, share, &attrs)
}

/// `eglDestroyContext`.
pub unsafe extern "C" fn egl_destroy_context(display: u64, context: u64) -> i32 {
    egl::destroy_context(display, context)
}

/// `eglMakeCurrent`.
pub unsafe extern "C" fn egl_make_current(display: u64, draw: u64, read: u64, context: u64) -> i32 {
    egl::make_current(display, draw, read, context)
}

/// `eglSwapBuffers`.
pub unsafe extern "C" fn egl_swap_buffers(display: u64, surface: u64) -> i32 {
    egl::swap_buffers(display, surface)
}

/// `eglQuerySurface`.
pub unsafe extern "C" fn egl_query_surface(
    display: u64,
    surface: u64,
    name: i32,
    value: usize,
) -> i32 {
    match egl::query_surface(display, surface, name) {
        Some(v) => {
            unsafe { write_i32(value, v) };
            egl::EGL_TRUE
        }
        None => egl::EGL_FALSE,
    }
}

/// `eglGetCurrentDisplay`.
pub unsafe extern "C" fn egl_get_current_display() -> u64 {
    egl::current_display()
}

/// `eglGetCurrentContext`.
pub unsafe extern "C" fn egl_get_current_context() -> u64 {
    egl::current_context()
}

/// `eglGetCurrentSurface`.
pub unsafe extern "C" fn egl_get_current_surface(readdraw: i32) -> u64 {
    egl::current_surface(readdraw == egl::attrib::READ)
}

/// `eglQueryString`.
pub unsafe extern "C" fn egl_query_string(display: u64, name: i32) -> usize {
    match egl::query_string(display, name) {
        Some(s) => c_string(s),
        None => 0,
    }
}

/// `eglBindAPI`.
pub unsafe extern "C" fn egl_bind_api(api: i32) -> i32 {
    egl::bind_api(api)
}

/// `eglReleaseThread`.
pub unsafe extern "C" fn egl_release_thread() -> i32 {
    egl::release_thread()
}

/// `eglSwapInterval`.
pub unsafe extern "C" fn egl_swap_interval(display: u64, interval: i32) -> i32 {
    egl::swap_interval(display, interval)
}

/// `eglGetConfigs`.
pub unsafe extern "C" fn egl_get_configs(
    display: u64,
    configs: usize,
    config_size: i32,
    num_config: usize,
) -> i32 {
    let all = [1u64];
    if num_config != 0 {
        unsafe { write_i32(num_config, all.len() as i32) };
    }
    if configs != 0 {
        for (i, cfg) in all.iter().take(config_size.max(0) as usize).enumerate() {
            unsafe { (configs as *mut u64).add(i).write_unaligned(*cfg) };
        }
    }
    let _ = display;
    egl::EGL_TRUE
}

/// `eglGetProcAddress`.
pub unsafe extern "C" fn egl_get_proc_address(name: usize) -> usize {
    // SAFETY: the guest passes a NUL-terminated symbol name.
    match unsafe { cstr(name) } {
        Some(n) => lookup(&n).unwrap_or_else(|| {
            unsupported("eglGetProcAddress", format!("unknown entry point {n:?}"));
            0
        }),
        None => 0,
    }
}

/// `eglCreateImageKHR` — image objects are not implemented.
pub unsafe extern "C" fn egl_create_image_khr(
    _display: u64,
    _context: u64,
    _target: u32,
    _buffer: usize,
    _attribs: usize,
) -> usize {
    unsupported(
        "eglCreateImageKHR",
        "EGL image objects are not implemented (no GPU buffer sharing)",
    );
    egl::set_error_pub(egl::EGL_BAD_PARAMETER);
    0
}

/// `eglDestroyImageKHR`.
pub unsafe extern "C" fn egl_destroy_image_khr(_display: u64, _image: usize) -> i32 {
    unsupported("eglDestroyImageKHR", "EGL image objects are not implemented");
    egl::EGL_FALSE
}

// ---------------------------------------------------------------------------
// GLES2 — object/state space
// ---------------------------------------------------------------------------

/// `glGetError`.
pub unsafe extern "C" fn gl_get_error() -> u32 {
    gl_ctx("glGetError", c::INVALID_OPERATION, Gl::take_error)
}

/// `glEnable`.
pub unsafe extern "C" fn gl_enable(cap: u32) {
    gl_ctx("glEnable", (), |gl| gl.enable(cap));
}

/// `glDisable`.
pub unsafe extern "C" fn gl_disable(cap: u32) {
    gl_ctx("glDisable", (), |gl| gl.disable(cap));
}

/// `glClear`.
pub unsafe extern "C" fn gl_clear(mask: u32) {
    gl_draw("glClear", (), |gl, fb| gl.clear(fb, mask));
}

/// `glClearColor`.
pub unsafe extern "C" fn gl_clear_color(r: f32, g: f32, b: f32, a: f32) {
    gl_ctx("glClearColor", (), |gl| gl.clear_color(r, g, b, a));
}

/// `glClearStencil`.
pub unsafe extern "C" fn gl_clear_stencil(s: i32) {
    gl_ctx("glClearStencil", (), |gl| gl.clear_stencil(s));
}

/// `glClearDepthf`.
pub unsafe extern "C" fn gl_clear_depthf(d: f32) {
    gl_ctx("glClearDepthf", (), |gl| gl.clear_depthf(d));
}

/// `glViewport`.
pub unsafe extern "C" fn gl_viewport(x: i32, y: i32, w: i32, h: i32) {
    gl_ctx("glViewport", (), |gl| gl.viewport(x, y, w, h));
}

/// `glScissor`.
pub unsafe extern "C" fn gl_scissor(x: i32, y: i32, w: i32, h: i32) {
    gl_ctx("glScissor", (), |gl| gl.scissor(x, y, w, h));
}

/// `glColorMask`.
pub unsafe extern "C" fn gl_color_mask(r: u8, g: u8, b: u8, a: u8) {
    gl_ctx("glColorMask", (), |gl| {
        gl.color_mask(r != 0, g != 0, b != 0, a != 0);
    });
}

/// `glDepthMask`.
pub unsafe extern "C" fn gl_depth_mask(flag: u8) {
    gl_ctx("glDepthMask", (), |gl| gl.depth_mask(flag != 0));
}

/// `glDepthFunc`.
pub unsafe extern "C" fn gl_depth_func(func: u32) {
    gl_ctx("glDepthFunc", (), |gl| gl.depth_func(func));
}

/// `glBlendFunc`.
pub unsafe extern "C" fn gl_blend_func(src: u32, dst: u32) {
    gl_ctx("glBlendFunc", (), |gl| gl.blend_func(src, dst));
}

/// `glBlendFuncSeparate`.
pub unsafe extern "C" fn gl_blend_func_separate(srgb: u32, drgb: u32, sa: u32, da: u32) {
    gl_ctx("glBlendFuncSeparate", (), |gl| {
        gl.blend_func_separate(srgb, drgb, sa, da);
    });
}

/// `glBlendEquation`.
pub unsafe extern "C" fn gl_blend_equation(mode: u32) {
    gl_ctx("glBlendEquation", (), |gl| gl.blend_equation(mode));
}

/// `glBlendEquationSeparate`.
pub unsafe extern "C" fn gl_blend_equation_separate(rgb: u32, alpha: u32) {
    gl_ctx("glBlendEquationSeparate", (), |gl| {
        if rgb != alpha {
            unsupported(
                "glBlendEquationSeparate",
                "separate RGB/alpha equations are not implemented",
            );
        }
        gl.blend_equation(rgb);
    });
}

/// `glBlendColor` — accepted; the constant colour is not used by the blend
/// factors this substrate implements.
pub unsafe extern "C" fn gl_blend_color(r: f32, g: f32, b: f32, a: f32) {
    gl_ctx("glBlendColor", (), |_gl| {
        crate::error::diagnostic(
            "glBlendColor",
            format!("constant blend colour ({r},{g},{b},{a}) is not applied"),
        );
    });
}

/// `glCullFace`.
pub unsafe extern "C" fn gl_cull_face(mode: u32) {
    gl_ctx("glCullFace", (), |gl| gl.cull_face(mode));
}

/// `glFrontFace`.
pub unsafe extern "C" fn gl_front_face(mode: u32) {
    gl_ctx("glFrontFace", (), |gl| gl.front_face(mode));
}

/// `glPixelStorei`.
pub unsafe extern "C" fn gl_pixel_storei(pname: u32, value: i32) {
    gl_ctx("glPixelStorei", (), |gl| gl.pixel_storei(pname, value));
}

/// `glLineWidth`.
pub unsafe extern "C" fn gl_line_width(width: f32) {
    gl_ctx("glLineWidth", (), |gl| gl.line_width(width));
}

/// `glFinish`.
pub unsafe extern "C" fn gl_finish() {
    gl_ctx("glFinish", (), |_gl| {});
}

/// `glFlush`.
pub unsafe extern "C" fn gl_flush() {
    gl_ctx("glFlush", (), |_gl| {});
}

/// `glStencilFunc`.
pub unsafe extern "C" fn gl_stencil_func(_func: u32, _r: i32, _m: u32) {
    gl_ctx("glStencilFunc", (), |gl| gl.stencil_noop("glStencilFunc"));
}

/// `glStencilFuncSeparate`.
pub unsafe extern "C" fn gl_stencil_func_separate(_face: u32, _func: u32, _r: i32, _m: u32) {
    gl_ctx("glStencilFuncSeparate", (), |gl| {
        gl.stencil_noop("glStencilFuncSeparate")
    });
}

/// `glStencilMask`.
pub unsafe extern "C" fn gl_stencil_mask(_mask: u32) {
    gl_ctx("glStencilMask", (), |gl| gl.stencil_noop("glStencilMask"));
}

/// `glStencilMaskSeparate`.
pub unsafe extern "C" fn gl_stencil_mask_separate(_face: u32, _mask: u32) {
    gl_ctx("glStencilMaskSeparate", (), |gl| {
        gl.stencil_noop("glStencilMaskSeparate")
    });
}

/// `glStencilOp`.
pub unsafe extern "C" fn gl_stencil_op(_sf: u32, _dp: u32, _dpp: u32) {
    gl_ctx("glStencilOp", (), |gl| gl.stencil_noop("glStencilOp"));
}

/// `glStencilOpSeparate`.
pub unsafe extern "C" fn gl_stencil_op_separate(_face: u32, _sf: u32, _dp: u32, _dpp: u32) {
    gl_ctx("glStencilOpSeparate", (), |gl| {
        gl.stencil_noop("glStencilOpSeparate")
    });
}

/// `glGetString`.
pub unsafe extern "C" fn gl_get_string(name: u32) -> usize {
    match egl::with_current_state(|gl| gl.get_string(name)) {
        Some(Some(s)) => c_string(s),
        Some(None) => {
            gl_ctx("glGetString", (), |gl| {
                gl.set_error(c::INVALID_ENUM);
            });
            0
        }
        None => {
            unsupported("glGetString", "no current EGL context");
            0
        }
    }
}

/// `glGetStringi` — GLES3 only.
pub unsafe extern "C" fn gl_get_string_i(_name: u32, _index: u32) -> usize {
    unsupported("glGetStringi", "GLES3 indexed strings are not implemented");
    0
}

/// `glGetIntegerv`.
pub unsafe extern "C" fn gl_get_integerv(pname: u32, params: usize) {
    let values = gl_ctx("glGetIntegerv", Vec::new(), |gl| gl.get_integer_v(pname));
    for (i, v) in values.iter().enumerate() {
        unsafe { write_i32(params.wrapping_add(i * 4), *v) };
    }
}

/// `glGetIntegeri_v`.
pub unsafe extern "C" fn gl_get_integer_i_v(pname: u32, index: u32, params: usize) {
    let values = gl_ctx("glGetIntegeri_v", Vec::new(), |gl| {
        if index != 0 {
            unsupported(
                "glGetIntegeri_v",
                format!("pname {pname:#x} index {index} (indexed state is not implemented)"),
            );
            gl.set_error(c::INVALID_VALUE);
            Vec::new()
        } else {
            gl.get_integer_v(pname)
        }
    });
    for (i, v) in values.iter().enumerate() {
        unsafe { write_i32(params.wrapping_add(i * 4), *v) };
    }
}

// -- buffers ---------------------------------------------------------------

/// `glGenBuffers`.
pub unsafe extern "C" fn gl_gen_buffers(n: i32, buffers: usize) {
    let ids = gl_ctx("glGenBuffers", Vec::new(), |gl| gl.gen_buffers(n));
    for (i, id) in ids.iter().enumerate() {
        unsafe { write_i32(buffers.wrapping_add(i * 4), *id as i32) };
    }
}

/// `glDeleteBuffers`.
pub unsafe extern "C" fn gl_delete_buffers(n: i32, buffers: usize) {
    let ids: Vec<u32> = (0..n.max(0))
        .map(|i| unsafe { (buffers as *const u32).add(i as usize).read_unaligned() })
        .collect();
    gl_ctx("glDeleteBuffers", (), |gl| gl.delete_buffers(&ids));
}

/// `glBindBuffer`.
pub unsafe extern "C" fn gl_bind_buffer(target: u32, buffer: u32) {
    gl_ctx("glBindBuffer", (), |gl| gl.bind_buffer(target, buffer));
}

/// `glBufferData`.
pub unsafe extern "C" fn gl_buffer_data(target: u32, size: i32, data: usize, usage: u32) {
    let len = size.max(0) as usize;
    // SAFETY: the guest passes `size` readable bytes or a null pointer.
    let payload = unsafe { bytes(data, len) }.unwrap_or(&[]);
    gl_ctx("glBufferData", (), |gl| gl.buffer_data(target, payload, usage));
}

/// `glBufferSubData`.
pub unsafe extern "C" fn gl_buffer_sub_data(target: u32, offset: i32, size: i32, data: usize) {
    let len = size.max(0) as usize;
    let payload = unsafe { bytes(data, len) }.unwrap_or(&[]).to_vec();
    gl_ctx("glBufferSubData", (), |gl| {
        gl.buffer_sub_data(target, offset, &payload)
    });
}

/// `glGetBufferParameteriv`.
pub unsafe extern "C" fn gl_get_buffer_parameteriv(target: u32, pname: u32, params: usize) {
    /// `GL_BUFFER_SIZE`.
    const BUFFER_SIZE: u32 = 0x8764;
    /// `GL_BUFFER_USAGE`.
    const BUFFER_USAGE: u32 = 0x8765;
    let v = gl_ctx("glGetBufferParameteriv", 0, |gl| match pname {
        BUFFER_SIZE => gl.buffers.get(&gl.array_buffer).map_or(0, |b| b.len() as i32),
        BUFFER_USAGE => 0x88E4,
        other => {
            unsupported("glGetBufferParameteriv", format!("pname {other:#x}"));
            gl.set_error(c::INVALID_ENUM);
            0
        }
    });
    let _ = target;
    unsafe { write_i32(params, v) };
}

// -- shaders and programs --------------------------------------------------

/// `glCreateShader`.
pub unsafe extern "C" fn gl_create_shader(ty: u32) -> u32 {
    gl_ctx("glCreateShader", 0, |gl| gl.create_shader(ty))
}

/// `glDeleteShader`.
pub unsafe extern "C" fn gl_delete_shader(shader: u32) {
    gl_ctx("glDeleteShader", (), |gl| gl.delete_shader(shader));
}

/// `glShaderSource`.
pub unsafe extern "C" fn gl_shader_source(shader: u32, count: i32, string: usize, length: usize) {
    if count <= 0 || string == 0 {
        gl_ctx("glShaderSource", (), |gl| gl.shader_source(shader, String::new()));
        return;
    }
    let mut src = String::new();
    for i in 0..count.min(64) as usize {
        // SAFETY: `string` is a guest array of `count` pointers.
        let ptr = unsafe { (string as *const usize).add(i).read_unaligned() };
        if ptr == 0 {
            continue;
        }
        let declared = if length == 0 {
            None
        } else {
            // SAFETY: `length` is `count` GLint entries or null.
            let l = unsafe { (length as *const i32).add(i).read_unaligned() };
            if l >= 0 {
                Some(l as usize)
            } else {
                None
            }
        };
        match declared {
            Some(n) => {
                if n > MAX_SHADER_SOURCE {
                    unsupported("glShaderSource", format!("source chunk of {n} bytes"));
                    gl_ctx("glShaderSource", (), |gl| {
                        gl.set_error(c::INVALID_VALUE);
                    });
                    return;
                }
                if let Some(b) = unsafe { bytes(ptr, n) } {
                    src.push_str(&String::from_utf8_lossy(b));
                }
            }
            None => {
                if let Some(s) = unsafe { cstr(ptr) } {
                    if s.len() > MAX_SHADER_SOURCE {
                        unsupported("glShaderSource", "shader source exceeds the size ceiling");
                        gl_ctx("glShaderSource", (), |gl| {
                            gl.set_error(c::INVALID_VALUE);
                        });
                        return;
                    }
                    src.push_str(&s);
                }
            }
        }
    }
    gl_ctx("glShaderSource", (), |gl| gl.shader_source(shader, src));
}

/// `glCompileShader`.
pub unsafe extern "C" fn gl_compile_shader(shader: u32) {
    gl_ctx("glCompileShader", (), |gl| gl.compile_shader(shader));
}

/// `glGetShaderiv`.
pub unsafe extern "C" fn gl_get_shaderiv(shader: u32, pname: u32, params: usize) {
    let v = gl_ctx("glGetShaderiv", 0, |gl| gl.get_shader_iv(shader, pname));
    unsafe { write_i32(params, v) };
}

/// `glGetShaderInfoLog`.
pub unsafe extern "C" fn gl_get_shader_info_log(
    shader: u32,
    buf_size: i32,
    length: usize,
    info_log: usize,
) {
    let log = egl::with_current_state(|gl| gl.shader_info_log(shader)).unwrap_or_default();
    write_log(&log, buf_size, length, info_log);
}

fn write_log(log: &str, buf_size: i32, length: usize, info_log: usize) {
    let max = buf_size.max(0) as usize;
    let mut out = log.as_bytes().to_vec();
    out.truncate(max.saturating_sub(1));
    out.push(0);
    unsafe {
        write_bytes(info_log, &out);
        write_i32(length, out.len() as i32);
    }
}

/// `glGetShaderPrecisionFormat`.
pub unsafe extern "C" fn gl_get_shader_precision_format(
    _shader_type: u32,
    _precision_type: u32,
    range: usize,
    precision: usize,
) {
    unsafe {
        write_i32(range, 127);
        write_i32(range.wrapping_add(4), 127);
        write_i32(precision, 23);
    }
}

/// `glCreateProgram`.
pub unsafe extern "C" fn gl_create_program() -> u32 {
    gl_ctx("glCreateProgram", 0, Gl::create_program)
}

/// `glDeleteProgram`.
pub unsafe extern "C" fn gl_delete_program(program: u32) {
    gl_ctx("glDeleteProgram", (), |gl| gl.delete_program(program));
}

/// `glAttachShader`.
pub unsafe extern "C" fn gl_attach_shader(program: u32, shader: u32) {
    gl_ctx("glAttachShader", (), |gl| gl.attach_shader(program, shader));
}

/// `glBindAttribLocation`.
pub unsafe extern "C" fn gl_bind_attrib_location(program: u32, index: u32, name: usize) {
    // SAFETY: the guest passes a NUL-terminated attribute name.
    let Some(n) = (unsafe { cstr(name) }) else {
        gl_ctx("glBindAttribLocation", (), |gl| gl.set_error(c::INVALID_VALUE));
        return;
    };
    gl_ctx("glBindAttribLocation", (), |gl| {
        gl.bind_attrib_location(program, index, &n)
    });
}

/// `glLinkProgram`.
pub unsafe extern "C" fn gl_link_program(program: u32) {
    gl_ctx("glLinkProgram", (), |gl| gl.link_program(program));
}

/// `glUseProgram`.
pub unsafe extern "C" fn gl_use_program(program: u32) {
    gl_ctx("glUseProgram", (), |gl| gl.use_program(program));
}

/// `glValidateProgram` — the substrate validates at link time.
pub unsafe extern "C" fn gl_validate_program(program: u32) {
    gl_ctx("glValidateProgram", (), |_gl| {
        crate::error::diagnostic(
            "glValidateProgram",
            format!("program {program}: validation happens at link time"),
        );
    });
}

/// `glGetProgramiv`.
pub unsafe extern "C" fn gl_get_programiv(program: u32, pname: u32, params: usize) {
    let v = gl_ctx("glGetProgramiv", 0, |gl| gl.get_program_iv(program, pname));
    unsafe { write_i32(params, v) };
}

/// `glGetProgramInfoLog`.
pub unsafe extern "C" fn gl_get_program_info_log(
    program: u32,
    buf_size: i32,
    length: usize,
    info_log: usize,
) {
    let log = egl::with_current_state(|gl| gl.program_info_log(program)).unwrap_or_default();
    write_log(&log, buf_size, length, info_log);
}

/// `glGetUniformLocation`.
pub unsafe extern "C" fn gl_get_uniform_location(program: u32, name: usize) -> i32 {
    let Some(n) = (unsafe { cstr(name) }) else {
        return -1;
    };
    egl::with_current_state(|gl| gl.get_uniform_location(program, &n)).unwrap_or(-1)
}

/// `glGetAttribLocation`.
pub unsafe extern "C" fn gl_get_attrib_location(program: u32, name: usize) -> i32 {
    let Some(n) = (unsafe { cstr(name) }) else {
        return -1;
    };
    egl::with_current_state(|gl| gl.get_attrib_location(program, &n)).unwrap_or(-1)
}

// -- uniforms --------------------------------------------------------------

macro_rules! uniform_fn {
    ($name:ident, $call:literal, $location:ident $(, $p:ident : $t:ty)* ; $value:expr) => {
        #[doc = concat!("`", $call, "`")]
        pub unsafe extern "C" fn $name($location: i32 $(, $p: $t)*) {
            let value: Value = $value;
            gl_ctx($call, (), |gl| gl.uniform_value($location, value));
        }
    };
}

uniform_fn!(gl_uniform1f, "glUniform1f", location, x: f32; Value::F(x));
uniform_fn!(gl_uniform2f, "glUniform2f", location, x: f32, y: f32; Value::vec4(x, y, 0.0, 0.0));
uniform_fn!(
    gl_uniform3f,
    "glUniform3f",
    location,
    x: f32,
    y: f32,
    z: f32;
    Value::V([x, y, z, 0.0], 3)
);
uniform_fn!(
    gl_uniform4f,
    "glUniform4f",
    location,
    x: f32,
    y: f32,
    z: f32,
    w: f32;
    Value::vec4(x, y, z, w)
);
uniform_fn!(gl_uniform1i, "glUniform1i", location, x: i32; Value::S(x));
uniform_fn!(gl_uniform2i, "glUniform2i", location, x: i32, y: i32; Value::vec4(x as f32, y as f32, 0.0, 0.0));
uniform_fn!(
    gl_uniform3i,
    "glUniform3i",
    location,
    x: i32,
    y: i32,
    z: i32;
    Value::V([x as f32, y as f32, z as f32, 0.0], 3)
);
uniform_fn!(
    gl_uniform4i,
    "glUniform4i",
    location,
    x: i32,
    y: i32,
    z: i32,
    w: i32;
    Value::vec4(x as f32, y as f32, z as f32, w as f32)
);

/// `glUniform1fv`/`2fv`/`3fv`/`4fv`/`1iv`... share one shape: a count and a
/// pointer. Uniform arrays are outside the interpreted subset, so only the
/// first element of a single-element array is bound.
macro_rules! uniform_array_fn {
    ($name:ident, $call:literal, $width:expr, $index:expr) => {
        #[doc = concat!("`", $call, "`")]
        pub unsafe extern "C" fn $name(location: i32, count: i32, value: usize) {
            let n = $width;
            let vals = unsafe { floats(value, n) };
            if count > 1 {
                unsupported(
                    $call,
                    format!("uniform array of {count} elements (only element 0 is bound)"),
                );
            }
            if vals.len() < n {
                gl_ctx($call, (), |gl| gl.set_error(c::INVALID_VALUE));
                return;
            }
            let value = if n == 1 {
                if $index {
                    Value::S(vals[0] as i32)
                } else {
                    Value::F(vals[0])
                }
            } else {
                let mut v = [0.0f32; 4];
                v[..n].copy_from_slice(&vals[..n]);
                Value::V(v, n)
            };
            gl_ctx($call, (), |gl| gl.uniform_value(location, value));
        }
    };
}

uniform_array_fn!(gl_uniform1fv, "glUniform1fv", 1, false);
uniform_array_fn!(gl_uniform2fv, "glUniform2fv", 2, false);
uniform_array_fn!(gl_uniform3fv, "glUniform3fv", 3, false);
uniform_array_fn!(gl_uniform4fv, "glUniform4fv", 4, false);
uniform_array_fn!(gl_uniform1iv, "glUniform1iv", 1, true);
uniform_array_fn!(gl_uniform2iv, "glUniform2iv", 2, true);
uniform_array_fn!(gl_uniform3iv, "glUniform3iv", 3, true);
uniform_array_fn!(gl_uniform4iv, "glUniform4iv", 4, true);

/// `glUniformMatrix2fv` / `3fv` / `4fv`.
macro_rules! uniform_matrix_fn {
    ($name:ident, $call:literal, $n:expr) => {
        #[doc = concat!("`", $call, "`")]
        pub unsafe extern "C" fn $name(location: i32, count: i32, transpose: u8, value: usize) {
            if count > 1 {
                unsupported(
                    $call,
                    format!("matrix array of {count} elements (only element 0 is bound)"),
                );
            }
            let data = unsafe { floats(value, $n * $n) };
            gl_ctx($call, (), |gl| {
                gl.uniform_matrix(location, $n, transpose != 0, &data)
            });
        }
    };
}

uniform_matrix_fn!(gl_uniform_matrix2fv, "glUniformMatrix2fv", 2);
uniform_matrix_fn!(gl_uniform_matrix3fv, "glUniformMatrix3fv", 3);
uniform_matrix_fn!(gl_uniform_matrix4fv, "glUniformMatrix4fv", 4);

// -- vertex attributes -----------------------------------------------------

/// `glVertexAttribPointer`.
pub unsafe extern "C" fn gl_vertex_attrib_pointer(
    index: u32,
    size: i32,
    ty: u32,
    normalized: u8,
    stride: i32,
    pointer: usize,
) {
    let has_buffer = egl::with_current_state(|gl| gl.array_buffer != 0).unwrap_or(false);
    if !has_buffer && pointer >= 4096 {
        unsupported(
            "glVertexAttribPointer",
            "client-side vertex arrays are not supported; bind an ARRAY_BUFFER",
        );
        gl_ctx("glVertexAttribPointer", (), |gl| {
            gl.set_error(c::INVALID_OPERATION)
        });
        return;
    }
    gl_ctx("glVertexAttribPointer", (), |gl| {
        gl.vertex_attrib_pointer(index, size, ty, normalized != 0, stride, pointer as i32)
    });
}

/// `glEnableVertexAttribArray`.
pub unsafe extern "C" fn gl_enable_vertex_attrib_array(index: u32) {
    gl_ctx("glEnableVertexAttribArray", (), |gl| {
        gl.set_attrib_array_enabled(index, true)
    });
}

/// `glDisableVertexAttribArray`.
pub unsafe extern "C" fn gl_disable_vertex_attrib_array(index: u32) {
    gl_ctx("glDisableVertexAttribArray", (), |gl| {
        gl.set_attrib_array_enabled(index, false)
    });
}

/// `glVertexAttrib{1,2,3,4}f`.
macro_rules! attrib_fn {
    ($name:ident, $call:literal; $($p:ident : $t:ty),*) => {
        #[doc = concat!("`", $call, "`")]
        pub unsafe extern "C" fn $name(index: u32 $(, $p: $t)*) {
            let mut out = [0.0f32, 0.0, 0.0, 1.0];
            let vals = [$(($p) as f32),*];
            out[..vals.len()].copy_from_slice(&vals);
            gl_ctx($call, (), |gl| gl.vertex_attrib4f(index, out));
        }
    };
}

macro_rules! attrib_array_fn {
    ($name:ident, $call:literal, $n:expr) => {
        #[doc = concat!("`", $call, "`")]
        pub unsafe extern "C" fn $name(index: u32, value: usize) {
            let vals = unsafe { floats(value, $n) };
            if vals.len() < $n {
                gl_ctx($call, (), |gl| gl.set_error(c::INVALID_VALUE));
                return;
            }
            let mut out = [0.0f32, 0.0, 0.0, 1.0];
            out[..$n].copy_from_slice(&vals[..$n]);
            gl_ctx($call, (), |gl| gl.vertex_attrib4f(index, out));
        }
    };
}

attrib_fn!(gl_vertex_attrib1f, "glVertexAttrib1f"; x: f32);
attrib_fn!(gl_vertex_attrib2f, "glVertexAttrib2f"; x: f32, y: f32);
attrib_fn!(gl_vertex_attrib3f, "glVertexAttrib3f"; x: f32, y: f32, z: f32);
attrib_fn!(gl_vertex_attrib4f, "glVertexAttrib4f"; x: f32, y: f32, z: f32, w: f32);

attrib_array_fn!(gl_vertex_attrib2fv, "glVertexAttrib2fv", 2);
attrib_array_fn!(gl_vertex_attrib3fv, "glVertexAttrib3fv", 3);
attrib_array_fn!(gl_vertex_attrib4fv, "glVertexAttrib4fv", 4);

// -- draws -----------------------------------------------------------------

/// `glDrawArrays`.
pub unsafe extern "C" fn gl_draw_arrays(mode: u32, first: i32, count: i32) {
    gl_draw("glDrawArrays", (), |gl, fb| {
        gl.draw_arrays(fb, mode, first, count)
    });
}

/// `glDrawElements`.
pub unsafe extern "C" fn gl_draw_elements(mode: u32, count: i32, ty: u32, indices: usize) {
    gl_draw("glDrawElements", (), |gl, fb| {
        gl.draw_elements(fb, mode, count, ty, indices as i32)
    });
}

/// `glDrawBuffers` — a single draw buffer is supported.
pub unsafe extern "C" fn gl_draw_buffers(n: i32, bufs: usize) {
    if n != 1 || bufs == 0 {
        unsupported("glDrawBuffers", format!("{n} draw buffers"));
        gl_ctx("glDrawBuffers", (), |gl| gl.set_error(c::INVALID_OPERATION));
        return;
    }
    let first = unsafe { (bufs as *const u32).read_unaligned() };
    if first != c::COLOR_ATTACHMENT0 && first != c::NONE {
        unsupported(
            "glDrawBuffers",
            format!("draw buffer {first:#x} (only COLOR_ATTACHMENT0)"),
        );
        gl_ctx("glDrawBuffers", (), |gl| gl.set_error(c::INVALID_OPERATION));
    }
}

// -- textures --------------------------------------------------------------

/// `glGenTextures`.
pub unsafe extern "C" fn gl_gen_textures(n: i32, textures: usize) {
    let ids = gl_ctx("glGenTextures", Vec::new(), |gl| gl.gen_textures(n));
    for (i, id) in ids.iter().enumerate() {
        unsafe { write_i32(textures.wrapping_add(i * 4), *id as i32) };
    }
}

/// `glDeleteTextures`.
pub unsafe extern "C" fn gl_delete_textures(n: i32, textures: usize) {
    let ids: Vec<u32> = (0..n.max(0))
        .map(|i| unsafe { (textures as *const u32).add(i as usize).read_unaligned() })
        .collect();
    gl_ctx("glDeleteTextures", (), |gl| gl.delete_textures(&ids));
}

/// `glBindTexture`.
pub unsafe extern "C" fn gl_bind_texture(target: u32, texture: u32) {
    gl_ctx("glBindTexture", (), |gl| gl.bind_texture(target, texture));
}

/// `glActiveTexture`.
pub unsafe extern "C" fn gl_active_texture(texture: u32) {
    gl_ctx("glActiveTexture", (), |gl| gl.active_texture(texture));
}

/// `glTexParameteri`.
pub unsafe extern "C" fn gl_tex_parameteri(target: u32, pname: u32, value: i32) {
    gl_ctx("glTexParameteri", (), |gl| {
        gl.tex_parameteri(target, pname, value as u32)
    });
}

/// `glTexParameterf`.
pub unsafe extern "C" fn gl_tex_parameterf(target: u32, pname: u32, value: f32) {
    gl_ctx("glTexParameterf", (), |gl| {
        gl.tex_parameteri(target, pname, value as u32)
    });
}

/// `glTexParameterfv` / `glTexParameteriv`.
pub unsafe extern "C" fn gl_tex_parameterfv(target: u32, pname: u32, value: usize) {
    let v = unsafe { floats(value, 1) };
    if let Some(v) = v.first() {
        gl_ctx("glTexParameterfv", (), |gl| {
            gl.tex_parameteri(target, pname, *v as u32)
        });
    }
}

/// `glTexParameteriv`.
pub unsafe extern "C" fn gl_tex_parameteriv(target: u32, pname: u32, value: usize) {
    if value == 0 {
        return;
    }
    let v = unsafe { (value as *const i32).read_unaligned() };
    gl_ctx("glTexParameteriv", (), |gl| {
        gl.tex_parameteri(target, pname, v as u32)
    });
}

/// `glTexImage2D`.
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn gl_tex_image2d(
    target: u32,
    level: i32,
    internal_format: i32,
    width: i32,
    height: i32,
    border: i32,
    format: u32,
    ty: u32,
    pixels: usize,
) {
    let comps = match format {
        c::RGBA => 4usize,
        c::RGB => 3,
        c::LUMINANCE_ALPHA => 2,
        c::LUMINANCE | c::ALPHA => 1,
        _ => 0,
    };
    let len = if comps == 0 || width <= 0 || height <= 0 {
        0
    } else {
        (width as usize).saturating_mul(height as usize).saturating_mul(comps)
    };
    let payload = if len == 0 {
        Vec::new()
    } else {
        unsafe { bytes(pixels, len) }.unwrap_or(&[]).to_vec()
    };
    gl_ctx("glTexImage2D", (), |gl| {
        gl.tex_image_2d(
            target,
            level,
            internal_format,
            width,
            height,
            border,
            format,
            ty,
            &payload,
        )
    });
}

/// `glTexSubImage2D`.
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn gl_tex_sub_image2d(
    target: u32,
    level: i32,
    xoffset: i32,
    yoffset: i32,
    width: i32,
    height: i32,
    format: u32,
    ty: u32,
    pixels: usize,
) {
    let comps = match format {
        c::RGBA => 4usize,
        c::RGB => 3,
        c::LUMINANCE_ALPHA => 2,
        c::LUMINANCE | c::ALPHA => 1,
        _ => 0,
    };
    let len = if comps == 0 || width <= 0 || height <= 0 {
        0
    } else {
        (width as usize).saturating_mul(height as usize).saturating_mul(comps)
    };
    let payload = if len == 0 {
        Vec::new()
    } else {
        unsafe { bytes(pixels, len) }.unwrap_or(&[]).to_vec()
    };
    gl_ctx("glTexSubImage2D", (), |gl| {
        gl.tex_sub_image_2d(
            target, level, xoffset, yoffset, width, height, format, ty, &payload,
        )
    });
}

/// `glGenerateMipmap`.
pub unsafe extern "C" fn gl_generate_mipmap(target: u32) {
    gl_ctx("glGenerateMipmap", (), |gl| gl.generate_mipmap(target));
}

/// `glIsTexture`.
pub unsafe extern "C" fn gl_is_texture(texture: u32) -> u8 {
    gl_ctx("glIsTexture", 0u8, |gl| {
        u8::from(gl.textures.contains_key(&texture))
    })
}

/// `glCompressedTexImage2D` — no compressed formats are advertised.
pub unsafe extern "C" fn gl_compressed_tex_image2d(
    _target: u32,
    _level: i32,
    _internal: i32,
    width: i32,
    height: i32,
    _border: i32,
    image_size: i32,
    _data: usize,
) {
    unsupported(
        "glCompressedTexImage2D",
        format!(
            "compressed texture upload ({width}x{height}, {image_size} bytes): no compressed \
             formats are available"
        ),
    );
    gl_ctx("glCompressedTexImage2D", (), |gl| {
        gl.set_error(c::INVALID_OPERATION)
    });
}

/// `glCompressedTexSubImage2D`.
pub unsafe extern "C" fn gl_compressed_tex_sub_image2d(
    _target: u32,
    _level: i32,
    _x: i32,
    _y: i32,
    _w: i32,
    _h: i32,
    _format: u32,
    _size: i32,
    _data: usize,
) {
    unsupported(
        "glCompressedTexSubImage2D",
        "compressed texture uploads are not supported",
    );
    gl_ctx("glCompressedTexSubImage2D", (), |gl| {
        gl.set_error(c::INVALID_OPERATION)
    });
}

/// `glCopyTexSubImage2D`.
pub unsafe extern "C" fn gl_copy_tex_sub_image2d(
    _target: u32,
    _level: i32,
    _x: i32,
    _y: i32,
    _sx: i32,
    _sy: i32,
    _w: i32,
    _h: i32,
) {
    unsupported(
        "glCopyTexSubImage2D",
        "framebuffer-to-texture copies are not implemented; use glReadPixels",
    );
    gl_ctx("glCopyTexSubImage2D", (), |gl| {
        gl.set_error(c::INVALID_OPERATION)
    });
}

/// `glEGLImageTargetTexture2DOES`.
pub unsafe extern "C" fn gl_egl_image_target_texture2d_oes(_target: u32, _image: usize) {
    unsupported(
        "glEGLImageTargetTexture2DOES",
        "EGL image objects are not implemented",
    );
    gl_ctx("glEGLImageTargetTexture2DOES", (), |gl| {
        gl.set_error(c::INVALID_OPERATION)
    });
}

/// `glTexStorage2D` — immutable storage is GLES3.
pub unsafe extern "C" fn gl_tex_storage2d(
    _target: u32,
    _levels: i32,
    _format: u32,
    _width: i32,
    _height: i32,
) {
    unsupported("glTexStorage2D", "GLES3 immutable texture storage is not implemented");
    gl_ctx("glTexStorage2D", (), |gl| gl.set_error(c::INVALID_OPERATION));
}

// -- framebuffers / renderbuffers -----------------------------------------

/// `glGenFramebuffers`.
pub unsafe extern "C" fn gl_gen_framebuffers(n: i32, framebuffers: usize) {
    let ids = gl_ctx("glGenFramebuffers", Vec::new(), |gl| gl.gen_framebuffers(n));
    for (i, id) in ids.iter().enumerate() {
        unsafe { write_i32(framebuffers.wrapping_add(i * 4), *id as i32) };
    }
}

/// `glDeleteFramebuffers`.
pub unsafe extern "C" fn gl_delete_framebuffers(n: i32, framebuffers: usize) {
    let ids: Vec<u32> = (0..n.max(0))
        .map(|i| unsafe { (framebuffers as *const u32).add(i as usize).read_unaligned() })
        .collect();
    gl_ctx("glDeleteFramebuffers", (), |gl| gl.delete_framebuffers(&ids));
}

/// `glBindFramebuffer`.
pub unsafe extern "C" fn gl_bind_framebuffer(target: u32, framebuffer: u32) {
    gl_ctx("glBindFramebuffer", (), |gl| {
        gl.bind_framebuffer(target, framebuffer)
    });
}

/// `glFramebufferTexture2D`.
pub unsafe extern "C" fn gl_framebuffer_texture2d(
    target: u32,
    attachment: u32,
    textarget: u32,
    texture: u32,
    level: i32,
) {
    if textarget != c::TEXTURE_2D {
        unsupported(
            "glFramebufferTexture2D",
            format!("textarget {textarget:#x}"),
        );
        gl_ctx("glFramebufferTexture2D", (), |gl| {
            gl.set_error(c::INVALID_ENUM)
        });
        return;
    }
    gl_ctx("glFramebufferTexture2D", (), |gl| {
        gl.framebuffer_texture_2d(target, attachment, texture, level)
    });
}

/// `glFramebufferRenderbuffer`.
pub unsafe extern "C" fn gl_framebuffer_renderbuffer(
    target: u32,
    attachment: u32,
    _renderbuffertarget: u32,
    renderbuffer: u32,
) {
    gl_ctx("glFramebufferRenderbuffer", (), |gl| {
        gl.framebuffer_renderbuffer(target, attachment, renderbuffer)
    });
}

/// `glCheckFramebufferStatus`.
pub unsafe extern "C" fn gl_check_framebuffer_status(target: u32) -> u32 {
    gl_ctx("glCheckFramebufferStatus", c::NONE, |gl| {
        gl.check_framebuffer_status(target)
    })
}

/// `glGetFramebufferAttachmentParameteriv`.
pub unsafe extern "C" fn gl_get_framebuffer_attachment_parameteriv(
    _target: u32,
    attachment: u32,
    pname: u32,
    params: usize,
) {
    /// `GL_FRAMEBUFFER_ATTACHMENT_OBJECT_TYPE`.
    const OBJECT_TYPE: u32 = 0x8CD0;
    /// `GL_FRAMEBUFFER_ATTACHMENT_OBJECT_NAME`.
    const OBJECT_NAME: u32 = 0x8CD1;
    /// `GL_FRAMEBUFFER_ATTACHMENT_TEXTURE_LEVEL`.
    const TEXTURE_LEVEL: u32 = 0x8CDC;
    let v = gl_ctx("glGetFramebufferAttachmentParameteriv", 0, |gl| {
        match pname {
            OBJECT_TYPE => 0x1702,
            OBJECT_NAME => gl
                .fbo_color
                .get(&gl.framebuffer)
                .or_else(|| gl.fbo_depth.get(&gl.framebuffer))
                .copied()
                .unwrap_or(0) as i32,
            TEXTURE_LEVEL => 0,
            other => {
                unsupported(
                    "glGetFramebufferAttachmentParameteriv",
                    format!("attachment {attachment:#x} pname {other:#x}"),
                );
                gl.set_error(c::INVALID_ENUM);
                0
            }
        }
    });
    unsafe { write_i32(params, v) };
}

/// `glGenRenderbuffers`.
pub unsafe extern "C" fn gl_gen_renderbuffers(n: i32, renderbuffers: usize) {
    let ids = gl_ctx("glGenRenderbuffers", Vec::new(), |gl| {
        gl.gen_renderbuffers(n)
    });
    for (i, id) in ids.iter().enumerate() {
        unsafe { write_i32(renderbuffers.wrapping_add(i * 4), *id as i32) };
    }
}

/// `glDeleteRenderbuffers`.
pub unsafe extern "C" fn gl_delete_renderbuffers(n: i32, renderbuffers: usize) {
    let ids: Vec<u32> = (0..n.max(0))
        .map(|i| unsafe { (renderbuffers as *const u32).add(i as usize).read_unaligned() })
        .collect();
    gl_ctx("glDeleteRenderbuffers", (), |gl| {
        gl.delete_renderbuffers(&ids)
    });
}

/// `glBindRenderbuffer`.
pub unsafe extern "C" fn gl_bind_renderbuffer(target: u32, renderbuffer: u32) {
    gl_ctx("glBindRenderbuffer", (), |gl| {
        gl.bind_renderbuffer(target, renderbuffer)
    });
}

/// `glRenderbufferStorage`.
pub unsafe extern "C" fn gl_renderbuffer_storage(
    target: u32,
    format: u32,
    width: i32,
    height: i32,
) {
    gl_ctx("glRenderbufferStorage", (), |gl| {
        gl.renderbuffer_storage(target, format, width, height)
    });
}

/// `glGetRenderbufferParameteriv`.
pub unsafe extern "C" fn gl_get_renderbuffer_parameteriv(
    _target: u32,
    pname: u32,
    params: usize,
) {
    /// `GL_RENDERBUFFER_WIDTH`.
    const WIDTH: u32 = 0x8D42;
    /// `GL_RENDERBUFFER_HEIGHT`.
    const HEIGHT: u32 = 0x8D43;
    let v = gl_ctx("glGetRenderbufferParameteriv", 0, |gl| match pname {
        WIDTH => gl.renderbuffers.get(&gl.renderbuffer).map_or(0, |r| r.width as i32),
        HEIGHT => gl.renderbuffers.get(&gl.renderbuffer).map_or(0, |r| r.height as i32),
        other => {
            unsupported("glGetRenderbufferParameteriv", format!("pname {other:#x}"));
            gl.set_error(c::INVALID_ENUM);
            0
        }
    });
    unsafe { write_i32(params, v) };
}

/// `glReadPixels`.
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn gl_read_pixels(
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    format: u32,
    ty: u32,
    pixels: usize,
) {
    let data = gl_draw("glReadPixels", Vec::new(), |gl, fb| {
        gl.read_pixels(fb, x, y, width, height, format, ty)
    });
    unsafe { write_bytes(pixels, &data) };
}

// -- sync objects ----------------------------------------------------------

/// `glFenceSync`.
pub unsafe extern "C" fn gl_fence_sync(_condition: u32, _flags: u32) -> usize {
    // A CPU rasteriser has completed every command it accepted, so a fence is
    // already signalled. Handing back a token keeps the guest's frame pacing
    // correct instead of stalling it on a sync that can never be waited on.
    gl_ctx("glFenceSync", 0, |_gl| SYNC_TOKEN)
}

/// `glClientWaitSync`.
pub unsafe extern "C" fn gl_client_wait_sync(_sync: usize, _flags: u32, _timeout: u64) -> u32 {
    // `GL_ALREADY_SIGNALED`
    0x911A
}

/// `glWaitSync`.
pub unsafe extern "C" fn gl_wait_sync(_sync: usize, _flags: u32, _timeout: u64) {}

/// `glDeleteSync`.
pub unsafe extern "C" fn gl_delete_sync(_sync: usize) {}

/// Stable token returned by `glFenceSync`.
const SYNC_TOKEN: usize = 0x5_1A11;

/// `glGenQueries` / `glDeleteQueries` / `glGetQueryObjectuiv` — occlusion
/// queries are not implemented.
pub unsafe extern "C" fn gl_gen_queries(_n: i32, _ids: usize) {
    unsupported(
        "glGenQueries",
        "occlusion queries are not implemented (software rasteriser)",
    );
    gl_ctx("glGenQueries", (), |gl| gl.set_error(c::INVALID_OPERATION));
}

/// `glDeleteQueries`.
pub unsafe extern "C" fn gl_delete_queries(_n: i32, _ids: usize) {
    unsupported("glDeleteQueries", "occlusion queries are not implemented");
}

/// `glGetQueryObjectuiv`.
pub unsafe extern "C" fn gl_get_query_objectuiv(_id: u32, _pname: u32, params: usize) {
    unsupported("glGetQueryObjectuiv", "occlusion queries are not implemented");
    unsafe { write_i32(params, 0) };
}

// ---------------------------------------------------------------------------
// Entry-point table
// ---------------------------------------------------------------------------

/// Return a pointer to a static NUL-terminated copy of `s`.
fn c_string(s: &'static str) -> usize {
    // Leaking one small allocation per distinct string is bounded: every
    // string here is a compile-time constant from the table below.
    static STRINGS: OnceLock<std::sync::Mutex<Vec<&'static std::ffi::CString>>> = OnceLock::new();
    let cache = STRINGS.get_or_init(|| std::sync::Mutex::new(Vec::new()));
    let mut g = match cache.lock() {
        Ok(g) => g,
        Err(e) => e.into_inner(),
    };
    if let Some(existing) = g.iter().find(|c| c.to_str() == Ok(s)) {
        return existing.as_ptr() as usize;
    }
    let Ok(c) = std::ffi::CString::new(s) else {
        return 0;
    };
    let leaked: &'static std::ffi::CString = Box::leak(Box::new(c));
    g.push(leaked);
    leaked.as_ptr() as usize
}

macro_rules! table_entries {
    ($($name:literal => $func:expr),+ $(,)?) => {
        vec![$(($name, $func as usize)),+]
    };
}

/// The `libEGL`/`libGLESv2` symbol table.
///
/// Installed into `acos-native-loader`'s bionic table by `acos-jni` on the
/// native (`--native`) path; until then the loader keeps its structured traps.
pub fn table() -> &'static [(&'static str, usize)] {
    static TABLE: OnceLock<Vec<(&'static str, usize)>> = OnceLock::new();
    TABLE.get_or_init(|| {
        table_entries![
            // --- EGL ---
            "eglGetDisplay" => egl_get_display,
            "eglGetError" => egl_get_error,
            "eglInitialize" => egl_initialize,
            "eglTerminate" => egl_terminate,
            "eglChooseConfig" => egl_choose_config,
            "eglGetConfigAttrib" => egl_get_config_attrib,
            "eglGetConfigs" => egl_get_configs,
            "eglCreateWindowSurface" => egl_create_window_surface,
            "eglCreatePbufferSurface" => egl_create_pbuffer_surface,
            "eglDestroySurface" => egl_destroy_surface,
            "eglCreateContext" => egl_create_context,
            "eglDestroyContext" => egl_destroy_context,
            "eglMakeCurrent" => egl_make_current,
            "eglSwapBuffers" => egl_swap_buffers,
            "eglSwapInterval" => egl_swap_interval,
            "eglQuerySurface" => egl_query_surface,
            "eglQueryString" => egl_query_string,
            "eglGetCurrentDisplay" => egl_get_current_display,
            "eglGetCurrentContext" => egl_get_current_context,
            "eglGetCurrentSurface" => egl_get_current_surface,
            "eglBindAPI" => egl_bind_api,
            "eglReleaseThread" => egl_release_thread,
            "eglGetProcAddress" => egl_get_proc_address,
            "eglCreateImageKHR" => egl_create_image_khr,
            "eglDestroyImageKHR" => egl_destroy_image_khr,
            // --- GLES2: state ---
            "glGetError" => gl_get_error,
            "glEnable" => gl_enable,
            "glDisable" => gl_disable,
            "glClear" => gl_clear,
            "glClearColor" => gl_clear_color,
            "glClearStencil" => gl_clear_stencil,
            "glClearDepthf" => gl_clear_depthf,
            "glViewport" => gl_viewport,
            "glScissor" => gl_scissor,
            "glColorMask" => gl_color_mask,
            "glDepthMask" => gl_depth_mask,
            "glDepthFunc" => gl_depth_func,
            "glBlendFunc" => gl_blend_func,
            "glBlendFuncSeparate" => gl_blend_func_separate,
            "glBlendEquation" => gl_blend_equation,
            "glBlendEquationSeparate" => gl_blend_equation_separate,
            "glBlendColor" => gl_blend_color,
            "glCullFace" => gl_cull_face,
            "glFrontFace" => gl_front_face,
            "glPixelStorei" => gl_pixel_storei,
            "glLineWidth" => gl_line_width,
            "glFinish" => gl_finish,
            "glFlush" => gl_flush,
            "glStencilFunc" => gl_stencil_func,
            "glStencilFuncSeparate" => gl_stencil_func_separate,
            "glStencilMask" => gl_stencil_mask,
            "glStencilMaskSeparate" => gl_stencil_mask_separate,
            "glStencilOp" => gl_stencil_op,
            "glStencilOpSeparate" => gl_stencil_op_separate,
            "glGetString" => gl_get_string,
            "glGetStringi" => gl_get_string_i,
            "glGetIntegerv" => gl_get_integerv,
            "glGetIntegeri_v" => gl_get_integer_i_v,
            // --- GLES2: buffers ---
            "glGenBuffers" => gl_gen_buffers,
            "glDeleteBuffers" => gl_delete_buffers,
            "glBindBuffer" => gl_bind_buffer,
            "glBufferData" => gl_buffer_data,
            "glBufferSubData" => gl_buffer_sub_data,
            "glGetBufferParameteriv" => gl_get_buffer_parameteriv,
            // --- GLES2: shaders/programs ---
            "glCreateShader" => gl_create_shader,
            "glDeleteShader" => gl_delete_shader,
            "glShaderSource" => gl_shader_source,
            "glCompileShader" => gl_compile_shader,
            "glGetShaderiv" => gl_get_shaderiv,
            "glGetShaderInfoLog" => gl_get_shader_info_log,
            "glGetShaderPrecisionFormat" => gl_get_shader_precision_format,
            "glCreateProgram" => gl_create_program,
            "glDeleteProgram" => gl_delete_program,
            "glAttachShader" => gl_attach_shader,
            "glBindAttribLocation" => gl_bind_attrib_location,
            "glLinkProgram" => gl_link_program,
            "glUseProgram" => gl_use_program,
            "glValidateProgram" => gl_validate_program,
            "glGetProgramiv" => gl_get_programiv,
            "glGetProgramInfoLog" => gl_get_program_info_log,
            "glGetUniformLocation" => gl_get_uniform_location,
            "glGetAttribLocation" => gl_get_attrib_location,
            // --- GLES2: uniforms ---
            "glUniform1f" => gl_uniform1f,
            "glUniform2f" => gl_uniform2f,
            "glUniform3f" => gl_uniform3f,
            "glUniform4f" => gl_uniform4f,
            "glUniform1i" => gl_uniform1i,
            "glUniform2i" => gl_uniform2i,
            "glUniform3i" => gl_uniform3i,
            "glUniform4i" => gl_uniform4i,
            "glUniform1fv" => gl_uniform1fv,
            "glUniform2fv" => gl_uniform2fv,
            "glUniform3fv" => gl_uniform3fv,
            "glUniform4fv" => gl_uniform4fv,
            "glUniform1iv" => gl_uniform1iv,
            "glUniform2iv" => gl_uniform2iv,
            "glUniform3iv" => gl_uniform3iv,
            "glUniform4iv" => gl_uniform4iv,
            "glUniformMatrix2fv" => gl_uniform_matrix2fv,
            "glUniformMatrix3fv" => gl_uniform_matrix3fv,
            "glUniformMatrix4fv" => gl_uniform_matrix4fv,
            // --- GLES2: attributes/draw ---
            "glVertexAttribPointer" => gl_vertex_attrib_pointer,
            "glEnableVertexAttribArray" => gl_enable_vertex_attrib_array,
            "glDisableVertexAttribArray" => gl_disable_vertex_attrib_array,
            "glVertexAttrib1f" => gl_vertex_attrib1f,
            "glVertexAttrib2f" => gl_vertex_attrib2f,
            "glVertexAttrib3f" => gl_vertex_attrib3f,
            "glVertexAttrib4f" => gl_vertex_attrib4f,
            "glVertexAttrib2fv" => gl_vertex_attrib2fv,
            "glVertexAttrib3fv" => gl_vertex_attrib3fv,
            "glVertexAttrib4fv" => gl_vertex_attrib4fv,
            "glDrawArrays" => gl_draw_arrays,
            "glDrawElements" => gl_draw_elements,
            "glDrawBuffers" => gl_draw_buffers,
            // --- GLES2: textures ---
            "glGenTextures" => gl_gen_textures,
            "glDeleteTextures" => gl_delete_textures,
            "glBindTexture" => gl_bind_texture,
            "glActiveTexture" => gl_active_texture,
            "glTexParameteri" => gl_tex_parameteri,
            "glTexParameterf" => gl_tex_parameterf,
            "glTexParameterfv" => gl_tex_parameterfv,
            "glTexParameteriv" => gl_tex_parameteriv,
            "glTexImage2D" => gl_tex_image2d,
            "glTexSubImage2D" => gl_tex_sub_image2d,
            "glGenerateMipmap" => gl_generate_mipmap,
            "glIsTexture" => gl_is_texture,
            "glCompressedTexImage2D" => gl_compressed_tex_image2d,
            "glCompressedTexSubImage2D" => gl_compressed_tex_sub_image2d,
            "glCopyTexSubImage2D" => gl_copy_tex_sub_image2d,
            "glEGLImageTargetTexture2DOES" => gl_egl_image_target_texture2d_oes,
            "glTexStorage2D" => gl_tex_storage2d,
            // --- GLES2: framebuffers ---
            "glGenFramebuffers" => gl_gen_framebuffers,
            "glDeleteFramebuffers" => gl_delete_framebuffers,
            "glBindFramebuffer" => gl_bind_framebuffer,
            "glFramebufferTexture2D" => gl_framebuffer_texture2d,
            "glFramebufferRenderbuffer" => gl_framebuffer_renderbuffer,
            "glCheckFramebufferStatus" => gl_check_framebuffer_status,
            "glGetFramebufferAttachmentParameteriv" => gl_get_framebuffer_attachment_parameteriv,
            "glGenRenderbuffers" => gl_gen_renderbuffers,
            "glDeleteRenderbuffers" => gl_delete_renderbuffers,
            "glBindRenderbuffer" => gl_bind_renderbuffer,
            "glRenderbufferStorage" => gl_renderbuffer_storage,
            "glGetRenderbufferParameteriv" => gl_get_renderbuffer_parameteriv,
            "glReadPixels" => gl_read_pixels,
            // --- sync / queries ---
            "glFenceSync" => gl_fence_sync,
            "glClientWaitSync" => gl_client_wait_sync,
            "glWaitSync" => gl_wait_sync,
            "glDeleteSync" => gl_delete_sync,
            "glGenQueries" => gl_gen_queries,
            "glDeleteQueries" => gl_delete_queries,
            "glGetQueryObjectuiv" => gl_get_query_objectuiv,
        ]
    })
}

/// Entry point by name (`eglGetProcAddress`, tests).
#[must_use]
pub fn lookup(name: &str) -> Option<usize> {
    table()
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, p)| *p)
}
