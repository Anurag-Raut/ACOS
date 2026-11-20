//! T-42 acceptance tests: snapshot/restore correctness, invariants and
//! structured refusal.
//!
//! The differential part snapshots a freshly booted VM **once**, restores it for
//! every `fixtures/build/dexprogs` program, and requires byte-identical stdout
//! and exit code versus a fresh `Vm::new` + `add_dex` + `run`, normal and
//! GC-stress. The regex corpus needs the real Android 14 boot classpath and is
//! skipped loudly when `.cache/boot/android-34` is absent.

use std::path::{Path, PathBuf};

use acos_dex::DexFile;
use acos_vm::snapshot::SNAPSHOT_FORMAT_VERSION;
use acos_vm::vm::{CoreLib, ThreadRT, Vm, VmOptions};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The two full-real-corpus tests each boot the Android-14 DEX set 2x per
/// program. In an unoptimised debug test binary that is minutes of interpreter
/// work, so they run in release by default and can be forced with
/// `ACOS_SNAPSHOT_FULL_REAL=1`. The regex subset and all bootstrap tests still
/// run in debug.
fn guard_heavy_real() -> bool {
    if cfg!(debug_assertions) && std::env::var_os("ACOS_SNAPSHOT_FULL_REAL").is_none() {
        acos_test_support::require_prereq(false, "guard_heavy_real", "full real corpus in debug (release-only; set ACOS_SNAPSHOT_FULL_REAL=1 to force)");
        return true;
    }
    false
}

fn boot_dir() -> Option<PathBuf> {
    // Same resolution as the VM itself (explicit -> $ACOS_BOOT_DIR -> walk up),
    // so the test also finds the cache when run from a git worktree.
    acos_vm::vm::default_boot_dir(None)
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

const REAL_CORELIB_ONLY: &[&str] = &[
    "21_regex_basic",
    "22_regex_advanced",
    "23_regex_split_replace",
    "26_regex_matrix",
];

struct Output {
    exit: i32,
    stdout: String,
}

fn options(corelib: CoreLib, gc_stress: bool) -> VmOptions {
    VmOptions {
        heap_max_bytes: 256 * 1024 * 1024,
        gc_stress,
        corelib,
        boot_dir: if corelib == CoreLib::Real {
            boot_dir()
        } else {
            None
        },
        trace_natives: false,
    }
}

fn run_program(vm: &mut Vm, name: &str) -> Output {
    let dex_path = repo_root().join(format!("fixtures/build/dexprogs/{name}/classes.dex"));
    let bytes = std::fs::read(&dex_path).expect("read dex");
    vm.add_dex(DexFile::parse(bytes).expect("parse dex"));
    let outcome = vm.run("Main", &[]);
    Output {
        exit: outcome.exit_code,
        stdout: outcome.stdout,
    }
}

/// Fresh `Vm::new` + `add_dex` + `run` (the pre-snapshot reference path).
fn fresh_run(name: &str, corelib: CoreLib, gc_stress: bool) -> Output {
    let mut vm = Vm::new(options(corelib, gc_stress)).expect("fresh vm");
    run_program(&mut vm, name)
}

fn for_each_case(corelib: CoreLib, gc_stress: bool, mut f: impl FnMut(&str, &[u8])) {
    let names = fixture_names();
    if acos_test_support::require_prereq(!(names.is_empty()), "for_each_case", "snapshot differential: fixtures/build/dexprogs is absent").is_none() {
        return;
    }
    for name in &names {
        match corelib {
            CoreLib::Bootstrap => {
                if REAL_CORELIB_ONLY.contains(&name.as_str()) {
                    continue;
                }
                // 19 is heavy; the per-allocation stress run is skipped for it
                // exactly as in tests/dexprogs.rs.
                if gc_stress && name == "19_threads_sync" {
                    continue;
                }
            }
            CoreLib::Real => {
                if !REAL_CORELIB_ONLY.contains(&name.as_str()) {
                    continue;
                }
                if boot_dir().is_none() {
                    acos_test_support::require_prereq(false, "for_each_case", "real-corelib snapshot differential: .cache/boot/android-34 absent");
                    return;
                }
            }
        }
        f(name, &[]);
    }
}

#[test]
fn snapshot_restore_bootstrap_dexprogs_normal() {
    let corelib = CoreLib::Bootstrap;
    let template = Vm::new(options(corelib, false)).expect("template");
    let snap = template.snapshot().expect("snapshot");
    let mut failures = Vec::new();
    let mut count = 0;
    for_each_case(corelib, false, |name, _| {
        let want = fresh_run(name, corelib, false);
        let mut vm = Vm::restore(&snap).expect("restore");
        let got = run_program(&mut vm, name);
        count += 1;
        if got.stdout != want.stdout || got.exit != want.exit {
            failures.push(format!("{name}: restore diverged from fresh"));
        }
    });
    assert!(count > 0, "no dexprogs executed");
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn snapshot_restore_bootstrap_dexprogs_gc_stress() {
    let corelib = CoreLib::Bootstrap;
    let template = Vm::new(options(corelib, true)).expect("template");
    let snap = template.snapshot().expect("snapshot");
    let mut failures = Vec::new();
    let mut count = 0;
    for_each_case(corelib, true, |name, _| {
        let want = fresh_run(name, corelib, true);
        let mut vm = Vm::restore(&snap).expect("restore");
        let got = run_program(&mut vm, name);
        count += 1;
        if got.stdout != want.stdout || got.exit != want.exit {
            failures.push(format!("{name}: restore diverged from fresh (gc-stress)"));
        }
    });
    assert!(count > 0, "no dexprogs executed");
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn snapshot_restore_real_regex() {
    if acos_test_support::require_prereq(!(boot_dir().is_none()), "snapshot_restore_real_regex", ".cache/boot/android-34 absent").is_none() {
        return;
    }
    let corelib = CoreLib::Real;
    let template = Vm::new(options(corelib, false)).expect("template");
    let snap = template.snapshot().expect("snapshot");
    let mut failures = Vec::new();
    let mut count = 0;
    for_each_case(corelib, false, |name, _| {
        let want = fresh_run(name, corelib, false);
        let mut vm = Vm::restore(&snap).expect("restore");
        let got = run_program(&mut vm, name);
        count += 1;
        if got.stdout != want.stdout || got.exit != want.exit {
            failures.push(format!("{name}: restore diverged from fresh"));
        }
    });
    assert!(count > 0, "no regex dexprogs executed");
    assert!(failures.is_empty(), "{failures:#?}");
}

/// GC-stress variant of the real `tools/diff-dexprogs.sh --corelib real`
/// corpus: a collection before every allocation must not change restore output.
#[test]
fn snapshot_restore_real_dexprogs_gc_stress() {
    if guard_heavy_real() {
        return;
    }
    if acos_test_support::require_prereq(!(boot_dir().is_none()), "snapshot_restore_real_dexprogs_gc_stress", "boot cache absent").is_none() {
        return;
    }
    let corelib = CoreLib::Real;
    let names = fixture_names();
    if names.is_empty() {
        return;
    }
    let template = Vm::new(options(corelib, true)).expect("template");
    let snap = template.snapshot().expect("snapshot");
    let mut failures = Vec::new();
    for name in &names {
        if name == "19_threads_sync" {
            continue;
        }
        let want = fresh_run(name, corelib, true);
        let mut vm = Vm::restore(&snap).expect("restore");
        let got = run_program(&mut vm, name);
        if got.stdout != want.stdout || got.exit != want.exit {
            failures.push(format!("{name}: restore diverged from fresh (gc-stress)"));
        }
    }
    assert_eq!(failures.len(), 0, "{failures:#?}");
}

/// All 23 dexprogs through the real boot classpath, fresh vs restore. This is
/// the `tools/diff-dexprogs.sh --corelib real` corpus, exercised via restore.
#[test]
fn snapshot_restore_real_all_dexprogs() {
    if guard_heavy_real() {
        return;
    }
    if acos_test_support::require_prereq(!(boot_dir().is_none()), "snapshot_restore_real_all_dexprogs", ".cache/boot/android-34 absent").is_none() {
        return;
    }
    let corelib = CoreLib::Real;
    let names = fixture_names();
    if acos_test_support::require_prereq(!(names.is_empty()), "snapshot_restore_real_all_dexprogs", "fixtures absent").is_none() {
        return;
    }
    let template = Vm::new(options(corelib, false)).expect("template");
    let snap = template.snapshot().expect("snapshot");
    let mut failures = Vec::new();
    for name in &names {
        let want = fresh_run(name, corelib, false);
        let mut vm = Vm::restore(&snap).expect("restore");
        let got = run_program(&mut vm, name);
        if got.stdout != want.stdout || got.exit != want.exit {
            failures.push(format!(
                "{name}: exit {} vs {}, stdout {} vs {} bytes",
                got.exit,
                want.exit,
                got.stdout.len(),
                want.stdout.len()
            ));
        }
    }
    assert_eq!(failures.len(), 0, "{failures:#?}");
}

#[test]
fn snapshot_restore_100_cycles_is_stable() {
    let corelib = CoreLib::Bootstrap;
    let template = Vm::new(options(corelib, false)).expect("template");
    let snap = template.snapshot().expect("snapshot");
    for _ in 0..100 {
        let mut vm = Vm::restore(&snap).expect("restore");
        let out = run_program(&mut vm, "01_hello");
        assert_eq!(out.exit, 0);
        assert!(!out.stdout.is_empty());
    }
}

#[test]
fn snapshot_is_deterministic_and_idempotent() {
    let corelib = CoreLib::Bootstrap;
    let a = Vm::new(options(corelib, false))
        .unwrap()
        .snapshot()
        .unwrap();
    let b = Vm::new(options(corelib, false))
        .unwrap()
        .snapshot()
        .unwrap();
    assert_eq!(
        a, b,
        "two snapshots of the same boot state must be byte-equal"
    );

    let restored = Vm::restore(&a).unwrap();
    let again = restored.snapshot().unwrap();
    assert_eq!(again, a, "snapshot(restore(s)) must equal s");

    // And twice through the round trip.
    let twice = Vm::restore(&again).unwrap().snapshot().unwrap();
    assert_eq!(twice, a);
}

#[test]
fn snapshot_header_round_trips() {
    let vm = Vm::new(options(CoreLib::Bootstrap, false)).unwrap();
    let snap = vm.snapshot().unwrap();
    let header = Vm::snapshot_header(&snap).unwrap();
    assert_eq!(header.version, SNAPSHOT_FORMAT_VERSION);
    assert_eq!(header.corelib, CoreLib::Bootstrap);
    assert!(matches!(header.class_count, n if n > 0));
}

#[test]
fn snapshot_refuses_bad_magic_and_version() {
    let vm = Vm::new(options(CoreLib::Bootstrap, false)).unwrap();
    let snap = vm.snapshot().unwrap();

    let mut bad_magic = snap.clone();
    bad_magic[0] ^= 0xff;
    let err = Vm::restore(&bad_magic).unwrap_err();
    assert_eq!(err.code(), "SNAPSHOT_FORMAT", "{err}");

    // Offset 8 is the little-endian format version.
    let mut bad_version = snap.clone();
    bad_version[8] = bad_version[8].wrapping_add(1);
    let err = Vm::restore(&bad_version).unwrap_err();
    assert_eq!(err.code(), "SNAPSHOT_FORMAT", "{err}");
}

#[test]
fn snapshot_refuses_fingerprint_mismatch() {
    let vm = Vm::new(options(CoreLib::Bootstrap, false)).unwrap();
    let snap = vm.snapshot().unwrap();

    // property hash lives at offset 31 (magic 8 + version 4 + corelib 1 +
    // trace 1 + gc_stress 1 + heap_max 8 + boot_fp 8).
    let mut tampered = snap.clone();
    let off = acos_vm::snapshot::header_property_hash_offset();
    tampered[off] ^= 0x01;
    let err = Vm::restore(&tampered).unwrap_err();
    assert_eq!(err.code(), "SNAPSHOT_MISMATCH", "{err}");

    // boot fingerprint lives immediately before it.
    let mut tampered = snap.clone();
    tampered[off - 1] ^= 0x01;
    let err = Vm::restore(&tampered).unwrap_err();
    assert_eq!(err.code(), "SNAPSHOT_MISMATCH", "{err}");
}

#[test]
fn snapshot_refuses_pending_exception() {
    let mut vm = Vm::new(options(CoreLib::Bootstrap, false)).unwrap();
    // A non-zero pending exception is a live control-flow signal.
    vm.pending_exception = 1;
    let err = vm.snapshot().unwrap_err();
    assert_eq!(err.code(), "SNAPSHOT_PRECONDITION", "{err}");
}

#[test]
fn snapshot_refuses_extra_live_thread() {
    let mut vm = Vm::new(options(CoreLib::Bootstrap, false)).unwrap();
    vm.threads.push(ThreadRT::new(0));
    vm.threads.push(ThreadRT::new(0));
    let err = vm.snapshot().unwrap_err();
    assert_eq!(err.code(), "SNAPSHOT_PRECONDITION", "{err}");
}

#[test]
fn snapshot_refuses_open_fd() {
    let mut vm = Vm::new(options(CoreLib::Bootstrap, false)).unwrap();
    let _ = vm.vfs.pipe2(0).expect("pipe");
    assert!(vm.vfs.open_fd_count() >= 2);
    let err = vm.snapshot().unwrap_err();
    assert_eq!(err.code(), "SNAPSHOT_PRECONDITION", "{err}");
}

#[test]
fn snapshot_refuses_held_monitor() {
    let mut vm = Vm::new(options(CoreLib::Bootstrap, false)).unwrap();
    vm.monitors.insert(1, (0, u32::MAX));
    let err = vm.snapshot().unwrap_err();
    assert_eq!(err.code(), "SNAPSHOT_PRECONDITION", "{err}");
}

#[test]
fn restored_vm_runs_a_second_program_after_the_first() {
    // Restore, run a program to completion, then run a *second* program on the
    // same restored VM: the heap/class state must remain coherent.
    let template = Vm::new(options(CoreLib::Bootstrap, false)).unwrap();
    let snap = template.snapshot().unwrap();
    let mut vm = Vm::restore(&snap).unwrap();
    let a = run_program(&mut vm, "02_int_arith");
    let b = run_program(&mut vm, "05_branches_loops");
    assert_eq!(a.exit, 0);
    assert_eq!(b.exit, 0);
    assert!(!a.stdout.is_empty() && !b.stdout.is_empty());
}

#[test]
fn snapshot_refuses_app_dex() {
    // T-42 scope: the stream carries descriptors only, so every DEX provider
    // must be re-loadable from the same boot files. An application DEX will not
    // be loaded by `Vm::restore`; rather than silently drop its code, snapshot
    // refuses. (Framework/app snapshots are T-43.)
    let mut vm = Vm::new(options(CoreLib::Bootstrap, false)).unwrap();
    let dex_path = repo_root().join("fixtures/build/dexprogs/01_hello/classes.dex");
    if let Ok(bytes) = std::fs::read(&dex_path) {
        vm.add_dex(DexFile::parse(bytes).unwrap());
        let err = vm.snapshot().unwrap_err();
        assert_eq!(err.code(), "SNAPSHOT_PRECONDITION", "{err}");
    }
}
