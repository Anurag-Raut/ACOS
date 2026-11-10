//! C-native feature integration tests.

mod common;

use acos_native_loader::{Loader, LoaderConfig};
use std::os::raw::c_char;

use common::{cstr, RecordingSink};

#[test]
fn c_library_all_features_including_packed_relocations() {
    let _g = common::serial();
    std::env::set_var("ACOS_TEST_ENV", "present");
    let loader = Loader::new(LoaderConfig::default().with_search_path(common::data_dir()));

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
    let config = LoaderConfig::default()
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
