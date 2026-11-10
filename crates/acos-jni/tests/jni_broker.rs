//! Brokered JNI from native OS threads (ADR-0021 follow-up, W57-A).
//!
//! Each test runs a real `std::thread` that attaches as a broker worker,
//! enqueues JNI requests and blocks; the owner thread services the queue with
//! [`JniRuntime::pump_broker`] (the explicit form of the VM safe point). The
//! request path exercised here is the same one the VM's interpreter/looper
//! safe points use in production.
//!
//! The fixture DEX is `tests/data/broker.dex`, compiled from
//! `tests/java/dev/acos/fixtures/broker/BrokerTarget.java`.

use std::ffi::{CStr, CString};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::ThreadId;
use std::time::{Duration, Instant};

use acos_dex::DexFile;
use acos_jni::{broker, jni, take_worker_error, JniConfig, JniRuntime};
use acos_vm::{Vm, VmOptions};

/// Broker state is keyed by owner and is process-global; serialise the tests so
/// the global attach slot and worker registry are exercised deterministically.
static SERIAL: Mutex<()> = Mutex::new(());

extern "C" {
    fn acosrjni_CallStaticIntMethodA(env: usize, cls: usize, mid: usize, args: *const u64) -> i32;
    fn acosrjni_CallIntMethodA(env: usize, obj: usize, mid: usize, args: *const u64) -> i32;
    fn acosrjni_CallStaticObjectMethodA(
        env: usize,
        cls: usize,
        mid: usize,
        args: *const u64,
    ) -> usize;
    fn acosrjni_CallStaticVoidMethodA(env: usize, cls: usize, mid: usize, args: *const u64);
}

fn setup(gc_stress: bool) -> (Vm, JniRuntime) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/broker.dex");
    let dex = DexFile::parse(std::fs::read(&path).expect("broker.dex exists")).expect("parse dex");
    let mut vm = Vm::new(VmOptions {
        heap_max_bytes: 64 * 1024 * 1024,
        gc_stress,
        ..VmOptions::default()
    })
    .expect("VM");
    vm.add_dex(dex);
    let rt = JniRuntime::new(JniConfig::default());
    rt.attach(&mut vm).expect("attach");
    // `attach` must not retain a raw VM pointer; bind once `vm` is final.
    (vm, rt)
}

fn c(s: &str) -> CString {
    CString::new(s).unwrap()
}

fn class(env: usize, name: &str) -> usize {
    let n = c(name);
    // SAFETY: runtime attached (worker) and NUL-terminated name.
    let h = unsafe { jni::jni_FindClass(env, n.as_ptr()) };
    assert_ne!(h, 0, "FindClass({name}): {:?}", take_worker_error());
    h
}

fn static_method(env: usize, cls: usize, name: &str, sig: &str) -> usize {
    let (n, s) = (c(name), c(sig));
    let m = unsafe { jni::jni_GetStaticMethodID(env, cls, n.as_ptr(), s.as_ptr()) };
    assert_ne!(m, 0, "GetStaticMethodID({name}{sig}): {:?}", take_worker_error());
    m
}

fn method(env: usize, cls: usize, name: &str, sig: &str) -> usize {
    let (n, s) = (c(name), c(sig));
    let m = unsafe { jni::jni_GetMethodID(env, cls, n.as_ptr(), s.as_ptr()) };
    assert_ne!(m, 0, "GetMethodID({name}{sig}): {:?}", take_worker_error());
    m
}

fn string_of(env: usize, h: usize) -> String {
    assert_ne!(h, 0, "null jstring");
    let chars = unsafe { jni::jni_GetStringUTFChars(env, h, std::ptr::null_mut()) };
    assert!(!chars.is_null(), "GetStringUTFChars null");
    let text = unsafe { CStr::from_ptr(chars) }.to_string_lossy().into_owned();
    unsafe { jni::jni_ReleaseStringUTFChars(env, h, chars) };
    text
}

/// Owner loop: service the broker until the worker count reaches zero.
fn pump_until(rt: &JniRuntime, vm: &mut Vm, remaining: &AtomicUsize, secs: u64) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while remaining.load(Ordering::SeqCst) > 0 {
        let n = rt.pump_broker(vm);
        if n == 0 {
            std::thread::yield_now();
            std::thread::sleep(Duration::from_micros(20));
        }
        assert!(Instant::now() < deadline, "broker workers did not finish");
    }
    // Drain the terminal detach requests.
    for _ in 0..1000 {
        if broker::pending_requests() == 0 && broker::attached_workers() == 0 {
            break;
        }
        rt.pump_broker(vm);
    }
}

fn worker_attach(owner: ThreadId) -> usize {
    broker::attach_worker_to(owner);
    // Ensure the owner knows this worker exists even before the first request.
    broker::attached_workers();
    0
}

// ---------------------------------------------------------------------------

#[test]
fn worker_static_and_virtual_calls() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (mut vm, rt) = setup(false);
    rt.bind(&mut vm);
    let env = rt.env() as usize;
    let owner = rt.owner_thread_id();
    let remaining = Arc::new(AtomicUsize::new(1));
    let done = remaining.clone();

    let h = std::thread::spawn(move || {
        worker_attach(owner);
        let cls = class(env, "dev/acos/fixtures/broker/BrokerTarget");
        let add = static_method(env, cls, "add", "(II)I");
        let args = [20u64, 22u64];
        let r = unsafe { acosrjni_CallStaticIntMethodA(env, cls, add, args.as_ptr()) };
        assert_eq!(r, 42, "static add: {:?}", take_worker_error());

        // Virtual method on a fresh object.
        let obj = unsafe { jni::jni_AllocObject(env, cls) };
        assert_ne!(obj, 0, "AllocObject: {:?}", take_worker_error());
        let twice = method(env, cls, "twice", "(I)I");
        let args2 = [21u64];
        let r2 = unsafe { acosrjni_CallIntMethodA(env, obj, twice, args2.as_ptr()) };
        assert_eq!(r2, 42, "virtual twice: {:?}", take_worker_error());

        // String round-trip through the broker.
        let s = unsafe { jni::jni_NewStringUTF(env, c("hello").as_ptr()) };
        assert_ne!(s, 0, "NewStringUTF: {:?}", take_worker_error());
        let echo = static_method(env, cls, "echo", "(Ljava/lang/String;)Ljava/lang/String;");
        let args3 = [s as u64];
        let out = unsafe { acosrjni_CallStaticObjectMethodA(env, cls, echo, args3.as_ptr()) };
        assert_eq!(string_of(env, out), "hello");

        unsafe { jni::jni_DeleteLocalRef(env, out) };
        unsafe { jni::jni_DeleteLocalRef(env, s) };
        unsafe { jni::jni_DeleteLocalRef(env, obj) };
        broker::detach_worker();
        done.fetch_sub(1, Ordering::SeqCst);
    });

    pump_until(&rt, &mut vm, &remaining, 30);
    h.join().expect("worker thread");
    drop(rt);
    drop(vm);
}

#[test]
fn eight_workers_hammer_concurrently() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (mut vm, rt) = setup(false);
    rt.bind(&mut vm);
    let env = rt.env() as usize;
    let owner = rt.owner_thread_id();
    const WORKERS: usize = 8;
    const CALLS: usize = 1_000;
    let remaining = Arc::new(AtomicUsize::new(WORKERS));

    let mut handles = Vec::new();
    for w in 0..WORKERS {
        let done = remaining.clone();
        handles.push(std::thread::spawn(move || {
            worker_attach(owner);
            let cls = class(env, "dev/acos/fixtures/broker/BrokerTarget");
            let add = static_method(env, cls, "add", "(II)I");
            for i in 0..CALLS {
                let args = [i as u64, w as u64];
                let r = unsafe { acosrjni_CallStaticIntMethodA(env, cls, add, args.as_ptr()) };
                assert_eq!(r, (i + w) as i32, "worker {w} call {i}");
            }
            broker::detach_worker();
            done.fetch_sub(1, Ordering::SeqCst);
        }));
    }

    pump_until(&rt, &mut vm, &remaining, 120);
    for h in handles {
        h.join().expect("worker thread");
    }
    assert_eq!(rt.take_error(), None);
    drop(rt);
    drop(vm);
}

#[test]
fn java_exception_is_visible_to_worker() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (mut vm, rt) = setup(false);
    rt.bind(&mut vm);
    let env = rt.env() as usize;
    let owner = rt.owner_thread_id();
    let remaining = Arc::new(AtomicUsize::new(1));
    let done = remaining.clone();

    let h = std::thread::spawn(move || {
        worker_attach(owner);
        let cls = class(env, "dev/acos/fixtures/broker/BrokerTarget");
        let boom = static_method(env, cls, "boom", "()V");
        unsafe { acosrjni_CallStaticVoidMethodA(env, cls, boom, std::ptr::null()) };
        assert_eq!(
            unsafe { jni::jni_ExceptionCheck(env) },
            1,
            "pending exception must be visible via ExceptionCheck"
        );
        let thrown = unsafe { jni::jni_ExceptionOccurred(env) };
        assert_ne!(thrown, 0, "ExceptionOccurred");
        unsafe { jni::jni_DeleteLocalRef(env, thrown) };
        unsafe { jni::jni_ExceptionClear(env) };
        assert_eq!(
            unsafe { jni::jni_ExceptionCheck(env) },
            0,
            "ExceptionClear must drop the pending exception"
        );
        broker::detach_worker();
        done.fetch_sub(1, Ordering::SeqCst);
    });

    pump_until(&rt, &mut vm, &remaining, 30);
    h.join().expect("worker thread");
    assert_eq!(rt.take_error(), None);
    drop(rt);
    drop(vm);
}

/// The synchronous owner→native→worker→Java→owner pattern: the owner is inside
/// a native call and drives the *nested* pump via the exported C entry point.
#[test]
fn owner_native_loop_services_synchronously() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (mut vm, rt) = setup(false);
    rt.bind(&mut vm);
    let env = rt.env() as usize;
    let owner = rt.owner_thread_id();
    let remaining = Arc::new(AtomicUsize::new(1));
    let done = remaining.clone();

    let h = std::thread::spawn(move || {
        worker_attach(owner);
        let cls = class(env, "dev/acos/fixtures/broker/BrokerTarget");
        let add = static_method(env, cls, "add", "(II)I");
        let args = [1u64, 2u64];
        let r = unsafe { acosrjni_CallStaticIntMethodA(env, cls, add, args.as_ptr()) };
        assert_eq!(r, 3);
        broker::detach_worker();
        done.fetch_sub(1, Ordering::SeqCst);
    });

    // Drive the public C nested-pump entry point instead of `pump_broker`.
    let deadline = Instant::now() + Duration::from_secs(30);
    while remaining.load(Ordering::SeqCst) > 0 {
        let _ = acos_jni::acos_jni_broker_pump();
        std::thread::yield_now();
        assert!(Instant::now() < deadline, "nested pump did not progress");
    }
    h.join().expect("worker thread");
    // Note: `acos_jni_broker_pump` uses the owner VM pointer installed by
    // `bind`, so the serviced calls act on `vm` while it is borrowed here.
    drop(rt);
    drop(vm);
}

#[test]
fn gc_stress_holds_local_and_global_refs() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (mut vm, rt) = setup(true);
    rt.bind(&mut vm);
    let env = rt.env() as usize;
    let owner = rt.owner_thread_id();
    let remaining = Arc::new(AtomicUsize::new(1));
    let done = remaining.clone();

    let h = std::thread::spawn(move || {
        worker_attach(owner);
        let cls = class(env, "dev/acos/fixtures/broker/BrokerTarget");
        let echo = static_method(env, cls, "echo", "(Ljava/lang/String;)Ljava/lang/String;");
        // A global ref survives many broker calls / GCs; a per-iteration local
        // is rooted while it is live.
        let global = unsafe { jni::jni_NewStringUTF(env, c("global-ref").as_ptr()) };
        let global = unsafe { jni::jni_NewGlobalRef(env, global) };
        assert_ne!(global, 0);
        for i in 0..200 {
            let s = unsafe { jni::jni_NewStringUTF(env, c("local").as_ptr()) };
            assert_ne!(s, 0, "iter {i}: {:?}", take_worker_error());
            let args = [s as u64];
            let out = unsafe { acosrjni_CallStaticObjectMethodA(env, cls, echo, args.as_ptr()) };
            assert_eq!(string_of(env, out), "local");
            unsafe { jni::jni_DeleteLocalRef(env, out) };
            unsafe { jni::jni_DeleteLocalRef(env, s) };
        }
        // The global is still a valid, live reference.
        let args = [global as u64];
        let out = unsafe { acosrjni_CallStaticObjectMethodA(env, cls, echo, args.as_ptr()) };
        assert_eq!(string_of(env, out), "global-ref");
        unsafe { jni::jni_DeleteLocalRef(env, out) };
        unsafe { jni::jni_DeleteGlobalRef(env, global) };
        broker::detach_worker();
        done.fetch_sub(1, Ordering::SeqCst);
    });

    pump_until(&rt, &mut vm, &remaining, 60);
    h.join().expect("worker thread");
    assert_eq!(rt.take_error(), None);
    drop(rt);
    drop(vm);
}

#[test]
fn reference_accounting_is_balanced() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (mut vm, rt) = setup(false);
    rt.bind(&mut vm);
    let env = rt.env() as usize;
    let owner = rt.owner_thread_id();
    // Baseline after attach/bind (the owner's own roots).
    let _ = rt.pump_broker(&mut vm);
    let base = vm.native_roots.len();

    let remaining = Arc::new(AtomicUsize::new(1));
    let done = remaining.clone();
    let h = std::thread::spawn(move || {
        worker_attach(owner);
        let cls = class(env, "dev/acos/fixtures/broker/BrokerTarget");
        let echo = static_method(env, cls, "echo", "(Ljava/lang/String;)Ljava/lang/String;");
        let s = unsafe { jni::jni_NewStringUTF(env, c("rooted").as_ptr()) };
        let g = unsafe { jni::jni_NewGlobalRef(env, s) };
        unsafe { jni::jni_DeleteLocalRef(env, s) };
        unsafe { jni::jni_DeleteGlobalRef(env, g) };
        broker::detach_worker();
        let _ = echo;
        done.fetch_sub(1, Ordering::SeqCst);
    });
    pump_until(&rt, &mut vm, &remaining, 30);
    h.join().expect("worker thread");
    assert_eq!(
        broker::attached_workers(),
        0,
        "worker must be fully detached"
    );
    assert_eq!(
        vm.native_roots.len(),
        base,
        "worker local/global refs must be released"
    );
    drop(rt);
    drop(vm);
}

/// The production attach path: `AttachCurrentThread` from a real OS thread.
#[test]
fn attach_current_thread_and_call() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (mut vm, rt) = setup(false);
    rt.bind(&mut vm);
    let jvm = rt.java_vm() as usize;
    let remaining = Arc::new(AtomicUsize::new(1));
    let done = remaining.clone();

    let h = std::thread::spawn(move || {
        let mut env = 0usize;
        let rc = unsafe {
            jni::vm_AttachCurrentThread(jvm, &mut env, std::ptr::null_mut())
        };
        assert_eq!(rc, 0, "AttachCurrentThread");
        assert_ne!(env, 0);
        let cls = class(env, "dev/acos/fixtures/broker/BrokerTarget");
        let add = static_method(env, cls, "add", "(II)I");
        let args = [7u64, 8u64];
        let r = unsafe { acosrjni_CallStaticIntMethodA(env, cls, add, args.as_ptr()) };
        assert_eq!(r, 15);
        unsafe { jni::vm_DetachCurrentThread(jvm) };
        done.fetch_sub(1, Ordering::SeqCst);
    });

    pump_until(&rt, &mut vm, &remaining, 30);
    h.join().expect("worker thread");
    drop(rt);
    drop(vm);
}

