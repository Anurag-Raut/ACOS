//! Regression guard for the interpretation fast path.
//!
//! `Vm::count_opcodes == true` forces `run_batch` to execute every instruction
//! through the general path (`exec_one` + `execute`); `false` uses the tight
//! `src/dispatch.rs` fast path. The two must produce byte-identical stdout and
//! exit codes for every corpus program, with and without GC stress. This is the
//! standing check that quickening / inline caches / frame reuse did not change
//! semantics (polymorphic dispatch, class init, field narrowing, exceptions).
//!
//! Skipped loudly when the DEX fixtures are absent (no Android SDK on the host).

use std::path::{Path, PathBuf};

use acos_dex::DexFile;
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

fn run(name: &str, gc_stress: bool, fast: bool) -> (i32, String) {
    let dex = std::fs::read(repo_root().join(format!(
        "fixtures/build/dexprogs/{name}/classes.dex"
    )))
    .expect("read dex");
    let mut vm = Vm::new(VmOptions {
        heap_max_bytes: 256 * 1024 * 1024,
        gc_stress,
        ..VmOptions::default()
    })
    .expect("vm new");
    // `count_opcodes` is the switch that routes to the general path.
    vm.count_opcodes = !fast;
    vm.add_dex(DexFile::parse(dex).expect("parse dex"));
    let outcome = vm.run("Main", &[]);
    (outcome.exit_code, outcome.stdout)
}

fn check(gc_stress: bool, skip_threads: bool) {
    let names = fixture_names();
    if names.is_empty() {
        eprintln!("SKIP fast_path_equals_general_path: fixtures/build/dexprogs is absent");
        return;
    }
    for name in &names {
        if skip_threads && name == "19_threads_sync" {
            continue;
        }
        let fast = run(name, gc_stress, true);
        let general = run(name, gc_stress, false);
        assert_eq!(
            fast, general,
            "fast path diverged from general path for {name} (gc_stress={gc_stress})"
        );
    }
}

#[test]
fn fast_path_equals_general_path() {
    check(false, false);
}

/// GC-stress forces a collection before every allocation, so the frame pool and
/// reference bitmaps are exercised hard. 19 is skipped only to bound runtime;
/// `tools/diff-dexprogs.sh --gc-stress` covers it.
#[test]
fn fast_path_equals_general_path_gc_stress() {
    check(true, true);
}
