//! Manual JNI harness smoke test (development helper).
//!
//! Run with: `cargo run -p acos-native-loader --example jni_smoke`

use acos_native_loader::testjni::TestJni;
use acos_native_loader::{Loader, LoaderConfig};
use std::os::raw::c_void;
use std::path::PathBuf;

fn data(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

unsafe fn sym_fn<R>(p: *const c_void) -> R {
    std::mem::transmute_copy::<*const c_void, R>(&p)
}

fn main() {
    let jni = TestJni::install();
    let loader = Loader::new(LoaderConfig::default());
    let bytes = std::fs::read(data("libhellojni.so")).expect("read");
    let h = loader
        .load_bytes(&bytes, "libhellojni.so")
        .expect("load libhellojni");

    let onload = h.symbol("JNI_OnLoad").expect("JNI_OnLoad");
    let f: unsafe extern "C" fn(*mut c_void, *mut c_void) -> i32 = unsafe { sym_fn(onload) };
    let ver = unsafe { f(jni.vm(), std::ptr::null_mut()) };
    println!("JNI_OnLoad -> 0x{ver:08x}");
    println!("registered = {:?}", jni.registered());

    let thiz = jni.new_instance("dev/acos/fixtures/hellojni/MainActivity");
    let env = jni.env();

    let s = h
        .symbol("Java_dev_acos_fixtures_hellojni_MainActivity_stringFromJNI")
        .unwrap();
    let f: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void = unsafe { sym_fn(s) };
    let js = unsafe { f(env, thiz as *mut c_void) };
    println!("stringFromJNI = {:?}", jni.string_of(js as *mut _));

    let a = h
        .symbol("Java_dev_acos_fixtures_hellojni_MainActivity_add")
        .unwrap();
    let f: unsafe extern "C" fn(*mut c_void, *mut c_void, i32, i32) -> i32 = unsafe { sym_fn(a) };
    println!("add(2,3) = {}", unsafe { f(env, thiz as *mut c_void, 2, 3) });

    let sa = h
        .symbol("Java_dev_acos_fixtures_hellojni_MainActivity_sumArray")
        .unwrap();
    let f: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> i32 = unsafe { sym_fn(sa) };
    let arr = jni.new_int_array(&[1, 2, 3, 4, 5]);
    println!("sumArray = {}", unsafe { f(env, thiz as *mut c_void, arr as *mut c_void) });

    let cb = h
        .symbol("Java_dev_acos_fixtures_hellojni_MainActivity_callBack")
        .unwrap();
    let f: unsafe extern "C" fn(*mut c_void, *mut c_void) = unsafe { sym_fn(cb) };
    unsafe { f(env, thiz as *mut c_void) };
    println!("calls = {:?}", jni.calls());

    let tj = h
        .symbol("Java_dev_acos_fixtures_hellojni_MainActivity_throwFromJNI")
        .unwrap();
    let f: unsafe extern "C" fn(*mut c_void, *mut c_void) = unsafe { sym_fn(tj) };
    unsafe { f(env, thiz as *mut c_void) };
    println!("pending exception = {:?}", jni.pending_exception());

    // The dynamically registered native (from RegisterNatives in JNI_OnLoad).
    let reg = jni.registered();
    if let Some(r) = reg.first() {
        let f: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> *mut c_void =
            unsafe { sym_fn(r.fn_ptr as *const c_void) };
        let arg = jni.new_string("echo!");
        let out = unsafe { f(env, thiz as *mut c_void, arg as *mut c_void) };
        println!("registeredEcho = {:?}", jni.string_of(out as *mut _));
    }
}
