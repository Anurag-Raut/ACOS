#![allow(non_snake_case)]
//! Implemented JNI slots for the test harness.
//!
//! Every function here has the exact C ABI of the corresponding
//! `JNINativeInterface` (or `JNIInvokeInterface`) slot so it can be called
//! through the generated function-pointer table by guest code.

use super::*;
use std::os::raw::c_char;

// --- helpers ----------------------------------------------------------------

fn raw_args_for(mid: usize, args: *const u64) -> Vec<u64> {
    let n = with_state(|s| {
        method_name(s, mid)
            .map(|(_, c, _)| c.to_bytes().len())
            .unwrap_or(0)
    });
    if args.is_null() || n == 0 {
        Vec::new()
    } else {
        // SAFETY: `args` points to `n` jvalue-sized words supplied by the guest.
        unsafe { std::slice::from_raw_parts(args, n).to_vec() }
    }
}

// --- object model -----------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn jni_GetVersion(_env: usize) -> i32 {
    JNI_VERSION_1_6
}

#[no_mangle]
pub unsafe extern "C" fn jni_FindClass(_env: usize, name: *const c_char) -> usize {
    let name = cstr(name);
    alloc_obj(ObjKind::Class(name)) as usize
}

#[no_mangle]
pub unsafe extern "C" fn jni_GetObjectClass(_env: usize, obj: usize) -> usize {
    match class_name_of(obj) {
        Some(c) => alloc_obj(ObjKind::Class(c)) as usize,
        None => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn jni_GetMethodID(
    _env: usize,
    cls: usize,
    name: *const c_char,
    sig: *const c_char,
) -> usize {
    method_id(cls, name, sig, false)
}

#[no_mangle]
pub unsafe extern "C" fn jni_GetStaticMethodID(
    _env: usize,
    cls: usize,
    name: *const c_char,
    sig: *const c_char,
) -> usize {
    method_id(cls, name, sig, true)
}

unsafe fn method_id(cls: usize, name: *const c_char, sig: *const c_char, is_static: bool) -> usize {
    let class = match class_name_of(cls) {
        Some(c) => c,
        None => return 0,
    };
    let name = cstr(name);
    let sig = cstr(sig);
    with_state(|s| find_method(s, &class, &name, &sig, is_static))
}

#[no_mangle]
pub unsafe extern "C" fn jni_NewStringUTF(_env: usize, s: *const c_char) -> usize {
    alloc_obj(ObjKind::JString {
        s: cstr(s),
        c: None,
    }) as usize
}

#[no_mangle]
pub unsafe extern "C" fn jni_GetStringUTFChars(
    _env: usize,
    jstr: usize,
    is_copy: *mut u8,
) -> usize {
    if !is_copy.is_null() {
        *is_copy = 1; // JNI_TRUE
    }
    let o = match obj_mut(jstr) {
        Some(o) => o,
        None => return 0,
    };
    if let ObjKind::JString { s, c } = &mut o.0 {
        if c.is_none() {
            *c = CString::new(s.as_str()).ok();
        }
        match c {
            Some(cs) => cs.as_ptr() as usize,
            None => 0,
        }
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn jni_ReleaseStringUTFChars(_env: usize, _jstr: usize, _chars: usize) {
    // The CString is owned by the fake object; nothing to do.
}

#[no_mangle]
pub unsafe extern "C" fn jni_GetArrayLength(_env: usize, arr: usize) -> i32 {
    match obj(arr) {
        Some(GObject(ObjKind::IntArray(v))) => v.len() as i32,
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn jni_GetIntArrayElements(
    _env: usize,
    arr: usize,
    is_copy: *mut u8,
) -> usize {
    if !is_copy.is_null() {
        *is_copy = 1; // JNI_TRUE
    }
    match obj_mut(arr) {
        Some(GObject(ObjKind::IntArray(v))) => v.as_mut_ptr() as usize,
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn jni_ReleaseIntArrayElements(
    _env: usize,
    _arr: usize,
    _elems: usize,
    _mode: i32,
) {
    // Elements point straight into the fake array's backing store, so writes
    // are already visible; JNI_ABORT copy-back semantics are not emulated.
}

#[no_mangle]
pub unsafe extern "C" fn jni_GetIntArrayRegion(
    _env: usize,
    arr: usize,
    start: i32,
    len: i32,
    buf: *mut i32,
) {
    if buf.is_null() || len <= 0 {
        return;
    }
    if let Some(GObject(ObjKind::IntArray(v))) = obj(arr) {
        let s = start.max(0) as usize;
        let n = (len as usize).min(v.len().saturating_sub(s));
        std::ptr::copy_nonoverlapping(v.as_ptr().add(s), buf, n);
    }
}

#[no_mangle]
pub unsafe extern "C" fn jni_SetIntArrayRegion(
    _env: usize,
    arr: usize,
    start: i32,
    len: i32,
    buf: *const i32,
) {
    if buf.is_null() || len <= 0 {
        return;
    }
    if let Some(GObject(ObjKind::IntArray(v))) = obj_mut(arr) {
        let s = start.max(0) as usize;
        if s >= v.len() {
            return;
        }
        let n = (len as usize).min(v.len() - s);
        std::ptr::copy_nonoverlapping(buf, v.as_mut_ptr().add(s), n);
    }
}

#[no_mangle]
pub unsafe extern "C" fn jni_RegisterNatives(
    _env: usize,
    cls: usize,
    methods: *const JNINativeMethod,
    n: i32,
) -> i32 {
    let class = match class_name_of(cls) {
        Some(c) => c,
        None => return JNI_ERR,
    };
    if methods.is_null() || n < 0 {
        return JNI_ERR;
    }
    for i in 0..n as usize {
        let m = &*methods.add(i);
        with_state(|s| {
            s.registered.push(RegisteredNative {
                class: class.clone(),
                name: cstr(m.name),
                signature: cstr(m.signature),
                fn_ptr: m.fn_ptr as usize,
            });
        });
    }
    JNI_OK
}

#[no_mangle]
pub unsafe extern "C" fn jni_ThrowNew(_env: usize, cls: usize, message: *const c_char) -> i32 {
    let class = class_name_of(cls).unwrap_or_else(|| "java/lang/Throwable".to_string());
    let message = cstr(message);
    with_state(|s| s.pending = Some(Exception { class, message }));
    JNI_OK
}

#[no_mangle]
pub unsafe extern "C" fn jni_ExceptionCheck(_env: usize) -> u8 {
    with_state(|s| u8::from(s.pending.is_some()))
}

#[no_mangle]
pub unsafe extern "C" fn jni_ExceptionOccurred(_env: usize) -> usize {
    with_state(|s| match s.pending.take() {
        Some(e) => {
            s.last_exception = Some(e.clone());
            alloc_obj(ObjKind::Throwable {
                class: e.class,
                msg: e.message,
            }) as usize
        }
        None => 0,
    })
}

#[no_mangle]
pub unsafe extern "C" fn jni_ExceptionClear(_env: usize) {
    with_state(|s| s.pending = None);
}

#[no_mangle]
pub unsafe extern "C" fn jni_DeleteLocalRef(_env: usize, _obj: usize) {
    // Local references are never freed by the harness (short-lived tests).
}

#[no_mangle]
pub unsafe extern "C" fn jni_GetJavaVM(_env: usize, vm_out: *mut usize) -> i32 {
    if vm_out.is_null() {
        return JNI_ERR;
    }
    *vm_out = with_state(|s| s.vm_slot);
    JNI_OK
}

// --- array-form (`*A`) call entries ----------------------------------------

macro_rules! call_a {
    ($name:ident, $static:expr, $ret:ty) => {
        #[no_mangle]
        pub unsafe extern "C" fn $name(
            _env: usize,
            target: usize,
            mid: usize,
            args: *const u64,
        ) -> $ret {
            let raw = raw_args_for(mid, args);
            perform_call(target, mid, $static, &raw) as $ret
        }
    };
}

macro_rules! call_a_void {
    ($name:ident, $static:expr) => {
        #[no_mangle]
        pub unsafe extern "C" fn $name(_env: usize, target: usize, mid: usize, args: *const u64) {
            let raw = raw_args_for(mid, args);
            let _ = perform_call(target, mid, $static, &raw);
        }
    };
}

call_a!(jni_CallStaticIntMethodA, true, i32);
call_a_void!(jni_CallStaticVoidMethodA, true);
call_a!(jni_CallStaticObjectMethodA, true, usize);
call_a!(jni_CallIntMethodA, false, i32);
call_a_void!(jni_CallVoidMethodA, false);
call_a!(jni_CallObjectMethodA, false, usize);

// --- JNIInvokeInterface -----------------------------------------------------

pub unsafe extern "C" fn vm_DestroyJavaVM(_vm: usize) -> i32 {
    JNI_OK
}

pub unsafe extern "C" fn vm_AttachCurrentThread(
    _vm: usize,
    env_out: *mut usize,
    _args: usize,
) -> i32 {
    if env_out.is_null() {
        return JNI_ERR;
    }
    *env_out = with_state(|s| s.env_slot);
    JNI_OK
}

pub unsafe extern "C" fn vm_DetachCurrentThread(_vm: usize) -> i32 {
    JNI_OK
}

pub unsafe extern "C" fn vm_GetEnv(_vm: usize, env_out: *mut usize, version: i32) -> i32 {
    if env_out.is_null() {
        return JNI_ERR;
    }
    // Accept any JNI 1.x version.
    if (version >> 16) != 1 {
        return -3; // JNI_EVERSION
    }
    *env_out = with_state(|s| s.env_slot);
    JNI_OK
}

pub unsafe extern "C" fn vm_AttachCurrentThreadAsDaemon(
    vm: usize,
    env_out: *mut usize,
    args: usize,
) -> i32 {
    vm_AttachCurrentThread(vm, env_out, args)
}
