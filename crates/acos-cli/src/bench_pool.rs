//! `acos bench sessions-pool` — throughput, reset cost and memory of an
//! in-process [`SessionPool`] created from one shared template snapshot.
//!
//! For each `N` the parent spawns a fresh child process (memory must be measured
//! without a previous `N`'s allocator residue), which:
//!
//! 1. boots one [`SessionTemplate`] (`Vm::new` + `Vm::snapshot`),
//! 2. starts an `N`-session pool and warms it (`Vm::restore` per session),
//! 3. settles and reads `/proc/self/{smaps_rollup,status}`,
//! 4. runs `samples` rounds of one "small DEX program run + reset" job per
//!    session, concurrently, and reports jobs/s, CPU cores used and per-job
//!    run/reset latency,
//! 5. verifies every job produced the same (exit code, stdout) — a
//!    determinism check, not a correctness oracle.
//!
//! Metric types are kept separate: `anon+shmem` (`RssAnon+RssShmem`), PSS,
//! private and shared pages; wall time in ms; CPU in core-seconds / cores.
//! Nothing is estimated and a failed restore is reported, never hidden.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use acos_dex::DexFile;
use acos_protocol::ErrorCode;
use acos_runtime::session_pool::{SessionLimits, SessionPool, SessionTemplate};
use acos_vm::vm::{CoreLib, VmOptions};
use serde_json::{json, Value};

use super::bench::{
    kib, parse_session_counts, read_memory, resolve_apk, round3, settle, spawn_child, stats,
    BenchArgs, BenchFailure,
};

/// The default pooled workload when the caller passes no `--dex`.
const DEFAULT_JOB_DEX: &str = "fixtures/build/dexprogs/02_int_arith/classes.dex";
/// Host ticks per second (`USER_HZ`), for `/proc/self/stat` CPU accounting.
const CLOCK_TICKS: f64 = 100.0;

// ---------------------------------------------------------------------------
// metric: sessions-pool (parent)
// ---------------------------------------------------------------------------

pub(crate) fn run(args: &BenchArgs) -> Result<Value, BenchFailure> {
    // `resolve_apk` keeps the standard fixture provenance/error behaviour even
    // though the pooled workload itself is a bare DEX program.
    let _apk = resolve_apk(args)?;
    let dex = job_dex(args)?;
    let spec = if args.sessions.trim() == "1,2,5,10" {
        "1,2,5,10,20".to_string()
    } else {
        args.sessions.clone()
    };
    let counts = parse_session_counts(&spec)?;
    let probes = args.runs.clamp(3, 5);

    let mut per_n: Vec<(usize, Value)> = Vec::new();
    for n in &counts {
        let mut memory = Vec::new();
        let mut jobs_per_sec = Vec::new();
        let mut reset_nanos = Vec::new();
        let mut run_nanos = Vec::new();
        let mut cores_used = Vec::new();
        let mut sessions_per_sec = Vec::new();
        let mut create_nanos = Vec::new();
        let mut distinct_max = 0u64;
        let mut failed_total = 0u64;
        let mut jobs_total = 0u64;
        let mut timeouts_total = 0u64;
        let mut panics_total = 0u64;
        let mut template: Value = Value::Null;
        let mut warm_wall_ms = Vec::new();

        for _ in 0..probes {
            let probe = spawn_child(&[
                "bench",
                "sessions-pool",
                "--child",
                "sessions-pool",
                "--n",
                &n.to_string(),
                "--corelib",
                &args.corelib,
                "--dex",
                dex.to_string_lossy().as_ref(),
                "--main",
                &args.main,
                "--runs",
                &args.runs.to_string(),
                "--settle-ms",
                &args.settle_ms.to_string(),
            ])?;
            let mem = probe
                .get("memory")
                .cloned()
                .ok_or_else(|| BenchFailure::internal("sessions-pool probe returned no memory"))?;
            memory.push(mem);
            push_raw(&probe, "throughput/jobs_per_sec", &mut jobs_per_sec);
            push_raw(&probe, "reset_nanos", &mut reset_nanos);
            push_raw(&probe, "job_run_nanos", &mut run_nanos);
            push_raw(&probe, "cpu/cores_used", &mut cores_used);
            if let Some(v) = probe
                .pointer("/session_create/sessions_per_sec")
                .and_then(Value::as_f64)
            {
                sessions_per_sec.push(v);
            }
            if let Some(v) = probe
                .pointer("/session_create/warm_wall_ms")
                .and_then(Value::as_f64)
            {
                warm_wall_ms.push(v);
            }
            if let Some(v) = probe
                .pointer("/session_create/create_nanos/p50")
                .and_then(Value::as_f64)
            {
                create_nanos.push(v);
            }
            if let Some(v) = probe
                .pointer("/determinism/distinct_outputs_max")
                .and_then(Value::as_u64)
            {
                distinct_max = distinct_max.max(v);
            }
            failed_total += probe
                .pointer("/determinism/failed")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            jobs_total += probe
                .pointer("/throughput/total_jobs")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            timeouts_total += probe
                .pointer("/pool_stats/timeouts")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            panics_total += probe
                .pointer("/pool_stats/panics")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            if template.is_null() {
                template = probe.get("template").cloned().unwrap_or(Value::Null);
            }
        }

        per_n.push((
            *n,
            json!({
                "n": n,
                "samples": probes,
                "memory": {
                    "anon_plus_shmem_kib": stats(&memory.iter().map(|m| kib(m, "anon_plus_shmem_kib")).collect::<Vec<_>>()),
                    "pss_kib": stats(&memory.iter().map(|m| kib(m, "pss_kib")).collect::<Vec<_>>()),
                    "rss_kib": stats(&memory.iter().map(|m| kib(m, "rss_kib")).collect::<Vec<_>>()),
                    "private_kib": stats(&memory.iter().map(|m| kib(m, "private_kib")).collect::<Vec<_>>()),
                    "shared_kib": stats(&memory.iter().map(|m| kib(m, "shared_kib")).collect::<Vec<_>>()),
                },
                "miB_anon_plus_shmem_p50": round3(
                    stats(&memory.iter().map(|m| kib(m, "anon_plus_shmem_kib")).collect::<Vec<_>>())
                        .get("p50").and_then(Value::as_f64).unwrap_or(0.0) / 1024.0),
                "session_create": {
                    "sessions_per_sec": stats(&sessions_per_sec),
                    "warm_wall_ms": stats(&warm_wall_ms),
                    "create_nanos_p50_per_session": stats(&create_nanos),
                },
                "throughput": {
                    "jobs_per_sec": stats(&jobs_per_sec),
                    "total_jobs": jobs_total,
                },
                "cpu": {
                    "cores_used": stats(&cores_used),
                    "efficiency_cores_per_session_p50": round3(
                        stats(&cores_used).get("p50").and_then(Value::as_f64).unwrap_or(0.0)
                            / (*n).max(1) as f64),
                },
                "job_run_nanos": stats(&run_nanos),
                "reset_nanos": stats(&reset_nanos),
                "reset_ms_p50": round3(
                    stats(&reset_nanos).get("p50").and_then(Value::as_f64).unwrap_or(0.0) / 1e6),
                "determinism": {
                    "distinct_outputs_max": distinct_max,
                    "failed": failed_total,
                },
                "pool_stats": {
                    "timeouts": timeouts_total,
                    "panics": panics_total,
                },
                "template": template,
            }),
        ));
    }

    let marginal: Vec<Value> = per_n
        .windows(2)
        .map(|pair| {
            let (low_n, low) = &pair[0];
            let (high_n, high) = &pair[1];
            let delta = high
                .pointer("/memory/anon_plus_shmem_kib/p50")
                .and_then(Value::as_f64)
                .unwrap_or(0.0)
                - low
                    .pointer("/memory/anon_plus_shmem_kib/p50")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0);
            let d = (*high_n).saturating_sub(*low_n) as f64;
            json!({
                "from_n": low_n,
                "to_n": high_n,
                "marginal_miB_per_session": round3(delta / d / 1024.0),
            })
        })
        .collect();

    let sessions: Vec<Value> = per_n.into_iter().map(|(_, v)| v).collect();
    Ok(json!({
        "metric": "sessions-pool",
        "definition": "In-process SessionPool from one shared template snapshot: session create \
                       rate (Vm::restore), jobs/s of a small DEX program run+reset per session, \
                       reset latency, CPU scaling and total memory at N=1,2,5,10,20",
        "unit": "sessions/s and jobs/s; ms latency; KiB memory (MiB convenience); cores",
        "type": "anon+shmem (RssAnon+RssShmem) with PSS/private/shared separate; wall (parent-measured child); CPU core-seconds",
        "cold_or_warm": "template boot is cold once per fresh child; sessions are warm restores from the template snapshot",
        "corelib": args.corelib,
        "fixture": dex.display().to_string(),
        "sessions": sessions,
        "marginal": marginal,
        "isolation_note": "In-process pool: shared boot DEX/decoded code is immutable, but \
                           processes remain the isolation boundary for untrusted APKs. See \
                           results/bench/POOL_DESIGN.md. `HostGuard` RSS reads process RSS, not \
                           per-session RSS.",
        "legacy_reference": {
            "reset_app_cmd_p50_ms": 110.3,
            "reset_app_to_frame_p50_ms": 748.1,
            "worker_create_to_frame_p50_s": "1.251-1.343",
            "source": "BENCHMARKS.md §3",
        },
    }))
}

fn push_raw(doc: &Value, pointer: &str, out: &mut Vec<f64>) {
    let Some(raw) = doc
        .pointer(&format!("/{pointer}/raw"))
        .and_then(Value::as_array)
    else {
        return;
    };
    for v in raw {
        if let Some(f) = v.as_f64() {
            out.push(f);
        }
    }
}

// ---------------------------------------------------------------------------
// child probe
// ---------------------------------------------------------------------------

pub(crate) fn child_body(args: &BenchArgs) -> Result<Value, BenchFailure> {
    let corelib = corelib(&args.corelib)?;
    let dex_path = job_dex(args)?;
    let bytes = std::fs::read(&dex_path)
        .map_err(|e| BenchFailure::internal(format!("read {}: {e}", dex_path.display())))?;
    let dex = Arc::new(
        DexFile::parse(bytes)
            .map_err(|e| BenchFailure::internal(format!("parse {}: {e}", dex_path.display())))?,
    );

    let options = VmOptions {
        corelib,
        ..VmOptions::default()
    };
    let t_boot = Instant::now();
    let template = SessionTemplate::boot(options)
        .map_err(|e| BenchFailure::internal(format!("template boot: {e}")))?;
    let boot_ms = t_boot.elapsed().as_secs_f64() * 1000.0;
    let template_json = json!({
        "boot_ms": round3(boot_ms),
        "snapshot_bytes": template.snapshot_bytes(),
        "boot_classes": template.boot_classes(),
        "corelib": args.corelib,
    });

    let n = args.n.max(1);
    let limits = SessionLimits {
        // The pooled bench is a run+reset loop; a generous but bounded wall and
        // a per-session guest heap quota. RSS stays disabled (process-wide).
        heap_max_bytes: 0,
        wall_secs: 30,
        rss_max_bytes: 0,
    };
    let mut pool = SessionPool::start(template, n, limits)
        .map_err(|e| BenchFailure::internal(format!("pool start: {e}")))?;

    // ---- session creation ------------------------------------------------
    let t_warm = Instant::now();
    let warm = pool
        .warm()
        .map_err(|e| BenchFailure::internal(format!("pool warm: {e}")))?;
    let warm_wall = t_warm.elapsed();
    let created = warm.iter().filter(|w| w.created).count();
    let create_nanos: Vec<f64> = warm
        .iter()
        .filter(|w| w.created)
        .map(|w| w.create_nanos as f64)
        .collect();
    let sessions_per_sec = if warm_wall.as_secs_f64() > 0.0 {
        created as f64 / warm_wall.as_secs_f64()
    } else {
        0.0
    };

    // ---- settled memory --------------------------------------------------
    settle(args.settle_ms);
    let memory = read_memory()
        .ok_or_else(|| BenchFailure::internal("could not read /proc/self/smaps_rollup"))?;

    // ---- throughput ------------------------------------------------------
    let samples = args.runs.clamp(3, 10);
    let warmup = 1;
    let mut walls_ms = Vec::new();
    let mut jobs_per_sec = Vec::new();
    let mut cores_used = Vec::new();
    let mut run_nanos = Vec::new();
    let mut reset_nanos = Vec::new();
    let mut signatures: BTreeSet<(i32, String)> = BTreeSet::new();
    let mut failed = 0usize;
    let mut total_jobs = 0usize;
    let mut base_id = 0u64;

    for i in 0..(samples + warmup) {
        let ticks_before = cpu_ticks();
        let t = Instant::now();
        let results = pool
            .run_round(&args.main, Some(Arc::clone(&dex)), base_id, true)
            .map_err(|e| BenchFailure::internal(format!("pool run_round: {e}")))?;
        let wall = t.elapsed();
        base_id += n as u64;
        let ticks_after = cpu_ticks();

        if i >= warmup {
            walls_ms.push(wall.as_secs_f64() * 1000.0);
            jobs_per_sec.push(n as f64 / wall.as_secs_f64().max(f64::MIN_POSITIVE));
            if let (Some(a), Some(b)) = (ticks_before, ticks_after) {
                let core_secs = b.saturating_sub(a) as f64 / CLOCK_TICKS;
                cores_used.push(core_secs / wall.as_secs_f64().max(f64::MIN_POSITIVE));
            }
        }

        for r in &results {
            total_jobs += 1;
            if !r.is_completed() {
                failed += 1;
            }
            signatures.insert((r.exit_code, r.stdout.clone()));
            run_nanos.push(r.run_nanos as f64);
            reset_nanos.push(r.reset_nanos as f64);
        }
    }

    let pool_stats = pool.stats();
    Ok(json!({
        "mode": "sessions-pool",
        "n": n,
        "template": template_json,
        "memory": memory,
        "session_create": {
            "created": created,
            "sessions_per_sec": round3(sessions_per_sec),
            "warm_wall_ms": round3(warm_wall.as_secs_f64() * 1000.0),
            "create_nanos": stats(&create_nanos),
        },
        "throughput": {
            "samples": samples,
            "total_jobs": total_jobs,
            "walls_ms": stats(&walls_ms),
            "jobs_per_sec": stats(&jobs_per_sec),
        },
        "cpu": {
            "cores_used": stats(&cores_used),
            "efficiency_cores_per_session_p50": round3(
                stats(&cores_used).get("p50").and_then(Value::as_f64).unwrap_or(0.0)
                    / n as f64),
        },
        "job_run_nanos": stats(&run_nanos),
        "reset_nanos": stats(&reset_nanos),
        "determinism": {
            "distinct_outputs_max": signatures.len() as u64,
            "completed": total_jobs - failed,
            "failed": failed,
        },
        "pool_stats": {
            "sessions": pool_stats.sessions,
            "jobs": pool_stats.jobs,
            "resets": pool_stats.resets,
            "panics": pool_stats.panics,
            "timeouts": pool_stats.timeouts,
            "restore_failures": pool_stats.restore_failures,
        },
    }))
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn corelib(name: &str) -> Result<CoreLib, BenchFailure> {
    CoreLib::parse(name).ok_or_else(|| {
        BenchFailure::new(
            ErrorCode::InvalidArgument,
            format!("unknown corelib {name:?} (expected bootstrap|real)"),
        )
    })
}

fn job_dex(args: &BenchArgs) -> Result<PathBuf, BenchFailure> {
    if let Some(first) = args.dexes.first() {
        return Ok(first.clone());
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let path = root.join(DEFAULT_JOB_DEX);
    if path.is_file() {
        Ok(path)
    } else {
        Err(BenchFailure::new(
            ErrorCode::InvalidArgument,
            format!(
                "no --dex given and the default pooled workload {} is absent (run \
                 tools/build/build-fixtures.sh)",
                path.display()
            ),
        ))
    }
}

/// Process CPU ticks (utime + stime) from `/proc/self/stat`.
fn cpu_ticks() -> Option<u64> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    let close = stat.rfind(')')?;
    let fields: Vec<&str> = stat[close + 1..].split_whitespace().collect();
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    Some(utime + stime)
}
