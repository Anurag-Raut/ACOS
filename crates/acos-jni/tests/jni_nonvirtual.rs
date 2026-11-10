//! `CallNonvirtual*Method` semantics through the real C ABI.
//!
//! The fixture DEX (`tests/data/nonvirtual.dex`, compiled from
//! `tests/java/dev/acos/fixtures/nonvirtual/`) contains a two-class hierarchy:
//! `Derived.who()` overrides `Base.who()`. Calling the same `jmethodID`
//! (derived from `Base`) virtually must reach `Derived.who()`, while
//! `CallNonvirtual*Method(..., Base, mid)` must reach `Base.who()`.
//!
//! The calls go through the C varargs shims (`acosrjni_Call*MethodA`) that the
//! function table installs, so both the JNI ABI and the Rust implementation are
//! exercised.

use std::ffi::{CStr, CString};
use std::path::Path;

use acos_dex::DexFile;
use acos_jni::{jni, JniConfig, JniRuntime};
use acos_vm::{Vm, VmOptions};

extern "C" {
    fn acosrjni_CallObjectMethodA(
        env: usize,
        obj: usize,
        mid: usize,
        args: *const u64,
    ) -> usize;
    fn acosrjni_CallNonvirtualObjectMethodA(
        env: usize,
        obj: usize,
        cls: usize,
        mid: usize,
        args: *const u64,
    ) -> usize;
}

fn setup() -> (Vm, JniRuntime) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/nonvirtual.dex");
    let dex = DexFile::parse(std::fs::read(&path).expect("fixture dex")).expect("parse dex");
    let mut vm = Vm::new(VmOptions {
        heap_max_bytes: 64 * 1024 * 1024,
        gc_stress: std::env::var_os("ACOS_GC_STRESS").is_some(),
        ..VmOptions::default()
    })
    .expect("VM");
    vm.add_dex(dex);
    let rt = JniRuntime::new(JniConfig::default());
    // `attach` deliberately does not retain a raw `Vm` pointer: the value is
    // moved when this function returns. `bind` must run in the test once the
    // `Vm` is at its final address (see JNI_COVERAGE.md, W33-A correction).
    rt.attach(&mut vm).expect("attach");
    (vm, rt)
}

fn c(s: &str) -> CString {
    CString::new(s).unwrap()
}

fn find_class(env: usize, name: &str) -> usize {
    let n = c(name);
    // SAFETY: the runtime is attached and the string is NUL-terminated.
    unsafe { jni::jni_FindClass(env, n.as_ptr()) }
}

fn method(env: usize, class: usize, name: &str, sig: &str) -> usize {
    let (n, s) = (c(name), c(sig));
    // SAFETY: class is a live jclass from FindClass.
    unsafe { jni::jni_GetMethodID(env, class, n.as_ptr(), s.as_ptr()) }
}

fn string_of(env: usize, handle: usize) -> String {
    assert_ne!(handle, 0, "null jstring");
    // SAFETY: handle is a live java.lang.String.
    unsafe {
        let chars = jni::jni_GetStringUTFChars(env, handle, std::ptr::null_mut());
        assert!(!chars.is_null(), "GetStringUTFChars returned null");
        let text = CStr::from_ptr(chars).to_string_lossy().into_owned();
        jni::jni_ReleaseStringUTFChars(env, handle, chars);
        text
    }
}

#[test]
fn nonvirtual_call_uses_the_named_class_not_the_override() {
    let (mut vm, rt) = setup();
    rt.bind(&mut vm);
    let env = rt.env() as usize;

    let base = find_class(env, "dev/acos/fixtures/nonvirtual/Base");
    let derived = find_class(env, "dev/acos/fixtures/nonvirtual/Derived");
    assert_ne!(base, 0, "FindClass(Base): {:?}", rt.take_error());
    assert_ne!(derived, 0, "FindClass(Derived): {:?}", rt.take_error());

    // Allocate a Derived without running a constructor.
    // SAFETY: `derived` is a live jclass.
    let obj = unsafe { jni::jni_AllocObject(env, derived) };
    assert_ne!(obj, 0, "AllocObject(Derived): {:?}", rt.take_error());

    let mid = method(env, base, "who", "()Ljava/lang/String;");
    assert_ne!(mid, 0, "GetMethodID(Base.who): {:?}", rt.take_error());

    // Virtual dispatch sees the runtime class (Derived).
    // SAFETY: live receiver, method ID and no arguments.
    let virtual_result = unsafe {
        acosrjni_CallObjectMethodA(env, obj, mid, std::ptr::null())
    };
    // Non-virtual dispatch resolves `who` on Base.
    // SAFETY: as above; `base` is the class the method ID was derived from.
    let nonvirtual_result = unsafe {
        acosrjni_CallNonvirtualObjectMethodA(env, obj, base, mid, std::ptr::null())
    };

    assert_eq!(rt.take_error(), None, "no structured JNI error");
    assert_eq!(string_of(env, virtual_result), "derived");
    assert_eq!(string_of(env, nonvirtual_result), "base");

    drop(rt);
    drop(vm);
}
