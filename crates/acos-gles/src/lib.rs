//! # acos-gles — software GLES2 + EGL substrate (ADR-0023)
//!
//! Flutter draws its whole UI through GLES2, and several React Native modules
//! (`react-native-svg` on the new architecture, `@shopify/react-native-skia`,
//! `expo-gl`, `react-native-video`) call `egl*`/`gl*` from JNI. `acos-render`
//! only covers the `android.graphics` View path, so this crate provides the
//! missing substrate:
//!
//! ```text
//!  guest .so ──imports──> acos-native-loader bionic table
//!                              │  (installed by acos-jni on --native)
//!                              ▼
//!                         acos-gles::ffi       C ABI entry points
//!                          ├── egl.rs          display/config/context/surface
//!                          ├── gl.rs           object model + state machine
//!                          ├── glsl.rs         GLSL ES 1.00 interpreter (no JIT)
//!                          └── raster.rs       RGBA8888 scanline rasteriser
//!                              │
//!                              └── eglSwapBuffers → EglWindowHost → FrameStore
//! ```
//!
//! ## Honesty model
//!
//! * A shader is compiled by the interpreter or **rejected** with a positioned
//!   reason; `glGetShaderiv(GL_COMPILE_STATUS)` reports `GL_FALSE`.
//! * An entry point outside the implemented subset logs one structured
//!   `UNSUPPORTED_GLES` line and sets `GL_INVALID_OPERATION`; there are no
//!   silent no-ops and no default-shader fallbacks.
//! * `eglSwapBuffers` publishes through the installed [`egl::WindowHost`]; with
//!   no host it fails with `EGL_BAD_SURFACE` instead of dropping the frame.
//!
//! ## Safety
//!
//! The parser, interpreter, rasteriser and object model are `#![deny(unsafe_code)]`;
//! only [`ffi`] relaxes that, because unpacking C arguments (pointer/slice
//! conversion, writing guest buffers) cannot be expressed safely. Nothing in
//! this crate executes guest code or guest-provided machine words.
#![deny(unsafe_code)]
#![deny(missing_docs)]
#![deny(clippy::undocumented_unsafe_blocks)]

pub mod egl;
pub mod error;
pub mod ffi;
pub mod gl;
pub mod glsl;
pub mod raster;

pub use error::{diagnostic, drain_events, unsupported, Event};
pub use gl::Gl;
pub use raster::Framebuffer;

/// Install this substrate's `libEGL`/`libGLESv2` table into the native loader's
/// bionic symbol table (replacing its structured EGL/GLES traps).
///
/// Called by `acos-jni` from the native load path, which the CLI gates behind
/// `--native`; without it the loader keeps failing closed.
pub fn install_loader_table() {
    acos_native_loader::bionic::gles::install_table(ffi::table());
}

/// Whether the loader is currently answering EGL/GLES from this substrate.
#[must_use]
pub fn loader_table_installed() -> bool {
    acos_native_loader::bionic::gles::installed()
}

/// The version string reported by `glGetString(GL_VERSION)`.
pub const GL_VERSION: &str = "OpenGL ES 2.0 ACOS-1";
