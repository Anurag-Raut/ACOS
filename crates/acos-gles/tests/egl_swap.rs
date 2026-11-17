//! End-to-end: EGL context + GLES2 draw + `eglSwapBuffers` → published frame.
//!
//! The window host here is the same seam `acos-jni` fills with the
//! `ANativeWindow` → `FrameStore` path; this test asserts the pixels the host
//! receives are the ones the rasteriser produced, at the surface geometry, with
//! no extra copies losing orientation.

use std::ffi::CString;
use std::sync::Arc;

use acos_gles::egl;
use acos_gles::error::consts as c;
use acos_gles::ffi;

const WINDOW: usize = 0x1000_2000;
const SIZE: u32 = 64;

/// EGL state is process-wide (registry + current context); these tests mutate
/// it, so they run one at a time.
fn lock() -> std::sync::MutexGuard<'static, ()> {
    static GUARD: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    GUARD
        .get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

struct Ctx {
    display: u64,
    surface: u64,
    context: u64,
}

fn shader(kind: u32, source: &str) -> u32 {
    unsafe {
        let shader = ffi::gl_create_shader(kind);
        let cstr = CString::new(source).unwrap();
        let ptrs = [cstr.as_ptr() as usize];
        let lengths = [source.len() as i32];
        ffi::gl_shader_source(shader, 1, ptrs.as_ptr() as usize, lengths.as_ptr() as usize);
        ffi::gl_compile_shader(shader);
        let mut status: i32 = -1;
        ffi::gl_get_shaderiv(shader, c::COMPILE_STATUS, &mut status as *mut i32 as usize);
        if status != 1 {
            let mut buf = [0i8; 512];
            let mut len: i32 = 0;
            ffi::gl_get_shader_info_log(
                shader,
                buf.len() as i32,
                &mut len as *mut i32 as usize,
                buf.as_mut_ptr() as usize,
            );
            let log =
                String::from_utf8_lossy(&buf.map(|b| b as u8)[..len.max(0) as usize]).to_string();
            panic!("shader rejected: {log}");
        }
        shader
    }
}

fn program(vs: &str, fs: &str) -> u32 {
    unsafe {
        let vs = shader(c::VERTEX_SHADER, vs);
        let fs = shader(c::FRAGMENT_SHADER, fs);
        let program = ffi::gl_create_program();
        ffi::gl_attach_shader(program, vs);
        ffi::gl_attach_shader(program, fs);
        ffi::gl_link_program(program);
        let mut status: i32 = -1;
        ffi::gl_get_programiv(program, c::LINK_STATUS, &mut status as *mut i32 as usize);
        assert_eq!(status, 1, "program links");
        ffi::gl_use_program(program);
        program
    }
}

fn setup(host: &Arc<egl::RecordedWindowHost>) -> Ctx {
    egl::reset();
    egl::install_window_host(host.clone());
    unsafe {
        let display = ffi::egl_get_display(0);
        assert_ne!(display, egl::EGL_NO_HANDLE, "display");
        let mut major = 0;
        let mut minor = 0;
        assert_eq!(
            ffi::egl_initialize(
                display,
                &mut major as *mut i32 as usize,
                &mut minor as *mut i32 as usize
            ),
            egl::EGL_TRUE
        );
        let attribs = [
            egl::attrib::SURFACE_TYPE,
            egl::attrib::WINDOW_BIT,
            egl::attrib::RENDERABLE_TYPE,
            egl::attrib::OPENGL_ES2_BIT,
            egl::attrib::RED_SIZE,
            8,
            egl::attrib::DEPTH_SIZE,
            24,
            egl::attrib::NONE,
            0,
        ];
        let mut configs = [0u64; 1];
        let mut count = 0i32;
        assert_eq!(
            ffi::egl_choose_config(
                display,
                attribs.as_ptr() as usize,
                configs.as_mut_ptr() as usize,
                1,
                &mut count as *mut i32 as usize
            ),
            egl::EGL_TRUE
        );
        assert!(count >= 1, "one RGBA8 config is advertised");
        let surface = ffi::egl_create_window_surface(display, configs[0], WINDOW, 0);
        assert_ne!(surface, egl::EGL_NO_HANDLE, "window surface");
        let context_attribs = [egl::attrib::CONTEXT_CLIENT_VERSION, 2, egl::attrib::NONE, 0];
        let context =
            ffi::egl_create_context(display, configs[0], 0, context_attribs.as_ptr() as usize);
        assert_ne!(context, egl::EGL_NO_HANDLE, "context");
        assert_eq!(
            ffi::egl_make_current(display, surface, surface, context),
            egl::EGL_TRUE
        );
        ffi::gl_viewport(0, 0, SIZE as i32, SIZE as i32);
        Ctx {
            display,
            surface,
            context,
        }
    }
}

#[test]
fn egl_context_draws_and_swaps_a_frame_to_the_host() {
    let _g = lock();
    let host = Arc::new(egl::RecordedWindowHost::default());
    host.add_window(WINDOW as u64, SIZE, SIZE);
    let ctx = setup(&host);
    let program = program(
        "attribute vec2 a_pos; void main() { gl_Position = vec4(a_pos, 0.0, 1.0); }",
        "uniform vec4 u_color; void main() { gl_FragColor = u_color; }",
    );
    unsafe {
        let loc = {
            let name = CString::new("u_color").unwrap();
            ffi::gl_get_uniform_location(program, name.as_ptr() as usize)
        };
        assert!(loc >= 0, "u_color uniform location");
        ffi::gl_uniform4f(loc, 0.0, 1.0, 0.0, 1.0);
        ffi::gl_clear_color(0.0, 0.0, 0.0, 1.0);
        ffi::gl_clear(c::COLOR_BUFFER_BIT);

        // Bottom-left half of the viewport.
        let vertices: [f32; 6] = [-1.0, -1.0, 1.0, -1.0, -1.0, 1.0];
        let mut buffer = 0u32;
        ffi::gl_gen_buffers(1, &mut buffer as *mut u32 as usize);
        ffi::gl_bind_buffer(c::ARRAY_BUFFER, buffer);
        ffi::gl_buffer_data(
            c::ARRAY_BUFFER,
            (vertices.len() * 4) as i32,
            vertices.as_ptr() as usize,
            c::STATIC_DRAW,
        );
        let attr = {
            let name = CString::new("a_pos").unwrap();
            ffi::gl_get_attrib_location(program, name.as_ptr() as usize)
        };
        assert!(attr >= 0, "a_pos attribute location");
        ffi::gl_enable_vertex_attrib_array(attr as u32);
        ffi::gl_vertex_attrib_pointer(attr as u32, 2, c::FLOAT, 0, 0, 0);
        ffi::gl_draw_arrays(c::TRIANGLES, 0, 3);
        assert_eq!(ffi::gl_get_error(), c::NO_ERROR, "no GL error");
        assert_eq!(
            ffi::egl_swap_buffers(ctx.display, ctx.surface),
            egl::EGL_TRUE
        );
    }

    let frame = host.last_frame().expect("the host received the frame");
    assert_eq!((frame.width, frame.height), (SIZE, SIZE));
    assert_eq!(frame.window, WINDOW as u64);
    assert_eq!(frame.rgba.len(), (SIZE * SIZE * 4) as usize);

    let px = |x: u32, y: u32| -> [u8; 4] {
        let i = ((y * SIZE + x) * 4) as usize;
        [
            frame.rgba[i],
            frame.rgba[i + 1],
            frame.rgba[i + 2],
            frame.rgba[i + 3],
        ]
    };
    // Cleared background.
    assert_eq!(px(SIZE - 1, 0), [0, 0, 0, 255], "top-right stays cleared");
    // Triangle: solid green where `x <= y` in window space (bottom-left half),
    // i.e. `gl_Position.y = -1` maps to the last row.
    assert_eq!(px(0, SIZE - 1), [0, 255, 0, 255], "bottom-left triangle");
    assert_eq!(px(1, SIZE - 2), [0, 255, 0, 255], "triangle interior");
    assert_eq!(px(SIZE - 2, 1), [0, 0, 0, 255], "outside the hypotenuse");

    // Tearing down releases the surface and the context.
    unsafe {
        assert_eq!(ffi::egl_make_current(ctx.display, 0, 0, 0), egl::EGL_TRUE);
        assert_eq!(
            ffi::egl_destroy_context(ctx.display, ctx.context),
            egl::EGL_TRUE
        );
        assert_eq!(
            ffi::egl_destroy_surface(ctx.display, ctx.surface),
            egl::EGL_TRUE
        );
        assert_eq!(ffi::egl_terminate(ctx.display), egl::EGL_TRUE);
    }
    egl::reset();
}

#[test]
fn a_textured_draw_samples_the_uploaded_texels() {
    let _g = lock();
    let host = Arc::new(egl::RecordedWindowHost::default());
    host.add_window(WINDOW as u64, SIZE, SIZE);
    let ctx = setup(&host);
    let program = program(
        "attribute vec2 a_pos; attribute vec2 a_uv; varying vec2 v_uv; \
         void main() { v_uv = a_uv; gl_Position = vec4(a_pos, 0.0, 1.0); }",
        "precision mediump float; uniform sampler2D u_tex; varying vec2 v_uv; \
         void main() { gl_FragColor = texture2D(u_tex, v_uv); }",
    );
    // 2x2 texture: red, green / blue, white, all opaque.
    let texels: [u8; 16] = [
        255, 0, 0, 255, // (0,0) red
        0, 255, 0, 255, // (1,0) green
        0, 0, 255, 255, // (0,1) blue
        255, 255, 255, 255, // (1,1) white
    ];
    unsafe {
        ffi::gl_pixel_storei(c::UNPACK_ALIGNMENT, 1);
        let mut texture = 0u32;
        ffi::gl_gen_textures(1, &mut texture as *mut u32 as usize);
        ffi::gl_active_texture(c::TEXTURE0);
        ffi::gl_bind_texture(c::TEXTURE_2D, texture);
        ffi::gl_tex_parameteri(c::TEXTURE_2D, c::TEXTURE_MIN_FILTER, c::NEAREST as i32);
        ffi::gl_tex_parameteri(c::TEXTURE_2D, c::TEXTURE_MAG_FILTER, c::NEAREST as i32);
        ffi::gl_tex_parameteri(c::TEXTURE_2D, c::TEXTURE_WRAP_S, c::CLAMP_TO_EDGE as i32);
        ffi::gl_tex_parameteri(c::TEXTURE_2D, c::TEXTURE_WRAP_T, c::CLAMP_TO_EDGE as i32);
        ffi::gl_tex_image2d(
            c::TEXTURE_2D,
            0,
            c::RGBA as i32,
            2,
            2,
            0,
            c::RGBA,
            c::UNSIGNED_BYTE,
            texels.as_ptr() as usize,
        );
        // Full-screen quad as two triangles; uv origin is the top-left texel.
        let vertices: [f32; 24] = [
            // x, y, u, v
            -1.0, -1.0, 0.0, 1.0, // bottom-left
            1.0, -1.0, 1.0, 1.0, // bottom-right
            -1.0, 1.0, 0.0, 0.0, // top-left
            -1.0, 1.0, 0.0, 0.0, // top-left
            1.0, -1.0, 1.0, 1.0, // bottom-right
            1.0, 1.0, 1.0, 0.0, // top-right
        ];
        let mut buffer = 0u32;
        ffi::gl_gen_buffers(1, &mut buffer as *mut u32 as usize);
        ffi::gl_bind_buffer(c::ARRAY_BUFFER, buffer);
        ffi::gl_buffer_data(
            c::ARRAY_BUFFER,
            (vertices.len() * 4) as i32,
            vertices.as_ptr() as usize,
            c::STATIC_DRAW,
        );
        let pos = {
            let n = CString::new("a_pos").unwrap();
            ffi::gl_get_attrib_location(program, n.as_ptr() as usize)
        };
        let uv = {
            let n = CString::new("a_uv").unwrap();
            ffi::gl_get_attrib_location(program, n.as_ptr() as usize)
        };
        // Interleaved stride 16: pos at 0, uv at 8.
        ffi::gl_enable_vertex_attrib_array(pos as u32);
        ffi::gl_vertex_attrib_pointer(pos as u32, 2, c::FLOAT, 0, 16, 0);
        ffi::gl_enable_vertex_attrib_array(uv as u32);
        ffi::gl_vertex_attrib_pointer(uv as u32, 2, c::FLOAT, 0, 16, 8);
        ffi::gl_draw_arrays(c::TRIANGLES, 0, 6);
        assert_eq!(ffi::gl_get_error(), c::NO_ERROR);
        assert_eq!(
            ffi::egl_swap_buffers(ctx.display, ctx.surface),
            egl::EGL_TRUE
        );
    }
    let frame = host.last_frame().expect("frame");
    let px = |x: u32, y: u32| -> [u8; 4] {
        let i = ((y * SIZE + x) * 4) as usize;
        [
            frame.rgba[i],
            frame.rgba[i + 1],
            frame.rgba[i + 2],
            frame.rgba[i + 3],
        ]
    };
    // uv (0,0) is the first texel (red) and lands at the top-left of the window.
    assert_eq!(px(0, 0), [255, 0, 0, 255], "red texel");
    assert_eq!(px(SIZE - 1, 0), [0, 255, 0, 255], "green texel");
    assert_eq!(px(0, SIZE - 1), [0, 0, 255, 255], "blue texel");
    assert_eq!(px(SIZE - 1, SIZE - 1), [255, 255, 255, 255], "white texel");
    egl::reset();
}

#[test]
fn unknown_entry_points_are_reported_not_silent() {
    let _g = lock();
    let host = Arc::new(egl::RecordedWindowHost::default());
    host.add_window(WINDOW as u64, SIZE, SIZE);
    let _ctx = setup(&host);
    let _ = acos_gles::drain_events();
    unsafe {
        // Compressed textures are advertised nowhere; the call must fail loudly.
        ffi::gl_compressed_tex_image2d(c::TEXTURE_2D, 0, c::RGBA as i32, 4, 4, 0, 64, 0);
        assert_eq!(ffi::gl_get_error(), c::INVALID_OPERATION);
        assert_eq!(ffi::gl_get_error(), c::NO_ERROR, "error state clears");
    }
    let events = acos_gles::drain_events();
    assert!(
        events
            .iter()
            .any(|e| e.kind == "UNSUPPORTED_GLES" && e.call == "glCompressedTexImage2D"),
        "structured UNSUPPORTED_GLES event: {events:?}"
    );
    egl::reset();
}

#[test]
fn client_side_vertex_arrays_draw_without_a_buffer() {
    let _g = lock();
    let host = Arc::new(egl::RecordedWindowHost::default());
    host.add_window(WINDOW as u64, SIZE, SIZE);
    let ctx = setup(&host);
    let program = program(
        "attribute vec2 a_pos; void main() { gl_Position = vec4(a_pos, 0.0, 1.0); }",
        "void main() { gl_FragColor = vec4(0.0, 1.0, 0.0, 1.0); }",
    );
    unsafe {
        // No ARRAY_BUFFER is bound: the pointer is a client-side host array.
        let vertices: [f32; 6] = [-1.0, -1.0, 1.0, -1.0, -1.0, 1.0];
        let attr = {
            let n = CString::new("a_pos").unwrap();
            ffi::gl_get_attrib_location(program, n.as_ptr() as usize)
        };
        ffi::gl_enable_vertex_attrib_array(attr as u32);
        ffi::gl_vertex_attrib_pointer(attr as u32, 2, c::FLOAT, 0, 0, vertices.as_ptr() as usize);
        ffi::gl_clear_color(0.0, 0.0, 0.0, 1.0);
        ffi::gl_clear(c::COLOR_BUFFER_BIT);
        ffi::gl_draw_arrays(c::TRIANGLES, 0, 3);
        assert_eq!(ffi::gl_get_error(), c::NO_ERROR, "client array draw");
        assert_eq!(
            ffi::egl_swap_buffers(ctx.display, ctx.surface),
            egl::EGL_TRUE
        );
    }
    let frame = host.last_frame().expect("frame");
    let px = |x: u32, y: u32| -> [u8; 4] {
        let i = ((y * SIZE + x) * 4) as usize;
        [
            frame.rgba[i],
            frame.rgba[i + 1],
            frame.rgba[i + 2],
            frame.rgba[i + 3],
        ]
    };
    assert_eq!(px(0, SIZE - 1), [0, 255, 0, 255], "client triangle drawn");
    egl::reset();
}

#[test]
fn copy_tex_image_and_generate_mipmap_sample_without_error() {
    let _g = lock();
    let host = Arc::new(egl::RecordedWindowHost::default());
    host.add_window(WINDOW as u64, SIZE, SIZE);
    let ctx = setup(&host);
    let program = program(
        "attribute vec2 a_pos; attribute vec2 a_uv; varying vec2 v_uv; \
         void main() { v_uv = a_uv; gl_Position = vec4(a_pos, 0.0, 1.0); }",
        "precision mediump float; uniform sampler2D u_tex; varying vec2 v_uv; \
         void main() { gl_FragColor = texture2D(u_tex, v_uv); }",
    );
    unsafe {
        // Void out the old error state, then clear the draw buffer to a
        // distinctive colour and copy it into a texture.
        let _ = ffi::gl_get_error();
        ffi::gl_clear_color(0.2, 0.4, 0.8, 1.0);
        ffi::gl_clear(c::COLOR_BUFFER_BIT);
        let mut texture = 0u32;
        ffi::gl_gen_textures(1, &mut texture as *mut u32 as usize);
        ffi::gl_bind_texture(c::TEXTURE_2D, texture);
        ffi::gl_copy_tex_image2d(
            c::TEXTURE_2D,
            0,
            c::RGBA as i32,
            0,
            0,
            SIZE as i32,
            SIZE as i32,
            0,
        );
        ffi::gl_generate_mipmap(c::TEXTURE_2D);
        ffi::gl_tex_parameteri(
            c::TEXTURE_2D,
            c::TEXTURE_MIN_FILTER,
            c::LINEAR_MIPMAP_LINEAR as i32,
        );
        ffi::gl_tex_parameteri(c::TEXTURE_2D, c::TEXTURE_MAG_FILTER, c::LINEAR as i32);
        assert_eq!(ffi::gl_get_error(), c::NO_ERROR, "copy + mipmap");

        let vertices: [f32; 24] = [
            -1.0, -1.0, 0.0, 1.0, 1.0, -1.0, 1.0, 1.0, -1.0, 1.0, 0.0, 0.0, -1.0, 1.0, 0.0, 0.0,
            1.0, -1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 0.0,
        ];
        let mut buffer = 0u32;
        ffi::gl_gen_buffers(1, &mut buffer as *mut u32 as usize);
        ffi::gl_bind_buffer(c::ARRAY_BUFFER, buffer);
        ffi::gl_buffer_data(
            c::ARRAY_BUFFER,
            (vertices.len() * 4) as i32,
            vertices.as_ptr() as usize,
            c::STATIC_DRAW,
        );
        let pos = {
            let n = CString::new("a_pos").unwrap();
            ffi::gl_get_attrib_location(program, n.as_ptr() as usize)
        };
        let uv = {
            let n = CString::new("a_uv").unwrap();
            ffi::gl_get_attrib_location(program, n.as_ptr() as usize)
        };
        ffi::gl_enable_vertex_attrib_array(pos as u32);
        ffi::gl_vertex_attrib_pointer(pos as u32, 2, c::FLOAT, 0, 16, 0);
        ffi::gl_enable_vertex_attrib_array(uv as u32);
        ffi::gl_vertex_attrib_pointer(uv as u32, 2, c::FLOAT, 0, 16, 8);
        ffi::gl_draw_arrays(c::TRIANGLES, 0, 6);
        assert_eq!(ffi::gl_get_error(), c::NO_ERROR, "textured draw");
        assert_eq!(
            ffi::egl_swap_buffers(ctx.display, ctx.surface),
            egl::EGL_TRUE
        );
    }
    let frame = host.last_frame().expect("frame");
    let px = |x: u32, y: u32| -> [u8; 4] {
        let i = ((y * SIZE + x) * 4) as usize;
        [
            frame.rgba[i],
            frame.rgba[i + 1],
            frame.rgba[i + 2],
            frame.rgba[i + 3],
        ]
    };
    // The copied colour survives mip generation and is sampled everywhere.
    assert_eq!(px(0, 0), [51, 102, 204, 255]);
    assert_eq!(px(SIZE - 1, SIZE - 1), [51, 102, 204, 255]);
    egl::reset();
}
