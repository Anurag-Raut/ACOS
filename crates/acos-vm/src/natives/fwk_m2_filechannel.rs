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
    // `sun.nio.ch.IOUtil` static query surface (W168-A): `IOV_MAX` gates the
    // readv/writev path and is read from `IOUtil.<clinit>`, so the default
    // filesystem cannot open a channel without it.
    let u = "Lsun/nio/ch/IOUtil;";
    vm.register_native(u, "iovMax", "()I", iov_max);
    vm.register_native(u, "fdLimit", "()I", fd_limit);
    vm.register_native(u, "fdVal", "(Ljava/io/FileDescriptor;)I", fd_val);
    vm.register_native(u, "setfdVal", "(Ljava/io/FileDescriptor;I)V", set_fd_val);
    vm.register_native(
        u,
        "configureBlocking",
        "(Ljava/io/FileDescriptor;Z)V",
        configure_blocking,
    );
    vm.register_native(u, "makePipe", "(Z)J", make_pipe);
    vm.register_native(u, "drain", "(I)Z", drain);
    vm.register_native(u, "randomBytes", "([B)Z", random_bytes);
    // The remaining `sun.nio.ch.FileDispatcherImpl` operations.
    let d = "Lsun/nio/ch/FileDispatcherImpl;";
    vm.register_native(d, "read0", "(Ljava/io/FileDescriptor;JI)I", fd_read0);
    vm.register_native(d, "write0", "(Ljava/io/FileDescriptor;JI)I", fd_write0);
    vm.register_native(
        d,
        "pread0",
        "(Ljava/io/FileDescriptor;JIJ)I",
        fd_pread0,
    );
    vm.register_native(
        d,
        "pwrite0",
        "(Ljava/io/FileDescriptor;JIJ)I",
        fd_pwrite0,
    );
    vm.register_native(d, "readv0", "(Ljava/io/FileDescriptor;JI)J", fd_readv0);
    vm.register_native(d, "writev0", "(Ljava/io/FileDescriptor;JI)J", fd_writev0);
    vm.register_native(d, "close0", "(Ljava/io/FileDescriptor;)V", fd_close0);
    vm.register_native(d, "closeIntFD", "(I)V", fd_close_int);
    vm.register_native(d, "force0", "(Ljava/io/FileDescriptor;Z)I", fd_force0);
    vm.register_native(
        d,
        "truncate0",
        "(Ljava/io/FileDescriptor;J)I",
        fd_truncate0,
    );
    vm.register_native(d, "preClose0", "(Ljava/io/FileDescriptor;)V", fd_noop_obj);
    vm.register_native(
        d,
        "release0",
        "(Ljava/io/FileDescriptor;JJ)V",
        fd_release0,
    );
    vm.register_native(
        d,
        "lock0",
        "(Ljava/io/FileDescriptor;ZJJZ)I",
        fd_lock0,
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
    // Record where the bytes came from so a snapshot can re-read the file on
    // restore instead of embedding the mapping (W160-A). Only a *read-only*
    // map over a real host file gives a safe source: a writable map is a
    // private copy whose in-place mutations are not in the file, so it must
    // embed. Synthetic descriptors embed their bytes too.
    let source = if readonly {
        vm.vfs
            .fd_file_guest_path(fd)
            .map(str::to_string)
            .and_then(|path| {
                vm.vfs.fstat(fd).ok().map(|st| {
                    native_mem::RegionSource::capture(
                        path,
                        position as u64,
                        &buf,
                        st.size,
                        st.mtime,
                    )
                })
            })
    } else {
        None
    };
    let addr = match source {
        Some(src) => native_mem::alloc_mapped(buf, readonly, src),
        None => native_mem::alloc_with(buf, readonly),
    }
    .map_err(|e| e.throw(vm))?;
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

// ---------------------------------------------------------------------------
// sun.nio.ch.IOUtil
// ---------------------------------------------------------------------------

/// `IOUtil.iovMax() -> int`: the maximum iovec count for `readv`/`writev`.
/// Linux's `IOV_MAX` is 1024, and ART reports it from `sysconf(_SC_IOV_MAX)`.
fn iov_max(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i32(ctx, 1024);
    Ok(())
}

/// `IOUtil.fdLimit() -> int`: the soft `RLIMIT_NOFILE`, or `-1` when
/// unlimited. ACOS's descriptor table has its own ceiling; report the same
/// order of magnitude so the JDK sizes its thread-local buffers sanely.
fn fd_limit(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i32(ctx, 1024);
    Ok(())
}

/// `IOUtil.fdVal(FileDescriptor) -> int`
fn fd_val(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fd_obj = vm.arg_ref(ctx, 0);
    let fd = fd_descriptor(vm, fd_obj).unwrap_or(-1);
    vm.ret_i32(ctx, fd);
    Ok(())
}

/// `IOUtil.setfdVal(FileDescriptor, int)`
fn set_fd_val(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fd_obj = vm.arg_ref(ctx, 0);
    let fd = vm.arg_i32(ctx, 1);
    if fd_obj != NULL {
        vm.set_instance_field_by_name(fd_obj, "fd", fd as u32);
        vm.set_instance_field_by_name(fd_obj, "descriptor", fd as u32);
    }
    Ok(())
}

/// `IOUtil.configureBlocking(FileDescriptor, boolean)`
fn configure_blocking(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fd_obj = vm.arg_ref(ctx, 0);
    let blocking = vm.arg_i32(ctx, 1) != 0;
    let Some(fd) = fd_descriptor(vm, fd_obj) else {
        return Ok(());
    };
    // `F_GETFL` then set/clear `O_NONBLOCK`. ACOS I/O is synchronous, so this
    // only records the flag for observability.
    let flags = vm.vfs.fcntl(fd, 3, 0).unwrap_or(0);
    let new = if blocking {
        flags & !0o4000
    } else {
        flags | 0o4000
    };
    let _ = vm.vfs.fcntl(fd, 4, new);
    Ok(())
}

/// `IOUtil.makePipe(boolean blocking) -> long`: a pipe pair packed as
/// `(writeFd << 32) | readFd`, exactly as ART returns it.
fn make_pipe(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let blocking = vm.arg_i32(ctx, 0) != 0;
    let flags = if blocking { 0 } else { 0o4000 };
    match vm.vfs.pipe2(flags) {
        Ok((r, w)) => {
            let packed = ((w as i64) << 32) | (r as i64 & 0xffff_ffff);
            vm.ret_i64(ctx, packed);
            Ok(())
        }
        Err(e) => Err(io_error(
            vm,
            &format!("makePipe failed: {}", crate::vfs::errno_name(e)),
        )),
    }
}

/// `IOUtil.drain(int fd) -> boolean`: drain a nonblocking stream. ACOS's
/// `Vfs::poll` is the only readiness primitive; a socket here is a local
/// `AF_UNIX` pair, so there is nothing to force out.
fn drain(_vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    _vm.ret_bool(ctx, true);
    Ok(())
}

/// `IOUtil.randomBytes(byte[]) -> boolean`: fill from the host entropy pool.
fn random_bytes(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let arr = vm.arg_ref(ctx, 0);
    let len = match vm.heap.get(arr) {
        Some(crate::heap::Object::Array(a)) => a.data.len(),
        _ => 0,
    };
    let mut buf = vec![0u8; len];
    let ok = std::fs::File::open("/dev/urandom")
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut buf))
        .is_ok();
    if ok {
        if let Some(crate::heap::Object::Array(a)) = vm.heap.get_mut(arr) {
            if let crate::heap::ArrayData::Byte(v) = &mut a.data {
                for (slot, b) in v.iter_mut().zip(buf.iter()) {
                    *slot = *b as i8;
                }
            }
        }
    }
    vm.ret_bool(ctx, ok);
    Ok(())
}

// ---------------------------------------------------------------------------
// sun.nio.ch.FileDispatcherImpl
// ---------------------------------------------------------------------------

fn fd_read0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    fd_transfer(vm, ctx, "read0", None, false)
}

fn fd_write0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    fd_transfer(vm, ctx, "write0", None, true)
}

fn fd_pread0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let pos = vm.arg_i64(ctx, 3);
    fd_transfer(vm, ctx, "pread0", Some(pos as u64), false)
}

fn fd_pwrite0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let pos = vm.arg_i64(ctx, 3);
    fd_transfer(vm, ctx, "pwrite0", Some(pos as u64), true)
}

/// Shared body of `read0`/`write0`/`pread0`/`pwrite0`: move `len` bytes
/// between the descriptor and a synthetic native-memory address.
fn fd_transfer(
    vm: &mut Vm,
    ctx: NativeCtx,
    function: &str,
    position: Option<u64>,
    write: bool,
) -> VmResult<()> {
    let fd_obj = vm.arg_ref(ctx, 0);
    let addr = vm.arg_i64(ctx, 1);
    let len = vm.arg_i32(ctx, 2).max(0) as usize;
    let Some(fd) = fd_descriptor(vm, fd_obj) else {
        return Err(io_error(vm, &format!("{function}: invalid FileDescriptor")));
    };
    let err = |vm: &mut Vm, e: i32| {
        io_error(
            vm,
            &format!("{function} failed: {}", crate::vfs::errno_name(e)),
        )
    };
    if write {
        let data = native_mem::read_bytes_at(vm, addr, len)?;
        let n = match position {
            Some(off) => vm.vfs.pwrite(fd, &data, off).map_err(|e| err(vm, e))?,
            None => vm.vfs.write(fd, &data).map_err(|e| err(vm, e))?,
        };
        vm.ret_i32(ctx, n as i32);
    } else {
        let mut buf = vec![0u8; len];
        let n = match position {
            Some(off) => vm.vfs.pread(fd, &mut buf, off).map_err(|e| err(vm, e))?,
            None => vm.vfs.read(fd, &mut buf).map_err(|e| err(vm, e))?,
        };
        native_mem::write_bytes_at(vm, addr, &buf[..n])?;
        vm.ret_i32(ctx, n as i32);
    }
    Ok(())
}

/// Read the `address`/`length` pairs of a native `struct iovec` array.
fn read_iovecs(vm: &mut Vm, addr: i64, count: usize) -> VmResult<Vec<(i64, usize)>> {
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let base = native_mem::read_bytes_at(vm, addr + (i * 16) as i64, 8)?;
        let len = native_mem::read_bytes_at(vm, addr + (i * 16 + 8) as i64, 8)?;
        let base = i64::from_le_bytes(base.try_into().unwrap());
        let len = u64::from_le_bytes(len.try_into().unwrap()) as usize;
        out.push((base, len));
    }
    Ok(out)
}

fn fd_readv0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fd_obj = vm.arg_ref(ctx, 0);
    let addr = vm.arg_i64(ctx, 1);
    let count = vm.arg_i32(ctx, 2).max(0) as usize;
    let Some(fd) = fd_descriptor(vm, fd_obj) else {
        return Err(io_error(vm, "readv0: invalid FileDescriptor"));
    };
    let iovecs = read_iovecs(vm, addr, count)?;
    let mut total = 0usize;
    for (base, len) in iovecs {
        if total > 0 && len == 0 {
            break;
        }
        let mut buf = vec![0u8; len];
        let n = vm.vfs.read(fd, &mut buf).map_err(|e| {
            io_error(vm, &format!("readv0 failed: {}", crate::vfs::errno_name(e)))
        })?;
        native_mem::write_bytes_at(vm, base, &buf[..n])?;
        total += n;
        if n < len {
            break;
        }
    }
    vm.ret_i64(ctx, total as i64);
    Ok(())
}

fn fd_writev0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fd_obj = vm.arg_ref(ctx, 0);
    let addr = vm.arg_i64(ctx, 1);
    let count = vm.arg_i32(ctx, 2).max(0) as usize;
    let Some(fd) = fd_descriptor(vm, fd_obj) else {
        return Err(io_error(vm, "writev0: invalid FileDescriptor"));
    };
    let iovecs = read_iovecs(vm, addr, count)?;
    let mut total = 0usize;
    for (base, len) in iovecs {
        if total > 0 && len == 0 {
            break;
        }
        let data = native_mem::read_bytes_at(vm, base, len)?;
        let n = vm.vfs.write(fd, &data).map_err(|e| {
            io_error(vm, &format!("writev0 failed: {}", crate::vfs::errno_name(e)))
        })?;
        total += n;
        if n < len {
            break;
        }
    }
    vm.ret_i64(ctx, total as i64);
    Ok(())
}

fn fd_close0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fd_obj = vm.arg_ref(ctx, 0);
    if let Some(fd) = fd_descriptor(vm, fd_obj) {
        if fd > 2 {
            let _ = vm.vfs.close(fd);
        }
        vm.set_instance_field_by_name(fd_obj, "fd", (-1i32) as u32);
        vm.set_instance_field_by_name(fd_obj, "descriptor", (-1i32) as u32);
    }
    Ok(())
}

fn fd_close_int(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fd = vm.arg_i32(ctx, 0);
    if fd > 2 {
        let _ = vm.vfs.close(fd);
    }
    Ok(())
}

fn fd_force0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fd_obj = vm.arg_ref(ctx, 0);
    let meta = vm.arg_i32(ctx, 1) != 0;
    if let Some(fd) = fd_descriptor(vm, fd_obj) {
        vm.vfs.fsync(fd, !meta).map_err(|e| {
            io_error(vm, &format!("force0 failed: {}", crate::vfs::errno_name(e)))
        })?;
    }
    vm.ret_i32(ctx, 0);
    Ok(())
}

fn fd_truncate0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fd_obj = vm.arg_ref(ctx, 0);
    let size = vm.arg_i64(ctx, 1).max(0) as u64;
    let Some(fd) = fd_descriptor(vm, fd_obj) else {
        return Err(io_error(vm, "truncate0: invalid FileDescriptor"));
    };
    vm.vfs.ftruncate(fd, size).map_err(|e| {
        io_error(vm, &format!("truncate0 failed: {}", crate::vfs::errno_name(e)))
    })?;
    vm.ret_i32(ctx, 0);
    Ok(())
}

fn fd_noop_obj(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Ok(())
}

fn fd_release0(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    // The synthetic mmap region is freed by `unmap0`; nothing to release here.
    Ok(())
}

fn fd_lock0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    // ACOS runs a single guest process, so advisory byte-range locks cannot
    // conflict within the VM; report success without a cross-process table.
    let _ = vm.arg_i32(ctx, 1);
    vm.ret_i32(ctx, 0);
    Ok(())
}
