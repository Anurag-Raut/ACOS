//! Manual smoke test used during development (not part of `cargo test`).
//!
//! Run with: `cargo run -p acos-native-loader --example smoke`

use acos_native_loader::{Loader, LoaderConfig, MissingPolicy};
use std::path::PathBuf;

fn data(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

fn main() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data");
    let loader = Loader::new(
        LoaderConfig::for_tests_without_confinement()
            .with_search_path(dir)
            .with_missing_policy(MissingPolicy::Strict),
    );

    // C library.
    let c = loader
        .load_file(&data("libtestc.so"))
        .expect("load libtestc");
    let run: extern "C" fn() -> i32 =
        unsafe { std::mem::transmute(c.symbol("tc_run_all").unwrap()) };
    println!("tc_run_all = {} (0 == all pass)", run());
    println!("tc_ctor_ran = {}", unsafe {
        let f: extern "C" fn() -> i32 = std::mem::transmute(c.symbol("tc_ctor_ran").unwrap());
        f()
    });

    for variant in ["libtestc-android.so", "libtestc-relr.so"] {
        let v = loader.load_file(&data(variant)).expect("load variant");
        let f: extern "C" fn() -> i32 =
            unsafe { std::mem::transmute(v.symbol("tc_run_all").unwrap()) };
        println!("{variant}: tc_run_all = {} (0 == all pass)", f());
    }

    // C++ library.
    eprintln!("loading libtestcpp...");
    let cpp = loader
        .load_file(&data("libtestcpp.so"))
        .expect("load libtestcpp");
    eprintln!("loaded libtestcpp");
    let crun: extern "C" fn() -> i32 =
        unsafe { std::mem::transmute(cpp.symbol("cpp_run_all").unwrap()) };
    eprintln!("calling cpp_run_all...");
    println!("cpp_run_all = {} (0 == all pass)", crun());
    eprintln!("cpp_run_all returned");

    // Dependency + interposition.
    let user = loader
        .load_file(&data("libtestuser.so"))
        .expect("load libtestuser");
    let urun: extern "C" fn() -> i32 =
        unsafe { std::mem::transmute(user.symbol("user_run").unwrap()) };
    println!("user_run = {} (1 == pass)", urun());

    println!("{}", loader.report().to_json());
}
