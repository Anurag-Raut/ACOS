//! W25-C acceptance tests for the in-process [`SessionPool`].
//!
//! Covered:
//!
//! * **determinism across `N`** — the same job list through pools of 1/2/3/5
//!   sessions yields byte-identical stdout and exit codes;
//! * **crash isolation** — a deliberately panicking session is caught, dropped
//!   and replaced while the other sessions finish, and the pool keeps working;
//! * **reset equivalence** — a stateful program run twice in one session leaks
//!   state, and after `reset_all` the same program produces the original output
//!   again (no leaked statics, no leaked stdout, no leaked class state);
//! * **watchdog isolation** — a deliberately infinite program is stopped by the
//!   per-session wall guard and the session is replaced (skipped loudly when the
//!   `javac`/`d8` toolchain is absent).

mod common;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use acos_dex::DexFile;
use acos_runtime::session_pool::{
    JobStatus, SessionFault, SessionJob, SessionLimits, SessionPool, SessionTemplate,
};
use acos_vm::vm::{CoreLib, Vm, VmOptions};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn options() -> VmOptions {
    VmOptions {
        heap_max_bytes: 256 * 1024 * 1024,
        gc_stress: false,
        corelib: CoreLib::Bootstrap,
        boot_dir: None,
        trace_natives: false,
    }
}

fn dex_arc(name: &str) -> Option<Arc<DexFile>> {
    let path = repo_root().join(format!("fixtures/build/dexprogs/{name}/classes.dex"));
    let bytes = std::fs::read(&path).ok()?;
    let parsed = DexFile::parse(bytes).ok()?;
    Some(Arc::new(parsed))
}

/// Reference output from one fresh `Vm` (the pre-snapshot path).
fn reference_output(dex: &Arc<DexFile>, main: &str) -> (i32, String) {
    let mut vm = Vm::new(options()).expect("fresh vm");
    vm.add_shared_dex(Arc::clone(dex));
    let out = vm.run(main, &[]);
    (out.exit_code, out.stdout)
}

fn require(name: &str) -> Option<Arc<DexFile>> {
    match dex_arc(name) {
        Some(d) => Some(d),
        None => {
            eprintln!("SKIP session_pool test: fixtures/build/dexprogs/{name} absent");
            None
        }
    }
}

#[test]
fn deterministic_across_worker_counts() {
    let Some(hello) = require("01_hello") else {
        return;
    };
    let Some(objs) = require("10_objects_fields") else {
        return;
    };
    // A deterministic workload: two programs, alternating, six jobs per pool.
    let programs: Vec<Arc<DexFile>> = vec![hello, objs];
    let expected: Vec<(i32, String)> = programs
        .iter()
        .map(|d| reference_output(d, "Main"))
        .collect();

    let template = SessionTemplate::boot(options()).expect("template boot");
    let mut per_n: Vec<Vec<(i32, String)>> = Vec::new();
    for &n in &[1usize, 2, 3, 5] {
        let mut pool =
            SessionPool::start(template.clone(), n, SessionLimits::default()).expect("pool");
        let jobs: Vec<SessionJob> = (0..6)
            .map(|i| SessionJob::new(i as u64, "Main", Some(Arc::clone(&programs[i % 2]))))
            .collect();
        let results = pool.run_batch(jobs).expect("batch");
        assert_eq!(results.len(), 6);
        let mut got = Vec::with_capacity(results.len());
        for (i, r) in results.iter().enumerate() {
            assert!(
                r.is_completed(),
                "N={n} job {i} status {:?} error {:?}",
                r.status,
                r.error
            );
            assert_eq!(
                (r.exit_code, r.stdout.clone()),
                expected[i % 2],
                "N={n} job {i} diverged from the reference run"
            );
            assert!(r.run_nanos > 0, "N={n} job {i} has no run time");
            assert!(r.reset_nanos > 0, "N={n} job {i} has no reset time");
            got.push((r.exit_code, r.stdout.clone()));
        }
        per_n.push(got);
    }
    for (i, got) in per_n.iter().enumerate().skip(1) {
        assert_eq!(
            got,
            &per_n[0],
            "worker count {} produced a different result than N=1",
            [1usize, 2, 3, 5][i]
        );
    }
}

#[test]
fn panicking_session_is_isolated_and_replaced() {
    let Some(dex) = require("01_hello") else {
        return;
    };
    let (want_exit, want_out) = reference_output(&dex, "Main");
    let template = SessionTemplate::boot(options()).expect("template boot");
    let mut pool = SessionPool::start(template, 3, SessionLimits::default()).expect("pool");

    let mut poisoned = SessionJob::new(100, "Main", Some(Arc::clone(&dex)));
    poisoned.fault = Some(SessionFault::Panic);
    let jobs = vec![
        poisoned,
        SessionJob::new(101, "Main", Some(Arc::clone(&dex))),
        SessionJob::new(102, "Main", Some(Arc::clone(&dex))),
    ];
    let results = pool.run_batch(jobs).expect("batch");
    let by_id: HashMap<u64, &acos_runtime::session_pool::JobResult> =
        results.iter().map(|r| (r.job_id, r)).collect();

    assert_eq!(by_id[&100].status, JobStatus::Panicked);
    assert!(by_id[&100]
        .error
        .as_deref()
        .unwrap_or_default()
        .contains("injected panic"));
    for id in [101u64, 102] {
        assert_eq!(
            by_id[&id].status,
            JobStatus::Completed,
            "job {id} disturbed"
        );
        assert_eq!(by_id[&id].exit_code, want_exit);
        assert_eq!(by_id[&id].stdout, want_out);
    }

    // The panicked worker restores its session lazily and keeps serving.
    let more: Vec<SessionJob> = (200..209)
        .map(|i| SessionJob::new(i, "Main", Some(Arc::clone(&dex))))
        .collect();
    for r in pool.run_batch(more).expect("recovery batch") {
        assert_eq!(r.status, JobStatus::Completed, "recovery job {}", r.job_id);
        assert_eq!(r.stdout, want_out, "recovery job {} output", r.job_id);
    }
    let stats = pool.stats();
    assert_eq!(stats.panics, 1, "exactly one injected panic");
    assert_eq!(stats.restore_failures, 0);
    assert_eq!(stats.sessions, 3);
}

#[test]
fn reset_restores_clean_state() {
    let Some(dex) = require("10_objects_fields") else {
        return;
    };
    let (_, want) = reference_output(&dex, "Main");
    let template = SessionTemplate::boot(options()).expect("template boot");
    // One session: run the stateful program twice without a reset, then reset
    // and run it a third time.
    let mut pool = SessionPool::start(template, 1, SessionLimits::default()).expect("pool");

    let mut first = SessionJob::new(1, "Main", Some(Arc::clone(&dex)));
    first.reset_after = false;
    let r1 = pool.run_batch(vec![first]).expect("batch 1").remove(0);
    assert_eq!(r1.status, JobStatus::Completed);
    assert_eq!(r1.stdout, want, "clean first run");

    let mut second = SessionJob::new(2, "Main", Some(Arc::clone(&dex)));
    second.reset_after = false;
    let r2 = pool.run_batch(vec![second]).expect("batch 2").remove(0);
    assert_eq!(r2.status, JobStatus::Completed);
    assert_ne!(
        r2.stdout, want,
        "running a stateful program twice in one session must leak state"
    );
    assert_eq!(pool.stats().resets, 0, "no reset was requested yet");

    let resets = pool.reset_all().expect("reset");
    assert_eq!(resets.len(), 1);
    assert!(resets[0].ok, "reset must succeed");
    assert!(resets[0].reset_nanos > 0, "reset must be measured");
    assert_eq!(pool.stats().resets, 1);

    // After an explicit reset, a *run + reset* job starts clean and the reset it
    // performs makes the next one clean too: the two outputs are identical and
    // equal the reference run (no leaked statics, no leaked stdout).
    let r3 = pool
        .run_batch(vec![SessionJob::new(3, "Main", Some(Arc::clone(&dex)))])
        .expect("batch 3")
        .remove(0);
    assert_eq!(r3.status, JobStatus::Completed);
    assert_eq!(
        r3.stdout, want,
        "after reset the same program must be byte-identical and leak-free"
    );
    assert!(r3.reset_nanos > 0);

    let r4 = pool
        .run_batch(vec![SessionJob::new(4, "Main", Some(Arc::clone(&dex)))])
        .expect("batch 4")
        .remove(0);
    assert_eq!(r4.status, JobStatus::Completed);
    assert_eq!(r4.stdout, want, "reset loop must be idempotent");
    assert!(r4.reset_nanos > 0);
}

#[test]
fn looping_session_is_killed_by_wall_guard() {
    let Some(loop_dex_path) = common::compile_case(&["LoopForever"]) else {
        eprintln!("SKIP looping_session_is_killed_by_wall_guard: javac/d8 toolchain absent");
        return;
    };
    let bytes = match std::fs::read(&loop_dex_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("SKIP looping_session_is_killed_by_wall_guard: {e}");
            return;
        }
    };
    let Some(loop_dex) = DexFile::parse(bytes).ok().map(Arc::new) else {
        eprintln!("SKIP looping_session_is_killed_by_wall_guard: LoopForever DEX did not parse");
        return;
    };
    let Some(ok_dex) = require("01_hello") else {
        return;
    };
    let (_, want_out) = reference_output(&ok_dex, "Main");

    let template = SessionTemplate::boot(options()).expect("template boot");
    let limits = SessionLimits {
        wall_secs: 1,
        ..SessionLimits::default()
    };
    let mut pool = SessionPool::start(template, 3, limits).expect("pool");

    let jobs = vec![
        SessionJob::new(10, "LoopForever", Some(loop_dex)),
        SessionJob::new(11, "Main", Some(Arc::clone(&ok_dex))),
        SessionJob::new(12, "Main", Some(Arc::clone(&ok_dex))),
    ];
    let results = pool.run_batch(jobs).expect("batch");
    let by_id: HashMap<u64, &acos_runtime::session_pool::JobResult> =
        results.iter().map(|r| (r.job_id, r)).collect();
    assert_eq!(by_id[&10].status, JobStatus::Killed, "loop must be killed");
    assert_eq!(by_id[&10].error.as_deref(), Some("TIMEOUT"));
    for id in [11u64, 12] {
        assert_eq!(
            by_id[&id].status,
            JobStatus::Completed,
            "job {id} disturbed"
        );
        assert_eq!(by_id[&id].stdout, want_out);
    }

    // The killed session was replaced; it serves normal jobs afterwards.
    let more: Vec<SessionJob> = (20..29)
        .map(|i| SessionJob::new(i, "Main", Some(Arc::clone(&ok_dex))))
        .collect();
    for r in pool.run_batch(more).expect("recovery batch") {
        assert_eq!(r.status, JobStatus::Completed, "recovery job {}", r.job_id);
        assert_eq!(r.stdout, want_out);
    }
    assert!(pool.stats().timeouts >= 1, "the loop must be counted");
    assert_eq!(pool.stats().panics, 0);
}
