//! Structured GLES errors and the `UNSUPPORTED_GLES` diagnostic channel.
//!
//! ADR-0023 fixes the honesty rule for this substrate: an entry point the
//! interpreter/rasteriser cannot honour must (a) set the GL error state, and
//! (b) emit exactly one structured line naming the call and the reason. It must
//! never render a plausible-looking frame for a call it ignored.
//!
//! [`unsupported`] is that channel. Tests assert on [`drain_events`], so the
//! log is observable without scraping stderr.

use std::sync::Mutex;
use std::sync::OnceLock;

/// GL error codes used by the substrate.
pub mod glerr {
    /// `GL_NO_ERROR`.
    pub const NO_ERROR: u32 = 0;
    /// `GL_INVALID_ENUM`.
    pub const INVALID_ENUM: u32 = 0x0500;
    /// `GL_INVALID_VALUE`.
    pub const INVALID_VALUE: u32 = 0x0501;
    /// `GL_INVALID_OPERATION`.
    pub const INVALID_OPERATION: u32 = 0x0502;
    /// `GL_OUT_OF_MEMORY`.
    pub const OUT_OF_MEMORY: u32 = 0x0505;
    /// `GL_INVALID_FRAMEBUFFER_OPERATION`.
    pub const INVALID_FRAMEBUFFER_OPERATION: u32 = 0x0506;
}

/// `EGL_NOT_INITIALIZED`.
pub const EGL_NOT_INITIALIZED: i32 = 0x3001;

/// One structured event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    /// `UNSUPPORTED_GLES` for an entry point the substrate does not implement,
    /// `GLES_DIAGNOSTIC` for an accepted call with a caveat.
    pub kind: &'static str,
    /// The GL/EGL entry point.
    pub call: String,
    /// Why it could not be honoured.
    pub reason: String,
}

fn events() -> &'static Mutex<Vec<Event>> {
    static EVENTS: OnceLock<Mutex<Vec<Event>>> = OnceLock::new();
    EVENTS.get_or_init(|| Mutex::new(Vec::new()))
}

/// Record + print a structured `UNSUPPORTED_GLES` event.
pub fn unsupported(call: &str, reason: impl Into<String>) {
    let reason = reason.into();
    eprintln!("UNSUPPORTED_GLES call={call} reason={reason}");
    if let Ok(mut g) = events().lock() {
        g.push(Event {
            kind: "UNSUPPORTED_GLES",
            call: call.to_string(),
            reason,
        });
    }
}

/// Record + print a structured diagnostic for an accepted call.
pub fn diagnostic(call: &str, reason: impl Into<String>) {
    let reason = reason.into();
    eprintln!("GLES_DIAGNOSTIC call={call} reason={reason}");
    if let Ok(mut g) = events().lock() {
        g.push(Event {
            kind: "GLES_DIAGNOSTIC",
            call: call.to_string(),
            reason,
        });
    }
}

/// Drain the recorded events (tests, `--trace-gles`).
#[must_use]
pub fn drain_events() -> Vec<Event> {
    events()
        .lock()
        .map(|mut g| std::mem::take(&mut *g))
        .unwrap_or_default()
}

/// The subset of OpenGL ES 2.0 constants the substrate implements.
///
/// Values are the Khronos ABI values; the guest JNI/NDK layer passes them
/// through unchanged.
#[allow(missing_docs)]
pub mod consts {
    pub const NO_ERROR: u32 = 0;
    pub const INVALID_ENUM: u32 = 0x0500;
    pub const INVALID_VALUE: u32 = 0x0501;
    pub const INVALID_OPERATION: u32 = 0x0502;
    pub const OUT_OF_MEMORY: u32 = 0x0505;
    pub const INVALID_FRAMEBUFFER_OPERATION: u32 = 0x0506;

    pub const POINTS: u32 = 0;
    pub const LINES: u32 = 1;
    pub const LINE_LOOP: u32 = 2;
    pub const LINE_STRIP: u32 = 3;
    pub const TRIANGLES: u32 = 4;
    pub const TRIANGLE_STRIP: u32 = 5;
    pub const TRIANGLE_FAN: u32 = 6;

    pub const DEPTH_BUFFER_BIT: u32 = 0x0000_0100;
    pub const STENCIL_BUFFER_BIT: u32 = 0x0000_0400;
    pub const COLOR_BUFFER_BIT: u32 = 0x0000_4000;

    pub const NEVER: u32 = 0x0200;
    pub const LESS: u32 = 0x0201;
    pub const EQUAL: u32 = 0x0202;
    pub const LEQUAL: u32 = 0x0203;
    pub const GREATER: u32 = 0x0204;
    pub const NOTEQUAL: u32 = 0x0205;
    pub const GEQUAL: u32 = 0x0206;
    pub const ALWAYS: u32 = 0x0207;

    pub const ZERO: u32 = 0;
    pub const ONE: u32 = 1;
    pub const SRC_COLOR: u32 = 0x0300;
    pub const ONE_MINUS_SRC_COLOR: u32 = 0x0301;
    pub const SRC_ALPHA: u32 = 0x0302;
    pub const ONE_MINUS_SRC_ALPHA: u32 = 0x0303;
    pub const DST_ALPHA: u32 = 0x0304;
    pub const ONE_MINUS_DST_ALPHA: u32 = 0x0305;
    pub const DST_COLOR: u32 = 0x0306;
    pub const ONE_MINUS_DST_COLOR: u32 = 0x0307;
    pub const SRC_ALPHA_SATURATE: u32 = 0x0308;
    pub const FUNC_ADD: u32 = 0x8006;
    pub const FUNC_SUBTRACT: u32 = 0x800A;
    pub const FUNC_REVERSE_SUBTRACT: u32 = 0x800B;

    pub const FRONT: u32 = 0x0404;
    pub const BACK: u32 = 0x0405;
    pub const FRONT_AND_BACK: u32 = 0x0408;
    pub const CW: u32 = 0x0900;
    pub const CCW: u32 = 0x0901;

    pub const BYTE: u32 = 0x1400;
    pub const UNSIGNED_BYTE: u32 = 0x1401;
    pub const SHORT: u32 = 0x1402;
    pub const UNSIGNED_SHORT: u32 = 0x1403;
    pub const INT: u32 = 0x1404;
    pub const UNSIGNED_INT: u32 = 0x1405;
    pub const FLOAT: u32 = 0x1406;
    pub const FIXED: u32 = 0x140C;

    pub const DEPTH_COMPONENT: u32 = 0x1902;
    pub const ALPHA: u32 = 0x1906;
    pub const RGB: u32 = 0x1907;
    pub const RGBA: u32 = 0x1908;
    pub const LUMINANCE: u32 = 0x1909;
    pub const LUMINANCE_ALPHA: u32 = 0x190A;

    pub const TEXTURE_2D: u32 = 0x0DE1;
    pub const TEXTURE_CUBE_MAP: u32 = 0x8513;
    pub const TEXTURE0: u32 = 0x84C0;
    pub const TEXTURE_MAG_FILTER: u32 = 0x2800;
    pub const TEXTURE_MIN_FILTER: u32 = 0x2801;
    pub const TEXTURE_WRAP_S: u32 = 0x2802;
    pub const TEXTURE_WRAP_T: u32 = 0x2803;
    pub const NEAREST: u32 = 0x2600;
    pub const LINEAR: u32 = 0x2601;
    pub const NEAREST_MIPMAP_NEAREST: u32 = 0x2700;
    pub const LINEAR_MIPMAP_NEAREST: u32 = 0x2701;
    pub const NEAREST_MIPMAP_LINEAR: u32 = 0x2702;
    pub const LINEAR_MIPMAP_LINEAR: u32 = 0x2703;
    pub const CLAMP_TO_EDGE: u32 = 0x812F;
    pub const REPEAT: u32 = 0x2901;
    pub const MIRRORED_REPEAT: u32 = 0x8370;
    pub const UNPACK_ALIGNMENT: u32 = 0x0CF5;
    pub const PACK_ALIGNMENT: u32 = 0x0D05;
    pub const UNPACK_FLIP_Y_WEBGL: u32 = 0x9240;

    pub const VERTEX_SHADER: u32 = 0x8B31;
    pub const FRAGMENT_SHADER: u32 = 0x8B30;
    pub const COMPILE_STATUS: u32 = 0x8B81;
    pub const LINK_STATUS: u32 = 0x8B82;
    pub const VALIDATE_STATUS: u32 = 0x8B83;
    pub const INFO_LOG_LENGTH: u32 = 0x8B84;
    pub const ACTIVE_UNIFORMS: u32 = 0x8B86;
    pub const ACTIVE_ATTRIBUTES: u32 = 0x8B89;
    pub const DELETE_STATUS: u32 = 0x8B80;

    pub const ARRAY_BUFFER: u32 = 0x8892;
    pub const ELEMENT_ARRAY_BUFFER: u32 = 0x8893;
    pub const STREAM_DRAW: u32 = 0x88E0;
    pub const STATIC_DRAW: u32 = 0x88E4;
    pub const DYNAMIC_DRAW: u32 = 0x88E8;

    pub const FRAMEBUFFER: u32 = 0x8D40;
    pub const RENDERBUFFER: u32 = 0x8D41;
    pub const COLOR_ATTACHMENT0: u32 = 0x8CE0;
    pub const DEPTH_ATTACHMENT: u32 = 0x8D00;
    pub const FRAMEBUFFER_COMPLETE: u32 = 0x8CD5;
    pub const FRAMEBUFFER_INCOMPLETE_ATTACHMENT: u32 = 0x8CD6;
    pub const FRAMEBUFFER_INCOMPLETE_MISSING_ATTACHMENT: u32 = 0x8CD7;
    pub const FRAMEBUFFER_UNSUPPORTED: u32 = 0x8CDD;
    pub const NONE: u32 = 0;

    pub const VENDOR: u32 = 0x1F00;
    pub const RENDERER: u32 = 0x1F01;
    pub const VERSION: u32 = 0x1F02;
    pub const EXTENSIONS: u32 = 0x1F03;
    pub const SHADING_LANGUAGE_VERSION: u32 = 0x8B8C;

    pub const MAX_TEXTURE_SIZE: u32 = 0x0D33;
    pub const MAX_VIEWPORT_DIMS: u32 = 0x0D3A;
    pub const MAX_VERTEX_ATTRIBS: u32 = 0x8869;
    pub const MAX_TEXTURE_IMAGE_UNITS: u32 = 0x8872;
    pub const MAX_VERTEX_UNIFORM_VECTORS: u32 = 0x8DFB;
    pub const MAX_VARYING_VECTORS: u32 = 0x8DFC;
    pub const MAX_FRAGMENT_UNIFORM_VECTORS: u32 = 0x8DFD;
    pub const MAX_COMBINED_TEXTURE_IMAGE_UNITS: u32 = 0x8B4D;
    pub const MAX_CUBE_MAP_TEXTURE_SIZE: u32 = 0x851C;
    pub const MAX_RENDERBUFFER_SIZE: u32 = 0x84E8;
    pub const COMPRESSED_TEXTURE_FORMATS: u32 = 0x86A3;
    pub const NUM_COMPRESSED_TEXTURE_FORMATS: u32 = 0x86A2;
    pub const VIEWPORT: u32 = 0x0BA2;
    pub const CURRENT_PROGRAM: u32 = 0x8B8D;
    pub const ARRAY_BUFFER_BINDING: u32 = 0x8894;
    pub const ELEMENT_ARRAY_BUFFER_BINDING: u32 = 0x8895;
    pub const FRAMEBUFFER_BINDING: u32 = 0x8CA6;
    pub const RENDERBUFFER_BINDING: u32 = 0x8CA7;
    pub const TEXTURE_BINDING_2D: u32 = 0x8069;
    pub const ACTIVE_TEXTURE: u32 = 0x84E0;
    pub const DEPTH_WRITEMASK: u32 = 0x0B72;
    pub const DEPTH_TEST: u32 = 0x0B71;
    pub const BLEND: u32 = 0x0BE2;
    pub const CULL_FACE: u32 = 0x0B44;
    pub const SCISSOR_TEST: u32 = 0x0C11;
    pub const DITHER: u32 = 0x0BD0;
    pub const LINE_WIDTH: u32 = 0x0B21;

    /// Number of texture units the substrate exposes.
    pub const TEXTURE_UNITS: usize = 8;
    /// Maximum vertex attribute slots.
    pub const MAX_ATTRIBS: usize = 16;
    /// Largest texture/framebuffer edge the software substrate allows.
    pub const MAX_DIM: u32 = 4096;
}
