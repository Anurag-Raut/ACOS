//! C-native feature integration tests.

mod common;

use acos_native_loader::{Loader, LoaderConfig};
use std::os::raw::c_char;

use common::{cstr, RecordingSink};

#[test]
fn c_library_all_features_including_packed_relocations() {
    let _g = common::serial();
    std::env::set_var("ACOS_TEST_ENV", "present");
    let loader = Loader::new(LoaderConfig::for_tests_without_confinement().with_search_path(common::data_dir()));

    // Plain RELA, Android APS2 packed RELA, and RELR packed forms must all load
    // and pass the same feature checks.
    for name in ["libtestc.so", "libtestc-android.so", "libtestc-relr.so"] {
        let h = loader.load_file(&common::data(name)).unwrap();
        let run: extern "C" fn() -> i32 = unsafe { common::sym(h.symbol("tc_run_all").unwrap()) };
        assert_eq!(run(), 0, "{name}: tc_run_all bitmask (0 == all pass)");

        let ctor: extern "C" fn() -> i32 =
            unsafe { common::sym(h.symbol("tc_ctor_ran").unwrap()) };
        assert_eq!(ctor(), 1, "{name}: global constructor ran");

        let sstr: extern "C" fn() -> *const c_char =
            unsafe { common::sym(h.symbol("tc_static_str").unwrap()) };
        assert_eq!(cstr(sstr()), "static-initialized", "{name}: relative reloc");

        // ELF TLS (`__thread`) on the calling thread.
        let set: extern "C" fn(i32) = unsafe { common::sym(h.symbol("tc_tls_set").unwrap()) };
        let get: extern "C" fn() -> i32 = unsafe { common::sym(h.symbol("tc_tls_get").unwrap()) };
        set(1234);
        assert_eq!(get(), 1234, "{name}: __thread variable");

        loader.unload(&h).unwrap();
    }
}

#[test]
fn log_lines_are_captured_by_the_sink() {
    let _g = common::serial();
    std::env::set_var("ACOS_TEST_ENV", "present");
    let sink = RecordingSink::new_arc();
    let config = LoaderConfig::for_tests_without_confinement()
        .with_search_path(common::data_dir())
        .with_log_sink(sink.clone());
    let loader = Loader::new(config);
    let h = loader.load_file(&common::data("libtestc.so")).unwrap();
    let run: extern "C" fn() -> i32 = unsafe { common::sym(h.symbol("tc_run_all").unwrap()) };
    let _ = run();
    loader.unload(&h).unwrap();

    let lines = sink.lines();
    assert!(
        lines.iter().any(|l| l.contains("acos-testc") && l.contains("constructor ran")),
        "constructor log captured: {lines:?}"
    );
    assert!(
        lines.iter().any(|l| l.contains("log from guest 42")),
        "guest log captured: {lines:?}"
    );
    assert!(
        lines.iter().any(|l| l.contains("destructor ran")),
        "destructor log captured on unload: {lines:?}"
    );
}

/// Build `tests/native/testjmp.c` with the NDK clang (bionic headers) into the
/// cargo target dir. Returns `None` (skip loudly) when no NDK is installed.
fn build_testjmp() -> Option<std::path::PathBuf> {
    let ndk = std::env::var("ANDROID_NDK_HOME")
        .unwrap_or_else(|_| "/home/Qui/Android/Sdk/ndk/27.1.12297006".to_string());
    let cc = std::path::PathBuf::from(&ndk)
        .join("toolchains/llvm/prebuilt/linux-x86_64/bin/x86_64-linux-android26-clang");
    let src = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/native/testjmp.c");
    if !cc.is_file() || !src.is_file() {
        return None;
    }
    let out_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp/acos-jmp");
    std::fs::create_dir_all(&out_dir).ok()?;
    let so = out_dir.join("libtestjmp.so");
    let rebuilt = std::process::Command::new(&cc)
        .args(["-shared", "-fPIC", "-O2", "-o"])
        .arg(&so)
        .arg(&src)
        .args(["-llog"])
        .status()
        .ok()?
        .success();
    if !rebuilt {
        return None;
    }
    Some(so)
}

/// bionic `setjmp`/`longjmp`/`_setjmp`/`_longjmp`/`sigsetjmp`/`siglongjmp`
/// must really return twice at the guest call site, with the callee-saved
/// frame live and bionic's signal-mask split (`setjmp` saves, `_setjmp` does
/// not). This runs a real NDK-built bionic library through the loader, so the
/// shims are exercised exactly as `libhermesvm.so` would call them.
#[test]
fn bionic_setjmp_longjmp_roundtrip() {
    let _g = common::serial();
    let Some(so) = build_testjmp() else {
        eprintln!("SKIP bionic_setjmp_longjmp_roundtrip: NDK clang/testjmp.c unavailable");
        return;
    };
    let loader = Loader::new(
        LoaderConfig::for_tests_without_confinement().with_search_path(common::data_dir()),
    );
    let h = loader.load_file(&so).unwrap();
    let run: extern "C" fn() -> i32 = unsafe { common::sym(h.symbol("tj_run_all").unwrap()) };
    let failed = run();
    loader.unload(&h).unwrap();
    assert_eq!(
        failed, 0,
        "bionic jmp_buf checks failed (bitmask): setjmp_roundtrip={} underscore={} \
         zero_value={} sigsetjmp_nomask={} setjmp_saves_mask={} underscore_leaves_mask={} \
         sigsetjmp_saves_mask={}",
        failed & 1,
        failed & 2,
        failed & 4,
        failed & 8,
        failed & 16,
        failed & 32,
        failed & 64
    );
}
