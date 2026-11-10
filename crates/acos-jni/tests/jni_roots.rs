//! Root-accounting regression for JNI throw/clear.
//!
//! `Throw`/`ThrowNew` install a pending exception and push one `native_roots`
//! entry; `ExceptionClear` must release exactly that entry and `ExceptionOccurred`
//! / `DeleteLocalRef` must round-trip their local root one-for-one. A prior
//! report of `native_roots` corruption came from binding the runtime before the
//! `Vm` reached its final address, not from the accounting itself, so these
//! tests pin the balanced behaviour with the canonical `attach`-then-`bind`
//! pattern.

use std::ffi::CString;

use acos_jni::jni;
use acos_jni::{JniConfig, JniRuntime};
use acos_vm::{Vm, VmOptions};

fn setup() -> (Vm, JniRuntime) {
    let mut vm = Vm::new(VmOptions {
        heap_max_bytes: 64 * 1024 * 1024,
        gc_stress: std::env::var_os("ACOS_GC_STRESS").is_some(),
        ..VmOptions::default()
    })
    .unwrap();
    let rt = JniRuntime::new(JniConfig::default());
    rt.attach(&mut vm).expect("attach");
    // `attach` intentionally does not retain a `*mut Vm`; bind only after the
    // caller has moved `vm` into its final location.
    (vm, rt)
}

fn c(s: &str) -> CString {
    CString::new(s).unwrap()
}

#[test]
fn throw_new_then_clear_is_root_balanced() {
    let (mut vm, rt) = setup();
    rt.bind(&mut vm);
    let env = rt.env() as usize;
    let name = c("java/lang/IllegalStateException");
    let cls = unsafe { jni::jni_FindClass(env, name.as_ptr()) };
    assert_ne!(cls, 0);
    let msg = c("boom");
    let base = vm.native_roots.len();
    assert_eq!(unsafe { jni::jni_ThrowNew(env, cls, msg.as_ptr()) }, 0);
    let after_throw = vm.native_roots.len();
    assert_eq!(after_throw, base + 1, "ThrowNew must push exactly one root");
    unsafe { jni::jni_ExceptionClear(env) };
    let after_clear = vm.native_roots.len();
    assert_eq!(after_clear, base, "ExceptionClear must release the pending root");
    drop(rt);
    drop(vm);
}

#[test]
fn throw_then_delete_local_then_clear_releases_each_root() {
    let (mut vm, rt) = setup();
    rt.bind(&mut vm);
    let env = rt.env() as usize;
    let name = c("java/lang/IllegalStateException");
    let cls = unsafe { jni::jni_FindClass(env, name.as_ptr()) };
    assert_ne!(cls, 0);
    let msg = c("boom");
    assert_eq!(unsafe { jni::jni_ThrowNew(env, cls, msg.as_ptr()) }, 0);
    let thrown = unsafe { jni::jni_ExceptionOccurred(env) };
    assert_ne!(thrown, 0);
    // `thrown` added one local root on top of the pending root.
    let base = vm.native_roots.len();
    unsafe { jni::jni_DeleteLocalRef(env, thrown) };
    let after_delete = vm.native_roots.len();
    assert_eq!(after_delete, base - 1, "DeleteLocalRef must release the local root");
    unsafe { jni::jni_ExceptionClear(env) };
    let after_clear = vm.native_roots.len();
    assert_eq!(
        after_clear,
        base - 2,
        "ExceptionClear must release the pending root after the local ref is gone"
    );
    drop(rt);
    drop(vm);
}
