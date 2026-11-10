//! `sun.nio.ch.FileChannelImpl` and `sun.nio.ch.NativeThread` natives (M2).
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
//!      -> FileChannelImpl.size()   -> FileDispatcherImpl.size0(fd)
//!      -> FileChannelImpl.map0(prot, position, size)
//! ```
//!
//! ACOS models no raw host pointers, so `map0` copies the VFS file contents
//! into a [`super::native_mem`] region and returns that region's synthetic
//! address. The returned `DirectByteBuffer` then reads through
//! `libcore.io.Memory`/`Unsafe` long-address natives. `MAP_RO` maps are
//! read-only; `MAP_RW`/`MAP_PV` maps are private copies (writes are not written
//! back to the descriptor — a declared divergence from a live `mmap`).

use crate::class::NativeCtx;
use crate::error::{VmError, VmResult};
use crate::heap::{Handle, NULL};
use crate::vm::Vm;

use super::native_mem::{self, MemError};

pub fn install(vm: &mut Vm) -> VmResult<()> {
    let c = "Lsun/nio/ch/FileChannelImpl;";
    vm.register_native(c, "initIDs", "()J", init_ids);
    vm.register_native(c, "map0", "(IJJ)J", map0);
    vm.register_native(c, "unmap0", "(JJ)I", unmap0);
    vm.register_native(
        "Lsun/nio/ch/FileDispatcherImpl;",
        "size0",
        "(Ljava/io/FileDescriptor;)J",
        size0,
    );
    // `NativeThreadSet.add/remove` brackets every blocking file operation and
    // needs an opaque per-thread value; the green-thread id is that value.
    vm.register_native(
        "Lsun/nio/ch/NativeThread;",
        "current",
        "()J",
        native_thread_current,
    );
    vm.register_native(
        "Lsun/nio/ch/NativeThread;",
        "signal",
        "(J)V",
        native_thread_signal,
    );
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

/// Read the integer descriptor from a `java.io.FileDescriptor` object.
fn fd_descriptor(vm: &Vm, fd_obj: Handle) -> Option<i32> {
    if fd_obj == NULL {
        return None;
    }
    vm.get_instance_field_by_name(fd_obj, "descriptor")
        .or_else(|| vm.get_instance_field_by_name(fd_obj, "fd"))
        .map(|v| v as i32)
}

/// `sun.nio.ch.FileDispatcherImpl.size0(FileDescriptor fd) -> long`
fn size0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fd_obj = vm.arg_ref(ctx, 0);
    let Some(fd) = fd_descriptor(vm, fd_obj) else {
        return Err(io_error(vm, "size0: invalid FileDescriptor"));
    };
    match vm.vfs.fstat(fd) {
        Ok(st) => {
            vm.ret_i64(ctx, st.size as i64);
            Ok(())
        }
        Err(errno) => Err(io_error(
            vm,
            &format!("size0 failed: {}", crate::vfs::errno_name(errno)),
        )),
    }
}

/// `private native long map0(int prot, long position, long size)`
///
/// `prot` is one of the `FileChannelImpl` constants `MAP_RO = 0`,
/// `MAP_RW = 1`, `MAP_PV = 2`. The file bytes from `position` are copied into a
/// fresh region; bytes past EOF stay zero, matching `mmap`'s zero-fill.
fn map0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let prot = vm.arg_i32(ctx, 0);
    let position = vm.arg_i64(ctx, 1);
    let size = vm.arg_i64(ctx, 2);
    if size < 0 || position < 0 {
        return Err(io_error(vm, "map0: negative position or size"));
    }
    let fd_obj = vm
        .get_instance_field_by_name(this, "fd")
        .map(|v| v as Handle)
        .unwrap_or(NULL);
    let Some(fd) = fd_descriptor(vm, fd_obj) else {
        return Err(io_error(vm, "map0: invalid FileDescriptor"));
    };
    let len = size as usize;
    if len > native_mem::cap_bytes() {
        return Err(MemError::CapExceeded.throw(vm));
    }
    let mut buf: Vec<u8> = Vec::new();
    if buf.try_reserve_exact(len).is_err() {
        return Err(MemError::CapExceeded.throw(vm));
    }
    buf.resize(len, 0);
    if len > 0 {
        // A short read is not an error: `mmap` zero-fills past EOF. A hard
        // errno (bad fd, permission) is reported as an IOException.
        if let Err(errno) = vm.vfs.pread(fd, &mut buf, position as u64) {
            return Err(io_error(
                vm,
                &format!("map0 failed: {}", crate::vfs::errno_name(errno)),
            ));
        }
    }
    let readonly = prot == 0; // MAP_RO
    let addr = native_mem::alloc_with(buf, readonly).map_err(|e| e.throw(vm))?;
    vm.ret_i64(ctx, addr);
    Ok(())
}

/// `private static native int unmap0(long address, long size)`
fn unmap0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let addr = vm.arg_i64(ctx, 0);
    native_mem::free(addr);
    vm.ret_i32(ctx, 0);
    Ok(())
}

/// `sun.nio.ch.NativeThread.current() -> long`
///
/// ART returns the calling `pthread_t`, or `-1` when the thread is not a
/// Java thread. ACOS returns the green-thread index + 1 so the value is nonzero
/// and stable for the duration of a blocking operation.
fn native_thread_current(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i64(ctx, vm.current_thread as i64 + 1);
    Ok(())
}

/// `sun.nio.ch.NativeThread.signal(long) -> void`
///
/// Green threads park cooperatively, so there is no native thread to signal.
fn native_thread_signal(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Ok(())
}

fn io_error(vm: &mut Vm, msg: &str) -> VmError {
    vm.throw_new("Ljava/io/IOException;", Some(msg))
}
