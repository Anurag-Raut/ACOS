//! `sun.nio.ch.FileChannelImpl` natives (M2: ICU data loading).
//!
//! `android.icu.impl.ICUBinary` memory-maps the ICU `.dat` files discovered on
//! `AndroidDataFiles.generateIcuDataPath()` to read resource bundles such as
//! `likelySubtags` (which `android.graphics.Typeface.<clinit>` needs). The Java
//! path is
//!
//! ```text
//! ICUBinary.mapFile(File)
//!   -> FileInputStream.getChannel().map(READ_ONLY, 0, size)
//!      -> FileChannelImpl.<clinit> -> initIDs()
//!      -> FileChannelImpl.map0(mode, position, size)   [native mmap]
//! ```
//!
//! ACOS's VM is `#![forbid(unsafe_code)]` and models no raw process addresses,
//! so `map0` cannot return a real `mmap` pointer yet. This module supplies the
//! cheap, side-effect-free member (`initIDs`); `map0`/`unmap0` stay honest
//! structured `UNSUPPORTED_JAVA_API` until a Rust-owned read-only direct-buffer
//! substrate exists.

use crate::class::NativeCtx;
use crate::error::VmResult;
use crate::vm::Vm;

pub fn install(vm: &mut Vm) -> VmResult<()> {
    let c = "Lsun/nio/ch/FileChannelImpl;";
    vm.register_native(c, "initIDs", "()J", init_ids);
    Ok(())
}

/// `private static native long initIDs();`
///
/// The returned `long` is stored in the static `allocationGranularity`. ART's
/// implementation caches JNI field IDs and returns the VM page size. ACOS has
/// no off-heap mapping, but the framework aligns its `map0` offset by this
/// value, so report the 4096-byte page size every supported Android ABI uses.
fn init_ids(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i64(ctx, 4096);
    Ok(())
}
