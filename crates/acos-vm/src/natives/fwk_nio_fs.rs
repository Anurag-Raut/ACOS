//! `sun.nio.fs` natives: the `java.nio.file` SPI on top of the sandbox VFS.
//!
//! `java.io.UnixFileSystem` (the `File`/`FileInputStream` backend) already runs
//! on [`crate::vfs`] through [`super::io_linux`]. `java.nio.file` uses a second,
//! independent file-operation surface, `sun.nio.fs.UnixNativeDispatcher`
//! (+`Linux` for mount/xattr), whose methods take a **native pointer to a
//! NUL-terminated path** rather than a Java `String`. React Native's
//! `SoLoader`/`ReactNativeHost` resolve their library directory through
//! `java.nio.file.Paths`, so the default filesystem cannot be constructed until
//! these are supplied.
//!
//! Every path is a `NativeBuffer` byte blob (`Unsafe.allocateMemory`), read
//! here through the synthetic native-memory table and resolved through the VFS;
//! the guest still never sees a host path. Errors are raised as the real
//! `sun.nio.fs.UnixException(errno)` — `errorString()` calls the real
//! `UnixNativeDispatcher.strerror` bytecode, which this module also supplies —
//! exactly as `io_linux` raises `android.system.ErrnoException`.
//!
//! `init()` returns the capability mask AOSP reports for Linux/Android:
//! `SUPPORTS_OPENAT | SUPPORTS_FUTIMES | SUPPORTS_XATTR`. The mask is a
//! contract: every native a set bit makes reachable is implemented here, and
//! unsupported operations fail with a structured errno instead of lying.

use crate::class::NativeCtx;
use crate::error::{VmError, VmResult};
use crate::heap::{ArrayData, ArrayObj, Handle, Object};
use crate::natives::native_mem;
use crate::vfs::{self, Stat, StatVfs};
use crate::vm::Vm;

// `UnixNativeDispatcher` capability bits (matches the class's `iand` tests).
const SUPPORTS_OPENAT: i32 = 1 << 1;
const SUPPORTS_FUTIMES: i32 = 1 << 2;
const SUPPORTS_XATTR: i32 = 1 << 5;

// `*at` syscall sentinels / flags (Linux generic numbering).
const AT_FDCWD: i32 = -100;
const AT_SYMLINK_NOFOLLOW: i32 = 0x100;

// `pathconf(2)` names the sandbox can answer; `_PC_NAME_MAX` is the only one
// the JDK actually asks for.
const _PC_NAME_MAX: i32 = 3;

// `chown`/`mknod` need a capability ACOS does not model.
const EPERM: i32 = 1;

/// Register the `sun.nio.fs` natives.
pub(crate) fn install(vm: &mut Vm) {
    let c = "Lsun/nio/fs/UnixNativeDispatcher;";
    vm.register_native(c, "init", "()I", init);
    vm.register_native(c, "getcwd", "()[B", getcwd);
    vm.register_native(c, "dup", "(I)I", dup);
    vm.register_native(c, "close", "(I)V", close);
    vm.register_native(c, "open0", "(JII)I", open0);
    vm.register_native(c, "openat0", "(IJII)I", openat0);
    vm.register_native(c, "stat0", "(JLsun/nio/fs/UnixFileAttributes;)V", stat0);
    vm.register_native(c, "lstat0", "(JLsun/nio/fs/UnixFileAttributes;)V", lstat0);
    vm.register_native(c, "fstat", "(ILsun/nio/fs/UnixFileAttributes;)V", fstat);
    vm.register_native(
        c,
        "fstatat0",
        "(IJILsun/nio/fs/UnixFileAttributes;)V",
        fstatat0,
    );
    vm.register_native(c, "stat1", "(J)I", stat1);
    vm.register_native(c, "exists0", "(J)Z", exists0);
    vm.register_native(c, "access0", "(JI)V", access0);
    vm.register_native(c, "chmod0", "(JI)V", chmod0);
    vm.register_native(c, "fchmod", "(II)V", fchmod);
    vm.register_native(c, "chown0", "(JII)V", chown0);
    vm.register_native(c, "lchown0", "(JII)V", chown0);
    vm.register_native(c, "fchown", "(III)V", fchown);
    vm.register_native(c, "mkdir0", "(JI)V", mkdir0);
    vm.register_native(c, "rmdir0", "(J)V", unlink0);
    vm.register_native(c, "unlink0", "(J)V", unlink0);
    vm.register_native(c, "unlinkat0", "(IJI)V", unlinkat0);
    vm.register_native(c, "mknod0", "(JIJ)V", mknod0);
    vm.register_native(c, "rename0", "(JJ)V", rename0);
    vm.register_native(c, "renameat0", "(IJIJ)V", renameat0);
    vm.register_native(c, "link0", "(JJ)V", link0);
    vm.register_native(c, "symlink0", "(JJ)V", symlink0);
    vm.register_native(c, "readlink0", "(J)[B", readlink0);
    vm.register_native(c, "realpath0", "(J)[B", realpath0);
    vm.register_native(c, "utimes0", "(JJJ)V", utimes0);
    vm.register_native(c, "futimes", "(IJJ)V", futimes);
    vm.register_native(c, "opendir0", "(J)J", opendir0);
    vm.register_native(c, "fdopendir", "(I)J", fdopendir);
    vm.register_native(c, "readdir", "(J)[B", readdir);
    vm.register_native(c, "closedir", "(J)V", closedir);
    vm.register_native(c, "read", "(IJI)I", read);
    vm.register_native(c, "write", "(IJI)I", write);
    vm.register_native(c, "pathconf0", "(JI)J", pathconf0);
    vm.register_native(c, "fpathconf", "(II)J", fpathconf);
    vm.register_native(c, "fopen0", "(JJ)J", fopen0);
    vm.register_native(c, "fclose", "(J)V", noop_i64);
    vm.register_native(
        c,
        "statvfs0",
        "(JLsun/nio/fs/UnixFileStoreAttributes;)V",
        statvfs0,
    );
    vm.register_native(c, "strerror", "(I)[B", strerror);
    vm.register_native(c, "getpwuid", "(I)[B", getpwuid);
    vm.register_native(c, "getgrgid", "(I)[B", getgrgid);
    vm.register_native(c, "getpwnam0", "(J)I", getpwnam0);
    vm.register_native(c, "getgrnam0", "(J)I", getgrnam0);

    let l = "Lsun/nio/fs/LinuxNativeDispatcher;";
    vm.register_native(l, "init", "()V", noop);
    vm.register_native(l, "setmntent0", "(JJ)J", fopen0);
    vm.register_native(l, "getmntent", "(JLsun/nio/fs/UnixMountEntry;)I", getmntent);
    vm.register_native(l, "endmntent", "(J)V", noop_i64);
    vm.register_native(l, "fgetxattr0", "(IJJI)I", fgetxattr0);
    vm.register_native(l, "fsetxattr0", "(IJJI)V", fsetxattr0);
    vm.register_native(l, "fremovexattr0", "(IJ)V", fremovexattr0);
    vm.register_native(l, "flistxattr", "(IJI)I", flistxattr);
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn noop(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Ok(())
}

fn noop_i64(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Ok(())
}

/// Raise the real `sun.nio.fs.UnixException(errno)`.
///
/// The Java class exposes `errno()` (read by `asIOException`) and derives its
/// message from `strerror`; the Rust side sets the private `errno` field
/// directly, mirroring how the framework constructs it for a syscall failure.
fn unix_exception(vm: &mut Vm, errno: i32) -> VmError {
    match vm.new_throwable("Lsun/nio/fs/UnixException;", None) {
        Ok(h) => {
            vm.native_roots.push(h);
            vm.set_instance_field_by_name(h, "errno", errno as u32);
            vm.native_roots.pop();
            VmError::JavaException(h)
        }
        Err(_) => vm.throw_new(
            "Ljava/io/IOException;",
            Some(&format!("nio fs failed with errno {errno}")),
        ),
    }
}

/// Read the NUL-terminated C path at argument `n`.
fn path_arg(vm: &mut Vm, ctx: NativeCtx, n: usize) -> VmResult<String> {
    let addr = vm.arg_i64(ctx, n);
    let bytes = native_mem::read_cstr(addr).map_err(|e| e.throw(vm))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Read a raw NUL-terminated name at an explicit `jlong` address.
fn cstr_at(vm: &mut Vm, addr: i64) -> VmResult<String> {
    let bytes = native_mem::read_cstr(addr).map_err(|e| e.throw(vm))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Resolve a `*at`-family path argument against its directory descriptor.
fn resolve_at(vm: &mut Vm, dirfd: i32, path: &str) -> VmResult<String> {
    if path.starts_with('/') || dirfd == AT_FDCWD {
        return Ok(path.to_string());
    }
    let dir = vm
        .vfs
        .fd_file_guest_path(dirfd)
        .map(str::to_string)
        .ok_or_else(|| unix_exception(vm, vfs::EBADF))?;
    if dir.ends_with('/') {
        Ok(format!("{dir}{path}"))
    } else {
        Ok(format!("{dir}/{path}"))
    }
}

/// Allocate a `byte[]` from raw bytes.
fn byte_array(vm: &mut Vm, data: Vec<u8>) -> VmResult<Handle> {
    vm.alloc(Object::Array(ArrayObj {
        elem_desc: "B".to_string(),
        data: ArrayData::Byte(data.into_iter().map(|b| b as i8).collect()),
    }))
}

fn ret_bytes(vm: &mut Vm, ctx: NativeCtx, data: Vec<u8>) -> VmResult<()> {
    let h = byte_array(vm, data)?;
    vm.ret_ref(ctx, h);
    Ok(())
}

/// Write a `jlong` instance field (two 32-bit slots).
fn set_long_field(vm: &mut Vm, obj: Handle, name: &str, value: i64) {
    if let Some(class) = vm.class_of_object(obj) {
        let mut c = Some(class);
        while let Some(cur) = c {
            let off = vm.classes[cur as usize]
                .declared_instance_fields
                .iter()
                .find(|f| f.name == name)
                .map(|f| f.offset as usize);
            if let Some(off) = off {
                if let Some(Object::Instance { fields, .. }) = vm.heap.get_mut(obj) {
                    if off + 1 < fields.len() {
                        let v = value as u64;
                        fields[off] = v as u32;
                        fields[off + 1] = (v >> 32) as u32;
                    }
                }
                return;
            }
            c = vm.classes[cur as usize].super_class;
        }
    }
}

/// Populate a `sun.nio.fs.UnixFileAttributes` from a VFS [`Stat`].
fn fill_attributes(vm: &mut Vm, attrs: Handle, st: &Stat) {
    vm.set_instance_field_by_name(attrs, "st_mode", st.mode);
    set_long_field(vm, attrs, "st_ino", st.ino as i64);
    set_long_field(vm, attrs, "st_dev", st.dev as i64);
    set_long_field(vm, attrs, "st_rdev", st.rdev as i64);
    vm.set_instance_field_by_name(attrs, "st_nlink", st.nlink as u32);
    vm.set_instance_field_by_name(attrs, "st_uid", st.uid);
    vm.set_instance_field_by_name(attrs, "st_gid", st.gid);
    set_long_field(vm, attrs, "st_size", st.size as i64);
    set_long_field(vm, attrs, "st_atime_sec", st.atime);
    set_long_field(vm, attrs, "st_atime_nsec", 0);
    set_long_field(vm, attrs, "st_mtime_sec", st.mtime);
    set_long_field(vm, attrs, "st_mtime_nsec", 0);
    set_long_field(vm, attrs, "st_ctime_sec", st.ctime);
    set_long_field(vm, attrs, "st_ctime_nsec", 0);
    set_long_field(vm, attrs, "st_birthtime_sec", 0);
    set_long_field(vm, attrs, "st_birthtime_nsec", 0);
}

/// Populate a `sun.nio.fs.UnixFileStoreAttributes` from a VFS [`StatVfs`].
fn fill_file_store(vm: &mut Vm, attrs: Handle, st: &StatVfs) {
    set_long_field(vm, attrs, "f_frsize", st.frsize as i64);
    set_long_field(vm, attrs, "f_blocks", st.blocks as i64);
    set_long_field(vm, attrs, "f_bfree", st.bfree as i64);
    set_long_field(vm, attrs, "f_bavail", st.bavail as i64);
}

fn stat_result(vm: &mut Vm, r: Result<Stat, i32>, attrs: Handle) -> VmResult<()> {
    match r {
        Ok(st) => {
            fill_attributes(vm, attrs, &st);
            Ok(())
        }
        Err(e) => Err(unix_exception(vm, e)),
    }
}

// ---------------------------------------------------------------------------
// lifecycle / metadata
// ---------------------------------------------------------------------------

fn init(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    // openat(), futimes(), and the `user.*` xattr surface are all backed by the
    // VFS below; every bit set here has a real implementation.
    vm.ret_i32(ctx, SUPPORTS_OPENAT | SUPPORTS_FUTIMES | SUPPORTS_XATTR);
    Ok(())
}

fn getcwd(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    // The sandbox root is `/`; relative paths resolve against the app data dir
    // supplied to the filesystem constructor, not this value.
    ret_bytes(vm, ctx, b"/".to_vec())
}

fn strerror(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let errno = vm.arg_i32(ctx, 0);
    ret_bytes(vm, ctx, vfs::strerror(errno).as_bytes().to_vec())
}

fn getpwuid(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let uid = vm.arg_i32(ctx, 0);
    if let Some(name) = android_account_name(uid) {
        ret_bytes(vm, ctx, name.into_bytes())
    } else {
        Err(unix_exception(vm, vfs::ENOENT))
    }
}

fn getgrgid(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    // Android groups mirror the app uid for the sandbox's single account.
    let gid = vm.arg_i32(ctx, 0);
    if let Some(name) = android_account_name(gid) {
        ret_bytes(vm, ctx, name.into_bytes())
    } else {
        Err(unix_exception(vm, vfs::ENOENT))
    }
}

/// The device's account naming for the system/app uid range.
fn android_account_name(id: i32) -> Option<String> {
    match id {
        0 => Some("root".to_string()),
        1000 => Some("system".to_string()),
        10000..=19999 => Some(format!("u0_a{}", id - 10000)),
        _ => None,
    }
}

fn getpwnam0(vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    // Name lookup is not modelled; the JDK falls back to the numeric string
    // when this raises (see `UnixUserPrincipals.fromUid`).
    Err(unix_exception(vm, vfs::ENOENT))
}

fn getgrnam0(vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Err(unix_exception(vm, vfs::ENOENT))
}

// ---------------------------------------------------------------------------
// paths: stat / access / mode / ownership
// ---------------------------------------------------------------------------

fn stat0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let path = path_arg(vm, ctx, 0)?;
    let attrs = vm.arg_ref(ctx, 1);
    let r = vm.vfs.stat(&path, true);
    stat_result(vm, r, attrs)
}

fn lstat0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let path = path_arg(vm, ctx, 0)?;
    let attrs = vm.arg_ref(ctx, 1);
    let r = vm.vfs.stat(&path, false);
    stat_result(vm, r, attrs)
}

fn fstat(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fd = vm.arg_i32(ctx, 0);
    let attrs = vm.arg_ref(ctx, 1);
    let r = vm.vfs.fstat(fd);
    stat_result(vm, r, attrs)
}

fn fstatat0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let dirfd = vm.arg_i32(ctx, 0);
    let raw = path_arg(vm, ctx, 1)?;
    let flags = vm.arg_i32(ctx, 2);
    let attrs = vm.arg_ref(ctx, 3);
    let path = resolve_at(vm, dirfd, &raw)?;
    let follow = flags & AT_SYMLINK_NOFOLLOW == 0;
    let r = vm.vfs.stat(&path, follow);
    stat_result(vm, r, attrs)
}

fn stat1(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let path = path_arg(vm, ctx, 0)?;
    let code = match vm.vfs.stat(&path, true) {
        Ok(_) => 0,
        Err(e) => e,
    };
    vm.ret_i32(ctx, code);
    Ok(())
}

fn exists0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let path = path_arg(vm, ctx, 0)?;
    let exists = vm.vfs.stat(&path, true).is_ok();
    vm.ret_bool(ctx, exists);
    Ok(())
}

fn access0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let path = path_arg(vm, ctx, 0)?;
    let mode = vm.arg_i32(ctx, 1);
    vm.vfs
        .access(&path, mode)
        .map_err(|e| unix_exception(vm, e))
}

fn chmod0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let path = path_arg(vm, ctx, 0)?;
    let mode = vm.arg_i32(ctx, 1);
    vm.vfs
        .chmod(&path, mode, None)
        .map_err(|e| unix_exception(vm, e))
}

fn fchmod(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fd = vm.arg_i32(ctx, 0);
    let mode = vm.arg_i32(ctx, 1);
    vm.vfs
        .chmod("", mode, Some(fd))
        .map_err(|e| unix_exception(vm, e))
}

fn chown0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    // The sandbox has no capability layer and therefore cannot change inode
    // ownership; report the same `EPERM` a real app process gets.
    let _ = path_arg(vm, ctx, 0)?;
    Err(unix_exception(vm, EPERM))
}

fn fchown(vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Err(unix_exception(vm, EPERM))
}

// ---------------------------------------------------------------------------
// paths: mutation
// ---------------------------------------------------------------------------

fn mkdir0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let path = path_arg(vm, ctx, 0)?;
    let mode = vm.arg_i32(ctx, 1);
    vm.vfs.mkdir(&path, mode).map_err(|e| unix_exception(vm, e))
}

fn unlink0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let path = path_arg(vm, ctx, 0)?;
    vm.vfs.remove(&path).map_err(|e| unix_exception(vm, e))
}

fn unlinkat0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let dirfd = vm.arg_i32(ctx, 0);
    let raw = path_arg(vm, ctx, 1)?;
    let _ = vm.arg_i32(ctx, 2); // AT_REMOVEDIR: `remove` handles both kinds.
    let path = resolve_at(vm, dirfd, &raw)?;
    vm.vfs.remove(&path).map_err(|e| unix_exception(vm, e))
}

fn mknod0(vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    // Special files (devices, fifos) cannot be materialised in the sandbox.
    Err(unix_exception(vm, vfs::EOPNOTSUPP))
}

fn rename0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let from = path_arg(vm, ctx, 0)?;
    let to = path_arg(vm, ctx, 1)?;
    vm.vfs.rename(&from, &to).map_err(|e| unix_exception(vm, e))
}

fn renameat0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let oldfd = vm.arg_i32(ctx, 0);
    let old_raw = path_arg(vm, ctx, 1)?;
    let newfd = vm.arg_i32(ctx, 2);
    let new_raw = path_arg(vm, ctx, 3)?;
    let from = resolve_at(vm, oldfd, &old_raw)?;
    let to = resolve_at(vm, newfd, &new_raw)?;
    vm.vfs.rename(&from, &to).map_err(|e| unix_exception(vm, e))
}

fn link0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let old = path_arg(vm, ctx, 0)?;
    let new = path_arg(vm, ctx, 1)?;
    vm.vfs.link(&old, &new).map_err(|e| unix_exception(vm, e))
}

fn symlink0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let target = path_arg(vm, ctx, 0)?;
    let link = path_arg(vm, ctx, 1)?;
    vm.vfs
        .symlink(&target, &link)
        .map_err(|e| unix_exception(vm, e))
}

fn readlink0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let path = path_arg(vm, ctx, 0)?;
    match vm.vfs.readlink(&path) {
        Ok(target) => ret_bytes(vm, ctx, target.into_bytes()),
        Err(e) => Err(unix_exception(vm, e)),
    }
}

fn realpath0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let path = path_arg(vm, ctx, 0)?;
    match vm.vfs.canonical_guest(&path) {
        Ok(canon) => ret_bytes(vm, ctx, canon.into_bytes()),
        Err(e) => Err(unix_exception(vm, e)),
    }
}

fn utimes0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let path = path_arg(vm, ctx, 0)?;
    let mtime = vm.arg_i64(ctx, 2);
    vm.vfs
        .utimens(&path, mtime)
        .map_err(|e| unix_exception(vm, e))
}

fn futimes(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fd = vm.arg_i32(ctx, 0);
    let mtime = vm.arg_i64(ctx, 2);
    let guest = vm
        .vfs
        .fd_file_guest_path(fd)
        .map(str::to_string)
        .ok_or_else(|| unix_exception(vm, vfs::EBADF))?;
    vm.vfs
        .utimens(&guest, mtime)
        .map_err(|e| unix_exception(vm, e))
}

// ---------------------------------------------------------------------------
// open / close / read / write
// ---------------------------------------------------------------------------

fn dup(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fd = vm.arg_i32(ctx, 0);
    let new = vm.vfs.dup(fd).map_err(|e| unix_exception(vm, e))?;
    vm.ret_i32(ctx, new);
    Ok(())
}

fn close(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fd = vm.arg_i32(ctx, 0);
    if fd <= 2 {
        return Ok(());
    }
    vm.vfs.close(fd).map_err(|e| unix_exception(vm, e))
}

fn open0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let path = path_arg(vm, ctx, 0)?;
    let flags = vm.arg_i32(ctx, 1);
    let mode = vm.arg_i32(ctx, 2);
    let fd = vm
        .vfs
        .open(&path, flags, mode)
        .map_err(|e| unix_exception(vm, e))?;
    vm.ret_i32(ctx, fd);
    Ok(())
}

fn openat0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let dirfd = vm.arg_i32(ctx, 0);
    let raw = path_arg(vm, ctx, 1)?;
    let flags = vm.arg_i32(ctx, 2);
    let mode = vm.arg_i32(ctx, 3);
    let path = resolve_at(vm, dirfd, &raw)?;
    let fd = vm
        .vfs
        .open(&path, flags, mode)
        .map_err(|e| unix_exception(vm, e))?;
    vm.ret_i32(ctx, fd);
    Ok(())
}

fn read(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fd = vm.arg_i32(ctx, 0);
    let addr = vm.arg_i64(ctx, 1);
    let len = vm.arg_i32(ctx, 2).max(0) as usize;
    let mut buf = vec![0u8; len];
    let n = vm
        .vfs
        .read(fd, &mut buf)
        .map_err(|e| unix_exception(vm, e))?;
    native_mem::write_bytes_at(vm, addr, &buf[..n])?;
    vm.ret_i32(ctx, n as i32);
    Ok(())
}

fn write(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fd = vm.arg_i32(ctx, 0);
    let addr = vm.arg_i64(ctx, 1);
    let len = vm.arg_i32(ctx, 2).max(0) as usize;
    let data = native_mem::read_bytes_at(vm, addr, len)?;
    let n = vm.vfs.write(fd, &data).map_err(|e| unix_exception(vm, e))?;
    vm.ret_i32(ctx, n as i32);
    Ok(())
}

// ---------------------------------------------------------------------------
// directories
// ---------------------------------------------------------------------------

fn opendir0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let path = path_arg(vm, ctx, 0)?;
    let handle = vm.vfs.opendir(&path).map_err(|e| unix_exception(vm, e))?;
    vm.ret_i64(ctx, handle as i64);
    Ok(())
}

fn fdopendir(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fd = vm.arg_i32(ctx, 0);
    let handle = vm.vfs.fdopendir(fd).map_err(|e| unix_exception(vm, e))?;
    vm.ret_i64(ctx, handle as i64);
    Ok(())
}

fn readdir(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let dir = vm.arg_i64(ctx, 0) as u64;
    match vm.vfs.readdir(dir) {
        Ok(Some(name)) => ret_bytes(vm, ctx, name.into_bytes()),
        Ok(None) => {
            vm.ret_ref(ctx, crate::heap::NULL);
            Ok(())
        }
        Err(e) => Err(unix_exception(vm, e)),
    }
}

fn closedir(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let dir = vm.arg_i64(ctx, 0) as u64;
    vm.vfs.closedir(dir).map_err(|e| unix_exception(vm, e))
}

// ---------------------------------------------------------------------------
// misc metadata
// ---------------------------------------------------------------------------

fn pathconf0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let _ = path_arg(vm, ctx, 0)?;
    let name = vm.arg_i32(ctx, 1);
    vm.ret_i64(ctx, pathconf_value(name));
    Ok(())
}

fn fpathconf(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let _fd = vm.arg_i32(ctx, 0);
    let name = vm.arg_i32(ctx, 1);
    vm.ret_i64(ctx, pathconf_value(name));
    Ok(())
}

fn pathconf_value(name: i32) -> i64 {
    if name == _PC_NAME_MAX {
        255
    } else {
        -1
    }
}

fn fopen0(_vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    // `fopen` is not on any `java.nio.file` path in the boot classpath; return
    // the null stream rather than fabricate a `FILE*`.
    _vm.ret_i64(ctx, 0);
    Ok(())
}

fn statvfs0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let path = path_arg(vm, ctx, 0)?;
    let attrs = vm.arg_ref(ctx, 1);
    let st = vm.vfs.statvfs(&path).map_err(|e| unix_exception(vm, e))?;
    fill_file_store(vm, attrs, &st);
    Ok(())
}

// ---------------------------------------------------------------------------
// LinuxNativeDispatcher: mount table + xattrs
// ---------------------------------------------------------------------------

fn getmntent(vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    // ACOS has no mount namespaces; report end-of-stream so `getFileStores()`
    // yields an empty list instead of inventing mount entries.
    vm.ret_i32(_ctx, 0);
    Ok(())
}

fn fgetxattr0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fd = vm.arg_i32(ctx, 0);
    let name_addr = vm.arg_i64(ctx, 1);
    let value_addr = vm.arg_i64(ctx, 2);
    let value_len = vm.arg_i32(ctx, 3);
    let name = cstr_at(vm, name_addr)?;
    let guest = vm
        .vfs
        .fd_file_guest_path(fd)
        .map(str::to_string)
        .ok_or_else(|| unix_exception(vm, vfs::EBADF))?;
    let value = vm
        .vfs
        .getxattr(&guest, &name)
        .map_err(|e| unix_exception(vm, e))?;
    if value_addr == 0 || value_len == 0 {
        vm.ret_i32(ctx, value.len() as i32);
        return Ok(());
    }
    if value.len() > value_len as usize {
        return Err(unix_exception(vm, vfs::ERANGE));
    }
    native_mem::write_bytes_at(vm, value_addr, &value)?;
    vm.ret_i32(ctx, value.len() as i32);
    Ok(())
}

fn fsetxattr0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fd = vm.arg_i32(ctx, 0);
    let name_addr = vm.arg_i64(ctx, 1);
    let value_addr = vm.arg_i64(ctx, 2);
    let value_len = vm.arg_i32(ctx, 3).max(0) as usize;
    let name = cstr_at(vm, name_addr)?;
    let value = native_mem::read_bytes_at(vm, value_addr, value_len)?;
    let guest = vm
        .vfs
        .fd_file_guest_path(fd)
        .map(str::to_string)
        .ok_or_else(|| unix_exception(vm, vfs::EBADF))?;
    vm.vfs
        .setxattr(&guest, &name, &value, 0)
        .map_err(|e| unix_exception(vm, e))
}

fn fremovexattr0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fd = vm.arg_i32(ctx, 0);
    let name_addr = vm.arg_i64(ctx, 1);
    let name = cstr_at(vm, name_addr)?;
    let guest = vm
        .vfs
        .fd_file_guest_path(fd)
        .map(str::to_string)
        .ok_or_else(|| unix_exception(vm, vfs::EBADF))?;
    vm.vfs
        .removexattr(&guest, &name)
        .map_err(|e| unix_exception(vm, e))
}

fn flistxattr(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fd = vm.arg_i32(ctx, 0);
    let list_addr = vm.arg_i64(ctx, 1);
    let size = vm.arg_i32(ctx, 2);
    let guest = vm
        .vfs
        .fd_file_guest_path(fd)
        .map(str::to_string)
        .ok_or_else(|| unix_exception(vm, vfs::EBADF))?;
    let names = vm
        .vfs
        .listxattr(&guest)
        .map_err(|e| unix_exception(vm, e))?;
    // `listxattr` packs the names NUL-terminated, back to back.
    let mut packed = Vec::new();
    for name in names {
        packed.extend_from_slice(name.as_bytes());
        packed.push(0);
    }
    if list_addr == 0 || size == 0 {
        vm.ret_i32(ctx, packed.len() as i32);
        return Ok(());
    }
    if packed.len() > size as usize {
        return Err(unix_exception(vm, vfs::ERANGE));
    }
    native_mem::write_bytes_at(vm, list_addr, &packed)?;
    vm.ret_i32(ctx, packed.len() as i32);
    Ok(())
}
