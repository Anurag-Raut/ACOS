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
mod fwk_debug;
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
mod gfx_state;
pub mod gfx_text;
pub mod fwk_jni;
mod fwk_m2_filechannel;
pub mod fwk_surface;
mod fwk_reflect;
mod fwk_res;
mod icu;
mod icu_regex;
mod io;
mod io_linux;
pub mod jni_glue;
mod lang;
pub mod looper;
mod math;
mod memintarray;
mod os_clock;
mod parcel;
pub mod os_log;
pub mod os_misc;
pub mod os_props;
mod reflect;
#[path = "unsafe.rs"]
mod unsafe_natives;
mod vmrt;
mod zip;

use crate::error::VmResult;
use crate::vm::Vm;

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
    zip::install(vm)?;
    vmrt::install(vm)?;
    math::install(vm)?;
    reflect::install(vm)?;
    fwk_reflect::install(vm)?;
    fwk_res::install(vm)?;
    fwk_debug::install(vm)?;
    fwk_graphics::install(vm)?;
    fwk_input::install(vm)?;
    gfx_paint::install(vm)?;
    gfx_path::install(vm)?;
    gfx_matrix::install(vm)?;
    gfx_text::install(vm)?;
    gfx_node::install(vm)?;
    gfx_canvas::install(vm)?;
    gfx_bitmap::install(vm)?;
    gfx_hwui::install(vm)?;
    fwk_jni::install(vm)?;
    fwk_m2_filechannel::install(vm)?;
    fwk_surface::install(vm)?;
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
    Ok(())
}
