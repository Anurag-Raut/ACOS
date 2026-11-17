//! Missing-symbol and ABI policy tests — the loader must never fake an import.

mod common;

use acos_native_loader::bionic::trap;
use acos_native_loader::{Loader, LoaderConfig, LoaderError, MissingPolicy};

#[test]
fn strict_mode_reports_all_missing_symbols() {
    let _g = common::serial();
    let loader = Loader::new(
        LoaderConfig::for_tests_without_confinement().with_search_path(common::data_dir()),
    );
    let err = loader
        .load_file(&common::data("libtestmissing.so"))
        .err()
        .expect("loading a library with an unknown import must fail");
    assert_eq!(err.code(), "UNSUPPORTED_NATIVE_SYMBOL");
    match err {
        LoaderError::UnsupportedSymbols(libs) => {
            assert!(
                libs.iter().any(|l| l.library.contains("libtestmissing")),
                "{libs:?}"
            );
            assert!(
                libs.iter().any(|l| l
                    .symbols
                    .iter()
                    .any(|s| s == "acos_definitely_missing_symbol")),
                "missing symbol listed: {libs:?}"
            );
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn lazy_trap_binds_and_reports_symbol() {
    let _g = common::serial();
    let before = trap::stub_count();
    let loader = Loader::new(
        LoaderConfig::for_tests_without_confinement()
            .with_search_path(common::data_dir())
            .with_missing_policy(MissingPolicy::LazyTrap),
    );
    let h = loader
        .load_file(&common::data("libtestmissing.so"))
        .expect("LazyTrap mode should load");
    assert!(trap::stub_count() > before, "a trap stub was allocated");
    // The present symbol still works.
    let present: extern "C" fn() -> i32 =
        unsafe { common::sym(h.symbol("testmissing_present").unwrap()) };
    assert_eq!(present(), 1);
    loader.unload(&h).unwrap();
}

#[test]
fn arm64_library_is_reported_as_wrong_abi() {
    let _g = common::serial();
    let loader = Loader::new(LoaderConfig::for_tests_without_confinement());
    let err = loader
        .load_file(&common::data("libhellojni-arm64.so"))
        .err()
        .expect("arm64 library must not load");
    assert_eq!(err.code(), "WRONG_ABI");
    assert!(format!("{err}").contains("aarch64"), "{err}");
}

#[test]
fn missing_dependency_is_structured() {
    let _g = common::serial();
    // libtestuser needs libtestdep; point the search path somewhere empty.
    let loader = Loader::new(LoaderConfig::for_tests_without_confinement());
    let err = loader
        .load_file(&common::data("libtestuser.so"))
        .err()
        .expect("dependency must not resolve");
    assert_eq!(err.code(), "NATIVE_DEP_NOT_FOUND");
    assert!(format!("{err}").contains("libtestdep.so"), "{err}");
}

/// HONESTY_AUDIT #1: `dlopen` must not map an arbitrary absolute host path.
/// With the default config a path outside the configured search roots is
/// refused, so a guest cannot reach `/lib/.../libc.so.6` or any host object.
#[test]
fn dlopen_refuses_an_absolute_host_path() {
    let _g = common::serial();
    let _loader = Loader::new(
        LoaderConfig::for_tests_without_confinement().with_search_path(common::data_dir()),
    );
    let c = std::ffi::CString::new("/lib/x86_64-linux-gnu/libc.so.6").unwrap();
    let h = unsafe { acos_native_loader::bionic::dl::dlopen(c.as_ptr(), 0) };
    assert!(
        h.is_null(),
        "dlopen of an absolute host library must be refused by the default policy"
    );
}

/// ADR-0022: the default loader refuses to map any guest `.so` unless the
/// process reports an active confinement. This test process is not confined,
/// so even a checked-in test library must be refused.
#[test]
fn default_config_refuses_without_active_confinement() {
    let _g = common::serial();
    let loader = Loader::new(LoaderConfig::default().with_search_path(common::data_dir()));
    let err = loader
        .load_file(&common::data("libtestc.so"))
        .err()
        .expect("an unconfined process must not load native code");
    assert_eq!(err.code(), "CONFINEMENT_UNAVAILABLE");
    assert!(matches!(err, LoaderError::ConfinementRequired));
}
