//! DT_NEEDED dependency ordering and ELF symbol interposition.

mod common;

use acos_native_loader::{Loader, LoaderConfig};

#[test]
fn dependency_order_and_interposition() {
    let _g = common::serial();
    let loader = Loader::new(LoaderConfig::for_tests_without_confinement().with_search_path(common::data_dir()));
    let h = loader.load_file(&common::data("libtestuser.so")).unwrap();

    // user_run checks: dep_greet, dep_calls_hook (dep's undefined symbol binds
    // to the user library), dep_interpose_call (dep's R_X86_64_64 to
    // `interpose_target` binds to the user's definition because the user
    // object is earlier in the global search order), dep_only.
    let run: extern "C" fn() -> i32 = unsafe { common::sym(h.symbol("user_run").unwrap()) };
    assert_eq!(run(), 0, "user_run bitmask (0 == all pass)");

    // Constructors run dependencies-first.
    let order: extern "C" fn(i32) -> i32 =
        unsafe { common::sym(h.symbol("user_order").unwrap()) };
    assert_eq!(order(0), 1, "dependency constructor runs first");
    assert_eq!(order(1), 2, "dependent constructor runs second");

    // The dependency was loaded automatically.
    let report = loader.report();
    let names: Vec<&str> = report.libraries.iter().map(|l| l.name.as_str()).collect();
    assert!(names.contains(&"libtestdep.so"), "dep loaded: {names:?}");

    loader.unload(&h).unwrap();
}
