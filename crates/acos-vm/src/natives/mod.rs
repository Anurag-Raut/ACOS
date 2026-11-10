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

mod character;
mod icu;
mod io;
mod lang;
pub mod looper;
mod math;
mod os_clock;
mod parcel;
pub mod os_log;
pub mod os_misc;
pub mod os_props;
mod reflect;
#[path = "unsafe.rs"]
mod unsafe_natives;
mod vmrt;

use crate::error::VmResult;
use crate::vm::Vm;

/// Register every true native the M1 console tier needs.
pub(crate) fn install(vm: &mut Vm) -> VmResult<()> {
    lang::install(vm)?;
    character::install(vm)?;
    icu::install(vm)?;
    io::install(vm)?;
    vmrt::install(vm)?;
    math::install(vm)?;
    reflect::install(vm)?;
    parcel::install(vm)?;
    unsafe_natives::install(vm)?;
    looper::install(vm)?;
    os_props::install(vm);
    os_clock::install(vm);
    os_log::install(vm);
    os_misc::install(vm);
    Ok(())
}
