//! Native regression tests for `shim/gralloc_shim.c`.
//!
//! Compiles `tests/native/gralloc_shim_test.c` (which `#include`s the real
//! `shim/gralloc_shim.c` source directly) with the host C compiler and runs
//! each case. Covers:
//!   1. `GraphicBufferMapper::getDataspace` + the other 22 `libui.so` mapper
//!      methods operating safely with `self = NULL` (no `mMapper` SIGSEGV).
//!   2. Refcounted per-handle mappings across concurrent `lock` / `unlock`
//!      callers (SwiftShader `AHardwareBuffer_lockPlanes` + `Surface::lock`).
//!   3. Multi-threaded lock/unlock stress with concurrent readers.

use std::path::PathBuf;
use std::process::Command;

fn manifest() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn build_harness(tag: &str) -> PathBuf {
    let dir = manifest()
        .join("../../.cache/gralloc-shim-native")
        .join(format!("{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let bin = dir.join("gralloc_shim_test");
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let mut cmd = Command::new(&cc);
    cmd.arg("-O1")
        .arg("-g")
        .arg("-pthread")
        .arg("-Wall")
        .arg("-Wno-unused-parameter")
        .arg("-o")
        .arg(&bin)
        .arg(manifest().join("tests/native/gralloc_shim_test.c"));
    let out = cmd.output().unwrap_or_else(|e| panic!("run {cc}: {e}"));
    assert!(
        out.status.success(),
        "gralloc_shim_test build failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    bin
}

fn run_case(case: &str) {
    let bin = build_harness(case);
    let out = Command::new(&bin).arg(case).output().expect("run harness");
    let _ = std::fs::remove_dir_all(bin.parent().unwrap());
    assert!(
        out.status.success(),
        "case {case} failed (status {:?}):\n{}{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn mapper_dataspace_and_metadata_survive_null_hal_object() {
    run_case("dataspace_and_metadata_without_hal");
}

#[test]
fn concurrent_lock_unlock_preserves_shared_mapping() {
    run_case("concurrent_lock_unlock_preserves_mapping");
}

#[test]
fn multithreaded_lock_unlock_churn_keeps_reader_pages_mapped() {
    run_case("multithreaded_lock_unlock_stress");
}
