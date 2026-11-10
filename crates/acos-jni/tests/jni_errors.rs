//! JNI error-path tests.
//!
//! These call the generated table directly (the same entry points guest code
//! uses) and assert the structured errors. Nothing here may panic, abort the
//! process, or silently return a value for a contract violation.

use std::ffi::CString;

use acos_jni::jni;
use acos_jni::state::{JNI_GLOBAL_REF_TYPE, JNI_LOCAL_REF_TYPE};
use acos_jni::{JniConfig, JniError, JniRuntime};
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
    (vm, rt)
}

fn c(s: &str) -> CString {
    CString::new(s).unwrap()
}

unsafe fn find_class(env: usize, name: &str) -> usize {
    let n = c(name);
    jni::jni_FindClass(env, n.as_ptr())
}

#[test]
fn string_round_trip_and_global_refs() {
    let (mut vm, rt) = setup();
    rt.bind(&mut vm);
    let env = rt.env() as usize;
    let s = c("hello");
    let js = unsafe { jni::jni_NewStringUTF(env, s.as_ptr()) };
    assert_ne!(js, 0);
    assert_eq!(unsafe { jni::jni_GetStringLength(env, js) }, 5);

    let chars = unsafe { jni::jni_GetStringUTFChars(env, js, std::ptr::null_mut()) };
    assert!(!chars.is_null());
    let back = unsafe { std::ffi::CStr::from_ptr(chars) }.to_str().unwrap();
    assert_eq!(back, "hello");
    unsafe { jni::jni_ReleaseStringUTFChars(env, js, chars) };

    assert_eq!(
        unsafe { jni::jni_GetObjectRefType(env, js) },
        JNI_LOCAL_REF_TYPE
    );
    let g = unsafe { jni::jni_NewGlobalRef(env, js) };
    assert_ne!(g, 0);
    assert_eq!(
        unsafe { jni::jni_GetObjectRefType(env, g) },
        JNI_GLOBAL_REF_TYPE
    );
    unsafe { jni::jni_DeleteGlobalRef(env, g) };
    unsafe { jni::jni_DeleteLocalRef(env, js) };
    assert_eq!(rt.take_error(), None);
    drop(rt);
    drop(vm);
}

#[test]
fn null_reference_is_structured() {
    let (mut vm, rt) = setup();
    rt.bind(&mut vm);
    let env = rt.env() as usize;
    assert_eq!(unsafe { jni::jni_GetObjectClass(env, 0) }, 0);
    match rt.take_error() {
        Some(JniError::NullReference { function, .. }) => assert_eq!(function, "GetObjectClass"),
        other => panic!("expected NullReference, got {other:?}"),
    }
    drop(rt);
    drop(vm);
}

#[test]
fn dead_handle_is_a_hard_error() {
    let (mut vm, rt) = setup();
    rt.bind(&mut vm);
    let env = rt.env() as usize;
    assert_eq!(unsafe { jni::jni_GetObjectClass(env, 0x7FFF_FFF0) }, 0);
    match rt.take_error() {
        Some(JniError::BadReference { handle, .. }) => assert_eq!(handle, 0x7FFF_FFF0),
        other => panic!("expected BadReference, got {other:?}"),
    }
    drop(rt);
    drop(vm);
}

#[test]
fn pending_exception_blocks_non_exception_safe_calls() {
    let (mut vm, rt) = setup();
    rt.bind(&mut vm);
    let env = rt.env() as usize;
    let cls = unsafe { find_class(env, "java/lang/IllegalStateException") };
    assert_ne!(cls, 0);
    let msg = c("boom");
    assert_eq!(unsafe { jni::jni_ThrowNew(env, cls, msg.as_ptr()) }, 0);
    assert_eq!(unsafe { jni::jni_ExceptionCheck(env) }, 1);

    // Not exception-safe with a pending throwable.
    let s = c("should-not-exist");
    assert_eq!(unsafe { jni::jni_NewStringUTF(env, s.as_ptr()) }, 0);
    match rt.take_error() {
        Some(JniError::PendingException { function }) => assert_eq!(function, "NewStringUTF"),
        other => panic!("expected PendingException, got {other:?}"),
    }

    // Exception-safe slots still work.
    let occurred = unsafe { jni::jni_ExceptionOccurred(env) };
    assert_ne!(occurred, 0);
    unsafe { jni::jni_ExceptionClear(env) };
    assert_eq!(unsafe { jni::jni_ExceptionCheck(env) }, 0);

    let ok = unsafe { jni::jni_NewStringUTF(env, s.as_ptr()) };
    assert_ne!(ok, 0);
    drop(rt);
    drop(vm);
}

#[test]
fn bad_signature_and_missing_method_are_structured() {
    let (mut vm, rt) = setup();
    rt.bind(&mut vm);
    let env = rt.env() as usize;
    let cls = unsafe { find_class(env, "java/lang/String") };
    assert_ne!(cls, 0);
    let name = c("length");
    let bad_sig = c("(!)V");
    assert_eq!(
        unsafe { jni::jni_GetMethodID(env, cls, name.as_ptr(), bad_sig.as_ptr()) },
        0
    );
    match rt.take_error() {
        Some(JniError::BadArgument { function, .. }) => assert_eq!(function, "GetMethodID"),
        other => panic!("expected BadArgument, got {other:?}"),
    }

    let missing = c("definitelyNotAMethod");
    let sig = c("()V");
    assert_eq!(
        unsafe { jni::jni_GetMethodID(env, cls, missing.as_ptr(), sig.as_ptr()) },
        0
    );
    // Android semantics: a Java `NoSuchMethodError` is pending. The bootstrap
    // class provider does not define it; then the failure is the structured
    // `NativeFault` naming the missing class (never a silent null).
    let pending = unsafe { jni::jni_ExceptionCheck(env) } == 1;
    let err = rt.take_error();
    assert!(
        pending || matches!(err, Some(JniError::NativeFault { .. })),
        "missing method must throw or report a structured error, got {err:?}"
    );
    unsafe { jni::jni_ExceptionClear(env) };
    drop(rt);
    drop(vm);
}

#[test]
fn unsupported_slot_records_unsupported() {
    let (mut vm, rt) = setup();
    rt.bind(&mut vm);
    // `DefineClass` is present in the table (slot 5) but not implemented.
    let p = unsafe { acos_jni::traps::trap_DefineClass(0, 0, 0, 0, 0, 0, 0, 0) };
    assert_eq!(p, 0);
    match rt.take_error() {
        Some(JniError::Unsupported { slot }) => assert_eq!(slot, "DefineClass"),
        other => panic!("expected Unsupported, got {other:?}"),
    }
    assert_eq!(
        JniError::Unsupported { slot: "X".into() }.code(),
        "JNI_UNSUPPORTED"
    );
    drop(rt);
    drop(vm);
}

#[test]
fn register_natives_rejects_unknown_method() {
    let (mut vm, rt) = setup();
    rt.bind(&mut vm);
    let env = rt.env() as usize;
    let cls = unsafe { find_class(env, "java/lang/String") };
    assert_ne!(cls, 0);
    let name = c("notNative");
    let sig = c("()V");
    let m = jni::JNINativeMethod {
        name: name.as_ptr(),
        signature: sig.as_ptr(),
        fn_ptr: std::ptr::dangling_mut::<std::os::raw::c_void>(),
    };
    let rc = unsafe { jni::jni_RegisterNatives(env, cls, &m, 1) };
    assert_eq!(rc, -1);
    let pending = unsafe { jni::jni_ExceptionCheck(env) } == 1;
    let err = rt.take_error();
    assert!(
        pending || matches!(err, Some(JniError::NativeFault { .. })),
        "unknown native must throw or report a structured error, got {err:?}"
    );
    unsafe { jni::jni_ExceptionClear(env) };
    drop(rt);
    drop(vm);
}

#[test]
fn vm_only_setup_drop() {
    let (mut vm, rt) = setup();
    rt.bind(&mut vm);
    drop(rt);
    drop(vm);
}

#[test]
fn static_fields_and_primitive_arrays_round_trip() {
    let (mut vm, rt) = setup();
    rt.bind(&mut vm);
    let env = rt.env() as usize;

    let system = unsafe { find_class(env, "java/lang/System") };
    assert_ne!(system, 0);
    let out_name = c("out");
    let out_sig = c("Ljava/io/PrintStream;");
    let out_fid =
        unsafe { jni::jni_GetStaticFieldID(env, system, out_name.as_ptr(), out_sig.as_ptr()) };
    assert_ne!(out_fid, 0, "System.out field id; err {:?}", rt.take_error());
    let out = unsafe { jni::jni_GetStaticObjectField(env, system, out_fid) };
    assert_ne!(out, 0, "System.out value");
    assert_ne!(unsafe { jni::jni_GetObjectClass(env, out) }, 0);

    let array = unsafe { jni::jni_NewIntArray(env, 5) };
    assert_ne!(array, 0);
    assert_eq!(unsafe { jni::jni_GetArrayLength(env, array) }, 5);
    let values: [i32; 5] = [1, 2, 3, 4, 5];
    unsafe {
        jni::jni_SetIntArrayRegion(env, array, 0, 5, values.as_ptr());
    }
    let mut back: [i32; 5] = [0; 5];
    unsafe {
        jni::jni_GetIntArrayRegion(env, array, 0, 5, back.as_mut_ptr());
    }
    assert_eq!(back, values);

    let elems = unsafe { jni::jni_GetIntArrayElements(env, array, std::ptr::null_mut()) };
    assert!(!elems.is_null());
    unsafe {
        assert_eq!(std::slice::from_raw_parts(elems, 5), &values);
        *elems = 42;
        // JNI_COMMIT (1) writes back but keeps the buffer valid.
        jni::jni_ReleaseIntArrayElements(env, array, elems, 1);
        let mut first = [0i32; 1];
        jni::jni_GetIntArrayRegion(env, array, 0, 1, first.as_mut_ptr());
        assert_eq!(first[0], 42);
        // JNI_ABORT (2) discards and frees the buffer.
        jni::jni_ReleaseIntArrayElements(env, array, elems, 2);
    }
    assert_eq!(rt.take_error(), None);
    drop(rt);
    drop(vm);
}

#[test]
fn missing_class_is_structured() {
    let (mut vm, rt) = setup();
    rt.bind(&mut vm);
    let env = rt.env() as usize;
    let cls = unsafe { find_class(env, "not/a/RealClass") };
    assert_eq!(cls, 0);
    let pending = unsafe { jni::jni_ExceptionCheck(env) } == 1;
    let err = rt.take_error();
    assert!(
        pending
            || matches!(
                err,
                Some(JniError::ClassNotFound { .. }) | Some(JniError::NativeFault { .. })
            ),
        "missing class must throw or report a structured error, got {err:?}"
    );
    unsafe { jni::jni_ExceptionClear(env) };
    drop(rt);
    drop(vm);
}
