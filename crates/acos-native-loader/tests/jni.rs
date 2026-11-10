//! End-to-end JNI test: a real NDK `libhellojni.so` calls into the minimal
//! Rust-provided test JNI environment.
//!
//! This proves the JNI `JNINativeInterface`/`JNIInvokeInterface` ABI, the
//! variadic `Call*Method` trampolines, `RegisterNatives`, exceptions, strings
//! and primitive arrays. It is **not** the production ACOS JNI (that is the
//! future `acos-jni` crate).

mod common;

use acos_native_loader::testjni::TestJni;
use acos_native_loader::{Loader, LoaderConfig};
use std::os::raw::c_void;

#[test]
fn hello_jni_end_to_end() {
    let _g = common::serial();
    let jni = TestJni::install();
    let loader = Loader::new(LoaderConfig::default());
    let bytes = std::fs::read(common::data("libhellojni.so")).expect("read fixture .so");
    let h = loader
        .load_bytes(&bytes, "libhellojni.so")
        .expect("load libhellojni.so");

    let env = jni.env();
    let vm = jni.vm();
    let null = std::ptr::null_mut();

    // --- JNI_OnLoad ---------------------------------------------------------
    let on_load: unsafe extern "C" fn(*mut c_void, *mut c_void) -> i32 =
        unsafe { common::sym(h.symbol("JNI_OnLoad").expect("JNI_OnLoad export")) };
    let version = unsafe { on_load(vm, null) };
    assert_eq!(version, 0x0001_0006, "JNI_OnLoad returns JNI_VERSION_1_6");

    // --- RegisterNatives ----------------------------------------------------
    let registered = jni.registered();
    assert_eq!(registered.len(), 1, "one native registered: {registered:?}");
    let r = &registered[0];
    assert_eq!(r.class, "dev/acos/fixtures/hellojni/MainActivity");
    assert_eq!(r.name, "registeredEcho");
    assert_eq!(r.signature, "(Ljava/lang/String;)Ljava/lang/String;");
    assert_ne!(r.fn_ptr, 0);

    let thiz = jni.new_instance("dev/acos/fixtures/hellojni/MainActivity") as *mut c_void;

    // --- stringFromJNI ------------------------------------------------------
    let f: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
        unsafe { common::sym(h.symbol("Java_dev_acos_fixtures_hellojni_MainActivity_stringFromJNI").unwrap()) };
    let js = unsafe { f(env, thiz) };
    assert_eq!(jni.string_of(js as *mut _), "Hello from native C");

    // --- add(int,int) -------------------------------------------------------
    let f: unsafe extern "C" fn(*mut c_void, *mut c_void, i32, i32) -> i32 =
        unsafe { common::sym(h.symbol("Java_dev_acos_fixtures_hellojni_MainActivity_add").unwrap()) };
    assert_eq!(unsafe { f(env, thiz, 20, 22) }, 42);

    // --- sumArray(int[]) ----------------------------------------------------
    let f: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> i32 =
        unsafe { common::sym(h.symbol("Java_dev_acos_fixtures_hellojni_MainActivity_sumArray").unwrap()) };
    let arr = jni.new_int_array(&[1, 2, 3, 4, 5]) as *mut c_void;
    assert_eq!(unsafe { f(env, thiz, arr) }, 15);

    // --- callBack(): native -> Java static int + instance void --------------
    jni.clear_calls();
    let f: unsafe extern "C" fn(*mut c_void, *mut c_void) =
        unsafe { common::sym(h.symbol("Java_dev_acos_fixtures_hellojni_MainActivity_callBack").unwrap()) };
    unsafe { f(env, thiz) };
    let calls = jni.calls();
    assert_eq!(calls.len(), 2, "two Java calls observed: {calls:?}");
    assert_eq!(calls[0].name, "javaCallback");
    assert!(calls[0].is_static);
    assert_eq!(calls[0].args[0].int(), 21, "varargs int argument captured");
    assert_eq!(calls[1].name, "voidCallback");
    assert!(!calls[1].is_static);
    assert_eq!(
        jni.string_of(calls[1].args[0].ptr() as *mut _),
        "from-native",
        "object argument captured"
    );

    // --- registeredEcho(String) --------------------------------------------
    let f: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> *mut c_void =
        unsafe { common::sym(r.fn_ptr as *const c_void) };
    let arg = jni.new_string("echo!") as *mut c_void;
    let out = unsafe { f(env, thiz, arg) };
    assert_eq!(jni.string_of(out as *mut _), "echo!");

    // --- throwFromJNI(): ThrowNew ------------------------------------------
    let f: unsafe extern "C" fn(*mut c_void, *mut c_void) =
        unsafe { common::sym(h.symbol("Java_dev_acos_fixtures_hellojni_MainActivity_throwFromJNI").unwrap()) };
    unsafe { f(env, thiz) };
    let ex = jni.pending_exception().expect("exception pending");
    assert_eq!(ex.class, "java/lang/IllegalStateException");
    assert_eq!(ex.message, "thrown from native");

    loader.unload(&h).unwrap();
}
