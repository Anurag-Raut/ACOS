//! `dalvik.system.DexFile` natives — the app class loader's DEX provider.
//!
//! Real Android's `PathClassLoader` / `BaseDexClassLoader` / `DexPathList`
//! create one `DexFile` per APK `classes*.dex` and resolve app classes through
//! `DexPathList.Element.findClass` -> `DexFile.loadClassBinaryName` ->
//! `DexFile.defineClassNative`. This module supplies those natives against the
//! ACOS VM's DEX providers and the sandboxed VFS.
//!
//! # What is real here
//!
//! * `openDexFileNative(sourceName, ...)` opens the guest path through
//!   [`crate::vfs::Vfs`] (never a host path), reads the container, and registers
//!   every `classes*.dex` entry as a VM provider via `Vm::add_shared_dex`.
//!   A plain `.dex` file works too, and a directory is enumerated in sorted
//!   name order exactly like `DexPathList` does for directory elements.
//! * The returned cookie is a real Java `long[]` (ART's cookie type); each
//!   element is the VM's provider index. Nothing outside the VM sees a Rust
//!   pointer.
//! * `defineClassNative` resolves the requested binary name *in the cookie's
//!   dex providers only* and returns the real `java.lang.Class` mirror
//!   (`Vm::class_object`), so `ClassLoader.loadClass`/`loadClassBinaryName`
//!   executes the real framework bytecode.
//! * `getClassNameList` enumerates the real `class_defs` of the cookie.
//!
//! # Declared divergences
//!
//! * A DEX provider is never unloaded: `closeDexFile` drops the cookie and
//!   reports success, but the VM has no class unloading (ART does not unload
//!   either for app class loaders in practice). The class identity stays valid.
//! * `isDexOptNeeded`/`getDexOptNeeded` report "not needed": ACOS runs the DEX
//!   directly and has no oat/dexopt pipeline, so there is nothing to optimise.
//!   This is the same answer a device gives when the APK already has a valid
//!   oat file; it is not a fake success for an operation ACOS claims to perform.
//! * OAT/dexopt introspection (`getDexFileOutputPaths`, `getDexFileStatus`,
//!   `getDexFileOptimizationStatus`, `getStaticSizeOfDexFile`) is left
//!   unregistered so it surfaces as a structured `UNSUPPORTED_JAVA_API`.

use std::io::{Cursor, Read};

use acos_dex::DexFile;

use crate::class::NativeCtx;
use crate::error::{VmError, VmResult};
use crate::heap::{ArrayData, Handle, Object, NULL};
use crate::javabridge::JavaValue;
use crate::vm::Vm;

/// Register every `dalvik.system.DexFile` native the class-loader path needs.
pub(crate) fn install(vm: &mut Vm) -> VmResult<()> {
    let c = "Ldalvik/system/DexFile;";
    vm.register_native(
        c,
        "openDexFileNative",
        "(Ljava/lang/String;Ljava/lang/String;ILjava/lang/ClassLoader;[Ldalvik/system/DexPathList$Element;)Ljava/lang/Object;",
        open_dex_file,
    );
    vm.register_native(
        c,
        "openInMemoryDexFilesNative",
        "([Ljava/nio/ByteBuffer;[[B[I[ILjava/lang/ClassLoader;[Ldalvik/system/DexPathList$Element;)Ljava/lang/Object;",
        open_in_memory_dex_files,
    );
    vm.register_native(
        c,
        "defineClassNative",
        "(Ljava/lang/String;Ljava/lang/ClassLoader;Ljava/lang/Object;Ldalvik/system/DexFile;)Ljava/lang/Class;",
        define_class,
    );
    vm.register_native(
        c,
        "getClassNameList",
        "(Ljava/lang/Object;)[Ljava/lang/String;",
        get_class_name_list,
    );
    vm.register_native(c, "closeDexFile", "(Ljava/lang/Object;)Z", close_dex_file);
    vm.register_native(c, "setTrusted", "(Ljava/lang/Object;)V", noop_cookie);
    vm.register_native(
        c,
        "verifyInBackgroundNative",
        "(Ljava/lang/Object;Ljava/lang/ClassLoader;)V",
        noop_verify,
    );
    vm.register_native(c, "isBackedByOatFile", "(Ljava/lang/Object;)Z", ret_false);
    vm.register_native(c, "isDexOptNeeded", "(Ljava/lang/String;)Z", ret_false);
    vm.register_native(
        c,
        "getDexOptNeeded",
        "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;ZZ)I",
        ret_no_dexopt_needed,
    );
    vm.register_native(
        c,
        "getNonProfileGuidedCompilerFilter",
        "(Ljava/lang/String;)Ljava/lang/String;",
        ret_filter,
    );
    vm.register_native(
        c,
        "getSafeModeCompilerFilter",
        "(Ljava/lang/String;)Ljava/lang/String;",
        ret_filter,
    );
    vm.register_native(
        c,
        "isValidCompilerFilter",
        "(Ljava/lang/String;)Z",
        ret_true,
    );
    vm.register_native(
        c,
        "isProfileGuidedCompilerFilter",
        "(Ljava/lang/String;)Z",
        ret_false,
    );
    vm.register_native(
        c,
        "isOptimizedCompilerFilter",
        "(Ljava/lang/String;)Z",
        ret_false,
    );
    vm.register_native(
        c,
        "isVerifiedCompilerFilter",
        "(Ljava/lang/String;)Z",
        ret_true,
    );
    vm.register_native(c, "isReadOnlyJavaDclEnforced", "()Z", ret_false);
    // `BaseDexClassLoader.reportClassLoaderChain()` (called at the end of the
    // `PathClassLoader` constructor) reaches this native. It is part of the
    // dexopt class-loader-context protocol, not class loading itself.
    vm.register_native(
        "Ldalvik/system/BaseDexClassLoader;",
        "computeClassLoaderContextsNative",
        "()[Ljava/lang/String;",
        compute_class_loader_contexts,
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Cookie <-> provider mapping
// ---------------------------------------------------------------------------

/// ART's `DexFile.mCookie` is a `long[]`; ACOS uses the VM provider index in
/// element 0..n. This reads it back without trusting the guest: a non-array or
/// an out-of-range index is reported as `IllegalArgumentException` by the
/// caller.
fn cookie_dexs(vm: &mut Vm, cookie: Handle) -> VmResult<Vec<u32>> {
    if cookie == NULL {
        return Err(vm.throw_new(
            "Ljava/lang/IllegalArgumentException;",
            Some("null DexFile cookie"),
        ));
    }
    let Some(Object::Array(a)) = vm.heap.get(cookie) else {
        return Err(vm.throw_new(
            "Ljava/lang/IllegalArgumentException;",
            Some("DexFile cookie is not a long[]"),
        ));
    };
    let ArrayData::Long(v) = &a.data else {
        return Err(vm.throw_new(
            "Ljava/lang/IllegalArgumentException;",
            Some("DexFile cookie is not a long[]"),
        ));
    };
    let mut out = Vec::with_capacity(v.len());
    for &idx in v {
        if idx < 0 || idx as usize >= vm.dexs.len() {
            return Err(vm.throw_new(
                "Ljava/lang/IllegalArgumentException;",
                Some("DexFile cookie references an unknown DEX"),
            ));
        }
        out.push(idx as u32);
    }
    Ok(out)
}

fn new_cookie(vm: &mut Vm, dexs: &[u32]) -> VmResult<Handle> {
    let arr = vm.new_array_empty("J", dexs.len()).map_err(bridge)?;
    // Root across the element stores: `new_array_empty` does not root, and a
    // GC in `set_array_element` (stress mode) would otherwise reclaim it.
    vm.native_roots.push(arr);
    let result = (|| {
        for (i, &idx) in dexs.iter().enumerate() {
            vm.set_array_element(arr, i, JavaValue::Long(idx as i64))
                .map_err(bridge)?;
        }
        Ok(arr)
    })();
    vm.native_roots.pop();
    result
}

/// Binary name (`a.b.C`) -> DEX descriptor (`La/b/C;`).
fn binary_name_to_desc(name: &str) -> String {
    format!("L{};", name.replace('.', "/"))
}

/// DEX descriptor -> binary name (`La/b/C;` -> `a.b.C`).
fn desc_to_binary_name(desc: &str) -> String {
    let inner = desc
        .strip_prefix('L')
        .and_then(|d| d.strip_suffix(';'))
        .unwrap_or(desc);
    inner.replace('/', ".")
}

/// VM provider indices for an opened dex source; `Pending` is used while the
/// source bytes are parsed.
struct OpenedDex {
    indices: Vec<u32>,
}

// ---------------------------------------------------------------------------
// Reading guest files
// ---------------------------------------------------------------------------

fn read_guest_file(vm: &mut Vm, path: &str) -> Result<Vec<u8>, i32> {
    let fd = vm.vfs.open(path, 0, 0)?;
    let mut out = Vec::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        match vm.vfs.read(fd, &mut buf) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(e) => {
                let _ = vm.vfs.close(fd);
                return Err(e);
            }
        }
    }
    let _ = vm.vfs.close(fd);
    Ok(out)
}

fn register_dex_bytes(vm: &mut Vm, bytes: Vec<u8>) -> VmResult<u32> {
    let parsed = DexFile::parse(bytes)
        .map_err(|e| VmError::Internal(format!("DexFile: parse failed: {e}")))?;
    Ok(vm.add_shared_dex(std::sync::Arc::new(parsed)) as u32)
}

/// Register every `classes*.dex` entry of a zip container (APK/JAR).
fn register_zip(vm: &mut Vm, bytes: Vec<u8>) -> VmResult<Vec<u32>> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| VmError::Internal(format!("DexFile: not a zip container: {e}")))?;
    let mut names: Vec<String> = (0..archive.len())
        .filter_map(|i| archive.by_index_raw(i).ok().map(|f| f.name().to_string()))
        .filter(|n| is_dex_entry(n))
        .collect();
    names.sort();
    let mut indices = Vec::new();
    for name in names {
        let mut file = archive
            .by_name(&name)
            .map_err(|e| VmError::Internal(format!("DexFile: zip entry {name}: {e}")))?;
        let mut buf = Vec::with_capacity(file.size() as usize);
        file.read_to_end(&mut buf)
            .map_err(|e| VmError::Internal(format!("DexFile: read {name}: {e}")))?;
        indices.push(register_dex_bytes(vm, buf)?);
    }
    Ok(indices)
}

fn is_dex_entry(name: &str) -> bool {
    let Some(base) = name.rsplit('/').next() else {
        return false;
    };
    base == "classes.dex" || (base.starts_with("classes") && base.ends_with(".dex"))
}

fn bridge(e: crate::javabridge::BridgeError) -> VmError {
    VmError::Internal(format!("DexFile: {e}"))
}

fn looks_like_dex(bytes: &[u8]) -> bool {
    bytes.starts_with(b"dex\n") || bytes.starts_with(b"dey\n")
}

fn looks_like_zip(bytes: &[u8]) -> bool {
    bytes.starts_with(b"PK\x03\x04")
}

/// Open one guest source name and register its DEX providers.
fn open_source(vm: &mut Vm, source: &str) -> VmResult<OpenedDex> {
    // `DexFile(File)` also accepts a directory of `.dex` files.
    if let Ok(meta) = vm.vfs.stat(source, true) {
        if meta.mode & 0o170000 == 0o040000 {
            let entries = vm.vfs.list(source).map_err(|e| {
                vm.throw_new("Ljava/io/IOException;", Some(&format!("{source}: {e}")))
            })?;
            let mut dexs: Vec<String> = entries
                .into_iter()
                .filter(|n| n.ends_with(".dex"))
                .collect();
            dexs.sort();
            let mut indices = Vec::new();
            for name in dexs {
                let guest = format!("{source}/{name}");
                let bytes = read_guest_file(vm, &guest).map_err(|e| {
                    vm.throw_new(
                        "Ljava/io/IOException;",
                        Some(&format!("{guest}: errno {e}")),
                    )
                })?;
                indices.push(register_dex_bytes(vm, bytes)?);
            }
            return Ok(OpenedDex { indices });
        }
    }
    let bytes = read_guest_file(vm, source).map_err(|e| {
        vm.throw_new(
            "Ljava/io/IOException;",
            Some(&format!("{source}: errno {e}")),
        )
    })?;
    let indices = if looks_like_zip(&bytes) {
        register_zip(vm, bytes)?
    } else if looks_like_dex(&bytes) {
        vec![register_dex_bytes(vm, bytes)?]
    } else {
        return Err(vm.throw_new(
            "Ljava/io/IOException;",
            Some(&format!("{source}: not a DEX or zip container")),
        ));
    };
    if indices.is_empty() {
        return Err(vm.throw_new(
            "Ljava/io/IOException;",
            Some(&format!("{source}: no classes.dex entries")),
        ));
    }
    Ok(OpenedDex { indices })
}

// ---------------------------------------------------------------------------
// Natives
// ---------------------------------------------------------------------------

fn open_dex_file(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let source = vm
        .string_to_rust(vm.arg_ref(ctx, 0))
        .ok_or_else(|| vm.throw_new("Ljava/lang/NullPointerException;", Some("sourceName")))?;
    let opened = open_source(vm, &source)?;
    let cookie = new_cookie(vm, &opened.indices)?;
    vm.ret_ref(ctx, cookie);
    Ok(())
}

fn open_in_memory_dex_files(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let bufs = vm.arg_ref(ctx, 0);
    let arrays = vm.arg_ref(ctx, 1);
    let starts = vm.arg_ref(ctx, 2);
    let ends = vm.arg_ref(ctx, 3);
    let n = vm.array_length(bufs).unwrap_or(0);
    let mut indices = Vec::new();
    for i in 0..n {
        let start = int_array_at(vm, starts, i).unwrap_or(0).max(0) as usize;
        let end = int_array_at(vm, ends, i).unwrap_or(0).max(0) as usize;
        if end < start {
            return Err(vm.throw_new(
                "Ljava/lang/IllegalArgumentException;",
                Some("dex buffer end before start"),
            ));
        }
        let len = end - start;
        let array = match vm.get_array_element(arrays, i) {
            Ok(JavaValue::Object(h)) if h != NULL => Some(h),
            _ => None,
        };
        let bytes = match array {
            Some(h) => byte_slice(vm, h, start, len).ok_or_else(|| {
                vm.throw_new(
                    "Ljava/lang/IllegalArgumentException;",
                    Some("dex buffer is not a byte[]"),
                )
            })?,
            None => {
                // A direct buffer: use its `memoryRef.allocatedAddress`.
                let buf = match vm.get_array_element(bufs, i) {
                    Ok(JavaValue::Object(h)) if h != NULL => h,
                    _ => {
                        return Err(vm.throw_new(
                            "Ljava/lang/IllegalArgumentException;",
                            Some("null dex buffer"),
                        ));
                    }
                };
                let addr = direct_buffer_base(vm, buf).ok_or_else(|| {
                    vm.throw_new(
                        "Ljava/lang/IllegalArgumentException;",
                        Some("dex buffer has no address"),
                    )
                })?;
                super::native_mem::read_bytes_at(vm, addr + start as i64, len)?
            }
        };
        indices.push(register_dex_bytes(vm, bytes)?);
    }
    let cookie = new_cookie(vm, &indices)?;
    vm.ret_ref(ctx, cookie);
    Ok(())
}

fn int_array_at(vm: &Vm, h: Handle, i: usize) -> Option<i32> {
    match vm.heap.get(h)? {
        Object::Array(a) => match &a.data {
            ArrayData::Int(v) => v.get(i).copied(),
            _ => None,
        },
        _ => None,
    }
}

fn byte_slice(vm: &Vm, h: Handle, off: usize, len: usize) -> Option<Vec<u8>> {
    let Object::Array(a) = vm.heap.get(h)? else {
        return None;
    };
    let ArrayData::Byte(v) = &a.data else {
        return None;
    };
    let end = off.checked_add(len)?;
    if end > v.len() {
        return None;
    }
    Some(v[off..end].iter().map(|&b| b as u8).collect())
}

/// The base address of a direct `ByteBuffer` (the `memoryRef` allocated block).
fn direct_buffer_base(vm: &mut Vm, buf: Handle) -> Option<i64> {
    let mem = match vm.get_field(buf, "memoryRef") {
        Ok(JavaValue::Object(h)) if h != NULL => h,
        _ => return None,
    };
    match vm.get_field(mem, "allocatedAddress") {
        Ok(JavaValue::Long(addr)) if addr != 0 => Some(addr),
        _ => None,
    }
}

fn define_class(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let name = match vm.string_to_rust(vm.arg_ref(ctx, 0)) {
        Some(n) => n,
        None => {
            vm.ret_ref(ctx, NULL);
            return Ok(());
        }
    };
    let cookie = vm.arg_ref(ctx, 2);
    let dexs = cookie_dexs(vm, cookie)?;
    let desc = binary_name_to_desc(&name);
    for dex in dexs {
        let defines = vm.dexs[dex as usize].file.find_class_index(&desc).is_some();
        if !defines {
            continue;
        }
        let id = vm.load_class_from_dex(dex, &desc)?;
        let class = vm.class_object(id)?;
        vm.ret_ref(ctx, class);
        return Ok(());
    }
    // Not in this DexFile: ART returns null and the Java wrapper records a
    // `NoClassDefFoundError` in the suppressed list.
    vm.ret_ref(ctx, NULL);
    Ok(())
}

fn get_class_name_list(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let cookie = vm.arg_ref(ctx, 0);
    let dexs = cookie_dexs(vm, cookie)?;
    let mut names = Vec::new();
    for dex in dexs {
        let file = vm.dexs[dex as usize].file.clone();
        for def in file.class_defs() {
            if let Ok(desc) = file.class_descriptor(def) {
                names.push(desc_to_binary_name(desc));
            }
        }
    }
    let arr = vm
        .new_array_empty("java.lang.String", names.len())
        .map_err(bridge)?;
    vm.native_roots.push(arr);
    let result = (|| {
        for (i, name) in names.iter().enumerate() {
            let s = vm.new_string_rooted(name).map_err(bridge)?;
            vm.set_array_element(arr, i, JavaValue::Object(s))
                .map_err(bridge)?;
        }
        Ok(arr)
    })();
    vm.native_roots.pop();
    match result {
        Ok(arr) => {
            vm.ret_ref(ctx, arr);
            Ok(())
        }
        Err(e) => Err(e),
    }
}

fn close_dex_file(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    // The cookie is dropped by the Java object; the VM's DEX providers are
    // permanent (there is no class unloading). Reporting `true` means the
    // cookie was valid and nothing else is outstanding.
    let ok = vm.arg_ref(ctx, 0) != NULL;
    vm.ret_bool(ctx, ok);
    Ok(())
}

fn noop_cookie(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Ok(())
}

fn noop_verify(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Ok(())
}

fn ret_false(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_bool(ctx, false);
    Ok(())
}

fn ret_true(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_bool(ctx, true);
    Ok(())
}

fn ret_no_dexopt_needed(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    // `DexFile.NO_DEXOPT_NEEDED == 0`.
    vm.ret_i32(ctx, 0);
    Ok(())
}

fn ret_filter(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = vm.new_string("assume-verified")?;
    vm.ret_ref(ctx, h);
    Ok(())
}

/// `BaseDexClassLoader.computeClassLoaderContextsNative()`.
///
/// ART builds the dexopt class-loader-context chain here (the context strings
/// the runtime records per loaded dex so dexopt can re-optimise the whole
/// chain). ACOS runs the DEX directly and has no dexopt/oat pipeline, so there
/// is no context to hand to `DexLoadReporter`. `reportClassLoaderChain` treats
/// a zero-length array as "nothing to report" and returns without invoking the
/// reporter, which is the structural result of a chain with no dexopt work.
/// With a non-empty array the real `DexLoadReporter.report` would run and reach
/// `VMRuntime.notifyDexLoad` (also absent), so returning empty is the honest
/// terminal answer rather than a fabricated context that would only fail
/// downstream.
fn compute_class_loader_contexts(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let arr = vm.new_array_empty("java.lang.String", 0).map_err(bridge)?;
    vm.ret_ref(ctx, arr);
    Ok(())
}
