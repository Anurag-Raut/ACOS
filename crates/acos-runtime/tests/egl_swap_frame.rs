//! W86-A: an EGL swap publishes a completed frame into the real `FrameStore`.
//!
//! The chain under test is the production one:
//!
//! ```text
//! eglSwapBuffers → acos-gles window host → acos-jni ANativeWindow registry
//!               → substrate::submit_frame → installed FrameSink
//!               → WmsSurfaceBackend (compositor) → FrameStore
//! ```
//!
//! Nothing here fabricates a `CompletedFrame`: the test asserts the store is
//! empty until the swap, and that the pixels the rasteriser produced are the
//! pixels the compositor captured.

use std::cell::RefCell;
use std::ffi::CString;
use std::rc::Rc;
use std::sync::Arc;

use acos_gles::error::consts as c;
use acos_gles::ffi;
use acos_runtime::frame_handoff::{FrameHandoffSink, FrameStore};
use acos_runtime::wms_java::WmSurfaceBackend;
use acos_vm::natives::fwk_surface::SurfaceBackend;
use acos_vm::natives::gfx_hwui::install_sink;

const SIZE: u32 = 32;
const LAYER: u64 = 0x77_0001;
const SURFACE: u64 = 0x5AA1;

/// `true` once the loader answered an EGL symbol from the installed substrate
/// rather than from its fail-closed trap.
fn loader_has_gles() -> bool {
    use acos_native_loader::bionic;
    let resolved = bionic::resolve("eglGetDisplay").map(|s| s.ptr as usize);
    let expected = ffi::lookup("eglGetDisplay");
    resolved.is_some() && resolved == expected
}

#[test]
fn egl_swap_publishes_a_completed_frame() {
    // --- production frame sink over a real WmsSurfaceBackend ---------------
    let backend = Rc::new(RefCell::new(WmSurfaceBackend::new().expect("compositor")));
    let layer = backend
        .borrow_mut()
        .create_layer(LAYER, "egl-surface", SIZE as i32, SIZE as i32);
    let store = Arc::new(FrameStore::new());
    install_sink(Box::new(FrameHandoffSink::new(
        Rc::clone(&backend),
        Arc::clone(&store),
    )));

    // --- the native-path wiring: loader table + ANativeWindow host ---------
    acos_jni::ndk_host::install_forced();
    assert!(
        loader_has_gles(),
        "the loader must resolve eglGetDisplay from acos-gles, not its trap"
    );
    let window = acos_jni::ndk_host::register_native_window(
        SIZE as i32,
        SIZE as i32,
        layer,
        SURFACE,
    );

    // Nothing is published before a real swap.
    assert!(store.latest().is_none());
    assert_eq!(store.published_count(), 0);

    // --- EGL + GLES2: clear to blue, draw a green triangle ----------------
    let host = acos_gles::egl::RecordedWindowHost::default();
    let _ = host; // the installed host is the real one; this is only a marker
    acos_gles::egl::reset();
    acos_jni::gles_host::install();

    unsafe {
        let display = ffi::egl_get_display(0);
        assert_ne!(display, 0);
        let mut major = 0i32;
        let mut minor = 0i32;
        assert_eq!(
            ffi::egl_initialize(
                display,
                &mut major as *mut i32 as usize,
                &mut minor as *mut i32 as usize
            ),
            1
        );
        let attribs = [
            acos_gles::egl::attrib::SURFACE_TYPE,
            acos_gles::egl::attrib::WINDOW_BIT,
            acos_gles::egl::attrib::RENDERABLE_TYPE,
            acos_gles::egl::attrib::OPENGL_ES2_BIT,
            acos_gles::egl::attrib::NONE,
            0,
        ];
        let mut config = [0u64; 1];
        let mut count = 0i32;
        assert_eq!(
            ffi::egl_choose_config(
                display,
                attribs.as_ptr() as usize,
                config.as_mut_ptr() as usize,
                1,
                &mut count as *mut i32 as usize
            ),
            1
        );
        let surface = ffi::egl_create_window_surface(display, config[0], window as usize, 0);
        assert_ne!(surface, 0, "window surface over the ANativeWindow");
        let cattribs = [acos_gles::egl::attrib::CONTEXT_CLIENT_VERSION, 2, acos_gles::egl::attrib::NONE, 0];
        let context =
            ffi::egl_create_context(display, config[0], 0, cattribs.as_ptr() as usize);
        assert_ne!(context, 0);
        assert_eq!(ffi::egl_make_current(display, surface, surface, context), 1);
        ffi::gl_viewport(0, 0, SIZE as i32, SIZE as i32);

        // Shaders.
        let vs_src = "attribute vec2 a_pos; void main(){ gl_Position = vec4(a_pos, 0.0, 1.0); }";
        let fs_src = "uniform vec4 u_color; void main(){ gl_FragColor = u_color; }";
        let build = |kind: u32, src: &str| -> u32 {
            let shader = ffi::gl_create_shader(kind);
            let text = CString::new(src).unwrap();
            let ptrs = [text.as_ptr() as usize];
            let lens = [src.len() as i32];
            ffi::gl_shader_source(shader, 1, ptrs.as_ptr() as usize, lens.as_ptr() as usize);
            ffi::gl_compile_shader(shader);
            let mut ok = 0i32;
            ffi::gl_get_shaderiv(shader, c::COMPILE_STATUS, &mut ok as *mut i32 as usize);
            assert_eq!(ok, 1, "shader compiles");
            shader
        };
        let vs = build(c::VERTEX_SHADER, vs_src);
        let fs = build(c::FRAGMENT_SHADER, fs_src);
        let program = ffi::gl_create_program();
        ffi::gl_attach_shader(program, vs);
        ffi::gl_attach_shader(program, fs);
        ffi::gl_link_program(program);
        ffi::gl_use_program(program);
        let loc = {
            let n = CString::new("u_color").unwrap();
            ffi::gl_get_uniform_location(program, n.as_ptr() as usize)
        };
        ffi::gl_uniform4f(loc, 0.0, 0.0, 1.0, 1.0);
        ffi::gl_clear_color(1.0, 0.0, 0.0, 1.0);
        ffi::gl_clear(c::COLOR_BUFFER_BIT);

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
            let n = CString::new("a_pos").unwrap();
            ffi::gl_get_attrib_location(program, n.as_ptr() as usize)
        };
        ffi::gl_enable_vertex_attrib_array(attr as u32);
        ffi::gl_vertex_attrib_pointer(attr as u32, 2, c::FLOAT, 0, 0, 0);
        ffi::gl_draw_arrays(c::TRIANGLES, 0, 3);
        assert_eq!(ffi::gl_get_error(), c::NO_ERROR);

        // Still nothing published: drawing alone never publishes a frame.
        assert_eq!(store.published_count(), 0);
        assert_eq!(ffi::egl_swap_buffers(display, surface), 1, "swap succeeds");
    }

    // --- the swap was the publish signal ----------------------------------
    let frame = store.latest().expect("a completed frame after the swap");
    assert_eq!(store.published_count(), 1);
    assert_eq!(frame.window, layer, "the compositor layer the window bound");
    assert_ne!(frame.content_hash, 0, "content hash is set");
    assert_eq!(
        (frame.width, frame.height),
        (1080, 2400),
        "the completed frame is the display, not the layer"
    );
    // The layer sits at the display origin, so its pixels are the frame's
    // top-left `SIZE x SIZE` box. The rasterised content must survive the whole
    // hand-off unchanged: the red clear colour plus the blue triangle, which
    // covers the lower-left half (`SIZE * (SIZE + 1) / 2 = 528` fragments).
    let stride = frame.width as usize * 4;
    let (mut red, mut blue, mut other) = (0usize, 0usize, 0usize);
    for y in 0..SIZE as usize {
        for x in 0..SIZE as usize {
            let i = y * stride + x * 4;
            match &frame.pixels[i..i + 4] {
                [255, 0, 0, 255] => red += 1,
                [0, 0, 255, 255] => blue += 1,
                _ => other += 1,
            }
        }
    }
    let total = (SIZE * SIZE) as usize;
    assert_eq!(other, 0, "only the clear colour and the draw are present");
    assert_eq!(red + blue, total);
    assert_eq!(blue, 528, "exactly the triangle's rasterised area is blue");
    assert_eq!(red, total - 528, "the rest is the cleared background");

    // A second swap publishes a second, strictly newer frame.
    unsafe {
        assert_eq!(ffi::egl_swap_buffers(acos_gles::egl::current_display(), acos_gles::egl::current_surface(false)), 1);
    }
    assert_eq!(store.published_count(), 2);
    assert_eq!(store.latest().expect("frame 2").frame_id, 2);

    acos_jni::gles_host::uninstall();
    acos_gles::egl::reset();
    install_sink(Box::new(acos_vm::natives::gfx_hwui::MemorySink::default()));
}
