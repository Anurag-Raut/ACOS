//! C++ feature integration tests (static libc++ + libunwind, exceptions).

mod common;

use acos_native_loader::{Loader, LoaderConfig};

#[test]
fn cpp_features_and_destructor_on_unload() {
    let _g = common::serial();
    let sink = common::RecordingSink::new_arc();
    let loader = Loader::new(
        LoaderConfig::default()
            .with_search_path(common::data_dir())
            .with_log_sink(sink.clone()),
    );
    let h = loader.load_file(&common::data("libtestcpp.so")).unwrap();

    // std::string, std::vector, new/delete, and exceptions caught inside the
    // library (the exception path requires dl_iterate_phdr to expose the
    // object's PT_GNU_EH_FRAME to the guest unwinder).
    let run: extern "C" fn() -> i32 = unsafe { common::sym(h.symbol("cpp_run_all").unwrap()) };
    assert_eq!(run(), 0, "cpp_run_all bitmask (0 == all pass)");

    let dtor: extern "C" fn() -> i32 =
        unsafe { common::sym(h.symbol("cpp_dtor_count").unwrap()) };
    assert_eq!(dtor(), 0, "static object destructor not run before unload");

    loader.unload(&h).unwrap();

    let lines = sink.lines();
    assert!(
        lines
            .iter()
            .any(|l| l.contains("acos-testcpp") && l.contains("static object destructor")),
        "static C++ destructor ran on unload: {lines:?}"
    );
}
