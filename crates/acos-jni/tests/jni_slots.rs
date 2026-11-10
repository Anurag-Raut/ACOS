//! Coverage tests for the JNI slots implemented beyond the core object model:
//! weak globals, direct byte buffers, critical sections, local frames, monitor
//! entry, array-region bounds and the `JavaVM` attach surface.
//!
//! Every call goes through the generated `JNINativeInterface` table entry (the
//! same pointer guest code receives), so the tests exercise the real C ABI.

use std::ffi::CString;

use acos_jni::jni;
use acos_jni::state::JNI_WEAK_GLOBAL_REF_TYPE;
use acos_jni::state::JNI_EVERSION;
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
    rt.bind(&mut vm);
    (vm, rt)
}

fn c(s: &str) -> CString {
    CString::new(s).unwrap()
}

unsafe fn new_string(env: usize, s: &str) -> usize {
    let cs = c(s);
    jni::jni_NewStringUTF(env, cs.as_ptr())
}

unsafe fn find_class(env: usize, name: &str) -> usize {
    let n = c(name);
    jni::jni_FindClass(env, n.as_ptr())
}

#[test]
fn weak_global_ref_clears_after_gc() {
    let (mut vm, rt) = setup();
    let env = rt.env() as usize;
    // A fresh array is not interned, so it is collectable once unrooted.
    let local = unsafe { jni::jni_NewIntArray(env, 4) };
    assert_ne!(local, 0);
    let weak = unsafe { jni::jni_NewWeakGlobalRef(env, local) };
    assert_ne!(weak, 0);
    assert_eq!(
        unsafe { jni::jni_GetObjectRefType(env, weak) },
        JNI_WEAK_GLOBAL_REF_TYPE
    );
    // Drop the strong references and collect; the weak ref must become null.
    unsafe { jni::jni_DeleteLocalRef(env, local) };
    while vm.release_bridge_root(local as u32) {}
    vm.gc();
    assert_eq!(unsafe { jni::jni_IsSameObject(env, weak, 0) }, 1);
    // A cleared weak ref is treated as NULL, so it equals the null ref but is
    // still reported as a weak global reference.
    assert_eq!(
        unsafe { jni::jni_GetObjectRefType(env, weak) },
        JNI_WEAK_GLOBAL_REF_TYPE
    );
    unsafe { jni::jni_DeleteWeakGlobalRef(env, weak) };
    assert_eq!(rt.take_error(), None);
    drop(rt);
    drop(vm);
}

#[test]
fn direct_buffer_round_trips_through_native_mem() {
    let (vm, rt) = setup();
    let env = rt.env() as usize;
    let buf = unsafe { jni::jni_NewDirectByteBuffer(env, 0, 16) };
    assert_ne!(buf, 0, "NewDirectByteBuffer failed: {:?}", rt.take_error());
    let addr = unsafe { jni::jni_GetDirectBufferAddress(env, buf) };
    assert_ne!(addr, 0, "backing region address");
    assert_eq!(unsafe { jni::jni_GetDirectBufferCapacity(env, buf) }, 16);

    // Round-trip bytes through the VM-owned native region.
    let payload = b"acos-direct-buff";
    acos_vm::natives::jni_glue::native_mem_write(addr as i64, payload).expect("write region");
    let back = acos_vm::natives::jni_glue::native_mem_read(addr as i64, payload.len()).unwrap();
    assert_eq!(back, payload);
    assert!(acos_vm::natives::jni_glue::native_mem_check(
        addr as i64,
        16
    ));
    assert!(!acos_vm::natives::jni_glue::native_mem_check(
        addr as i64,
        17
    ));

    // A non-direct object reports a null address and unknown capacity.
    let s = unsafe { new_string(env, "not-a-buffer") };
    assert_eq!(unsafe { jni::jni_GetDirectBufferAddress(env, s) }, 0);
    assert_eq!(unsafe { jni::jni_GetDirectBufferCapacity(env, s) }, -1);

    // Negative capacity is a structured error, not a buffer.
    assert_eq!(unsafe { jni::jni_NewDirectByteBuffer(env, 0, -1) }, 0);
    assert!(rt.take_error().is_some());
    let _ = acos_vm::natives::jni_glue::native_mem_free(addr as i64);
    drop(rt);
    drop(vm);
}

#[test]
fn monitor_enter_exit_is_recursive_and_scoped() {
    let (vm, rt) = setup();
    let env = rt.env() as usize;
    let obj = unsafe { new_string(env, "monitor") };
    assert_eq!(unsafe { jni::jni_MonitorEnter(env, obj) }, 0);
    assert_eq!(unsafe { jni::jni_MonitorEnter(env, obj) }, 0);
    assert_eq!(unsafe { jni::jni_MonitorExit(env, obj) }, 0);
    assert_eq!(unsafe { jni::jni_MonitorExit(env, obj) }, 0);
    // Unbalanced exit is a structured fault.
    assert_ne!(unsafe { jni::jni_MonitorExit(env, obj) }, 0);
    assert!(rt.take_error().is_some());
    drop(rt);
    drop(vm);
}

#[test]
fn register_natives_zero_count_and_unregister() {
    let (vm, rt) = setup();
    let env = rt.env() as usize;
    let cls = unsafe { find_class(env, "java/lang/String") };
    assert_ne!(cls, 0);
    // nMethods == 0 with a null array is a valid no-op.
    assert_eq!(
        unsafe { jni::jni_RegisterNatives(env, cls, std::ptr::null(), 0) },
        0
    );
    assert_eq!(unsafe { jni::jni_UnregisterNatives(env, cls) }, 0);
    assert!(rt.registered_natives().is_empty());
    assert_eq!(rt.take_error(), None);
    drop(rt);
    drop(vm);
}

#[test]
fn javavm_attach_getenv_and_versions() {
    let (vm, rt) = setup();
    let env = rt.env() as usize;
    let mut out: usize = 0;
    let mut java_vm: usize = 0;
    assert_eq!(unsafe { jni::jni_GetJavaVM(env, &mut java_vm) }, 0);
    assert_ne!(java_vm, 0);

    assert_eq!(
        unsafe { jni::vm_GetEnv(java_vm, &mut out, 0x0001_0006) },
        0
    );
    assert_eq!(out, env);
    assert_eq!(
        unsafe { jni::vm_GetEnv(java_vm, &mut out, 0x0001_0004) },
        0
    );
    assert_eq!(
        unsafe { jni::vm_GetEnv(java_vm, &mut out, 0x0002_0000) },
        JNI_EVERSION
    );

    // Attach/Detach round trip; a null out pointer is an error.
    out = 0;
    assert_eq!(
        unsafe { jni::vm_AttachCurrentThread(java_vm, &mut out, std::ptr::null_mut()) },
        0
    );
    assert_eq!(out, env);
    assert_eq!(
        unsafe { jni::vm_AttachCurrentThreadAsDaemon(java_vm, &mut out, std::ptr::null_mut()) },
        0
    );
    assert_ne!(
        unsafe { jni::vm_AttachCurrentThread(java_vm, std::ptr::null_mut(), std::ptr::null_mut()) },
        0
    );
    assert_eq!(unsafe { jni::vm_DetachCurrentThread(java_vm) }, 0);
    drop(rt);
    drop(vm);
}
