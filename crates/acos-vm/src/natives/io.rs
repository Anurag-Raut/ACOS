//! `libcore.io.Linux` and the posix layer (M1 subset).
//!
//! `System.out`/`System.err` are backed by `FileDescriptor.out`/`err`; writes
//! are appended to the VM's captured buffers so the fixture harness can compare
//! them byte-for-byte. Other descriptors operate on the host filesystem.

use crate::class::NativeCtx;
use crate::error::{VmError, VmResult};
use crate::heap::{ArrayData, ArrayObj, Handle, Object, NULL};
use crate::vm::Vm;

pub(crate) fn install(vm: &mut Vm) -> VmResult<()> {
    let c = "Llibcore/io/Linux;";
    vm.register_native(c, "writeBytes", "(Ljava/io/FileDescriptor;Ljava/lang/Object;II)I", write_bytes);
    vm.register_native(c, "readBytes", "(Ljava/io/FileDescriptor;Ljava/lang/Object;II)I", read_bytes);
    vm.register_native(c, "close", "(Ljava/io/FileDescriptor;)V", close);
    vm.register_native(c, "isatty", "(Ljava/io/FileDescriptor;)Z", isatty);
    vm.register_native(c, "getpid", "()I", getpid);
    vm.register_native(c, "gettid", "()I", gettid);
    vm.register_native(c, "getuid", "()I", getuid);
    vm.register_native(c, "geteuid", "()I", getuid);
    vm.register_native(c, "getgid", "()I", getuid);
    vm.register_native(c, "getegid", "()I", getuid);
    vm.register_native(c, "getppid", "()I", getpid);
    vm.register_native(c, "getenv", "(Ljava/lang/String;)Ljava/lang/String;", getenv);
    vm.register_native(c, "setenv", "(Ljava/lang/String;Ljava/lang/String;Z)V", setenv);
    vm.register_native(c, "unsetenv", "(Ljava/lang/String;)V", unsetenv);
    vm.register_native(c, "access", "(Ljava/lang/String;I)Z", access);
    vm.register_native(c, "uname", "()Landroid/system/StructUtsname;", uname);
    vm.register_native(c, "getpwuid", "(I)Landroid/system/StructPasswd;", getpwuid);
    vm.register_native(c, "strerror", "(I)Ljava/lang/String;", strerror);
    vm.register_native(c, "sysconf", "(I)J", sysconf);
    vm.register_native(c, "android_fdsan_exchange_owner_tag", "(Ljava/io/FileDescriptor;JJ)V", noop_fd);
    vm.register_native("Landroid/system/OsConstants;", "initConstants", "()V", noop_fd);
    // `java.io.UnixFileSystem.initIDs` only caches JNI field/method IDs; it has
    // no observable effect (ART's own implementation is a no-op in the fast
    // path). Called from the `<clinit>` that `LoadedApk`/`Context` trigger.
    vm.register_native("Ljava/io/UnixFileSystem;", "initIDs", "()V", noop_fd);
    // `File.getCanonicalPath` → `UnixFileSystem.canonicalize0`: the native
    // `realpath(3)` wrapper. ACOS runs on a real host filesystem, so this is
    // the real canonicalisation; a path that does not exist throws
    // `IOException` exactly as ART's `canonicalize_md.c` does.
    vm.register_native(
        "Ljava/io/UnixFileSystem;",
        "canonicalize0",
        "(Ljava/lang/String;)Ljava/lang/String;",
        canonicalize0,
    );
    // `File.list()` / `File.listFiles()` → `UnixFileSystem.list0`: readdir(3).
    // Returns entry names (not paths); `null` on error, matching ART (the
    // Java side turns that into an empty/null list).
    vm.register_native(
        "Ljava/io/UnixFileSystem;",
        "list0",
        "(Ljava/io/File;)[Ljava/lang/String;",
        list0,
    );
    icu_versions(vm);
    Ok(())
}

/// `libcore.icu.ICU` version queries (no ICU data required).
fn icu_versions(vm: &mut Vm) {
    let c = "Llibcore/icu/ICU;";
    vm.register_native(c, "getIcuVersion", "()Ljava/lang/String;", icu_version);
    vm.register_native(c, "getUnicodeVersion", "()Ljava/lang/String;", icu_version);
    vm.register_native(c, "getCldrVersion", "()Ljava/lang/String;", icu_version);
}

fn canonicalize0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let path = vm.string_to_rust(vm.arg_ref(ctx, 0)).unwrap_or_default();
    match std::fs::canonicalize(&path) {
        Ok(p) => {
            let h = vm.new_string(&p.to_string_lossy())?;
            vm.ret_ref(ctx, h);
            Ok(())
        }
        Err(e) => Err(vm.throw_new(
            "Ljava/io/IOException;",
            Some(&format!("canonicalize0({path}): {e}")),
        )),
    }
}

fn list0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let file = vm.arg_ref(ctx, 0);
    let path = match vm.get_field(file, "path") {
        Ok(crate::javabridge::JavaValue::Object(s)) if s != NULL => {
            vm.string_to_rust(s).unwrap_or_default()
        }
        _ => {
            vm.ret_ref(ctx, NULL);
            return Ok(());
        }
    };
    let Ok(entries) = std::fs::read_dir(&path) else {
        vm.ret_ref(ctx, NULL);
        return Ok(());
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort_unstable();
    let mut refs = Vec::with_capacity(names.len());
    // Root each name: the array allocation below may trigger a (stress) GC.
    for n in &names {
        let h = vm.new_string(n)?;
        vm.native_roots.push(h);
        refs.push(h);
    }
    let h = vm.alloc(Object::Array(ArrayObj {
        elem_desc: "Ljava/lang/String;".to_string(),
        data: ArrayData::Ref(refs),
    }))?;
    vm.ret_ref(ctx, h);
    for _ in &names {
        vm.native_roots.pop();
    }
    Ok(())
}

fn icu_version(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = vm.new_string("74.2")?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn getenv(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let name = vm.string_to_rust(vm.arg_ref(ctx, 0)).unwrap_or_default();
    match std::env::var(&name) {
        Ok(v) => {
            let h = vm.new_string(&v)?;
            vm.ret_ref(ctx, h);
        }
        Err(_) => vm.ret_ref(ctx, NULL),
    }
    Ok(())
}

fn setenv(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Ok(())
}

fn unsetenv(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Ok(())
}

fn strerror(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let msg = std::io::Error::from_raw_os_error(vm.arg_i32(ctx, 0)).to_string();
    let h = vm.new_string(&msg)?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn sysconf(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i64(ctx, 4096);
    Ok(())
}

fn new_struct(vm: &mut Vm, desc: &str, fields: &[(&str, StructValue)]) -> VmResult<Handle> {
    let cid = vm.load_class(desc)?;
    let obj = vm.alloc_instance(cid)?;
    vm.native_roots.push(obj);
    let result = (|| -> VmResult<()> {
        for (name, val) in fields {
            match val {
                StructValue::Str(s) => {
                    let h = vm.new_string(s)?;
                    vm.native_roots.push(h);
                    vm.set_instance_field_by_name(obj, name, h);
                    vm.native_roots.pop();
                }
                StructValue::Int(i) => vm.set_instance_field_by_name(obj, name, *i as u32),
            }
        }
        Ok(())
    })();
    vm.native_roots.pop();
    result?;
    Ok(obj)
}

enum StructValue {
    Str(&'static str),
    Int(i32),
}

fn uname(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let obj = new_struct(
        vm,
        "Landroid/system/StructUtsname;",
        &[
            ("sysname", StructValue::Str("Linux")),
            ("nodename", StructValue::Str("localhost")),
            ("release", StructValue::Str("5.10.0")),
            ("version", StructValue::Str("#1 SMP")),
            ("machine", StructValue::Str("x86_64")),
        ],
    )?;
    vm.ret_ref(ctx, obj);
    Ok(())
}

fn getpwuid(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let uid = vm.arg_i32(ctx, 0);
    let obj = new_struct(
        vm,
        "Landroid/system/StructPasswd;",
        &[
            ("pw_name", StructValue::Str("acos")),
            ("pw_passwd", StructValue::Str("x")),
            ("pw_uid", StructValue::Int(uid)),
            ("pw_gid", StructValue::Int(0)),
            ("pw_gecos", StructValue::Str("acos")),
            ("pw_dir", StructValue::Str("/")),
            ("pw_shell", StructValue::Str("/bin/sh")),
        ],
    )?;
    vm.ret_ref(ctx, obj);
    Ok(())
}

/// The integer fd stored in a `java.io.FileDescriptor`.
fn fd_of(vm: &Vm, fd_obj: Handle) -> Option<i32> {
    if fd_obj == NULL {
        return None;
    }
    vm.get_instance_field_by_name(fd_obj, "descriptor")
        .or_else(|| vm.get_instance_field_by_name(fd_obj, "fd"))
        .map(|v| v as i32)
}

fn write_bytes(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fd_obj = vm.arg_ref(ctx, 0);
    let obj = vm.arg_ref(ctx, 1);
    let offset = vm.arg_i32(ctx, 2).max(0) as usize;
    let count = vm.arg_i32(ctx, 3).max(0) as usize;
    let fd = fd_of(vm, fd_obj).unwrap_or(-1);
    let bytes = obj_bytes(vm, obj).unwrap_or_default();
    let end = (offset + count).min(bytes.len());
    let slice = &bytes[offset.min(end)..end];
    if fd == 1 {
        vm.out_buf.extend_from_slice(slice);
    } else if fd == 2 {
        vm.err_buf.extend_from_slice(slice);
    } else if fd >= 0 {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().write(true).open(format!("/proc/self/fd/{fd}")) {
            let _ = f.write_all(slice);
        }
    }
    vm.ret_i32(ctx, slice.len() as i32);
    Ok(())
}

fn read_bytes(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fd_obj = vm.arg_ref(ctx, 0);
    let obj = vm.arg_ref(ctx, 1);
    let offset = vm.arg_i32(ctx, 2).max(0) as usize;
    let count = vm.arg_i32(ctx, 3).max(0) as usize;
    let fd = fd_of(vm, fd_obj).unwrap_or(-1);
    let mut buf = vec![0u8; count];
    let n = if fd >= 0 {
        use std::io::Read;
        if let Ok(mut f) = std::fs::File::open(format!("/proc/self/fd/{fd}")) {
            f.read(&mut buf).unwrap_or(0)
        } else {
            0
        }
    } else {
        0
    };
    if let Some(Object::Array(ArrayObj { data: ArrayData::Byte(v), .. })) = vm.heap.get_mut(obj) {
        for (k, b) in buf[..n].iter().enumerate() {
            if offset + k < v.len() {
                v[offset + k] = *b as i8;
            }
        }
    }
    vm.ret_i32(ctx, n as i32);
    Ok(())
}


fn close(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Ok(())
}

fn isatty(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let fd_obj = vm.arg_ref(ctx, 0);
    let fd = fd_of(vm, fd_obj).unwrap_or(-1);
    vm.ret_bool(ctx, fd == 0 || fd == 1 || fd == 2);
    Ok(())
}

fn getpid(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i32(ctx, std::process::id() as i32);
    Ok(())
}

fn gettid(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i32(ctx, std::process::id() as i32);
    Ok(())
}

fn getuid(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i32(ctx, 0);
    Ok(())
}

fn access(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let path_h = vm.arg_ref(ctx, 0);
    let path = vm.string_to_rust(path_h).unwrap_or_default();
    vm.ret_bool(ctx, std::path::Path::new(&path).exists());
    Ok(())
}

fn noop_fd(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Ok(())
}

/// Extract the bytes behind a `byte[]` argument (the only object type used on
/// the M1 write path).
fn obj_bytes(vm: &Vm, h: Handle) -> Option<Vec<u8>> {
    match vm.heap.get(h) {
        Some(Object::Array(ArrayObj { data: ArrayData::Byte(v), .. })) => {
            Some(v.iter().map(|&b| b as u8).collect())
        }
        _ => None,
    }
}

/// Convert a CText/errno response; retained for future descriptor natives.
#[allow(dead_code)]
fn errno_exception(vm: &mut Vm, errno: i32, msg: &str) -> VmError {
    let _ = errno;
    vm.throw_new("Ljava/io/IOException;", Some(msg))
}
