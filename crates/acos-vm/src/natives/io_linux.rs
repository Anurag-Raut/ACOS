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
    // Extended attributes: `ContextImpl.getCacheDir`/`getCodeCacheDir` write
    // `user.inode_cache`/`user.inode_code_cache` on the app data dir. The
    // sandbox records the `user.*` namespace in the VFS (`vfs::Vfs::setxattr`);
    // `getxattr`/`listxattr` return exactly what was set, and any other
    // namespace raises `EOPNOTSUPP` because ACOS has no label/capability layer.
    vm.register_native(
        c,
        "setxattr",
        "(Ljava/lang/String;Ljava/lang/String;[BI)V",
        linux_setxattr,
    );
    vm.register_native(
        c,
        "getxattr",
        "(Ljava/lang/String;Ljava/lang/String;)[B",
        linux_getxattr,
    );
    vm.register_native(
        c,
        "listxattr",
        "(Ljava/lang/String;)[Ljava/lang/String;",
        linux_listxattr,
    );
    vm.register_native(
        c,
        "removexattr",
        "(Ljava/lang/String;Ljava/lang/String;)V",
        linux_removexattr,
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
    // Scatter/gather I/O over sandboxed descriptors (`Os.readv`/`Os.writev`).
    vm.register_native(
        c,
        "readv",
        "(Ljava/io/FileDescriptor;[Ljava/lang/Object;[I[I)I",
        linux_readv,
    );
    vm.register_native(
        c,
        "writev",
        "(Ljava/io/FileDescriptor;[Ljava/lang/Object;[I[I)I",
        linux_writev,
    );
    // `unlink` is the same sandboxed removal as `remove`.
    vm.register_native(c, "unlink", "(Ljava/lang/String;)V", linux_remove);
    // Process/file metadata that has a safe in-sandbox answer.
    vm.register_native(c, "umaskImpl", "(I)I", linux_umask);
    vm.register_native(
        c,
        "getrlimit",
        "(I)Landroid/system/StructRlimit;",
        linux_getrlimit,
    );
    vm.register_native(c, "getpgid", "(I)I", linux_getpgid);
    vm.register_native(c, "strsignal", "(I)Ljava/lang/String;", linux_strsignal);
    vm.register_native(c, "gai_strerror", "(I)Ljava/lang/String;", linux_gai_strerror);
    vm.register_native(c, "environ", "()[Ljava/lang/String;", linux_environ);
    vm.register_native(c, "sysconf", "(I)J", linux_sysconf);
    // Local-only socket pairs and anonymous memory fds, both tracked by the
    // VFS descriptor table; real network sockets stay unbound.
    vm.register_native(
        c,
        "socketpair",
        "(IIILjava/io/FileDescriptor;Ljava/io/FileDescriptor;)V",
        linux_socketpair,
    );
    vm.register_native(
        c,
        "memfd_create",
        "(Ljava/lang/String;I)Ljava/io/FileDescriptor;",
        linux_memfd_create,
    );
    vm.register_native(
        c,
        "posix_fallocate",
        "(Ljava/io/FileDescriptor;JJ)V",
        linux_posix_fallocate,
    );
    // Socket options on the close path: `java.io.FileDescriptor.close()`
    // probes `SO_DOMAIN` (is this an INET socket?) and `SO_LINGER` before
    // releasing a socket. ACOS only creates `AF_UNIX` sockets and never sets
    // linger, so both have a truthful sandbox answer without host network I/O.
    vm.register_native(
        c,
        "getsockoptInt",
        "(Ljava/io/FileDescriptor;II)I",
        linux_getsockopt_int,
    );
    vm.register_native(
        c,
        "getsockoptLinger",
        "(Ljava/io/FileDescriptor;II)Landroid/system/StructLinger;",
        linux_getsockopt_linger,
    );

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

// ---------------------------------------------------------------------------
// extended attributes
// ---------------------------------------------------------------------------

fn linux_setxattr(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let path = string_arg(vm, ctx, 0);
    let name = string_arg(vm, ctx, 1);
    let value = byte_array(vm, vm.arg_ref(ctx, 2)).unwrap_or_default();
    let flags = vm.arg_i32(ctx, 3);
    vm.vfs
        .setxattr(&path, &name, &value, flags)
        .map_err(|e| errno_exception(vm, "setxattr", e))?;
    Ok(())
}

fn linux_getxattr(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let path = string_arg(vm, ctx, 0);
    let name = string_arg(vm, ctx, 1);
    let value = vm
        .vfs
        .getxattr(&path, &name)
        .map_err(|e| errno_exception(vm, "getxattr", e))?;
    let array = vm.alloc(Object::Array(ArrayObj {
        elem_desc: "B".to_string(),
        data: ArrayData::Byte(value.into_iter().map(|b| b as i8).collect()),
    }))?;
    vm.ret_ref(ctx, array);
    Ok(())
}

fn linux_listxattr(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let path = string_arg(vm, ctx, 0);
    let names = vm
        .vfs
        .listxattr(&path)
        .map_err(|e| errno_exception(vm, "listxattr", e))?;
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

fn linux_removexattr(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let path = string_arg(vm, ctx, 0);
    let name = string_arg(vm, ctx, 1);
    vm.vfs
        .removexattr(&path, &name)
        .map_err(|e| errno_exception(vm, "removexattr", e))?;
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
// scatter/gather, sockets, limits, misc metadata
// ---------------------------------------------------------------------------

/// An `Object[]` of references, or `None` for any other array/type.
fn ref_array(vm: &Vm, h: Handle) -> Option<Vec<Handle>> {
    match vm.heap.get(h) {
        Some(Object::Array(ArrayObj {
            data: ArrayData::Ref(v),
            ..
        })) => Some(v.clone()),
        _ => None,
    }
}

/// An `int[]`, or `None`.
fn int_array(vm: &Vm, h: Handle) -> Option<Vec<i32>> {
    match vm.heap.get(h) {
        Some(Object::Array(ArrayObj {
            data: ArrayData::Int(v),
            ..
        })) => Some(v.clone()),
        _ => None,
    }
}

/// `(fd, Object[] buffers, int[] offsets, int[] lengths)` shared by
/// `readv`/`writev`; errors are already the real `ErrnoException` shape.
type IovArgs = (i32, Vec<Handle>, Vec<i32>, Vec<i32>);

fn iov_args(vm: &mut Vm, ctx: NativeCtx, function: &str) -> VmResult<IovArgs> {
    let fd_obj = vm.arg_ref(ctx, 0);
    let fd = fd_of(vm, fd_obj).ok_or_else(|| errno_exception(vm, function, vfs::EBADF))?;
    let bufs = ref_array(vm, vm.arg_ref(ctx, 1))
        .ok_or_else(|| errno_exception(vm, function, vfs::EINVAL))?;
    let offsets = int_array(vm, vm.arg_ref(ctx, 2))
        .ok_or_else(|| errno_exception(vm, function, vfs::EINVAL))?;
    let lengths = int_array(vm, vm.arg_ref(ctx, 3))
        .ok_or_else(|| errno_exception(vm, function, vfs::EINVAL))?;
    if bufs.len() != offsets.len() || bufs.len() != lengths.len() {
        return Err(errno_exception(vm, function, vfs::EINVAL));
    }
    Ok((fd, bufs, offsets, lengths))
}

fn linux_readv(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let (fd, bufs, offsets, lengths) = iov_args(vm, ctx, "readv")?;
    let mut total = 0i32;
    for i in 0..bufs.len() {
        let want = lengths[i].max(0) as usize;
        if want == 0 {
            continue;
        }
        if byte_array(vm, bufs[i]).is_none() {
            return Err(errno_exception(vm, "readv", vfs::EINVAL));
        }
        let mut tmp = vec![0u8; want];
        let got = if fd == 0 {
            use std::io::Read;
            std::io::stdin().read(&mut tmp).unwrap_or(0)
        } else {
            vm.vfs
                .read(fd, &mut tmp)
                .map_err(|e| errno_exception(vm, "readv", e))?
        };
        write_byte_array(vm, bufs[i], offsets[i].max(0) as usize, &tmp[..got]);
        total += got as i32;
        if got < want {
            break;
        }
    }
    vm.ret_i32(ctx, total);
    Ok(())
}

fn linux_writev(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let (fd, bufs, offsets, lengths) = iov_args(vm, ctx, "writev")?;
    let mut total = 0i32;
    for i in 0..bufs.len() {
        let Some(bytes) = byte_array(vm, bufs[i]) else {
            return Err(errno_exception(vm, "writev", vfs::EINVAL));
        };
        let off = offsets[i].max(0) as usize;
        let want = lengths[i].max(0) as usize;
        let end = (off + want).min(bytes.len());
        let slice = &bytes[off.min(end)..end];
        if fd == 1 {
            vm.out_buf.extend_from_slice(slice);
            total += slice.len() as i32;
            continue;
        }
        if fd == 2 {
            vm.err_buf.extend_from_slice(slice);
            total += slice.len() as i32;
            continue;
        }
        let wrote = vm
            .vfs
            .write(fd, slice)
            .map_err(|e| errno_exception(vm, "writev", e))?;
        total += wrote as i32;
        if wrote < slice.len() {
            break;
        }
    }
    vm.ret_i32(ctx, total);
    Ok(())
}

/// `umask(2)`: the ACOS sandbox always creates files with the caller-supplied
/// mode inside its own root, so the process umask is a fixed `0022` and only
/// the previous value is reported.
fn linux_umask(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let _ = vm.arg_i32(ctx, 0);
    vm.ret_i32(ctx, 0o022);
    Ok(())
}

/// `getrlimit(2)`: `RLIMIT_NOFILE` mirrors the VFS descriptor ceiling; every
/// other resource reports `RLIM_INFINITY`, since the sandbox imposes no other
/// host limit on guest file descriptors.
fn linux_getrlimit(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    const RLIMIT_NOFILE: i32 = 7;
    let resource = vm.arg_i32(ctx, 0);
    let (cur, max) = if resource == RLIMIT_NOFILE {
        (1024u64, 1048576u64)
    } else {
        (u64::MAX, u64::MAX)
    };
    let cid = vm.load_class("Landroid/system/StructRlimit;")?;
    let obj = vm.alloc_instance(cid)?;
    vm.native_roots.push(obj);
    let result = (|| -> VmResult<()> {
        let ctor = vm.find_method(cid, "<init>", "(JJ)V", InvokeKind::Direct)?;
        crate::interp::call_method(vm, ctor, obj, &[cur, max])?;
        Ok(())
    })();
    vm.native_roots.pop();
    result?;
    vm.ret_ref(ctx, obj);
    Ok(())
}

/// `getpgid(pid)`: the guest is a single process group, so any live pid maps
/// to its own process id.
fn linux_getpgid(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i32(ctx, std::process::id() as i32);
    Ok(())
}

/// `strsignal(2)`: bionic's short description for the common fatal signals.
fn linux_strsignal(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let sig = vm.arg_i32(ctx, 0);
    let text = match sig {
        1 => "Hangup",
        2 => "Interrupt",
        3 => "Quit",
        4 => "Illegal instruction",
        6 => "Aborted",
        8 => "Floating point exception",
        9 => "Killed",
        11 => "Segmentation fault",
        13 => "Broken pipe",
        14 => "Alarm clock",
        15 => "Terminated",
        17 => "Child exited",
        19 => "Stopped (signal)",
        20 => "Stopped",
        _ => "Unknown signal",
    };
    let h = vm.new_string(text)?;
    vm.ret_ref(ctx, h);
    Ok(())
}

/// `gai_strerror(2)`: bionic's text for the Android `EAI_*` values.
fn linux_gai_strerror(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let err = vm.arg_i32(ctx, 0);
    let text = match err {
        1 => "Address family for hostname not supported",
        2 => "Try again",
        3 => "Invalid value for ai_flags",
        4 => "Non-recoverable failure in name resolution",
        5 => "ai_family not supported",
        6 => "Memory allocation failure",
        7 => "No address associated with hostname",
        8 => "hostname nor servname provided, or not known",
        9 => "servname not supported for ai_socktype",
        10 => "ai_socktype not supported",
        11 => "System error",
        14 => "Argument buffer overflow",
        _ => "Unknown error",
    };
    let h = vm.new_string(text)?;
    vm.ret_ref(ctx, h);
    Ok(())
}

/// `environ(7)`: the guest sees the fixed, VM-owned Android-like environment
/// table seeded by [`super::io`], never the host process environment (which
/// can carry host paths/secrets) and never an implicit empty set that hides a
/// `setenv` round-trip.
fn linux_environ(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let entries = super::io::environ_entries();
    let mut refs = Vec::with_capacity(entries.len());
    let arr = vm.alloc(Object::Array(ArrayObj {
        elem_desc: "Ljava/lang/String;".to_string(),
        data: ArrayData::Ref(Vec::new()),
    }))?;
    vm.native_roots.push(arr);
    let result = (|| -> VmResult<()> {
        for entry in &entries {
            let h = vm.new_string(entry)?;
            refs.push(h);
        }
        if let Some(Object::Array(ArrayObj {
            data: ArrayData::Ref(v),
            ..
        })) = vm.heap.get_mut(arr)
        {
            *v = refs;
        }
        Ok(())
    })();
    vm.native_roots.pop();
    result?;
    vm.ret_ref(ctx, arr);
    Ok(())
}

/// `sysconf(3)` for the `_SC_*` questions the framework asks. Unknown names
/// return `-1` (indeterminate), exactly like libc.
fn linux_sysconf(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let name = vm.arg_i32(ctx, 0);
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get() as i64)
        .unwrap_or(4);
    let value = match name {
        0 => 2_097_152,  // _SC_ARG_MAX
        5 => 7_866,      // _SC_CHILD_MAX
        6 => 100,        // _SC_CLK_TCK
        10 => 65_536,    // _SC_NGROUPS_MAX
        11 => 32_768,    // _SC_OPEN_MAX
        27 => 20,        // _SC_STREAM_MAX
        28 => 6,         // _SC_TZNAME_MAX
        38 => 1_024,     // _SC_IOV_MAX
        39 => 4_096,     // _SC_PAGESIZE
        40 => 4_096,     // _SC_PAGE_SIZE
        73 => 256,       // _SC_LOGIN_NAME_MAX
        78 => 32,        // _SC_TTY_NAME_MAX
        96 | 97 => cpus, // _SC_NPROCESSORS_{CONF,ONLN}
        _ => -1,
    };
    vm.ret_i64(ctx, value);
    Ok(())
}

/// `socketpair(2)`: local `AF_UNIX` pairs, inserted into the VFS descriptor
/// table. The two caller-provided `FileDescriptor` objects are filled in, as
/// `jniSetFileDescriptorOfFD` does on ART.
fn linux_socketpair(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let domain = vm.arg_i32(ctx, 0);
    let type_ = vm.arg_i32(ctx, 1);
    let protocol = vm.arg_i32(ctx, 2);
    let fd1 = vm.arg_ref(ctx, 3);
    let fd2 = vm.arg_ref(ctx, 4);
    let (a, b) = vm
        .vfs
        .socketpair(domain, type_, protocol)
        .map_err(|e| errno_exception(vm, "socketpair", e))?;
    vm.set_instance_field_by_name(fd1, "descriptor", a as u32);
    vm.set_instance_field_by_name(fd2, "descriptor", b as u32);
    Ok(())
}

/// `memfd_create(2)`: a real anonymous host memory fd inside the VFS table.
fn linux_memfd_create(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let name = string_arg(vm, ctx, 0);
    let flags = vm.arg_i32(ctx, 1);
    let fd = vm
        .vfs
        .memfd_create(&name, flags)
        .map_err(|e| errno_exception(vm, "memfd_create", e))?;
    let obj = make_file_descriptor(vm, fd)?;
    vm.ret_ref(ctx, obj);
    Ok(())
}

/// `posix_fallocate(2)`: the sandbox only guarantees that the file is large
/// enough (`ftruncate`); the host is responsible for actually reserving space.
fn linux_posix_fallocate(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fd_obj = vm.arg_ref(ctx, 0);
    let offset = vm.arg_i64(ctx, 1).max(0) as u64;
    let len = vm.arg_i64(ctx, 2).max(0) as u64;
    let fd = fd_of(vm, fd_obj).ok_or_else(|| errno_exception(vm, "posix_fallocate", vfs::EBADF))?;
    let end = offset.saturating_add(len);
    let cur = vm
        .vfs
        .fstat(fd)
        .map_err(|e| errno_exception(vm, "posix_fallocate", e))?
        .size;
    if cur < end {
        vm.vfs
            .ftruncate(fd, end)
            .map_err(|e| errno_exception(vm, "posix_fallocate", e))?;
    }
    Ok(())
}

/// `getsockoptInt(SOL_SOCKET, SO_DOMAIN)`: every ACOS socket is `AF_UNIX`.
/// Any other option is refused with `ENOPROTOOPT`, exactly like a real
/// descriptor that does not carry it.
fn linux_getsockopt_int(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fd_obj = vm.arg_ref(ctx, 0);
    let fd = fd_of(vm, fd_obj).ok_or_else(|| errno_exception(vm, "getsockopt", vfs::EBADF))?;
    let level = vm.arg_i32(ctx, 1);
    let optname = vm.arg_i32(ctx, 2);
    let value = vm
        .vfs
        .getsockopt_int(fd, level, optname)
        .map_err(|e| errno_exception(vm, "getsockopt", e))?;
    vm.ret_i32(ctx, value);
    Ok(())
}

/// `getsockoptLinger(SOL_SOCKET, SO_LINGER)`: ACOS never enables linger, so
/// the answer is the default `(l_onoff = 0, l_linger = 0)`.
fn linux_getsockopt_linger(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fd_obj = vm.arg_ref(ctx, 0);
    let fd = fd_of(vm, fd_obj).ok_or_else(|| errno_exception(vm, "getsockopt", vfs::EBADF))?;
    if !vm.vfs.is_socket(fd) {
        return Err(errno_exception(vm, "getsockopt", 88)); // ENOTSOCK
    }
    let cid = vm.load_class("Landroid/system/StructLinger;")?;
    let obj = vm.alloc_instance(cid)?;
    vm.native_roots.push(obj);
    let result = (|| -> VmResult<()> {
        let ctor = vm.find_method(cid, "<init>", "(II)V", InvokeKind::Direct)?;
        crate::interp::call_method(vm, ctor, obj, &[0, 0])?;
        Ok(())
    })();
    vm.native_roots.pop();
    result?;
    vm.ret_ref(ctx, obj);
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
    // Android 14's `UnixFileSystem` declares most of its natives under the
    // unsuffixed names (`getBooleanAttributes0`, `list`, `getLength`, ...); the
    // `0`-suffixed spellings below are the legacy libcore names, kept because
    // the bootstrap class library and older fixtures use them. Both registries
    // are keyed by the exact (class, name, signature) declared in the DEX, so a
    // class that declares only one spelling reaches only that implementation.
    vm.register_native(
        c,
        "getBooleanAttributes0",
        "(Ljava/lang/String;)I",
        ufs_get_boolean_attributes0,
    );
    vm.register_native(
        c,
        "getBooleanAttributes0",
        "(Ljava/io/File;)I",
        ufs_get_boolean_attributes0_file,
    );
    vm.register_native(c, "checkAccess", "(Ljava/io/File;I)Z", ufs_check_access);
    vm.register_native(
        c,
        "createDirectory0",
        "(Ljava/io/File;)Z",
        ufs_create_directory0,
    );
    vm.register_native(
        c,
        "createDirectory",
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
        "createFileExclusively",
        "(Ljava/lang/String;)Z",
        ufs_create_file_exclusively0,
    );
    vm.register_native(c, "getLength", "(Ljava/io/File;)J", ufs_get_length);
    vm.register_native(c, "delete0", "(Ljava/io/File;)Z", ufs_delete0);
    vm.register_native(
        c,
        "rename0",
        "(Ljava/io/File;Ljava/io/File;)Z",
        ufs_rename0,
    );
    vm.register_native(
        c,
        "getLastModifiedTime0",
        "(Ljava/io/File;)J",
        ufs_get_last_modified_time0,
    );
    vm.register_native(
        c,
        "getLastModifiedTime",
        "(Ljava/io/File;)J",
        ufs_get_last_modified_time0,
    );
    vm.register_native(
        c,
        "setLastModifiedTime0",
        "(Ljava/io/File;J)Z",
        ufs_set_last_modified_time0,
    );
    vm.register_native(
        c,
        "setLastModifiedTime",
        "(Ljava/io/File;J)Z",
        ufs_set_last_modified_time0,
    );
    vm.register_native(
        c,
        "setPermission",
        "(Ljava/io/File;IZZ)Z",
        ufs_set_permission,
    );
    vm.register_native(c, "setReadOnly", "(Ljava/io/File;)Z", ufs_set_read_only);
    vm.register_native(c, "getNameMax0", "(Ljava/lang/String;)J", ufs_get_name_max0);
    vm.register_native(c, "getSpace0", "(Ljava/io/File;I)J", ufs_get_space0);
    vm.register_native(c, "getSpace", "(Ljava/io/File;I)J", ufs_get_space0);
    vm.register_native(c, "list0", "(Ljava/io/File;)[Ljava/lang/String;", ufs_list0);
    vm.register_native(c, "list", "(Ljava/io/File;)[Ljava/lang/String;", ufs_list0);
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

/// `stat(2)`-derived `java.io.File` attribute bits for one guest path.
fn boolean_attributes(vm: &Vm, path: &str) -> i32 {
    match vm.vfs.stat(path, false) {
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
    }
}

fn ufs_get_boolean_attributes0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let path = string_arg(vm, ctx, 0);
    let bits = boolean_attributes(vm, &path);
    vm.ret_i32(ctx, bits);
    Ok(())
}

fn ufs_get_boolean_attributes0_file(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let file = vm.arg_ref(ctx, 0);
    let bits = file_path(vm, file).map_or(0, |p| boolean_attributes(vm, &p));
    vm.ret_i32(ctx, bits);
    Ok(())
}

/// `access(2)` permission-bit evaluation (`java.io.FileSystem.ACCESS_*` are the
/// `R_OK`/`W_OK`/`X_OK` values).
///
/// The VFS runs guest code as the host process, so the ownership class comes
/// from the host `euid`/`egid` (the same identity the kernel uses for `open` on
/// the sandbox root); supplementary groups are not modelled, which only
/// matters for group-owned files outside the sandbox's own tree. Unlike
/// `libcore.io.Linux.access` (which deliberately keeps reads open on the
/// sandbox), `File.canRead()` must reflect the mode bits, because real Java
/// does.
fn unix_access(st: &Stat, access: i32, euid: u32, egid: u32) -> bool {
    if access == 0 {
        return true;
    }
    if euid == 0 {
        // root: R/W always allowed, X only when some execute bit is set.
        return access & 1 == 0 || st.mode & 0o111 != 0;
    }
    let (r, w, x) = if euid == st.uid {
        (0o400, 0o200, 0o100)
    } else if egid == st.gid {
        (0o040, 0o020, 0o010)
    } else {
        (0o004, 0o002, 0o001)
    };
    (access & 4 == 0 || st.mode & r != 0)
        && (access & 2 == 0 || st.mode & w != 0)
        && (access & 1 == 0 || st.mode & x != 0)
}

fn ufs_check_access(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let file = vm.arg_ref(ctx, 0);
    let access = vm.arg_i32(ctx, 1);
    let euid = rustix::process::geteuid().as_raw();
    let egid = rustix::process::getegid().as_raw();
    let allowed = file_path(vm, file).is_some_and(|p| {
        vm.vfs
            .stat(&p, true)
            .is_ok_and(|st| unix_access(&st, access, euid, egid))
    });
    vm.ret_bool(ctx, allowed);
    Ok(())
}

fn ufs_get_length(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let file = vm.arg_ref(ctx, 0);
    let len = file_path(vm, file)
        .and_then(|p| vm.vfs.stat(&p, true).ok())
        .map_or(0, |st| st.size as i64);
    vm.ret_i64(ctx, len);
    Ok(())
}

fn ufs_delete0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let file = vm.arg_ref(ctx, 0);
    let ok = file_path(vm, file).is_some_and(|p| vm.vfs.remove(&p).is_ok());
    vm.ret_bool(ctx, ok);
    Ok(())
}

fn ufs_rename0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let from = file_path(vm, vm.arg_ref(ctx, 0));
    let to = file_path(vm, vm.arg_ref(ctx, 1));
    let ok = match (from, to) {
        (Some(from), Some(to)) => vm.vfs.rename(&from, &to).is_ok(),
        _ => false,
    };
    vm.ret_bool(ctx, ok);
    Ok(())
}

/// The permission bit `mask` selects for one `FileSystem.ACCESS_*` bit, owner
/// or group+other (the AOSP `UnixFileSystem_setPermission0` mapping).
fn permission_mask(access: i32, owneronly: bool) -> u32 {
    let owner_bit = match access {
        4 => 0o400,  // ACCESS_READ
        2 => 0o200,  // ACCESS_WRITE
        1 => 0o100,  // ACCESS_EXECUTE
        _ => return 0,
    };
    if owneronly {
        owner_bit
    } else {
        owner_bit | (owner_bit >> 3) | (owner_bit >> 6)
    }
}

/// `UnixFileSystem.setPermission(File, int access, boolean enable, boolean
/// owneronly)`: rewrite the mode of an existing guest path. Returns `false`
/// (like the AOSP native) on stat/chmod failure, including a read-only boot
/// file and a synthetic path.
fn ufs_set_permission(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let file = vm.arg_ref(ctx, 0);
    let access = vm.arg_i32(ctx, 1);
    let enable = vm.arg_bool(ctx, 2);
    let owneronly = vm.arg_bool(ctx, 3);
    let Some(path) = file_path(vm, file) else {
        vm.ret_bool(ctx, false);
        return Ok(());
    };
    let Ok(st) = vm.vfs.stat(&path, true) else {
        vm.ret_bool(ctx, false);
        return Ok(());
    };
    let mut mode = st.mode & 0o7777;
    for bit in [4, 2, 1] {
        if access & bit == 0 {
            continue;
        }
        let mask = permission_mask(bit, owneronly);
        if enable {
            mode |= mask;
        } else {
            mode &= !mask;
        }
    }
    let ok = vm.vfs.chmod(&path, mode as i32, None).is_ok();
    vm.ret_bool(ctx, ok);
    Ok(())
}

fn ufs_set_read_only(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let file = vm.arg_ref(ctx, 0);
    let Some(path) = file_path(vm, file) else {
        vm.ret_bool(ctx, false);
        return Ok(());
    };
    let Ok(st) = vm.vfs.stat(&path, true) else {
        vm.ret_bool(ctx, false);
        return Ok(());
    };
    let mode = (st.mode & 0o7777) & !0o222;
    let ok = vm.vfs.chmod(&path, mode as i32, None).is_ok();
    vm.ret_bool(ctx, ok);
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
