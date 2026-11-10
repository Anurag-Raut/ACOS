//! Native JNI fixture: a real NDK `libacosjni_slots.so` exercises the newly
//! implemented slots through the real `JNINativeInterface` C ABI.
//!
//! The library is loaded with `acos-native-loader` — the same in-process
//! ELF mapping/relocation path used for `fixtures/apps/hello-jni` — and every
//! JNI call it makes is routed through the `acos-jni` function table. This is
//! the "isolation model": the guest `.so` is untrusted, holds no host
//! privileges, and can only reach the VM through the JNI table; the loader
//! resolves its imports against the Rust bionic table and never hands it raw
//! host pointers.
//!
//! The test is skipped (not failed) when the fixture has not been built; run
//! `fixtures/jni/build.sh` first.

use std::ffi::c_void;
use std::path::{Path, PathBuf};

use acos_jni::{JniConfig, JniRuntime};
use acos_native_loader::{Loader, LoaderConfig};
use acos_vm::{Vm, VmOptions};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn fixture() -> PathBuf {
    repo_root().join("fixtures/build/jni/x86_64/libacosjni_slots.so")
}

#[test]
fn native_fixture_exercises_slots_through_jni_table() {
    let path = fixture();
    if !path.is_file() {
        eprintln!(
            "SKIP native_fixture_exercises_slots_through_jni_table: {} is absent \
             (run fixtures/jni/build.sh)",
            path.display()
        );
        return;
    }

    let mut vm = Vm::new(VmOptions {
        heap_max_bytes: 64 * 1024 * 1024,
        gc_stress: std::env::var_os("ACOS_GC_STRESS").is_some(),
        ..VmOptions::default()
    })
    .expect("VM");
    let rt = JniRuntime::new(JniConfig::default());
    rt.attach(&mut vm).expect("attach");
    rt.bind(&mut vm);

    // The fixture is a trusted checked-in test object, not guest code; this
    // trusted test process is not confined, so opt out explicitly (ADR-0022).
    let loader = Loader::new(LoaderConfig::for_tests_without_confinement());
    let handle = loader.load_file(&path).expect("load fixture .so");

    let sym = handle
        .symbol("acos_jni_test_slots")
        .expect("acos_jni_test_slots export");
    // SAFETY: the symbol is the fixture's JNI-mandated entry point; the
    // signature matches the C definition (`jint (JNIEnv*, jclass)`).
    let f: unsafe extern "C" fn(*mut c_void, *mut c_void) -> i32 =
        unsafe { std::mem::transmute(sym) };
    let env = rt.env();
    // SAFETY: the runtime is attached and bound to `vm` for this thread.
    let pass = unsafe { f(env, std::ptr::null_mut()) };
    assert_eq!(
        pass, 0x7FF,
        "fixture slot checks bitmask (0b{:011b}); no structured error: {:?}",
        pass,
        rt.take_error()
    );

    drop(rt);
    drop(vm);
}
