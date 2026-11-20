//! # Minimal test-only JNI harness (NOT the real ACOS JNI)
//!
//! This module implements just enough of the JNI `JNINativeInterface` and
//! `JNIInvokeInterface` ABI to run the ACOS `hello-jni` fixture and prove that
//! a real NDK-compiled `libhellojni.so` can call *into* a Rust-provided JNI
//! environment. The production implementation lives in the future `acos-jni`
//! crate; nothing here talks to the ACOS managed VM.
//!
//! Design:
//! * The full 229-slot `JNINativeInterface` table is present in exact spec
//!   order (`table_gen.rs`); implemented slots point at `jni_impl`, everything
//!   else points at a distinct aborting trap (`traps_gen.rs`).
//! * Variadic `Call*Method(...)` entry points are C trampolines (stable Rust
//!   cannot define C-variadic functions) that read the arguments using the
//!   method descriptor supplied by this harness and forward them here.
//! * A tiny fake object model models classes, strings, int arrays and
//!   throwables.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

pub mod jni_impl;
pub mod table_gen;
pub mod traps_gen;

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_void};
use std::sync::{Mutex, MutexGuard};

/// JNI version returned by `GetVersion` and `JNI_OnLoad` (`JNI_VERSION_1_6`).
pub const JNI_VERSION_1_6: i32 = 0x0001_0006;
/// `JNI_OK`.
pub const JNI_OK: i32 = 0;
/// `JNI_ERR`.
pub const JNI_ERR: i32 = -1;

/// Fake guest object. Opaque to guest code; we allocate these on the Rust heap
/// and hand out raw pointers.
#[repr(C)]
pub struct GObject(pub ObjKind);

/// Kind of a fake guest object.
pub enum ObjKind {
    Class(String),
    Instance(String),
    JString { s: String, c: Option<CString> },
    IntArray(Vec<i32>),
    Throwable { class: String, msg: String },
}

/// Fake `jmethodID`.
pub struct Method {
    pub class: String,
    pub name: String,
    pub sig: String,
    pub is_static: bool,
    /// One byte per argument: `L` (reference), `I`, `J`, `F`, `D`, `Z`, …
    pub arg_codes: CString,
}

/// A native method registered through `RegisterNatives`.
#[derive(Clone, Debug)]
pub struct RegisteredNative {
    pub class: String,
    pub name: String,
    pub signature: String,
    pub fn_ptr: usize,
}

/// A recorded call from native code into "Java".
#[derive(Clone, Debug)]
pub struct CallRecord {
    pub name: String,
    pub is_static: bool,
    pub args: Vec<Arg>,
}

/// Argument captured from a varargs JNI call.
#[derive(Clone, Debug)]
pub enum Arg {
    Int(i64),
    Ptr(usize),
    Double(f64),
}

impl Arg {
    pub fn int(&self) -> i64 {
        match self {
            Arg::Int(v) => *v,
            _ => i64::MIN,
        }
    }
    pub fn ptr(&self) -> usize {
        match self {
            Arg::Ptr(p) => *p,
            _ => 0,
        }
    }
}

/// Pending / last Java exception.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exception {
    pub class: String,
    pub message: String,
}

/// `JNINativeMethod` as passed to `RegisterNatives`.
#[repr(C)]
pub struct JNINativeMethod {
    pub name: *const c_char,
    pub signature: *const c_char,
    pub fn_ptr: *mut c_void,
}

/// Global harness state. Tests that use the harness must be serialised (the
/// integration tests take a process-wide lock).
pub struct State {
    /// Address of the `JNIEnv*` slot (`JNIEnv` is a pointer-to-pointer).
    pub env_slot: usize,
    /// Address of the `JavaVM*` slot.
    pub vm_slot: usize,
    /// Owns all method objects so returned `jmethodID`s stay valid.
    pub methods: Vec<Box<Method>>,
    pub registered: Vec<RegisteredNative>,
    pub calls: Vec<CallRecord>,
    pub pending: Option<Exception>,
    pub last_exception: Option<Exception>,
}

pub(crate) static STATE: Mutex<Option<State>> = Mutex::new(None);

pub(crate) fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut g: MutexGuard<Option<State>> = STATE.lock().expect("testjni state poisoned");
    let s = g.as_mut().expect("TestJni harness not installed");
    f(s)
}

/// Descriptor parser: produce one code per argument.
fn arg_codes(sig: &str) -> CString {
    let bytes = sig.as_bytes();
    let mut out = Vec::new();
    let mut i = match sig.find('(') {
        Some(p) => p + 1,
        None => return CString::new("").unwrap(),
    };
    while i < bytes.len() && bytes[i] != b')' {
        match bytes[i] {
            b'[' => {
                out.push(b'L');
                // Skip to the end of the element type.
                while i < bytes.len() && bytes[i] == b'[' {
                    i += 1;
                }
                if i < bytes.len() && bytes[i] == b'L' {
                    while i < bytes.len() && bytes[i] != b';' {
                        i += 1;
                    }
                }
                i += 1;
            }
            b'L' => {
                out.push(b'L');
                while i < bytes.len() && bytes[i] != b';' {
                    i += 1;
                }
                i += 1;
            }
            c if b"IJFDZBCS".contains(&c) => {
                out.push(c);
                i += 1;
            }
            _ => i += 1,
        }
    }
    CString::new(out).unwrap_or_else(|_| CString::new("").unwrap())
}

/// A fake object pointer.
pub type JObject = *mut GObject;

fn alloc_obj(kind: ObjKind) -> JObject {
    Box::into_raw(Box::new(GObject(kind)))
}

/// Safe pointer to a fake object.
unsafe fn obj<'a>(p: usize) -> Option<&'a GObject> {
    if p == 0 {
        None
    } else {
        Some(&*(p as *const GObject))
    }
}

/// Mutable pointer to a fake object.
unsafe fn obj_mut<'a>(p: usize) -> Option<&'a mut GObject> {
    if p == 0 {
        None
    } else {
        Some(&mut *(p as *mut GObject))
    }
}

/// Install a fresh harness and return a handle. Any previous state is dropped.
pub struct TestJni {
    env: usize,
    vm: usize,
}

impl TestJni {
    /// Install (or reinstall) the harness.
    pub fn install() -> TestJni {
        // `JNIEnv`/`JavaVM` are pointers to a slot holding the table pointer
        // (the C struct also starts with 4 reserved pointer slots).
        let table = Box::leak(Box::new(table_gen::native_interface_table()));
        let env_table = table.as_mut_ptr() as usize;
        let invoke = Box::leak(Box::new(table_gen::invoke_interface_table()));
        let vm_table = invoke.as_mut_ptr() as usize;
        let env_slot = Box::leak(Box::new(env_table)) as *mut usize as usize;
        let vm_slot = Box::leak(Box::new(vm_table)) as *mut usize as usize;
        let env = env_slot;
        let vm = vm_slot;

        let mut methods: Vec<Box<Method>> = Vec::new();
        for (class, name, sig, is_static) in [
            (
                "dev/acos/fixtures/hellojni/MainActivity",
                "voidCallback",
                "(Ljava/lang/String;)V",
                false,
            ),
            (
                "dev/acos/fixtures/hellojni/MainActivity",
                "javaCallback",
                "(I)I",
                true,
            ),
        ] {
            methods.push(Box::new(Method {
                class: class.to_string(),
                name: name.to_string(),
                sig: sig.to_string(),
                is_static,
                arg_codes: arg_codes(sig),
            }));
        }

        let state = State {
            env_slot,
            vm_slot,
            methods,
            registered: Vec::new(),
            calls: Vec::new(),
            pending: None,
            last_exception: None,
        };
        *STATE.lock().unwrap() = Some(state);
        TestJni { env, vm }
    }

    /// `JNIEnv*` to pass to guest code.
    pub fn env(&self) -> *mut c_void {
        self.env as *mut c_void
    }

    /// `JavaVM*` to pass to `JNI_OnLoad`.
    pub fn vm(&self) -> *mut c_void {
        self.vm as *mut c_void
    }

    /// Allocate a fake `jstring`.
    pub fn new_string(&self, s: &str) -> JObject {
        alloc_obj(ObjKind::JString {
            s: s.to_string(),
            c: None,
        })
    }

    /// Allocate a fake `jintArray`.
    pub fn new_int_array(&self, v: &[i32]) -> JObject {
        alloc_obj(ObjKind::IntArray(v.to_vec()))
    }

    /// Allocate a fake instance of `class`.
    pub fn new_instance(&self, class: &str) -> JObject {
        alloc_obj(ObjKind::Instance(class.to_string()))
    }

    /// Read the string contents of a fake `jstring`.
    pub fn string_of(&self, p: JObject) -> String {
        // SAFETY: caller supplies a pointer produced by this harness.
        unsafe {
            match obj(p as usize) {
                Some(GObject(ObjKind::JString { s, .. })) => s.clone(),
                _ => String::new(),
            }
        }
    }

    /// Registered native methods (from `RegisterNatives`).
    pub fn registered(&self) -> Vec<RegisteredNative> {
        with_state(|s| s.registered.clone())
    }

    /// Recorded native → Java calls.
    pub fn calls(&self) -> Vec<CallRecord> {
        with_state(|s| s.calls.clone())
    }

    /// Clear the recorded call list.
    pub fn clear_calls(&self) {
        with_state(|s| s.calls.clear())
    }

    /// Pending exception, if any.
    pub fn pending_exception(&self) -> Option<Exception> {
        with_state(|s| s.pending.clone().or_else(|| s.last_exception.clone()))
    }
}

// --- helpers shared with jni_impl -----------------------------------------

pub(crate) fn find_method(
    state: &State,
    class: &str,
    name: &str,
    sig: &str,
    is_static: bool,
) -> usize {
    for m in &state.methods {
        if m.class == class && m.name == name && m.sig == sig && m.is_static == is_static {
            return m.as_ref() as *const Method as usize;
        }
    }
    0
}

pub(crate) fn method_name(state: &State, mid: usize) -> Option<(String, CString, bool)> {
    if mid == 0 {
        return None;
    }
    // SAFETY: `mid` was returned by `find_method`, pointing into `state.methods`.
    let m = unsafe { &*(mid as *const Method) };
    let _ = state;
    Some((m.name.clone(), m.arg_codes.clone(), m.is_static))
}

pub(crate) fn class_name_of(p: usize) -> Option<String> {
    // SAFETY: pointer is a harness object.
    unsafe {
        obj(p).map(|o| match &o.0 {
            ObjKind::Class(c) => c.clone(),
            ObjKind::Instance(c) => c.clone(),
            ObjKind::JString { .. } => "java/lang/String".to_string(),
            ObjKind::IntArray(_) => "[I".to_string(),
            ObjKind::Throwable { class, .. } => class.clone(),
        })
    }
}

pub(crate) fn cstr(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: caller guarantees a NUL-terminated C string.
    unsafe { CStr::from_ptr(p).to_string_lossy().into_owned() }
}

// --- Rust side of the C variadic trampolines -------------------------------

/// Used by the C varargs shims to discover argument kinds. Returns a pointer to
/// a NUL-terminated string with one code byte per argument. The pointer refers
/// to the `Method` owned by the harness state, so it stays valid.
#[no_mangle]
pub extern "C" fn acos_tj_arg_types(mid: usize) -> *const c_char {
    if mid == 0 {
        return std::ptr::null();
    }
    with_state(|_s| {
        // SAFETY: `mid` was returned by `find_method` and points at a boxed
        // `Method` owned by the harness state; it is never mutated.
        let m = unsafe { &*(mid as *const Method) };
        m.arg_codes.as_ptr()
    })
}

/// Used by the C varargs shims to perform the call.
#[no_mangle]
pub extern "C" fn acos_tj_call(
    _env: usize,
    target: usize,
    mid: usize,
    is_static: i32,
    args: *const u64,
    nargs: i32,
) -> u64 {
    let raw: Vec<u64> = if args.is_null() || nargs <= 0 {
        Vec::new()
    } else {
        // SAFETY: the C shim passes a valid array of `nargs` elements.
        unsafe { std::slice::from_raw_parts(args, nargs as usize).to_vec() }
    };
    perform_call(target, mid, is_static != 0, &raw)
}

/// Shared implementation for varargs (via C) and array-form (`*A`) calls.
pub(crate) fn perform_call(_target: usize, mid: usize, is_static: bool, raw: &[u64]) -> u64 {
    with_state(|s| {
        let (name, codes, m_is_static) = match method_name(s, mid) {
            Some(v) => v,
            None => return 0,
        };
        let code_bytes = codes.to_bytes();
        let mut decoded = Vec::new();
        for (i, &c) in code_bytes.iter().enumerate() {
            let v = raw.get(i).copied().unwrap_or(0);
            match c {
                b'L' => decoded.push(Arg::Ptr(v as usize)),
                b'D' | b'F' => decoded.push(Arg::Double(f64::from_bits(v))),
                _ => decoded.push(Arg::Int(v as i64)),
            }
        }
        s.calls.push(CallRecord {
            name: name.clone(),
            is_static: is_static || m_is_static,
            args: decoded.clone(),
        });
        match name.as_str() {
            "javaCallback" => {
                let x = decoded.first().map(|a| a.int()).unwrap_or(0);
                (x * 2) as u64
            }
            _ => 0,
        }
    })
}

// --- Rust side of the android logging shims --------------------------------

/// Log sink installed by the loader, if any.
pub(crate) static LOG_SINK: Mutex<Option<std::sync::Arc<dyn crate::LogSink>>> = Mutex::new(None);

/// Called by the C `__android_log_print` family.
#[no_mangle]
pub extern "C" fn acos_log_write(prio: i32, tag: *const c_char, msg: *const c_char) {
    let tag = cstr(tag);
    let msg = cstr(msg);
    let sink = LOG_SINK.lock().ok().and_then(|g| g.clone());
    if let Some(s) = sink {
        s.log(prio, &tag, &msg);
    } else {
        eprintln!("[android-log {prio}] {tag}: {msg}");
    }
}

/// Called by the C `__android_log_assert` shim (host abort follows).
#[no_mangle]
pub extern "C" fn acos_log_assert(cond: *const c_char, tag: *const c_char, msg: *const c_char) {
    let cond = cstr(cond);
    let tag = cstr(tag);
    let msg = cstr(msg);
    eprintln!("[android-assert] {tag}: {cond}: {msg}");
}

/// Install (or clear) the process-wide guest log sink.
pub fn set_log_sink(sink: Option<std::sync::Arc<dyn crate::LogSink>>) {
    *LOG_SINK.lock().unwrap() = sink;
}
