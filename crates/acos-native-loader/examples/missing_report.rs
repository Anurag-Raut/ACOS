//! Print the structured missing-symbol behaviour (Strict and LazyTrap).
//!
//! ```text
//! cargo run -p acos-native-loader --example missing_report
//! ```

use acos_native_loader::{Loader, LoaderConfig, MissingPolicy};

fn main() {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data");
    let path = dir.join("libtestmissing.so");

    let strict = Loader::new(LoaderConfig::for_tests_without_confinement().with_search_path(dir.clone()));
    match strict.load_file(&path) {
        Ok(_) => println!("unexpected success"),
        Err(e) => {
            println!("code = {}", e.code());
            println!("{e}");
        }
    }

    let lazy = Loader::new(
        LoaderConfig::for_tests_without_confinement()
            .with_search_path(dir)
            .with_missing_policy(MissingPolicy::LazyTrap),
    );
    let h = lazy.load_file(&path).expect("LazyTrap load");
    println!("\nLazyTrap loaded; trap stubs allocated = {}", acos_native_loader::bionic::trap::stub_count());
    println!("report = {}", lazy.report().to_json());
    lazy.unload(&h).unwrap();
}
