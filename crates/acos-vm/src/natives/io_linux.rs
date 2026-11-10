//! `libcore.io.Linux` file I/O and `java.io.UnixFileSystem` natives.
//!
//! Every path argument is resolved through the per-process [`Vfs`]
//! (`crate::vfs`); the guest never sees a raw host path. Descriptors are
//! real host descriptors kept in the VFS table, so offsets, flags and `dup`
//! semantics are the kernel's. Errors are raised as the real
//! `android.system.ErrnoException(functionName, errno)`, whose `getMessage()`
//! is computed by the real Android bytecode (`function failed: ENAME (text)`).
//!
//! The std streams (fd 0/1/2) are not in the VFS table: writes to 1/2 append to
//! the VM's captured `out_buf`/`err_buf` (byte-identical fixture comparison),
//! and fd 0 reads the host stdin.
//!
//! `android.system.ErrnoException` replaces the older `libcore.io.ErrnoException`
//! in Android 14; the VM builds the constructor's two fields directly because a
//! native method cannot run a Java constructor for the exception it is about to
//! throw.

use crate::class::NativeCtx;
use crate::error::{VmError, VmResult};
use crate::heap::{ArrayData, ArrayObj, Handle, Object, NULL};
use crate::vfs::{self, Stat, StatVfs};
use crate::vm::{InvokeKind, Vm};

/// Register the file-I/O natives. Called from [`crate::natives::io::install`].
pub(crate) fn install(vm: &mut Vm) {
    let c = "Llibcore/io/Linux;";
    vm.register_native(
        c,
        "open",
        "(Ljava/lang/String;II)Ljava/io/FileDescriptor;",
        linux_open,
    );
    vm.register_native(c, "close", "(Ljava/io/FileDescriptor;)V", linux_close);
    vm.register_native(
        c,
        "readBytes",
        "(Ljava/io/FileDescriptor;Ljava/lang/Object;II)I",
        linux_read_bytes,
    );
    vm.register_native(
        c,
        "preadBytes",
        "(Ljava/io/FileDescriptor;Ljava/lang/Object;IIJ)I",
        linux_pread_bytes,
    );
    vm.register_native(
        c,
        "writeBytes",
        "(Ljava/io/FileDescriptor;Ljava/lang/Object;II)I",
        linux_write_bytes,
    );
    vm.register_native(
        c,
        "pwriteBytes",
        "(Ljava/io/FileDescriptor;Ljava/lang/Object;IIJ)I",
        linux_pwrite_bytes,
    );
    vm.register_native(c, "lseek", "(Ljava/io/FileDescriptor;JI)J", linux_lseek);
    vm.register_native(
        c,
        "fstat",
        "(Ljava/io/FileDescriptor;)Landroid/system/StructStat;",
        linux_fstat,
    );
    vm.register_native(
        c,
        "stat",
        "(Ljava/lang/String;)Landroid/system/StructStat;",
        linux_stat,
    );
    vm.register_native(
        c,
        "lstat",
        "(Ljava/lang/String;)Landroid/system/StructStat;",
        linux_lstat,
    );
    vm.register_native(c, "access", "(Ljava/lang/String;I)Z", linux_access);
    vm.register_native(
        c,
        "dup",
        "(Ljava/io/FileDescriptor;)Ljava/io/FileDescriptor;",
        linux_dup,
    );
    vm.register_native(
        c,
        "dup2",
        "(Ljava/io/FileDescriptor;I)Ljava/io/FileDescriptor;",
        linux_dup2,
    );
    vm.register_native(
        c,
        "fcntlInt",
        "(Ljava/io/FileDescriptor;II)I",
        linux_fcntl_int,
    );
    vm.register_native(
        c,
        "fcntlVoid",
        "(Ljava/io/FileDescriptor;I)I",
        linux_fcntl_void,
    );
    vm.register_native(
        c,
        "ftruncate",
        "(Ljava/io/FileDescriptor;J)V",
        linux_ftruncate,
    );
    vm.register_native(c, "fsync", "(Ljava/io/FileDescriptor;)V", linux_fsync);
    vm.register_native(
        c,
        "fdatasync",
        "(Ljava/io/FileDescriptor;)V",
        linux_fdatasync,
    );
    vm.register_native(c, "fchmod", "(Ljava/io/FileDescriptor;I)V", linux_fchmod);
    vm.register_native(c, "fchown", "(Ljava/io/FileDescriptor;II)V", linux_fchown);
    vm.register_native(c, "mkdir", "(Ljava/lang/String;I)V", linux_mkdir);
    vm.register_native(
        c,
        "rename",
        "(Ljava/lang/String;Ljava/lang/String;)V",
        linux_rename,
    );
    vm.register_native(c, "remove", "(Ljava/lang/String;)V", linux_remove);
    vm.register_native(
        c,
        "readlink",
        "(Ljava/lang/String;)Ljava/lang/String;",
        linux_readlink,
    );
    vm.register_native(
        c,
        "realpath",
        "(Ljava/lang/String;)Ljava/lang/String;",
        linux_realpath,
    );
    vm.register_native(c, "chmod", "(Ljava/lang/String;I)V", linux_chmod);
    vm.register_native(c, "chown", "(Ljava/lang/String;II)V", linux_chown_path);
    vm.register_native(c, "lchown", "(Ljava/lang/String;II)V", linux_chown_path);
    vm.register_native(
        c,
        "link",
        "(Ljava/lang/String;Ljava/lang/String;)V",
        linux_link,
    );
    vm.register_native(
        c,
        "symlink",
        "(Ljava/lang/String;Ljava/lang/String;)V",
        linux_symlink,
    );
    vm.register_native(
        c,
        "statvfs",
        "(Ljava/lang/String;)Landroid/system/StructStatVfs;",
        linux_statvfs,
    );
    vm.register_native(
        c,
        "fstatvfs",
        "(Ljava/io/FileDescriptor;)Landroid/system/StructStatVfs;",
        linux_fstatvfs,
    );
    vm.register_native(c, "pipe2", "(I)[Ljava/io/FileDescriptor;", linux_pipe2);
    vm.register_native(c, "poll", "([Landroid/system/StructPollfd;I)I", linux_poll);

    // `java.io.FileDescriptor`'s two natives (core-oj).
    vm.register_native("Ljava/io/FileDescriptor;", "isSocket", "(I)Z", fd_is_socket);
    vm.register_native("Ljava/io/FileDescriptor;", "sync", "()V", fd_sync);

    // `FileInputStream` owns two natives for buffered-less queries.
    let fis = "Ljava/io/FileInputStream;";
    vm.register_native(fis, "available0", "()I", fis_available0);
    vm.register_native(fis, "skip0", "(J)J", fis_skip0);

    // Closing a descriptor interrupts threads blocked in that descriptor's
    // I/O. ACOS's I/O is synchronous and has no such OS waiters, so there is
    // nothing to signal; the call is a declared no-op.
    vm.register_native(
        "Llibcore/io/AsynchronousCloseMonitor;",
        "signalBlockedThreads",
        "(Ljava/io/FileDescriptor;)V",
        noop,
    );

    // `java.io.UnixFileSystem` (the default `File` backend).
    unix_file_system(vm);
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// Build an `android.system.ErrnoException(functionName, errno)`.
fn errno_exception(vm: &mut Vm, function: &str, errno: i32) -> VmError {
    let desc = "Landroid/system/ErrnoException;";
    match vm.new_throwable(desc, None) {
        Ok(h) => {
            vm.native_roots.push(h);
            let result = (|| -> VmResult<()> {
                let name = vm.new_string(function)?;
                vm.set_instance_field_by_name(h, "functionName", name);
                vm.set_instance_field_by_name(h, "errno", errno as u32);
                Ok(())
            })();
            vm.native_roots.pop();
            let _ = result;
            VmError::JavaException(h)
        }
        Err(_) => vm.throw_new(
            "Ljava/io/IOException;",
            Some(&format!(
                "{function} failed: {} ({})",
                vfs::errno_name(errno),
                vfs::strerror(errno)
            )),
        ),
    }
}

/// The integer fd held by a `java.io.FileDescriptor`.
fn fd_of(vm: &Vm, fd_obj: Handle) -> Option<i32> {
    if fd_obj == NULL {
        return None;
    }
    vm.get_instance_field_by_name(fd_obj, "descriptor")
        .or_else(|| vm.get_instance_field_by_name(fd_obj, "fd"))
        .map(|v| v as i32)
}

/// Wrap a host descriptor in a real `java.io.FileDescriptor`.
///
/// The object is created by running `FileDescriptor.<init>()V`, exactly as the
/// JNI helper function does, so `releaseLock` and `ownerId` are initialised.
fn make_file_descriptor(vm: &mut Vm, raw: i32) -> VmResult<Handle> {
    let cid = vm.load_class("Ljava/io/FileDescriptor;")?;
    let obj = vm.alloc_instance(cid)?;
    vm.native_roots.push(obj);
    let result = (|| -> VmResult<()> {
        let ctor = vm.find_method(cid, "<init>", "()V", InvokeKind::Direct)?;
        crate::interp::call_method(vm, ctor, obj, &[])?;
        vm.set_instance_field_by_name(obj, "descriptor", raw as u32);
        Ok(())
    })();
    vm.native_roots.pop();
    result?;
    Ok(obj)
}

fn noop(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Ok(())
}

fn fd_is_socket(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fd = vm.arg_i32(ctx, 0);
    vm.ret_bool(ctx, vm.vfs.is_socket(fd));
    Ok(())
}

fn fd_sync(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let Some(this) = vm.ctx_this(ctx) else {
        return Ok(());
    };
    let fd_obj = vm
        .get_instance_field_by_name(this, "descriptor")
        .unwrap_or(NULL);
    let Some(fd) = fd_of(vm, fd_obj) else {
        return Ok(());
    };
    if fd <= 2 {
        return Ok(());
    }
    vm.vfs
        .fsync(fd, false)
        .map_err(|e| errno_exception(vm, "sync", e))?;
    Ok(())
}

fn string_arg(vm: &Vm, ctx: NativeCtx, n: usize) -> String {
    vm.string_to_rust(vm.arg_ref(ctx, n)).unwrap_or_default()
}

/// Copy a `byte[]` argument out of the heap.
fn byte_array(vm: &Vm, h: Handle) -> Option<Vec<u8>> {
    match vm.heap.get(h) {
        Some(Object::Array(ArrayObj {
            data: ArrayData::Byte(v),
            ..
        })) => Some(v.iter().map(|&b| b as u8).collect()),
        _ => None,
    }
}

/// Write `data` into a `byte[]` starting at `off`.
fn write_byte_array(vm: &mut Vm, h: Handle, off: usize, data: &[u8]) {
    if let Some(Object::Array(ArrayObj {
        data: ArrayData::Byte(v),
        ..
    })) = vm.heap.get_mut(h)
    {
        for (k, b) in data.iter().enumerate() {
            if off + k < v.len() {
                v[off + k] = *b as i8;
            }
        }
    }
}

fn make_stat(vm: &mut Vm, st: &Stat) -> VmResult<Handle> {
    let cid = vm.load_class("Landroid/system/StructStat;")?;
    let obj = vm.alloc_instance(cid)?;
    vm.native_roots.push(obj);
    let result = (|| -> VmResult<()> {
        let ctor = vm.find_method(cid, "<init>", "(JJIJIIJJJJJJJ)V", InvokeKind::Direct)?;
        let args = [
            st.dev,
            st.ino,
            u64::from(st.mode),
            st.nlink,
            u64::from(st.uid),
            u64::from(st.gid),
            st.rdev,
            st.size,
            st.blksize,
            st.blocks,
            st.atime as u64,
            st.mtime as u64,
            st.ctime as u64,
        ];
        crate::interp::call_method(vm, ctor, obj, &args)?;
        Ok(())
    })();
    vm.native_roots.pop();
    result?;
    Ok(obj)
}

fn make_statvfs(vm: &mut Vm, st: &StatVfs) -> VmResult<Handle> {
    let cid = vm.load_class("Landroid/system/StructStatVfs;")?;
    let obj = vm.alloc_instance(cid)?;
    vm.native_roots.push(obj);
    let result = (|| -> VmResult<()> {
        let ctor = vm.find_method(cid, "<init>", "(JJJJJJJJJJJ)V", InvokeKind::Direct)?;
        let args = [
            st.bsize, st.frsize, st.blocks, st.bfree, st.bavail, st.files, st.ffree, st.favail,
            st.fsid, st.flag, st.namemax,
        ];
        crate::interp::call_method(vm, ctor, obj, &args)?;
        Ok(())
    })();
    vm.native_roots.pop();
    result?;
    Ok(obj)
}

// ---------------------------------------------------------------------------
// descriptor lifecycle
// ---------------------------------------------------------------------------

fn linux_open(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let path = string_arg(vm, ctx, 0);
    let flags = vm.arg_i32(ctx, 1);
    let mode = vm.arg_i32(ctx, 2);
    let fd = vm
        .vfs
        .open(&path, flags, mode)
        .map_err(|e| errno_exception(vm, "open", e))?;
    let obj = make_file_descriptor(vm, fd)?;
    vm.ret_ref(ctx, obj);
    Ok(())
}

fn linux_close(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fd_obj = vm.arg_ref(ctx, 0);
    let Some(fd) = fd_of(vm, fd_obj) else {
        return Err(errno_exception(vm, "close", vfs::EBADF));
    };
    if fd <= 2 {
        // Never close the host's std streams.
        return Ok(());
    }
    vm.vfs
        .close(fd)
        .map_err(|e| errno_exception(vm, "close", e))?;
    vm.set_instance_field_by_name(fd_obj, "descriptor", (-1i32) as u32);
    Ok(())
}

fn linux_dup(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fd_obj = vm.arg_ref(ctx, 0);
    let fd = fd_of(vm, fd_obj).ok_or_else(|| errno_exception(vm, "dup", vfs::EBADF))?;
    let new_fd = vm.vfs.dup(fd).map_err(|e| errno_exception(vm, "dup", e))?;
    let obj = make_file_descriptor(vm, new_fd)?;
    vm.ret_ref(ctx, obj);
    Ok(())
}

fn linux_dup2(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fd_obj = vm.arg_ref(ctx, 0);
    let new_fd = vm.arg_i32(ctx, 1);
    let fd = fd_of(vm, fd_obj).ok_or_else(|| errno_exception(vm, "dup2", vfs::EBADF))?;
    let got = vm
        .vfs
        .dup2(fd, new_fd)
        .map_err(|e| errno_exception(vm, "dup2", e))?;
    let obj = make_file_descriptor(vm, got)?;
    vm.ret_ref(ctx, obj);
    Ok(())
}

// ---------------------------------------------------------------------------
// byte I/O
// ---------------------------------------------------------------------------

/// Common `read`/`pread` argument extraction.
fn read_into(vm: &mut Vm, ctx: NativeCtx, function: &str, position: Option<u64>) -> VmResult<()> {
    let fd_obj = vm.arg_ref(ctx, 0);
    let target = vm.arg_ref(ctx, 1);
    let off = vm.arg_i32(ctx, 2).max(0) as usize;
    let count = vm.arg_i32(ctx, 3).max(0) as usize;
    let fd = fd_of(vm, fd_obj).ok_or_else(|| errno_exception(vm, function, vfs::EBADF))?;
    if fd == 0 {
        let mut buf = vec![0u8; count];
        use std::io::Read;
        let n = std::io::stdin().read(&mut buf).unwrap_or(0);
        write_byte_array(vm, target, off, &buf[..n]);
        vm.ret_i32(ctx, n as i32);
        return Ok(());
    }
    let mut buf = vec![0u8; count];
    let n = match position {
        Some(pos) => vm.vfs.pread(fd, &mut buf, pos),
        None => vm.vfs.read(fd, &mut buf),
    }
    .map_err(|e| errno_exception(vm, function, e))?;
    write_byte_array(vm, target, off, &buf[..n]);
    vm.ret_i32(ctx, n as i32);
    Ok(())
}

fn write_from(vm: &mut Vm, ctx: NativeCtx, function: &str, position: Option<u64>) -> VmResult<()> {
    let fd_obj = vm.arg_ref(ctx, 0);
    let source = vm.arg_ref(ctx, 1);
    let off = vm.arg_i32(ctx, 2).max(0) as usize;
    let count = vm.arg_i32(ctx, 3).max(0) as usize;
    let Some(bytes) = byte_array(vm, source) else {
        return Err(errno_exception(vm, function, vfs::EFAULT));
    };
    let end = (off + count).min(bytes.len());
    let slice = &bytes[off.min(end)..end];
    let fd = fd_of(vm, fd_obj).ok_or_else(|| errno_exception(vm, function, vfs::EBADF))?;
    if fd == 1 {
        vm.out_buf.extend_from_slice(slice);
        vm.ret_i32(ctx, slice.len() as i32);
        return Ok(());
    }
    if fd == 2 {
        vm.err_buf.extend_from_slice(slice);
        vm.ret_i32(ctx, slice.len() as i32);
        return Ok(());
    }
    let n = match position {
        Some(pos) => vm.vfs.pwrite(fd, slice, pos),
        None => vm.vfs.write(fd, slice),
    }
    .map_err(|e| errno_exception(vm, function, e))?;
    vm.ret_i32(ctx, n as i32);
    Ok(())
}

fn linux_read_bytes(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    read_into(vm, ctx, "read", None)
}

fn linux_pread_bytes(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let pos = vm.arg_i64(ctx, 4).max(0) as u64;
    read_into(vm, ctx, "pread", Some(pos))
}

fn linux_write_bytes(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    write_from(vm, ctx, "write", None)
}

fn linux_pwrite_bytes(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let pos = vm.arg_i64(ctx, 4).max(0) as u64;
    write_from(vm, ctx, "pwrite", Some(pos))
}

fn linux_lseek(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fd_obj = vm.arg_ref(ctx, 0);
    let offset = vm.arg_i64(ctx, 1);
    let whence = vm.arg_i32(ctx, 2);
    let fd = fd_of(vm, fd_obj).ok_or_else(|| errno_exception(vm, "lseek", vfs::EBADF))?;
    let pos = vm
        .vfs
        .lseek(fd, offset, whence)
        .map_err(|e| errno_exception(vm, "lseek", e))?;
    vm.ret_i64(ctx, pos);
    Ok(())
}

fn linux_fcntl_int(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fd_obj = vm.arg_ref(ctx, 0);
    let cmd = vm.arg_i32(ctx, 1);
    let arg = vm.arg_i32(ctx, 2);
    let fd = fd_of(vm, fd_obj).ok_or_else(|| errno_exception(vm, "fcntl", vfs::EBADF))?;
    let r = vm
        .vfs
        .fcntl(fd, cmd, arg)
        .map_err(|e| errno_exception(vm, "fcntl", e))?;
    vm.ret_i32(ctx, r);
    Ok(())
}

fn linux_fcntl_void(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fd_obj = vm.arg_ref(ctx, 0);
    let cmd = vm.arg_i32(ctx, 1);
    let fd = fd_of(vm, fd_obj).ok_or_else(|| errno_exception(vm, "fcntl", vfs::EBADF))?;
    let r = vm
        .vfs
        .fcntl(fd, cmd, 0)
        .map_err(|e| errno_exception(vm, "fcntl", e))?;
    vm.ret_i32(ctx, r);
    Ok(())
}

fn linux_ftruncate(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fd_obj = vm.arg_ref(ctx, 0);
    let len = vm.arg_i64(ctx, 1).max(0) as u64;
    let fd = fd_of(vm, fd_obj).ok_or_else(|| errno_exception(vm, "ftruncate", vfs::EBADF))?;
    vm.vfs
        .ftruncate(fd, len)
        .map_err(|e| errno_exception(vm, "ftruncate", e))?;
    Ok(())
}

fn linux_fsync(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fd_obj = vm.arg_ref(ctx, 0);
    let fd = fd_of(vm, fd_obj).ok_or_else(|| errno_exception(vm, "fsync", vfs::EBADF))?;
    if fd <= 2 {
        return Ok(());
    }
    vm.vfs
        .fsync(fd, false)
        .map_err(|e| errno_exception(vm, "fsync", e))?;
    Ok(())
}

fn linux_fdatasync(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fd_obj = vm.arg_ref(ctx, 0);
    let fd = fd_of(vm, fd_obj).ok_or_else(|| errno_exception(vm, "fdatasync", vfs::EBADF))?;
    if fd <= 2 {
        return Ok(());
    }
    vm.vfs
        .fsync(fd, true)
        .map_err(|e| errno_exception(vm, "fdatasync", e))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// metadata
// ---------------------------------------------------------------------------

fn linux_fstat(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fd_obj = vm.arg_ref(ctx, 0);
    let fd = fd_of(vm, fd_obj).ok_or_else(|| errno_exception(vm, "fstat", vfs::EBADF))?;
    let st = vm
        .vfs
        .fstat(fd)
        .map_err(|e| errno_exception(vm, "fstat", e))?;
    let obj = make_stat(vm, &st)?;
    vm.ret_ref(ctx, obj);
    Ok(())
}

fn linux_stat(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let path = string_arg(vm, ctx, 0);
    let st = vm
        .vfs
        .stat(&path, true)
        .map_err(|e| errno_exception(vm, "stat", e))?;
    let obj = make_stat(vm, &st)?;
    vm.ret_ref(ctx, obj);
    Ok(())
}

fn linux_lstat(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let path = string_arg(vm, ctx, 0);
    let st = vm
        .vfs
        .stat(&path, false)
        .map_err(|e| errno_exception(vm, "lstat", e))?;
    let obj = make_stat(vm, &st)?;
    vm.ret_ref(ctx, obj);
    Ok(())
}

fn linux_access(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let path = string_arg(vm, ctx, 0);
    let mode = vm.arg_i32(ctx, 1);
    match vm.vfs.access(&path, mode) {
        Ok(()) => {
            vm.ret_bool(ctx, true);
            Ok(())
        }
        Err(e) => Err(errno_exception(vm, "access", e)),
    }
}

fn linux_statvfs(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let path = string_arg(vm, ctx, 0);
    let st = vm
        .vfs
        .statvfs(&path)
        .map_err(|e| errno_exception(vm, "statvfs", e))?;
    let obj = make_statvfs(vm, &st)?;
    vm.ret_ref(ctx, obj);
    Ok(())
}

fn linux_fstatvfs(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fd_obj = vm.arg_ref(ctx, 0);
    let fd = fd_of(vm, fd_obj).ok_or_else(|| errno_exception(vm, "fstatvfs", vfs::EBADF))?;
    vm.vfs
        .fstat(fd)
        .map_err(|e| errno_exception(vm, "fstatvfs", e))?;
    let obj = make_statvfs(vm, &vm.vfs.fstatvfs())?;
    vm.ret_ref(ctx, obj);
    Ok(())
}

// ---------------------------------------------------------------------------
// path mutation
// ---------------------------------------------------------------------------

fn linux_mkdir(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let path = string_arg(vm, ctx, 0);
    let mode = vm.arg_i32(ctx, 1);
    vm.vfs
        .mkdir(&path, mode)
        .map_err(|e| errno_exception(vm, "mkdir", e))?;
    Ok(())
}

fn linux_rename(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let from = string_arg(vm, ctx, 0);
    let to = string_arg(vm, ctx, 1);
    vm.vfs
        .rename(&from, &to)
        .map_err(|e| errno_exception(vm, "rename", e))?;
    Ok(())
}

fn linux_remove(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let path = string_arg(vm, ctx, 0);
    vm.vfs
        .remove(&path)
        .map_err(|e| errno_exception(vm, "remove", e))?;
    Ok(())
}

fn linux_readlink(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let path = string_arg(vm, ctx, 0);
    let target = vm
        .vfs
        .readlink(&path)
        .map_err(|e| errno_exception(vm, "readlink", e))?;
    let h = vm.new_string(&target)?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn linux_realpath(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let path = string_arg(vm, ctx, 0);
    let canonical = vm
        .vfs
        .canonical_guest(&path)
        .map_err(|e| errno_exception(vm, "realpath", e))?;
    let h = vm.new_string(&canonical)?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn linux_symlink(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let target = string_arg(vm, ctx, 0);
    let link = string_arg(vm, ctx, 1);
    vm.vfs
        .symlink(&target, &link)
        .map_err(|e| errno_exception(vm, "symlink", e))?;
    Ok(())
}

fn linux_link(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let old = string_arg(vm, ctx, 0);
    let new = string_arg(vm, ctx, 1);
    vm.vfs
        .link(&old, &new)
        .map_err(|e| errno_exception(vm, "link", e))?;
    Ok(())
}

fn linux_chmod(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let path = string_arg(vm, ctx, 0);
    let mode = vm.arg_i32(ctx, 1);
    vm.vfs
        .chmod(&path, mode, None)
        .map_err(|e| errno_exception(vm, "chmod", e))?;
    Ok(())
}

fn linux_fchmod(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fd_obj = vm.arg_ref(ctx, 0);
    let mode = vm.arg_i32(ctx, 1);
    let fd = fd_of(vm, fd_obj).ok_or_else(|| errno_exception(vm, "fchmod", vfs::EBADF))?;
    vm.vfs
        .chmod("", mode, Some(fd))
        .map_err(|e| errno_exception(vm, "fchmod", e))?;
    Ok(())
}

/// `chown`/`lchown`/`fchown`: the sandbox is always owned by the app uid, so
/// changing the owner is a recorded no-op (there is no root to change it with).
fn linux_chown_path(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let path = string_arg(vm, ctx, 0);
    // Validate the path so a denied path still fails closed.
    vm.vfs
        .stat(&path, false)
        .map_err(|e| errno_exception(vm, "chown", e))?;
    Ok(())
}

fn linux_fchown(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fd_obj = vm.arg_ref(ctx, 0);
    let fd = fd_of(vm, fd_obj).ok_or_else(|| errno_exception(vm, "fchown", vfs::EBADF))?;
    vm.vfs
        .fstat(fd)
        .map_err(|e| errno_exception(vm, "fchown", e))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// pipes / poll
// ---------------------------------------------------------------------------

fn linux_pipe2(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let flags = vm.arg_i32(ctx, 0);
    let (r, w) = vm
        .vfs
        .pipe2(flags)
        .map_err(|e| errno_exception(vm, "pipe2", e))?;
    let r_obj = make_file_descriptor(vm, r)?;
    vm.native_roots.push(r_obj);
    let w_obj = match make_file_descriptor(vm, w) {
        Ok(h) => h,
        Err(e) => {
            vm.native_roots.pop();
            return Err(e);
        }
    };
    vm.native_roots.push(w_obj);
    let array = vm.alloc(Object::Array(ArrayObj {
        elem_desc: "Ljava/io/FileDescriptor;".to_string(),
        data: ArrayData::Ref(vec![r_obj, w_obj]),
    }));
    vm.native_roots.pop();
    vm.native_roots.pop();
    let array = array?;
    vm.ret_ref(ctx, array);
    Ok(())
}

fn linux_poll(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let array = vm.arg_ref(ctx, 0);
    let timeout = vm.arg_i32(ctx, 1);
    let handles: Vec<Handle> = match vm.heap.get(array) {
        Some(Object::Array(ArrayObj {
            data: ArrayData::Ref(v),
            ..
        })) => v.clone(),
        _ => return Err(errno_exception(vm, "poll", vfs::EFAULT)),
    };
    let mut slots: Vec<(i32, i16, i16)> = Vec::with_capacity(handles.len());
    for h in &handles {
        let fd_obj = vm.get_instance_field_by_name(*h, "fd").unwrap_or(NULL);
        let fd = if fd_obj == NULL {
            -1
        } else {
            fd_of(vm, fd_obj).unwrap_or(-1)
        };
        let events = vm.get_instance_field_by_name(*h, "events").unwrap_or(0) as i16;
        slots.push((fd, events, 0));
    }
    let n = vm
        .vfs
        .poll(&mut slots, timeout)
        .map_err(|e| errno_exception(vm, "poll", e))?;
    for (h, slot) in handles.iter().zip(slots.iter()) {
        vm.set_instance_field_by_name(*h, "revents", u32::from(slot.2 as u16));
    }
    vm.ret_i32(ctx, n as i32);
    Ok(())
}

// ---------------------------------------------------------------------------
// FileInputStream
// ---------------------------------------------------------------------------

fn fis_fd(vm: &Vm, ctx: NativeCtx) -> Option<i32> {
    let this = vm.ctx_this(ctx)?;
    let fd_obj = vm.get_instance_field_by_name(this, "fd")?;
    fd_of(vm, fd_obj)
}

fn fis_available0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fd = fis_fd(vm, ctx).ok_or_else(|| errno_exception(vm, "available", vfs::EBADF))?;
    let n = vm
        .vfs
        .available(fd)
        .map_err(|e| errno_exception(vm, "available", e))?;
    vm.ret_i32(ctx, n);
    Ok(())
}

fn fis_skip0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fd = fis_fd(vm, ctx).ok_or_else(|| errno_exception(vm, "skip", vfs::EBADF))?;
    let n = vm.arg_i64(ctx, 0);
    let cur = vm
        .vfs
        .lseek(fd, 0, 1)
        .map_err(|e| errno_exception(vm, "skip", e))?;
    let end = vm
        .vfs
        .lseek(fd, n, 1)
        .map_err(|e| errno_exception(vm, "skip", e))?;
    vm.ret_i64(ctx, (end - cur).max(0));
    Ok(())
}

// ---------------------------------------------------------------------------
// java.io.UnixFileSystem
// ---------------------------------------------------------------------------

/// `java.io.File` attribute bits returned by `getBooleanAttributes0`.
const BA_EXISTS: i32 = 0x01;
const BA_REGULAR: i32 = 0x02;
const BA_DIRECTORY: i32 = 0x04;
const BA_HIDDEN: i32 = 0x08;

fn unix_file_system(vm: &mut Vm) {
    let c = "Ljava/io/UnixFileSystem;";
    vm.register_native(
        c,
        "canonicalize0",
        "(Ljava/lang/String;)Ljava/lang/String;",
        ufs_canonicalize0,
    );
    vm.register_native(
        c,
        "getBooleanAttributes0",
        "(Ljava/lang/String;)I",
        ufs_get_boolean_attributes0,
    );
    vm.register_native(
        c,
        "createDirectory0",
        "(Ljava/io/File;)Z",
        ufs_create_directory0,
    );
    vm.register_native(
        c,
        "createFileExclusively0",
        "(Ljava/lang/String;)Z",
        ufs_create_file_exclusively0,
    );
    vm.register_native(
        c,
        "getLastModifiedTime0",
        "(Ljava/io/File;)J",
        ufs_get_last_modified_time0,
    );
    vm.register_native(
        c,
        "setLastModifiedTime0",
        "(Ljava/io/File;J)Z",
        ufs_set_last_modified_time0,
    );
    vm.register_native(c, "getNameMax0", "(Ljava/lang/String;)J", ufs_get_name_max0);
    vm.register_native(c, "getSpace0", "(Ljava/io/File;I)J", ufs_get_space0);
    vm.register_native(c, "list0", "(Ljava/io/File;)[Ljava/lang/String;", ufs_list0);
}

/// The `path` field of a `java.io.File` argument.
fn file_path(vm: &Vm, file: Handle) -> Option<String> {
    if file == NULL {
        return None;
    }
    vm.get_instance_field_by_name(file, "path")
        .and_then(|h| vm.string_to_rust(h))
}

fn ufs_canonicalize0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let path = string_arg(vm, ctx, 0);
    match vm.vfs.canonical_guest_lenient(&path) {
        Ok(canonical) => {
            let h = vm.new_string(&canonical)?;
            vm.ret_ref(ctx, h);
            Ok(())
        }
        Err(e) => Err(vm.throw_new(
            "Ljava/io/IOException;",
            Some(&format!(
                "canonicalize0({path}): {} ({})",
                vfs::errno_name(e),
                vfs::strerror(e)
            )),
        )),
    }
}

fn ufs_get_boolean_attributes0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let path = string_arg(vm, ctx, 0);
    let bits = match vm.vfs.stat(&path, false) {
        Ok(st) => {
            let mut bits = BA_EXISTS;
            if st.mode & 0o170000 == 0o100000 {
                bits |= BA_REGULAR;
            } else if st.mode & 0o170000 == 0o040000 {
                bits |= BA_DIRECTORY;
            }
            if path.rsplit('/').next().is_some_and(|n| n.starts_with('.')) {
                bits |= BA_HIDDEN;
            }
            bits
        }
        Err(_) => 0,
    };
    vm.ret_i32(ctx, bits);
    Ok(())
}

fn ufs_create_directory0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let file = vm.arg_ref(ctx, 0);
    let Some(path) = file_path(vm, file) else {
        vm.ret_bool(ctx, false);
        return Ok(());
    };
    let ok = vm.vfs.mkdir(&path, 0o777).is_ok();
    vm.ret_bool(ctx, ok);
    Ok(())
}

fn ufs_create_file_exclusively0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let path = string_arg(vm, ctx, 0);
    // O_RDWR | O_CREAT | O_EXCL, then close: the native helper only creates.
    let flags = 0o2 | 0o100 | 0o200;
    let ok = match vm.vfs.open(&path, flags, 0o666) {
        Ok(fd) => {
            let _ = vm.vfs.close(fd);
            true
        }
        Err(_) => false,
    };
    vm.ret_bool(ctx, ok);
    Ok(())
}

fn ufs_get_last_modified_time0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let file = vm.arg_ref(ctx, 0);
    let Some(path) = file_path(vm, file) else {
        vm.ret_i64(ctx, 0);
        return Ok(());
    };
    let millis = match vm.vfs.stat(&path, true) {
        Ok(st) => st.mtime.saturating_mul(1000),
        Err(_) => 0,
    };
    vm.ret_i64(ctx, millis);
    Ok(())
}

fn ufs_set_last_modified_time0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let file = vm.arg_ref(ctx, 0);
    let millis = vm.arg_i64(ctx, 1);
    let Some(path) = file_path(vm, file) else {
        vm.ret_bool(ctx, false);
        return Ok(());
    };
    let ok = vm.vfs.utimens(&path, millis / 1000).is_ok();
    vm.ret_bool(ctx, ok);
    Ok(())
}

fn ufs_get_name_max0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let _ = vm.arg_ref(ctx, 0);
    vm.ret_i64(ctx, 255);
    Ok(())
}

fn ufs_get_space0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let file = vm.arg_ref(ctx, 0);
    let kind = vm.arg_i32(ctx, 1);
    let Some(path) = file_path(vm, file) else {
        vm.ret_i64(ctx, 0);
        return Ok(());
    };
    let st = match vm.vfs.statvfs(&path) {
        Ok(st) => st,
        Err(_) => {
            vm.ret_i64(ctx, 0);
            return Ok(());
        }
    };
    let bytes = match kind {
        0 => st.blocks.saturating_mul(st.frsize),
        1 => st.bfree.saturating_mul(st.frsize),
        2 => st.bavail.saturating_mul(st.frsize),
        _ => 0,
    };
    vm.ret_i64(ctx, bytes.min(i64::MAX as u64) as i64);
    Ok(())
}

fn ufs_list0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let file = vm.arg_ref(ctx, 0);
    let names = match file_path(vm, file).and_then(|p| vm.vfs.list(&p).ok()) {
        Some(names) => names,
        None => {
            vm.ret_ref(ctx, NULL);
            return Ok(());
        }
    };
    let mut refs = Vec::with_capacity(names.len());
    for n in &names {
        let h = vm.new_string(n)?;
        vm.native_roots.push(h);
        refs.push(h);
    }
    let array = vm.alloc(Object::Array(ArrayObj {
        elem_desc: "Ljava/lang/String;".to_string(),
        data: ArrayData::Ref(refs),
    }));
    for _ in &names {
        vm.native_roots.pop();
    }
    let array = array?;
    vm.ret_ref(ctx, array);
    Ok(())
}
