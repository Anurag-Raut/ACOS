//! Bridge from DEX `native` declarations to the host JNI implementation.
//!
//! `acos-vm` must stay `#![forbid(unsafe_code)]` and cannot call guest code:
//! the JNI function table, ELF stack switching and the C ABI all live in the
//! separate `acos-jni` crate. This module is the only seam between them:
//!
//! * it registers the platform natives that load a library
//!   (`System.loadLibrary`, `Runtime.nativeLoad`, ...) and forwards the request
//!   to a host callback;
//! * [`dispatch_native`] is installed as the Rust implementation of every DEX
//!   `native` method the host knows about. It copies the incoming DEX arguments
//!   into a plain [`NativeCall`], calls the host, and writes the result back into
//!   the interpreter frame.
//!
//! Everything here is safe Rust. The host callback receives a raw `*mut Vm`
//! because the JNI implementation must be able to re-enter the VM from guest
//! code (`Call*Method`, `FindClass`, ...); the pointer is only dereferenced by
//! the host, never by this module after handing it over.

use std::sync::Mutex;

use crate::class::{type_width, NativeCtx, NativeFn, TypeKind};
use crate::error::{VmError, VmResult};
use crate::heap::{Handle, NULL};
use crate::vm::Vm;

/// A value crossing the native JNI boundary (already decoded from DEX slots).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum JniValue {
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    Ref(Handle),
}

/// The result a guest native returns to the interpreter.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum JniReturn {
    Void,
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    Ref(Handle),
}

/// One Java → native invocation, described without any VM internals.
#[derive(Clone, Debug)]
pub struct NativeCall {
    /// Declaring class descriptor, e.g. `Ldev/acos/fixtures/hellojni/MainActivity;`.
    pub class: String,
    pub name: String,
    pub sig: String,
    /// `jobject` for instance methods, [`NULL`] for static methods.
    pub this: Handle,
    pub args: Vec<JniValue>,
    /// Return descriptor (`V`, `I`, `Ljava/lang/String;`, ...).
    pub ret: String,
}

/// A structured native failure the interpreter can propagate.
#[derive(Clone, Debug)]
pub enum CallFailure {
    /// A Java throwable is pending (already a live, rooted handle).
    JavaException(Handle),
    /// The JNI entry point is not implemented.
    Unsupported { api: String, at: String },
    /// A JNI contract violation (pending exception, bad reference, wrong type,
    /// missing symbol, ...). `code` is a stable `JNI_*` string.
    Structured { code: String, message: String },
}

/// Host callback that loads a library (`System.loadLibrary`). Returns the
/// resolved path (the value of `Runtime.nativeLoad`).
pub type LoadLibraryFn = fn(*mut Vm, &str) -> Result<String, CallFailure>;

/// Host callback that invokes a guest native.
pub type CallNativeFn = fn(*mut Vm, NativeCall) -> Result<JniReturn, CallFailure>;

/// The installed JNI host.
#[derive(Clone, Copy, Debug)]
pub struct Host {
    pub load_library: LoadLibraryFn,
    pub call_native: CallNativeFn,
}

static HOST: Mutex<Option<Host>> = Mutex::new(None);

/// Install the JNI host. Idempotent; later calls replace the previous host.
pub fn set_host(host: Host) {
    *HOST.lock().unwrap_or_else(|e| e.into_inner()) = Some(host);
}

/// Remove the installed host (used by tests and teardown).
pub fn clear_host() {
    *HOST.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

fn host() -> Option<Host> {
    *HOST.lock().unwrap_or_else(|e| e.into_inner())
}

/// Whether a JNI host is installed (a `--native` product launch has attached
/// `acos-jni`). The framework edge (`natives::fwk_jni`) uses this to decide
/// between the real JNI loader and its own "served by Rust natives" stub.
pub fn host_installed() -> bool {
    host().is_some()
}

/// Register the DEX-level library-loading natives. Safe to call more than once.
pub fn install(vm: &mut Vm) -> VmResult<()> {
    // `java.lang.System.loadLibrary(String)` / `load(String)` are ordinary Java
    // wrappers on Android, but the fixture builtins and older boot jars declare
    // them native; registering here is harmless and makes both tiers work.
    vm.register_native(
        "Ljava/lang/System;",
        "loadLibrary",
        "(Ljava/lang/String;)V",
        load_library,
    );
    vm.register_native(
        "Ljava/lang/System;",
        "load",
        "(Ljava/lang/String;)V",
        load_library,
    );

    // The real Android 14 core library (`Runtime.loadLibrary0` →
    // `nativeLoad`). Both historical signatures are registered.
    vm.register_native(
        "Ljava/lang/Runtime;",
        "nativeLoad",
        "(Ljava/lang/String;Ljava/lang/ClassLoader;)Ljava/lang/String;",
        native_load,
    );
    vm.register_native(
        "Ljava/lang/Runtime;",
        "nativeLoad",
        "(Ljava/lang/String;Ljava/lang/ClassLoader;Ljava/lang/Class;)Ljava/lang/String;",
        native_load,
    );
    vm.register_native(
        "Ljava/lang/Runtime;",
        "loadLibrary0",
        "(Ljava/lang/ClassLoader;Ljava/lang/String;)V",
        load_library_runtime,
    );
    vm.register_native(
        "Ljava/lang/Runtime;",
        "loadLibrary0",
        "(Ljava/lang/Class;Ljava/lang/String;)V",
        load_library_runtime,
    );
    Ok(())
}

fn arg_string(vm: &Vm, ctx: NativeCtx, n: usize) -> Result<String, VmError> {
    let h = vm.arg_ref(ctx, n);
    if h == NULL {
        return Err(VmError::Internal("JNI: null library name".into()));
    }
    vm.string_to_rust(h)
        .ok_or_else(|| VmError::Internal("JNI: library name is not a String".into()))
}

fn library_name_arg(vm: &Vm, ctx: NativeCtx) -> Result<String, VmError> {
    // System.loadLibrary(String) -> arg 0; Runtime.nativeLoad(String, ...) -> arg 0;
    // Runtime.loadLibrary0(ClassLoader, String) -> arg 1, (Class, String) -> arg 1.
    let mid = vm.ctx_method_id(ctx);
    let sig = vm.methods[mid as usize].sig.clone();
    if sig.starts_with("(Ljava/lang/ClassLoader;Ljava/lang/String;")
        || sig.starts_with("(Ljava/lang/Class;Ljava/lang/String;")
    {
        arg_string(vm, ctx, 1)
    } else {
        arg_string(vm, ctx, 0)
    }
}

fn map_load_failure(vm: &mut Vm, e: CallFailure, what: &str) -> VmError {
    match e {
        CallFailure::JavaException(h) => VmError::JavaException(h),
        CallFailure::Unsupported { api, at } => VmError::unsupported_api(api, at),
        CallFailure::Structured { code, message } => {
            // A failed load is an `UnsatisfiedLinkError` on Android. Fall back
            // to the structured VM error if the class cannot be built.
            let msg = format!("{code}: {message} (while loading {what})");
            match vm.new_throwable("Ljava/lang/UnsatisfiedLinkError;", Some(&msg)) {
                Ok(h) => VmError::JavaException(h),
                Err(_) => VmError::Internal(msg),
            }
        }
    }
}

pub fn load_library_impl(vm: &mut Vm, ctx: NativeCtx, return_path: bool) -> VmResult<()> {
    let name = library_name_arg(vm, ctx)?;
    let h = host().ok_or_else(|| {
        VmError::unsupported_api(
            "JNI library loader",
            "no JNI host is installed; re-run with `acos launch --native` (ACOS_NATIVE=1) \
             to run the APK's confined native code",
        )
    })?;
    let vm_ptr: *mut Vm = vm;
    match (h.load_library)(vm_ptr, &name) {
        Ok(path) => {
            if return_path {
                let s = vm.new_string(&path)?;
                vm.ret_ref(ctx, s);
            } else {
                vm.ret_u32(ctx, 0);
            }
            Ok(())
        }
        Err(e) => Err(map_load_failure(vm, e, &name)),
    }
}

fn load_library(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    load_library_impl(vm, ctx, false)
}

fn load_library_runtime(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    load_library_impl(vm, ctx, false)
}

fn native_load(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    load_library_impl(vm, ctx, true)
}

/// Rust implementation installed for every DEX `native` method.
///
/// The host decides which guest function to call; this function only performs
/// the safe DEX-frame marshalling.
pub fn dispatch_native(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let mid = vm.ctx_method_id(ctx);
    let (class, name, sig, ret, param_kinds) = {
        let m = &vm.methods[mid as usize];
        let class = vm.class_desc(m.class).to_string();
        let params = m.params.clone();
        let kinds = params
            .iter()
            .map(|p| vm.type_kind(*p))
            .collect::<Vec<TypeKind>>();
        (class, m.name.clone(), m.sig.clone(), m.ret, kinds)
    };

    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let mut args = Vec::with_capacity(param_kinds.len());
    for (i, kind) in param_kinds.iter().enumerate() {
        args.push(match kind {
            TypeKind::Boolean
            | TypeKind::Byte
            | TypeKind::Char
            | TypeKind::Short
            | TypeKind::Int => JniValue::Int(vm.arg_i32(ctx, i)),
            TypeKind::Long => JniValue::Long(vm.arg_i64(ctx, i)),
            TypeKind::Float => JniValue::Float(vm.arg_f32(ctx, i)),
            TypeKind::Double => JniValue::Double(vm.arg_f64(ctx, i)),
            TypeKind::Reference => JniValue::Ref(vm.arg_ref(ctx, i)),
            TypeKind::Void => {
                return Err(VmError::Internal(format!(
                    "native method {class}->{name}{sig} has a void parameter"
                )))
            }
        });
    }

    let ret_desc = vm.type_desc(ret).to_string();
    let call = NativeCall {
        class: class.clone(),
        name: name.clone(),
        sig: sig.clone(),
        this,
        args,
        ret: ret_desc.clone(),
    };
    let h = host().ok_or_else(|| {
        VmError::unsupported_api(
            format!("{class}->{name}{sig}"),
            "no JNI host is installed; re-run with `acos launch --native` (ACOS_NATIVE=1) \
             to run the APK's confined native code",
        )
    })?;
    let vm_ptr: *mut Vm = vm;
    let result = (h.call_native)(vm_ptr, call);

    match result {
        Ok(value) => {
            let kind = TypeKind::from_descriptor(&ret_desc);
            let mismatch = || {
                VmError::Internal(format!(
                    "JNI native {class}->{name}{sig} returned {:?} for {ret_desc}",
                    value
                ))
            };
            match (kind, value) {
                (TypeKind::Void, JniReturn::Void) => {}
                (TypeKind::Boolean, JniReturn::Int(v)) => vm.ret_u32(ctx, (v != 0) as u32),
                (TypeKind::Byte, JniReturn::Int(v)) => vm.ret_u32(ctx, v as u8 as u32),
                (TypeKind::Char, JniReturn::Int(v)) => vm.ret_u32(ctx, v as u16 as u32),
                (TypeKind::Short, JniReturn::Int(v)) => vm.ret_u32(ctx, v as u16 as u32),
                (TypeKind::Int, JniReturn::Int(v)) => vm.ret_i32(ctx, v),
                (TypeKind::Long, JniReturn::Long(v)) => vm.ret_i64(ctx, v),
                (TypeKind::Float, JniReturn::Float(v)) => vm.ret_f32(ctx, v),
                (TypeKind::Double, JniReturn::Double(v)) => vm.ret_f64(ctx, v),
                (TypeKind::Reference, JniReturn::Ref(h)) => {
                    if h != NULL && !vm.heap.is_live(h) {
                        return Err(VmError::Internal(format!(
                            "JNI native {class}->{name}{sig} returned dead handle {h}"
                        )));
                    }
                    vm.ret_ref(ctx, h);
                }
                _ => return Err(mismatch()),
            }
            Ok(())
        }
        Err(CallFailure::JavaException(h)) => Err(VmError::JavaException(h)),
        Err(CallFailure::Unsupported { api, at }) => Err(VmError::unsupported_api(api, at)),
        Err(CallFailure::Structured { code, message }) => {
            Err(VmError::Internal(format!("{code}: {message}")))
        }
    }
}

/// The native function the host installs for a DEX native method.
pub fn bridge() -> NativeFn {
    dispatch_native
}

// ---------------------------------------------------------------------------
// Public native-memory surface for JNI direct buffers
// ---------------------------------------------------------------------------
//
// `NewDirectByteBuffer` wraps a process address that, in ACOS, is a synthetic
// (region id << 32 | offset) handle into the VM-owned native-memory substrate.
// The JNI crate cannot name `natives::native_mem` (it is `pub(crate)`), so this
// host module re-exports the small surface it needs. Addresses are opaque
// `i64`s; nothing here ever dereferences a host pointer.

/// Allocate a zero-filled native region and return its synthetic address.
pub fn native_mem_alloc(len: usize) -> Result<i64, String> {
    super::native_mem::alloc(len, false).map_err(|e| format!("{e:?}"))
}

/// True when `addr` names a live region with at least `len` accessible bytes.
pub fn native_mem_check(addr: i64, len: usize) -> bool {
    super::native_mem::with_bytes(addr, len, |_| ()).is_ok()
}

/// Read `len` bytes from a synthetic native region.
pub fn native_mem_read(addr: i64, len: usize) -> Result<Vec<u8>, String> {
    super::native_mem::read_bytes(addr, len).map_err(|e| format!("{e:?}"))
}

/// Real host address backing a synthetic `DirectByteBuffer`/`addressOf` alias.
///
/// Unlike the rest of this surface this deliberately exposes a host pointer, but
/// only to native code that dereferences `GetDirectBufferAddress` (the JNI
/// carrier in `acos-jni`). It is derived safely from the backing slice; the
/// caller must use it immediately and may not retain it across a Java call.
pub fn native_mem_host_pointer(vm: &crate::vm::Vm, addr: i64, len: usize) -> Option<usize> {
    super::native_mem::host_pointer(vm, addr, len)
}

/// Write `bytes` into a synthetic native region.
pub fn native_mem_write(addr: i64, bytes: &[u8]) -> Result<(), String> {
    super::native_mem::write_bytes(addr, bytes).map_err(|e| format!("{e:?}"))
}

/// Free a synthetic native region. Returns whether it was live.
pub fn native_mem_free(addr: i64) -> bool {
    super::native_mem::free(addr)
}

/// True when a method's argument list fits the JNI host marshalling rules.
///
/// Used by the host's DEX scan to reject impossible declarations early.
pub fn supported_signature(sig: &str) -> bool {
    crate::class::parse_signature(sig)
        .map(|(params, _)| {
            params
                .iter()
                .all(|p| type_width(TypeKind::from_descriptor(p)) <= 2)
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_widths_are_accepted() {
        assert!(supported_signature("(IJLjava/lang/String;[I)D"));
        assert!(!supported_signature("not a signature"));
    }
}
