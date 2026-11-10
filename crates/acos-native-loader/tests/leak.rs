//! Mapping-leak test: 1000 load/unload cycles must not grow `/proc/self/maps`.

mod common;

use acos_native_loader::{Loader, LoaderConfig};

fn maps_lines() -> usize {
    std::fs::read_to_string("/proc/self/maps")
        .expect("/proc/self/maps")
        .lines()
        .count()
}

#[test]
fn load_unload_1000_times_does_not_leak_mappings() {
    let _g = common::serial();
    let sink = common::RecordingSink::new_arc();
    let loader = Loader::new(
        LoaderConfig::for_tests_without_confinement()
            .with_search_path(common::data_dir())
            .with_log_sink(sink),
    );
    let before = maps_lines();
    for _ in 0..1000 {
        let h = loader.load_file(&common::data("libtestc.so")).unwrap();
        loader.unload(&h).unwrap();
    }
    let after = maps_lines();
    assert!(
        after <= before + 2,
        "mapping count grew from {before} to {after} over 1000 load/unload cycles"
    );
    assert_eq!(loader.report().libraries.len(), 0, "no libraries left registered");
}
