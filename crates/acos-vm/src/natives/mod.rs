//! True native methods for the real Android 14 core library.
//!
//! In `--corelib=real` mode the class provider is the real boot-classpath DEX
//! (`core-oj`/`core-libart`/`core-icu4j`), so every method declared `native`
//! there must be supplied by Rust. Natives are registered by
//! `(class descriptor, method name, signature)` and bound when the declaring
//! DEX class is loaded; anything unbound becomes a structured
//! `UNSUPPORTED_JAVA_API` naming `Lclass;->name(sig)`.
//!
//! Modules mirror the Android class packages:
//! `lang` (Object/String/System/Thread/Class/Throwable), `icu`, `io`, `vmrt`,
//! `math`, `reflect`, `looper` (the real `android.os.MessageQueue` epoll
//! looper) and the `os_*` framework-edge modules.

pub mod binder;
mod character;
mod fwk_conscrypt;
mod fwk_debug;
mod fwk_dexfile;
mod fwk_display;
pub(crate) mod fwk_fonts;
pub mod fwk_graphics;
pub mod fwk_input;
pub mod gfx_bitmap;
pub mod gfx_canvas;
pub mod gfx_handles;
pub mod gfx_hwui;
pub mod gfx_matrix;
pub mod gfx_node;
pub mod gfx_paint;
pub mod gfx_path;
mod gfx_ripple;
mod gfx_state;
pub mod gfx_vector;
pub mod gfx_text;
pub mod fwk_jni;
mod fwk_animation;
pub(crate) mod fwk_annotations;
mod fwk_m2_filechannel;
mod fwk_method_invoke;
mod fwk_m2_chain;
mod fwk_m2_chain2;
mod fwk_m2_chain6;
mod fwk_nio_fs;
mod fwk_proxy;
pub mod fwk_surface;
pub mod fwk_vsync;
pub(crate) mod fwk_reflect;
mod fwk_res;
mod fwk_sqlite;
mod icu;
pub(crate) mod icu_regex;
// The `.kcm` key-character-map parser backing the `android.view.KeyCharacterMap`
// natives (`fwk_input`).
mod kcm;
mod io;
mod io_linux;
pub mod jni_glue;
pub(crate) mod lang;
pub mod looper;
mod math;
mod memintarray;
pub(crate) mod native_mem;
pub mod ndk_substrate;
pub(crate) mod os_clock;
mod os_constants;
mod parcel;
pub mod os_log;
pub mod os_misc;
pub mod os_props;
pub mod safe_point;
mod reflect;
#[path = "unsafe.rs"]
mod unsafe_natives;
pub(crate) mod varhandle;
mod vmrt;
mod zip;

use crate::error::VmResult;
use crate::vm::Vm;

/// Restore re-seed hook for `android.os.Parcel` (T-43): allocate a fresh
/// native parcel handle for a restored Java `Parcel` object.
pub(crate) fn create_parcel_handle() -> u64 {
    parcel::create_parcel_handle()
}

/// Register every true native the M1 console tier needs.
pub(crate) fn install(vm: &mut Vm) -> VmResult<()> {
    lang::install(vm)?;
    character::install(vm)?;
    icu::install(vm)?;
    icu_regex::install(vm)?;
    io::install(vm)?;
    // Registered after `io`: the file-I/O natives replace the std-stream stubs
    // and own every path/descriptor operation.
    io_linux::install(vm);
    // After `io`/`io_linux`: the full 538-field bionic `OsConstants` table.
    os_constants::install(vm);
    zip::install(vm)?;
    vmrt::install(vm)?;
    math::install(vm)?;
    native_mem::install_memory(vm)?;
    reflect::install(vm)?;
    fwk_reflect::install(vm)?;
    fwk_res::install(vm)?;
    fwk_sqlite::install(vm)?;
    fwk_debug::install(vm)?;
    // `com.android.org.conscrypt.NativeCrypto`: the security provider surface.
    fwk_conscrypt::install(vm)?;
    fwk_dexfile::install(vm)?;
    fwk_graphics::install(vm)?;
    fwk_input::install(vm)?;
    gfx_paint::install(vm)?;
    gfx_path::install(vm)?;
    gfx_matrix::install(vm)?;
    gfx_text::install(vm)?;
    // After `gfx_text`: the real `android.graphics.fonts.*` natives, and two
    // `Typeface` natives that override the placeholder set.
    fwk_display::install(vm)?;
    fwk_fonts::install(vm)?;
    gfx_node::install(vm)?;
    gfx_canvas::install(vm)?;
    gfx_vector::install(vm)?;
    gfx_bitmap::install(vm)?;
    // `RuntimeShader` (AGSL) / `CanvasProperty` / `RenderNodeAnimator` — the
    // ripple press-feedback surface whose absence aborted interaction
    // traversals (see the module docs).
    gfx_ripple::install(vm)?;
    gfx_hwui::install(vm)?;
    fwk_jni::install(vm)?;
    fwk_m2_filechannel::install(vm)?;
    fwk_m2_chain::install(vm)?;
    fwk_m2_chain2::install(vm)?;
    fwk_m2_chain6::install(vm)?;
    // `java.nio.file`'s `sun.nio.fs` SPI on top of the sandbox VFS.
    fwk_nio_fs::install(vm);
    // `android.animation.PropertyValuesHolder`'s twelve JNI setters (the
    // `StateListAnimator` -> `AnimatorSet` -> `PropertyValuesHolder` path).
    fwk_animation::install(vm)?;
    // `java.lang.reflect.Method.invoke`, the reflection fallback the animation
    // setup uses for property getters.
    fwk_method_invoke::install(vm)?;
    fwk_surface::install(vm)?;
    fwk_vsync::install(vm)?;
    parcel::install(vm)?;
    unsafe_natives::install(vm)?;
    looper::install(vm)?;
    // Register last: the Binder transport re-registers the strong-binder
    // `Parcel` natives (map insert) with its own handle mapping.
    binder::install(vm)?;
    os_props::install(vm);
    os_clock::install(vm);
    os_log::install(vm);
    os_misc::install(vm);
    memintarray::install(vm);
    // Register last: real DEX annotation reflection, the Method/Constructor
    // mirror surface it needs, and `java.lang.reflect.Proxy`. These override
    // the earlier null-returning placeholders (notably `Field.getAnnotationNative`
    // from `fwk_m2_chain2`).
    fwk_annotations::install(vm)?;
    fwk_proxy::install(vm)?;
    Ok(())
}
