//! Differential guard for the opt-in typed-IR tier (`ACOS_EXEC=ir`).
//!
//! Runs every DEX corpus program twice — once through the general interpreter
//! (`Vm::count_opcodes == true`, which forces `exec_one`) and once through the
//! typed-IR tier (`exec::set_enabled(Some(true))`) — and requires byte-identical
//! stdout and exit codes, with and without GC stress.
//!
//! This is the standing `fast_path_equivalence` check for the IR tier: it
//! proves fused superinstructions, pre-resolved branch targets, the per-frame
//! program cache and per-instruction fallback did not change semantics
//! (exceptions, class init, field narrowing, array bounds, polymorphism).
//!
//! Skipped loudly when the DEX fixtures are absent (no Android SDK on the host).

use std::path::{Path, PathBuf};

use acos_dex::DexFile;
use acos_vm::exec;
use acos_vm::{Vm, VmOptions};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn fixture_names() -> Vec<String> {
    let dir = repo_root().join("fixtures/build/dexprogs");
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn run(name: &str, gc_stress: bool, general: bool) -> (i32, String) {
    let dex =
        std::fs::read(repo_root().join(format!("fixtures/build/dexprogs/{name}/classes.dex")))
            .expect("read dex");
    // The IR path is only taken when `count_opcodes` is false; the general path
    // is forced by `count_opcodes` and never consults the switch.
    exec::set_enabled(Some(!general));
    let mut vm = Vm::new(VmOptions {
        heap_max_bytes: 256 * 1024 * 1024,
        gc_stress,
        ..VmOptions::default()
    })
    .expect("vm new");
    vm.count_opcodes = general;
    vm.add_dex(DexFile::parse(dex).expect("parse dex"));
    let outcome = vm.run("Main", &[]);
    exec::set_enabled(None);
    (outcome.exit_code, outcome.stdout)
}

fn check(gc_stress: bool) {
    let names = fixture_names();
    if names.is_empty() {
        // The only differential guard for the IR trampoline (ADR-0023/0024);
        // under `ACOS_REQUIRE_FIXTURES` a missing corpus must fail, not skip.
        acos_test_support::have_fixture("fixtures/build/dexprogs");
        acos_test_support::require_prereq(false, "check", "ir_equals_general_path: fixtures/build/dexprogs is absent");
        return;
    }
    for name in &names {
        let general = run(name, gc_stress, true);
        let ir = run(name, gc_stress, false);
        assert_eq!(
            ir, general,
            "IR diverged from general path for {name} (gc_stress={gc_stress})"
        );
    }
}

#[test]
fn ir_equals_general_path() {
    check(false);
}

/// GC stress forces a collection before every allocation, exercising the frame
/// pool and reference bitmaps (the IR tier never changes root semantics).
#[test]
fn ir_equals_general_path_gc_stress() {
    check(true);
}
