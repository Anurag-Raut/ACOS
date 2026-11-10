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
//! `math`, `reflect`.

mod character;
mod icu;
mod io;
mod lang;
mod math;
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
    unsafe_natives::install(vm)?;
    Ok(())
}
