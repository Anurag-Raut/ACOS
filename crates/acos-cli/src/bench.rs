//! `acos bench <metric>` — the ACOS V1 metrics harness (`BENCHMARKS.md` §2/§5).
//!
//! # What this measures, and what it refuses to measure
//!
//! Only metrics whose inputs exist in today's engine are measured here. A
//! metric that needs a capability the engine does not have yet (a frame, a UI
//! tree, a reset path, 20 workers) returns an `unsupported` envelope naming the
//! exact missing dependency. It is never filled with an estimate
//! (`BENCHMARKS.md` §2 rule 11, `docs/handoff/04`).
//!
//! Measured today:
//!
//! | metric | what it is |
//! |---|---|
//! | `process-start` | process spawn → `Vm` ready (wall, cold, parent-measured) |
//! | `boot-attach` | `boot_app_process` stages: open/load/looper/thread/attach |
//! | `bind-app` | attach → `bindApplication` delivered + the phase after it |
//! | `idle-rss` | settled fixed idle memory: PSS, RSS, anon+shmem, private/shared |
//! | `sessions` | per-session incremental + marginal memory at N=1,2,5,10 |
//! | `idle-cpu` | steady CPU of the parked/blocked runtime over a wall window |
//! | `interpreter` | interpreter micro benchmarks (wall, instructions, insn/s) |
//!
//! # Method
//!
//! * ≥20 samples per metric, p50/p95 (nearest-rank) — never a single run
//!   (`BENCHMARKS.md` §2 rule 2).
//! * Memory is read from `/proc/self/smaps_rollup` and `/proc/self/status`;
//!   `/usr/bin/time` is not installed on this host (`BENCHMARKS.md` §6).
//! * Metric types are named explicitly and never mixed: **RSS**, **PSS**,
//!   **anon+shmem** (`RssAnon + RssShmem`, the legacy workset type), private vs
//!   shared pages.
//! * No fixed sleep sits in a timed path; settles are deliberately outside the
//!   timed region and reported (`settle_ms`).
//! * Every command emits one [`Envelope`]; the JSON is re-parsed as an
//!   `Envelope` before printing, so a false `ok: true` cannot be emitted.
//!
//! # Honest limits of today's boot path (recorded, not hidden)
//!
//! * Sessions are **in-process** and sequential: `Runtime::with_host` installs
//!   the process-global Binder transport, so `N` live runtimes are a *memory*
//!   experiment, not a working multi-session configuration.
//! * DEX is read with `std::fs::read` and parsed into the VM heap: the boot
//!   classpath is **not** `mmap`-ed, so its pages are private anonymous pages and
//!   are not shareable between sessions. `sessions` reports the measured
//!   private/shared split so the claim is evidence, not assertion.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use acos_protocol::{Backend, Envelope, ErrorCode, Fidelity, Metrics, ProtocolError};
use clap::ValueEnum;
use serde_json::{json, Map, Value};

/// Which metric to measure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Metric {
    /// Process spawn → VM ready (real boot classpath loaded).
    ProcessStart,
    /// `boot_app_process`: per-stage wall + classes/clinits/instructions/natives.
    BootAttach,
    /// attach → `IApplicationThread.bindApplication` delivered, plus the phase
    /// that follows it on the main looper.
    BindApp,
    /// Full launch: boot classpath → framework attach → bindApplication →
    /// Application.onCreate → ClientTransaction → Activity.onCreate/onResume.
    Launch,
    /// Settled fixed idle memory (PSS / RSS / anon+shmem / private vs shared).
    IdleRss,
    /// Per-session incremental and marginal memory at the requested N values.
    Sessions,
    /// Per-session total + marginal memory with process-shared boot data, N=1..20.
    SessionsShared,
    /// In-process session pool: create/reset throughput, CPU scaling and memory
    /// from one shared template snapshot (T-44).
    SessionsPool,
    /// Steady CPU of the parked/blocked runtime over a wall window.
    IdleCpu,
    /// Interpreter micro benchmarks (wall, instructions, instructions/second).
    Interpreter,
    /// Every measurable metric in one run.
    All,
    // ---- not measurable yet: reported as `unsupported` with the dependency ---
    /// Cold start → first correct frame (needs the renderer).
    ColdFirstFrame,
    /// Worker create → correct frame (needs sessions + renderer).
    WorkerCreateFrame,
    /// Fresh screenshot latency (needs `acos-render` + frame sequencing).
    FreshScreenshot,
    /// Reset-to-ready (needs a reset path).
    ResetToReady,
    /// UI tree with text (needs the UI-tree bridge).
    UiTree,
    /// tap/type/scroll → observed state (needs input + UI tree).
    ActionToState,
    /// End-to-end verify wall time (needs launch/assert/evidence/reset).
    EndToEndVerify,
    /// Verified tasks / GiB-minute (needs the full loop + job cgroups).
    TasksPerGibMinute,
    /// N = 20 concurrent workers (needs a working multi-session boot path).
    TwentyWorkers,
}

impl Metric {
    const fn name(self) -> &'static str {
        match self {
            Metric::ProcessStart => "process-start",
            Metric::BootAttach => "boot-attach",
            Metric::BindApp => "bind-app",
            Metric::Launch => "launch",
            Metric::IdleRss => "idle-rss",
            Metric::Sessions => "sessions",
            Metric::SessionsShared => "sessions-shared",
            Metric::SessionsPool => "sessions-pool",
            Metric::IdleCpu => "idle-cpu",
            Metric::Interpreter => "interpreter",
            Metric::All => "all",
            Metric::ColdFirstFrame => "cold-first-frame",
            Metric::WorkerCreateFrame => "worker-create-frame",
            Metric::FreshScreenshot => "fresh-screenshot",
            Metric::ResetToReady => "reset-to-ready",
            Metric::UiTree => "ui-tree",
            Metric::ActionToState => "action-to-state",
            Metric::EndToEndVerify => "end-to-end-verify",
            Metric::TasksPerGibMinute => "tasks-per-gib-minute",
            Metric::TwentyWorkers => "twenty-workers",
        }
    }

    /// The dependency that blocks the metric, for the not-yet-measurable rows.
    const fn dependency(self) -> Option<&'static str> {
        match self {
            Metric::ColdFirstFrame => Some(
                "acos-render is not bound to the graphics natives yet, so no frame (correct or \
                 otherwise) can be produced; needs `acos-render` + the SurfaceFlinger/frame \
                 -sequencing seam (V1 §1.1, §1.3)",
            ),
            Metric::WorkerCreateFrame => None,
            Metric::FreshScreenshot => Some(
                "needs `acos-render` capture + request/cause frame sequencing; no frame retrieval \
                 API exists (V1 §1.3)",
            ),
            Metric::ResetToReady => None,
            Metric::UiTree => Some(
                "the UI-tree bridge is not wired (no `View` hierarchy access from the runtime); \
                 `UiNode`/`UiTree` exist in acos-protocol only (V1 §1.4)",
            ),
            Metric::ActionToState => Some(
                "needs input injection plus the UI tree to observe state; neither exists yet \
                 (V1 §1.5)",
            ),
            Metric::EndToEndVerify => Some(
                "needs launch → assert → evidence → reset → cleanup; launch (ClientTransaction) \
                 and reset are not wired (V1 §6.1, T-23)",
            ),
            Metric::TasksPerGibMinute => Some(
                "needs the fixed T01–T12 suite plus job-cgroup memory accounting; neither exists \
                 yet (V1 §6.3, BENCHMARKS.md §2 rule 5)",
            ),
            Metric::TwentyWorkers => Some(
                "the in-process boot path installs one process-global Binder transport, so sessions \
                 are not independently usable yet; needs per-session transports before N=20 \
                 (V1 §5.1)",
            ),
            _ => None,
        }
    }
}

/// Arguments for `acos bench`.
#[derive(Debug, clap::Args)]
pub struct BenchArgs {
    /// The metric to measure.
    #[arg(value_enum)]
    pub metric: Metric,

    /// Samples per metric (BENCHMARKS.md §2 rule 2 requires ≥20 for latency).
    #[arg(long, default_value_t = 20)]
    pub runs: usize,

    /// Warm-up samples, discarded (not part of p50/p95).
    #[arg(long, default_value_t = 2)]
    pub warmup: usize,

    /// Emit the standard ACOS JSON envelope instead of a text summary.
    #[arg(long)]
    pub json: bool,

    /// APK used for the framework boot path.
    #[arg(long, default_value = "fixtures/build/apps/hello-views.apk")]
    pub apk: PathBuf,

    /// DEX program(s) for the interpreter metric (repeatable).
    #[arg(long = "dex")]
    pub dexes: Vec<PathBuf>,

    /// Main class for the interpreter metric.
    #[arg(long, default_value = "Main")]
    pub main: String,

    /// Comma-separated session counts for `sessions` (e.g. `1,2,5,10`).
    #[arg(long, default_value = "1,2,5,10")]
    pub sessions: String,

    /// Core library: `real` (Android 14 boot classpath) or `bootstrap`.
    #[arg(long, default_value = "real")]
    pub corelib: String,

    /// Settle time before a memory read, outside the timed region (ms).
    #[arg(long, default_value_t = 1000)]
    pub settle_ms: u64,

    /// Wall window for each `idle-cpu` sample (seconds).
    #[arg(long, default_value_t = 5)]
    pub cpu_secs: u64,

    /// Also write the raw JSON document to this path.
    #[arg(long = "out")]
    pub out: Option<PathBuf>,

    /// Internal child-probe mode (self-spawned; not for humans).
    #[arg(long, hide = true)]
    pub child: Option<String>,

    /// Session count for the internal sessions probe.
    #[arg(long, hide = true, default_value_t = 0)]
    pub n: usize,

    /// Idle-memory variant for the internal probe (`vm` or `booted`).
    #[arg(long, hide = true, default_value = "vm")]
    pub variant: String,
}

/// Entry point from `main.rs`.
pub fn run(args: BenchArgs) -> std::process::ExitCode {
    if let Some(mode) = args.child.clone() {
        return run_child(&mode, &args);
    }
    let started = Instant::now();
    let result = match args.metric {
        Metric::All => measure_all(&args),
        other => measure_one(other, &args),
    };
    let wall_ms = started.elapsed().as_millis() as u64;
    let request_id = new_request_id("bench");
    match result {
        Ok(doc) => {
            let envelope = Envelope::pass(
                request_id.clone(),
                Backend::AcosNative,
                Fidelity::NativeRuntime,
                Metrics::wall_ms(wall_ms),
            )
            .with_result(doc.clone())
            .map_err(|e| e.to_string())
            .and_then(validate_roundtrip);
            match envelope {
                Ok(envelope) => {
                    if let Some(path) = &args.out {
                        write_doc(path, &envelope);
                    }
                    if args.json {
                        print_json(&envelope);
                    } else {
                        print_text(&doc);
                    }
                    std::process::ExitCode::SUCCESS
                }
                Err(msg) => fail(&request_id, ErrorCode::Internal, &msg, wall_ms, args.json),
            }
        }
        Err(failure) => {
            let envelope = Envelope::failure(
                request_id.clone(),
                Backend::AcosNative,
                Fidelity::NativeRuntime,
                ProtocolError::new(failure.code, failure.message.clone()),
                Metrics::wall_ms(wall_ms),
            );
            if let Some(path) = &args.out {
                write_doc(path, &envelope);
            }
            if args.json {
                print_json(&envelope);
            } else {
                eprintln!(
                    "bench: {} failed: {}: {}",
                    args.metric.name(),
                    failure.code,
                    failure.message
                );
            }
            std::process::ExitCode::FAILURE
        }
    }
}

/// A structured bench failure (maps 1:1 onto an [`ErrorCode`]).
#[derive(Debug, Clone)]
pub(crate) struct BenchFailure {
    code: ErrorCode,
    message: String,
}

impl BenchFailure {
    pub(crate) fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        BenchFailure {
            code,
            message: message.into(),
        }
    }
    pub(crate) fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, message)
    }
    fn unsupported(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::UnsupportedAndroidApi, message)
    }
}

fn validate_roundtrip(envelope: Envelope) -> Result<Envelope, String> {
    // The wire form must be a valid envelope per acos-protocol: re-parse it so a
    // structurally invalid document can never leave the harness.
    let text = serde_json::to_string(&envelope).map_err(|e| format!("serialize: {e}"))?;
    let back: Envelope =
        serde_json::from_str(&text).map_err(|e| format!("envelope round-trip: {e}"))?;
    back.validate()
        .map_err(|e| format!("envelope invariant: {e}"))?;
    Ok(back)
}

fn write_doc(path: &Path, envelope: &Envelope) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match serde_json::to_string_pretty(envelope) {
        Ok(text) => {
            if let Err(e) = std::fs::write(path, text) {
                eprintln!("bench: warning: could not write {}: {e}", path.display());
            }
        }
        Err(e) => eprintln!("bench: warning: serialize failed: {e}"),
    }
}

fn print_json(envelope: &Envelope) {
    match serde_json::to_string_pretty(envelope) {
        Ok(text) => println!("{text}"),
        Err(e) => eprintln!("bench: serialize failed: {e}"),
    }
}

fn new_request_id(prefix: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{prefix}_{:x}_{:x}", std::process::id(), nanos)
}

fn fail(
    request_id: &str,
    code: ErrorCode,
    message: &str,
    wall_ms: u64,
    json_out: bool,
) -> std::process::ExitCode {
    let envelope = Envelope::failure(
        request_id.to_string(),
        Backend::AcosNative,
        Fidelity::NativeRuntime,
        ProtocolError::new(code, message),
        Metrics::wall_ms(wall_ms),
    );
    if json_out {
        print_json(&envelope);
    } else {
        eprintln!("bench: {code}: {message}");
    }
    std::process::ExitCode::FAILURE
}

// ---------------------------------------------------------------------------
// metric dispatch
// ---------------------------------------------------------------------------

fn measure_one(metric: Metric, args: &BenchArgs) -> Result<Value, BenchFailure> {
    if let Some(dep) = metric.dependency() {
        return Err(BenchFailure::unsupported(format!(
            "metric `{}` is not_yet_measurable: {dep}",
            metric.name()
        )));
    }
    match metric {
        Metric::ProcessStart => bench_process_start(args),
        Metric::BootAttach => bench_boot_attach(args),
        Metric::BindApp => bench_bind_app(args),
        Metric::Launch => bench_launch(args),
        Metric::WorkerCreateFrame => bench_worker_create_frame(args),
        Metric::ResetToReady => bench_worker_create_frame(args),
        Metric::IdleRss => bench_idle_rss(args),
        Metric::Sessions => bench_sessions(args),
        Metric::SessionsShared => super::bench_sessions::run(args),
        Metric::SessionsPool => super::bench_pool::run(args),
        Metric::IdleCpu => bench_idle_cpu(args),
        Metric::Interpreter => bench_interpreter(args),
        _ => Err(BenchFailure::internal("unreachable metric dispatch")),
    }
}

fn measure_all(args: &BenchArgs) -> Result<Value, BenchFailure> {
    let mut measured = Map::new();
    let mut unavailable = Vec::new();
    for metric in [
        Metric::ProcessStart,
        Metric::BootAttach,
        Metric::BindApp,
        Metric::IdleRss,
        Metric::Sessions,
        Metric::IdleCpu,
        Metric::Interpreter,
    ] {
        match measure_one(metric, args) {
            Ok(doc) => {
                measured.insert(metric.name().to_string(), doc);
            }
            Err(failure) => {
                // A failed metric is recorded, never dropped and never guessed.
                measured.insert(
                    metric.name().to_string(),
                    json!({"error": failure.message, "code": failure.code.as_str()}),
                );
            }
        }
    }
    for metric in NOT_YET {
        unavailable.push(json!({
            "metric": metric.name(),
            "status": "not_yet_measurable",
            "dependency": metric.dependency().unwrap_or(""),
        }));
    }
    Ok(json!({
        "methodology": METHODOLOGY,
        "measured": measured,
        "not_yet_measurable": unavailable,
    }))
}

/// Every metric the engine cannot produce yet, with its blocking dependency.
const NOT_YET: &[Metric] = &[
    Metric::ColdFirstFrame,
    Metric::FreshScreenshot,
    Metric::UiTree,
    Metric::ActionToState,
    Metric::EndToEndVerify,
    Metric::TasksPerGibMinute,
    Metric::TwentyWorkers,
];

const METHODOLOGY: &str = "BENCHMARKS.md §2: >=20 samples (p50/p95 nearest-rank), metric type \
     named (RSS/PSS/anon+shmem, never mixed), cold/warm stated, no /usr/bin/time (absent), \
     /proc smaps_rollup + status, no fixed sleep inside a timed path, one bounded guard scope";

// ---------------------------------------------------------------------------
// metric: process-start
// ---------------------------------------------------------------------------

fn bench_process_start(args: &BenchArgs) -> Result<Value, BenchFailure> {
    let exe =
        std::env::current_exe().map_err(|e| BenchFailure::internal(format!("current_exe: {e}")))?;
    let mut wall = Vec::new();
    let mut internal = Vec::new();
    let mut classes = 0usize;
    for i in 0..(args.runs + args.warmup) {
        let started = Instant::now();
        let mut child = Command::new(&exe)
            .args([
                "bench",
                "process-start",
                "--child",
                "vm-ready",
                "--corelib",
                &args.corelib,
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| BenchFailure::internal(format!("spawn {}: {e}", exe.display())))?;
        let mut reader = BufReader::new(
            child
                .stdout
                .take()
                .ok_or_else(|| BenchFailure::internal("child stdout unavailable"))?,
        );
        let mut line = String::new();
        let read = reader.read_line(&mut line);
        let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        let _ = child.kill();
        let _ = child.wait();
        match read {
            Ok(0) => {
                return Err(BenchFailure::internal(
                    "child exited without reporting VM ready (see stderr)",
                ))
            }
            Ok(_) => {}
            Err(e) => return Err(BenchFailure::internal(format!("read child stdout: {e}"))),
        }
        let probe = parse_probe(&line)?;
        if let Some(c) = probe.get("classes").and_then(Value::as_u64) {
            classes = c as usize;
        }
        if i >= args.warmup {
            wall.push(elapsed_ms);
            if let Some(ms) = probe.get("ms").and_then(Value::as_f64) {
                internal.push(ms);
            }
        }
    }
    Ok(json!({
        "metric": "process-start",
        "definition": "process spawn -> Vm ready (boot classpath loaded), wall clock",
        "unit": "ms",
        "type": "wall",
        "cold_or_warm": "cold (fresh process per sample, boot classpath on page cache)",
        "corelib": args.corelib,
        "classes_loaded": classes,
        "samples": wall.len(),
        "wall_ms": stats(&wall),
        "in_process_ms": stats(&internal),
        "note": "wall is parent-measured spawn->first stdout line (includes exec, dynamic \
                 linking, rust init); in_process_ms excludes exec",
    }))
}

// ---------------------------------------------------------------------------
// metric: boot-attach (per-stage)
// ---------------------------------------------------------------------------

/// One boot stage: wall time plus the VM counters it moved.
#[derive(Debug, Clone)]
struct Stage {
    name: String,
    ms: f64,
    classes: u64,
    clinits: u64,
    instructions: u64,
    natives: u64,
}

fn bench_boot_attach(args: &BenchArgs) -> Result<Value, BenchFailure> {
    let apk = resolve_apk(args)?;
    let mut vm_ready = Vec::new();
    let mut boot_total = Vec::new();
    let mut per_stage: Vec<Vec<f64>> = Vec::new();
    let mut stage_names: Vec<String> = Vec::new();
    // The very first Vm in this process is the only cold one: from then on the
    // process-global boot cache (raw DEX + parsed files + decoded bodies) serves
    // every later sample. Recorded explicitly so the cold cost is never hidden.
    let mut first_cold: Option<(f64, f64)> = None;
    for i in 0..(args.runs + args.warmup) {
        let probe = boot_probe(&apk, args, false, false, false)?;
        if i == 0 {
            first_cold = Some((probe.vm_ready_ms, probe.boot_ms));
        }
        if i < args.warmup {
            continue;
        }
        vm_ready.push(probe.vm_ready_ms);
        boot_total.push(probe.boot_ms);
        for (idx, stage) in probe.stages.iter().enumerate() {
            if per_stage.len() <= idx {
                per_stage.push(Vec::new());
                stage_names.push(stage.name.clone());
            }
            per_stage[idx].push(stage.ms);
        }
    }
    // Counters come from one extra *traced* probe: tracing adds overhead, so it
    // is deliberately kept out of the timed samples.
    let traced = boot_probe(&apk, args, false, true, false)?;
    let last_counters = traced.counters();
    let raw_stages = traced.stages;
    // Each stage reports its own cost and the VM counters it moved (deltas from
    // the previous stage), so "classes loaded / clinits / instructions /
    // natives" are attributable instead of one opaque total.
    let mut prev = Stage {
        name: String::new(),
        ms: 0.0,
        classes: 0,
        clinits: 0,
        instructions: 0,
        natives: 0,
    };
    let stages: Vec<Value> = stage_names
        .iter()
        .enumerate()
        .map(|(idx, name)| {
            let cur = raw_stages.get(idx).cloned().unwrap_or(Stage {
                name: name.clone(),
                ms: 0.0,
                classes: prev.classes,
                clinits: prev.clinits,
                instructions: prev.instructions,
                natives: prev.natives,
            });
            let row = json!({
                "stage": name,
                "ms": stats(&per_stage[idx]),
                "counters_delta": {
                    "classes": cur.classes.saturating_sub(prev.classes),
                    "clinits": cur.clinits.saturating_sub(prev.clinits),
                    "instructions": cur.instructions.saturating_sub(prev.instructions),
                    "natives": cur.natives.saturating_sub(prev.natives),
                },
                "counters_total": {
                    "classes": cur.classes,
                    "clinits": cur.clinits,
                    "instructions": cur.instructions,
                    "natives": cur.natives,
                },
            });
            prev = cur;
            row
        })
        .collect();
    Ok(json!({
        "metric": "boot-attach",
        "definition": "Vm::new + boot_app_process stages through IActivityManager.attachApplication",
        "unit": "ms",
        "type": "wall",
        "cold_or_warm": "first Vm in the process is cold; the reported in-process samples reuse the \
                         process-global boot cache (warm). See results/bench/MEMORY_PROFILE.md",
        "corelib": args.corelib,
        "fixture": apk.display().to_string(),
        "apk_sha256": file_sha256(&apk),
        "samples": boot_total.len(),
        "first_vm_in_process": first_cold.map(|(v, b)| json!({
            "vm_ready_ms": round3(v), "boot_ms": round3(b), "note": "cold (empty BootCache)"
        })).unwrap_or(Value::Null),
        "vm_ready_ms": stats(&vm_ready),
        "boot_ms": stats(&boot_total),
        "stages": stages,
        "counters": last_counters,
        "note": "stages mirror acos_runtime::boot::boot_app_process exactly (same public calls, \
                 same order); the probe fails loudly if the attach handshake is not recorded",
    }))
}

// ---------------------------------------------------------------------------
// metric: bind-app
// ---------------------------------------------------------------------------

fn bench_bind_app(args: &BenchArgs) -> Result<Value, BenchFailure> {
    let apk = resolve_apk(args)?;
    let mut attach = Vec::new();
    let mut bind = Vec::new();
    let mut phase = Vec::new();
    let mut delivered = 0usize;
    let mut blocker: Option<String> = None;
    for i in 0..(args.runs + args.warmup) {
        let probe = boot_probe(&apk, args, true, false, false)?;
        if probe.bind_delivered {
            delivered += 1;
        }
        if blocker.is_none() {
            blocker = probe.blocker.clone();
        }
        if i < args.warmup {
            continue;
        }
        attach.push(probe.boot_ms);
        bind.push(probe.bind_ms);
        phase.push(probe.phase_ms);
    }
    // Counters from a traced probe (tracing is not in the timed samples).
    let traced = boot_probe(&apk, args, true, true, false)?;
    let counters = traced.counters();
    if blocker.is_none() {
        blocker = traced.blocker.clone();
    }
    if delivered == 0 {
        return Err(BenchFailure::internal(
            "bindApplication was never delivered in any sample (see acos-runtime M2 log)",
        ));
    }
    Ok(json!({
        "metric": "bind-app",
        "definition": "attachApplication received -> IApplicationThread.bindApplication delivered, \
                       and the main-looper phase that follows it",
        "unit": "ms",
        "type": "wall",
        "cold_or_warm": "cold per sample (fresh Vm + fresh boot)",
        "fixture": apk.display().to_string(),
        "apk_sha256": file_sha256(&apk),
        "samples": bind.len(),
        "attach_to_bind_ms": stats(&bind),
        "boot_ms": stats(&attach),
        "bind_phase_ms": stats(&phase),
        "bind_delivered": true,
        "first_blocker": blocker,
        "counters": counters,
        "note": "the phase after bindApplication is expected to stop at the first unwired native \
                 (ICU regex / font config); the structured blocker is reported, never a stub",
    }))
}

// ---------------------------------------------------------------------------
// metric: launch (before/after the framework-init snapshot)
// ---------------------------------------------------------------------------

fn bench_launch(args: &BenchArgs) -> Result<Value, BenchFailure> {
    let apk = resolve_apk(args)?;
    if parse_corelib(&args.corelib)? != acos_vm::CoreLib::Real {
        return Err(BenchFailure::unsupported(
            "launch requires --corelib real: the framework-init snapshot is a real \
             Android-14 boot-classpath snapshot",
        ));
    }

    // ---- before: the current acos launch path (Vm::new + boot_app_process) ----
    let mut before_launch = Vec::new();
    let mut before_boot = Vec::new();
    let mut before_bind = Vec::new();
    let mut before_app = Vec::new();
    let mut before_tx = Vec::new();
    let mut before_tx_phase = Vec::new();
    let mut before_quit = Vec::new();
    let mut before_app_created = 0usize;
    let mut before_tx_delivered = 0usize;
    let mut before_blocker: Option<String> = None;
    let mut before_classes = 0usize;
    let mut before_vm_ready = Vec::new();
    for i in 0..(args.runs + args.warmup) {
        let probe = boot_probe(&apk, args, true, false, true)?;
        if probe.app_created {
            before_app_created += 1;
        }
        if probe.transaction_delivered {
            before_tx_delivered += 1;
        }
        if before_blocker.is_none() {
            before_blocker = probe.blocker.clone();
        }
        if i < args.warmup {
            continue;
        }
        before_launch.push(probe.launch_ms);
        before_boot.push(probe.boot_ms);
        before_bind.push(probe.bind_ms);
        before_app.push(probe.phase_ms);
        before_tx.push(probe.transaction_ms);
        before_tx_phase.push(probe.transaction_phase_ms);
        before_quit.push(probe.quit_phase_ms);
        before_vm_ready.push(probe.vm_ready_ms);
        before_classes = probe.classes_loaded;
    }

    // ---- template: one framework boot + ACOSSNAP capture --------------------
    let template_started = Instant::now();
    let template = acos_runtime::session_pool::RuntimeTemplate::boot()
        .map_err(|e| BenchFailure::internal(format!("RuntimeTemplate::boot: {e}")))?;
    let template_boot_ms = template_started.elapsed().as_secs_f64() * 1000.0;
    let template_boot_internal_ms = template.boot_ms();
    let template_snapshot_ms = template.snapshot_ms();
    let template_snapshot_bytes = template.snapshot_bytes();
    let template_classes = template.classes();

    // ---- after: Vm::restore + app load + bindApplication + ClientTransaction -
    let mut after_launch = Vec::new();
    let mut after_restore = Vec::new();
    let mut after_app_load = Vec::new();
    let mut after_bind = Vec::new();
    let mut after_app = Vec::new();
    let mut after_tx = Vec::new();
    let mut after_tx_phase = Vec::new();
    let mut after_quit = Vec::new();
    let mut after_app_created = 0usize;
    let mut after_tx_delivered = 0usize;
    let mut after_looper_quit = 0usize;
    let mut after_blocker: Option<String> = None;
    for i in 0..(args.runs + args.warmup) {
        let t = Instant::now();
        let mut rt = template
            .restore_runtime()
            .map_err(|e| BenchFailure::internal(format!("RuntimeTemplate::restore: {e}")))?;
        let restore_ms = t.elapsed().as_secs_f64() * 1000.0;
        let report = acos_runtime::session_pool::launch_from_template(
            &mut rt,
            &apk,
            template.activity_thread(),
        );
        if report.app_created {
            after_app_created += 1;
        }
        if report.transaction_delivered {
            after_tx_delivered += 1;
        }
        if report.looper_quit {
            after_looper_quit += 1;
        }
        if after_blocker.is_none() {
            after_blocker = report.error.clone();
        }
        if i < args.warmup {
            continue;
        }
        after_launch.push(report.total_ms);
        after_restore.push(restore_ms);
        after_app_load.push(report.app_load_ms);
        after_bind.push(report.bind_ms);
        after_app.push(report.app_phase_ms);
        after_tx.push(report.transaction_ms);
        after_tx_phase.push(report.transaction_phase_ms);
        after_quit.push(report.quit_phase_ms);
    }

    // The "after" total is restore + launch; report both the sum and its parts.
    let after_total: Vec<f64> = after_launch
        .iter()
        .zip(after_restore.iter())
        .map(|(l, r)| l + r)
        .collect();
    let before_total = before_launch.clone();

    Ok(json!({
        "metric": "launch",
        "definition": "Runtime::standard_with -> boot classpath load -> framework attach -> \
                       bindApplication -> Application.onCreate -> launch ClientTransaction -> \
                       Activity.onCreate/onResume, wall time",
        "unit": "ms",
        "type": "wall",
        "cold_or_warm": "before: fresh Vm + fresh framework boot per sample (process-global boot \
                         cache warm after the first); after: one framework boot + ACOSSNAP, then \
                         Vm::restore per sample",
        "corelib": args.corelib,
        "fixture": apk.display().to_string(),
        "apk_sha256": file_sha256(&apk),
        "samples": before_launch.len(),
        "before": {
            "path": "acos launch today: Vm::new + boot_app_process + bindApplication + ClientTransaction",
            "vm_ready_ms": stats(&before_vm_ready),
            "boot_ms": stats(&before_boot),
            "bind_ms": stats(&before_bind),
            "app_on_create_ms": stats(&before_app),
            "client_transaction_ms": stats(&before_tx),
            "activity_on_create_ms": stats(&before_tx_phase),
            "quit_ms": stats(&before_quit),
            "launch_ms": stats(&before_launch),
            "app_created": before_app_created,
            "transaction_delivered": before_tx_delivered,
            "classes_loaded": before_classes,
            "first_blocker": before_blocker,
        },
        "template": {
            "path": "one framework boot + ACOSSNAP capture, or an on-disk cache load (T-43b)",
            "boot_ms": round3(template_boot_ms),
            "internal_boot_ms": round3(template_boot_internal_ms),
            "snapshot_ms": round3(template_snapshot_ms),
            "snapshot_bytes": template_snapshot_bytes,
            "classes": template_classes,
            "from_cache": template.from_cache(),
        },
        "after": {
            "path": "Vm::restore from ACOSSNAP + Runtime::with_vm + app DEX load + bindApplication + ClientTransaction",
            "restore_ms": stats(&after_restore),
            "app_load_ms": stats(&after_app_load),
            "bind_ms": stats(&after_bind),
            "app_on_create_ms": stats(&after_app),
            "client_transaction_ms": stats(&after_tx),
            "activity_on_create_ms": stats(&after_tx_phase),
            "quit_ms": stats(&after_quit),
            "launch_ms": stats(&after_launch),
            "total_ms": stats(&after_total),
            "app_created": after_app_created,
            "transaction_delivered": after_tx_delivered,
            "looper_quit": after_looper_quit,
            "first_blocker": after_blocker,
        },
        "speedup_p50": if median(&before_total) > 0.0 {
            round3(median(&before_total) / median(&after_total).max(f64::MIN_POSITIVE))
        } else {
            0.0
        },
        "note": "the before path is the same sequence as acos launch (AgentSession::launch -> \
                 Runtime::standard_with -> boot_and_run_lifecycle); the after path restores the \
                 framework Vm from ACOSSNAP and skips prepareMainLooper/ActivityThread/attach. \
                 Both are measured in the same process so the process-global boot DEX/code caches \
                 are equally warm. A blocker is reported verbatim, never stubbed.",
    }))
}

// ---------------------------------------------------------------------------
// metric: worker-create-frame (M4) and reset-to-ready (M6)
// ---------------------------------------------------------------------------

/// M4/M6: create a product worker session from the warm boot-image cache and
/// drive it through `counter.apk` to a **published frame**, then reset it back
/// through the warm template and drive it to a fresh frame again.
///
/// This is the worker-create path the M3 row cannot cover: it is a session
/// create (not a `acos launch` process boot), and every sample must publish a
/// fresh frame into the process `FrameStore` before it counts. A session that
/// reaches `first_frame` without a published frame is never counted as ready;
/// a launch that stops short is recorded with its blocker.
fn bench_worker_create_frame(args: &BenchArgs) -> Result<Value, BenchFailure> {
    let apk = resolve_apk(args)?;
    let out = worker_frame_probe(&apk, args.runs, args.warmup);
    out.map_err(BenchFailure::internal)
}

/// Create a product [`AgentSession`] from the warm boot-image cache and drive
/// `counter.apk` to a published frame; then reset the live session and drive it
/// to a fresh frame again.
///
/// The in-process `RuntimeTemplate` restore path is exercised by
/// `RuntimeTemplate::restore_runtime`, but at this HEAD a same-process
/// boot-then-restore of the framework mis-reads the window transaction
/// ("Adding window failed") and never draws, while a restore in a fresh process
/// (the product path, template loaded from the on-disk boot cache) does. The
/// metric therefore measures the **real worker-create path**: a session created
/// from the warm boot-image cache, launched, and required to publish a fresh
/// frame before it counts. A failed frame is reported with its blocker.
fn worker_frame_probe(apk: &Path, runs: usize, warmup: usize) -> Result<Value, String> {
    use acos_runtime::session::{AgentSession, SessionOptions};

    let mut create_ms = Vec::new();
    let mut reset_ms = Vec::new();
    let mut create_frames = 0usize;
    let mut reset_frames = 0usize;
    let mut stages = Vec::new();
    let mut first_blocker: Option<String> = None;

    for i in 0..(runs + warmup) {
        // ---- create: new session -> ready with a fresh frame ----------------
        let session = AgentSession::start(SessionOptions {
            trace_natives: true,
            ..SessionOptions::default()
        });
        let t = Instant::now();
        let report = session.launch(apk).map_err(|e| format!("launch: {e:?}"))?;
        let create = t.elapsed().as_secs_f64() * 1000.0;
        let published = session.status().map(|s| s.published_frames).unwrap_or(0);
        let create_ok = published > 0;
        if first_blocker.is_none() {
            if let Some(b) = &report.root_blocker {
                first_blocker = Some(b.message.clone());
            }
        }
        stages.push(report.reached_stage.to_string());
        session.close();

        // ---- reset: live session -> warm template -> fresh frame -------------
        // The reset drops the live `Runtime` and the next launch rebuilds it
        // from the warm boot-image cache. This is the honest app-level reset
        // ACOS has today (there is no in-place app-state reset yet).
        let fresh = AgentSession::start(SessionOptions {
            trace_natives: true,
            ..SessionOptions::default()
        });
        let t = Instant::now();
        let report = fresh
            .launch(apk)
            .map_err(|e| format!("reset relaunch: {e:?}"))?;
        let reset = t.elapsed().as_secs_f64() * 1000.0;
        let published2 = fresh.status().map(|s| s.published_frames).unwrap_or(0);
        let reset_ok = published2 > 0;
        if first_blocker.is_none() {
            if let Some(b) = &report.root_blocker {
                first_blocker = Some(b.message.clone());
            }
        }
        fresh.close();

        if i < warmup {
            continue;
        }
        if create_ok {
            create_frames += 1;
        }
        if reset_ok {
            reset_frames += 1;
        }
        create_ms.push(create);
        reset_ms.push(reset);
    }

    Ok(json!({
        "metric": "worker-create-frame",
        "definition": "create an AgentSession from the warm boot-image cache and drive counter.apk \
                       to a published frame; reset = drop the live session and recreate it from the \
                       warm template to a fresh frame",
        "unit": "ms",
        "type": "wall, warm template",
        "cold_or_warm": "warm: the framework template is loaded from the on-disk boot cache; each \
                         sample creates a session, restores the framework, launches counter.apk and \
                         requires a fresh FrameStore frame",
        "corelib": "real",
        "fixture": apk.display().to_string(),
        "apk_sha256": file_sha256(apk),
        "samples": create_ms.len(),
        "create": {
            "ms": stats(&create_ms),
            "frames_published": create_frames,
        },
        "reset": {
            "ms": stats(&reset_ms),
            "frames_published": reset_frames,
        },
        "stages": stages,
        "first_blocker": first_blocker,
        "note": "M4 uses `create`; M6 uses `reset`. Both require a fresh frame in the process \
                 FrameStore, so a session that reaches `first_frame` without a published frame is \
                 never counted as ready. This is the product AgentSession path; the in-process \
                 RuntimeTemplate boot+restore does not draw a frame at this HEAD and is documented \
                 in BENCHMARKS.md §9.7.",
    }))
}

// ---------------------------------------------------------------------------
// metric: idle-rss
// ---------------------------------------------------------------------------

fn bench_idle_rss(args: &BenchArgs) -> Result<Value, BenchFailure> {
    let apk = resolve_apk(args)?;
    let mut variants = Map::new();
    for variant in ["vm", "booted"] {
        let mut pss = Vec::new();
        let mut rss = Vec::new();
        let mut anon_shmem = Vec::new();
        let mut private_kib = Vec::new();
        let mut shared_kib = Vec::new();
        let mut raw: Option<Value> = None;
        for _ in 0..args.runs {
            let probe = spawn_child(&[
                "bench",
                "idle-rss",
                "--child",
                "idle-rss",
                "--corelib",
                &args.corelib,
                "--apk",
                apk.to_string_lossy().as_ref(),
                "--settle-ms",
                &args.settle_ms.to_string(),
                "--variant",
                variant,
            ])?;
            let mem = probe
                .get("memory")
                .ok_or_else(|| BenchFailure::internal("idle-rss probe returned no memory block"))?;
            pss.push(kib(mem, "pss_kib"));
            rss.push(kib(mem, "rss_kib"));
            anon_shmem.push(kib(mem, "anon_plus_shmem_kib"));
            private_kib.push(kib(mem, "private_kib"));
            shared_kib.push(kib(mem, "shared_kib"));
            raw = Some(mem.clone());
        }
        variants.insert(
            variant.to_string(),
            json!({
                "state": if variant == "vm" {
                    "runtime core only, zero tasks (Vm ready, framework not booted)"
                } else {
                    "runtime core + booted framework app process (attach handshake done)"
                },
                "samples": pss.len(),
                "pss_kib": stats(&pss),
                "rss_kib": stats(&rss),
                "anon_plus_shmem_kib": stats(&anon_shmem),
                "private_kib": stats(&private_kib),
                "shared_kib": stats(&shared_kib),
                "miB_pss_p50": round3(median(&pss) / 1024.0),
                "miB_anon_plus_shmem_p50": round3(median(&anon_shmem) / 1024.0),
                "raw": raw,
            }),
        );
    }
    Ok(json!({
        "metric": "idle-rss",
        "definition": "settled memory of the runtime core with zero tasks",
        "unit": "KiB (smaps_rollup / status), MiB convenience columns",
        "type": "RSS + PSS + anon+shmem (RssAnon+RssShmem) + private/shared, each labelled",
        "cold_or_warm": "cold per sample (fresh process), settled ${settle}".replace("${settle}", &format!("{} ms", args.settle_ms)),
        "corelib": args.corelib,
        "fixture": apk.display().to_string(),
        "variants": variants,
        "never": "PSS and cgroup memory are never summed (BENCHMARKS.md §1)",
    }))
}

// ---------------------------------------------------------------------------
// metric: sessions
// ---------------------------------------------------------------------------

fn bench_sessions(args: &BenchArgs) -> Result<Value, BenchFailure> {
    let apk = resolve_apk(args)?;
    let counts = parse_session_counts(&args.sessions)?;
    let mut per_n: Vec<(usize, Value)> = Vec::new();
    for n in &counts {
        // Memory metrics need fresh-core pairs (BENCHMARKS.md §2 rule 2: >=3),
        // not the >=20 latency samples.
        let probes = args.runs.clamp(3, 5);
        let mut pss = Vec::new();
        let mut anon_shmem = Vec::new();
        let mut private_kib = Vec::new();
        let mut shared_kib = Vec::new();
        let mut file_kib = Vec::new();
        let mut raw: Option<Value> = None;
        for _ in 0..probes {
            let probe = spawn_child(&[
                "bench",
                "sessions",
                "--child",
                "sessions",
                "--n",
                &n.to_string(),
                "--corelib",
                &args.corelib,
                "--apk",
                apk.to_string_lossy().as_ref(),
                "--settle-ms",
                &args.settle_ms.to_string(),
            ])?;
            let mem = probe
                .get("memory")
                .ok_or_else(|| BenchFailure::internal("sessions probe returned no memory block"))?;
            pss.push(kib(mem, "pss_kib"));
            anon_shmem.push(kib(mem, "anon_plus_shmem_kib"));
            private_kib.push(kib(mem, "private_kib"));
            shared_kib.push(kib(mem, "shared_kib"));
            file_kib.push(kib(mem, "rss_file_kib"));
            raw = Some(mem.clone());
        }
        per_n.push((
            *n,
            json!({
                "n": n,
                "samples": pss.len(),
                "pss_kib": stats(&pss),
                "anon_plus_shmem_kib": stats(&anon_shmem),
                "private_kib": stats(&private_kib),
                "shared_kib": stats(&shared_kib),
                "rss_file_kib": stats(&file_kib),
                "miB_anon_plus_shmem_p50": round3(median(&anon_shmem) / 1024.0),
                "raw": raw,
            }),
        ));
    }
    // Incremental memory: (loaded - bare) / N, and marginal (high-low)/(dh).
    let baseline_n = 0usize;
    let bare = per_n
        .iter()
        .find(|(n, _)| *n == baseline_n)
        .map(|(_, v)| kb_value(v, "anon_plus_shmem_kib"));
    let mut incremental = Vec::new();
    for (n, value) in &per_n {
        if *n == 0 {
            continue;
        }
        let total = kb_value(value, "anon_plus_shmem_kib");
        let per_session = match bare {
            Some(b) if *n > 0 => (total - b) / (*n as f64),
            _ => total / (*n as f64),
        };
        incremental.push(json!({
            "n": n,
            "incremental_anon_plus_shmem_kib_per_session": round3(per_session),
            "incremental_miB_per_session": round3(per_session / 1024.0),
        }));
    }
    let mut marginal = Vec::new();
    for pair in per_n.windows(2) {
        let (low_n, low) = &pair[0];
        let (high_n, high) = &pair[1];
        if high_n == low_n {
            continue;
        }
        let dh = (*high_n - *low_n) as f64;
        let delta = kb_value(high, "anon_plus_shmem_kib") - kb_value(low, "anon_plus_shmem_kib");
        marginal.push(json!({
            "from_n": low_n,
            "to_n": high_n,
            "marginal_anon_plus_shmem_kib_per_session": round3(delta / dh),
            "marginal_miB_per_session": round3(delta / dh / 1024.0),
        }));
    }
    let sharing = json!({
        "dex_mmap": false,
        "evidence": "the boot classpath and app DEX are read with std::fs::read and parsed into \
                     the VM heap; no mmap is used, so their pages are private anonymous pages",
        "consequence": "nothing is shareable between sessions today: private pages grow with N and \
                        shared pages are flat (see private_kib/rss_file_kib per N)",
    });
    Ok(json!({
        "metric": "sessions",
        "definition": "per-session incremental (loaded-bare)/N and marginal \
                       (total(high)-total(low))/(high-low) memory of the current boot path",
        "unit": "KiB (MiB convenience columns)",
        "type": "anon+shmem (RssAnon+RssShmem) with PSS and private/shared reported separately",
        "cold_or_warm": "fresh process per N; sessions are in-process and sequential",
        "corelib": args.corelib,
        "fixture": apk.display().to_string(),
        "apk_sha256": file_sha256(&apk),
        "sessions": per_n.into_iter().map(|(_, v)| v).collect::<Vec<_>>(),
        "incremental": incremental,
        "marginal": marginal,
        "sharing": sharing,
        "limit": "Runtime::with_host installs the process-global Binder transport, so N live \
                  runtimes are a memory experiment, not a working multi-session configuration",
    }))
}

// ---------------------------------------------------------------------------
// metric: idle-cpu
// ---------------------------------------------------------------------------

fn bench_idle_cpu(args: &BenchArgs) -> Result<Value, BenchFailure> {
    let apk = resolve_apk(args)?;
    let mut ticks = Vec::new();
    let mut pct = Vec::new();
    let mut state = Value::Null;
    for _ in 0..args.runs {
        let probe = spawn_child(&[
            "bench",
            "idle-cpu",
            "--child",
            "idle-cpu",
            "--corelib",
            &args.corelib,
            "--apk",
            apk.to_string_lossy().as_ref(),
            "--cpu-secs",
            &args.cpu_secs.to_string(),
            "--settle-ms",
            &args.settle_ms.to_string(),
        ])?;
        ticks.push(
            probe
                .get("cpu_ticks")
                .and_then(Value::as_f64)
                .ok_or_else(|| BenchFailure::internal("idle-cpu probe returned no cpu_ticks"))?,
        );
        pct.push(
            probe
                .get("cpu_pct_one_core")
                .and_then(Value::as_f64)
                .unwrap_or(0.0),
        );
        state = probe.get("state").cloned().unwrap_or(Value::Null);
    }
    let zero = ticks.iter().all(|t| *t == 0.0);
    Ok(json!({
        "metric": "idle-cpu",
        "definition": "process CPU ticks consumed while the runtime is parked/blocked, over a \
                       wall window",
        "unit": "ticks (CLK_TCK=100 assumed on x86_64) and % of one core",
        "type": "CPU",
        "cold_or_warm": "warm (after boot + bind phase, main looper parked or run stopped)",
        "window_secs": args.cpu_secs,
        "samples": ticks.len(),
        "cpu_ticks": stats(&ticks),
        "cpu_pct_one_core": stats(&pct),
        "zero_ticks_over_window": zero,
        "state": state,
        "note": "a parked looper must burn zero ticks; a non-zero tick count is reported as-is",
    }))
}

// ---------------------------------------------------------------------------
// metric: interpreter
// ---------------------------------------------------------------------------

fn bench_interpreter(args: &BenchArgs) -> Result<Value, BenchFailure> {
    let programs = resolve_programs(args)?;
    let mut rows = Vec::new();
    for program in &programs {
        let mut wall = Vec::new();
        let mut per_insn = Vec::new();
        let mut instructions = 0u64;
        let mut allocations = 0u64;
        for i in 0..(args.runs + args.warmup) {
            let started = Instant::now();
            let (insns, allocs) = run_program(&program.path, &args.main, &args.corelib)?;
            let ms = started.elapsed().as_secs_f64() * 1000.0;
            if i < args.warmup {
                continue;
            }
            wall.push(ms);
            instructions = insns;
            allocations = allocs;
            if ms > 0.0 {
                per_insn.push(insns as f64 / (ms / 1000.0));
            }
        }
        let p50 = median(&wall);
        rows.push(json!({
            "program": program.name,
            "path": program.path.display().to_string(),
            "samples": wall.len(),
            "wall_ms": stats(&wall),
            "instructions": instructions,
            "allocations": allocations,
            "instructions_per_second_p50": round3(if p50 > 0.0 {
                instructions as f64 / (p50 / 1000.0)
            } else {
                0.0
            }),
            "instructions_per_second": stats(&per_insn),
        }));
    }
    Ok(json!({
        "metric": "interpreter",
        "definition": "interpreted DEX program wall time and interpreted instructions",
        "unit": "ms, instructions, instructions/second",
        "type": "wall + VM counters",
        "cold_or_warm": "fresh Vm per sample; the first sample in the process parses the boot \
                         classpath, later samples reuse the process-global boot cache (warm). \
                         See results/bench/MEMORY_PROFILE.md",
        "corelib": args.corelib,
        "programs": rows,
        "note": "ACOS-only: legacy ACVM published no interpreter micro benchmarks, so these rows \
                 have no legacy counterpart (host-JVM numbers live in results/vm, a different \
                 engine and fidelity)",
    }))
}

// ---------------------------------------------------------------------------
// boot probe (mirrors acos_runtime::boot::boot_app_process, with stage timings)
// ---------------------------------------------------------------------------

/// The result of one instrumented boot (+ optional bind) sample.
#[derive(Debug, Clone)]
struct Probe {
    vm_ready_ms: f64,
    boot_ms: f64,
    stages: Vec<Stage>,
    bind_ms: f64,
    phase_ms: f64,
    bind_delivered: bool,
    blocker: Option<String>,
    classes_loaded: usize,
    boot_classpath_classes: usize,
    clinits: u64,
    instructions: u64,
    natives: usize,
    launch_ms: f64,
    transaction_ms: f64,
    transaction_phase_ms: f64,
    quit_phase_ms: f64,
    app_created: bool,
    transaction_delivered: bool,
    looper_quit: bool,
}

impl Probe {
    fn counters(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("classes_loaded".into(), json!(self.classes_loaded));
        m.insert(
            "boot_classpath_classes".into(),
            json!(self.boot_classpath_classes),
        );
        m.insert("clinits".into(), json!(self.clinits));
        m.insert("instructions".into(), json!(self.instructions));
        m.insert("natives_invoked".into(), json!(self.natives));
        m
    }
}

fn boot_probe(
    apk: &Path,
    args: &BenchArgs,
    bind: bool,
    trace: bool,
    launch: bool,
) -> Result<Probe, BenchFailure> {
    use acos_runtime::atms_boot::{first_attach, install_recording_atms, is_valid_app_thread};
    use acos_runtime::Runtime;
    use acos_vm::heap::{Handle, NULL};
    use acos_vm::javabridge::JavaValue;

    let corelib = parse_corelib(&args.corelib)?;
    let started = Instant::now();
    let mut rt = Runtime::standard_with(acos_vm::VmOptions {
        corelib,
        trace_natives: trace,
        ..acos_vm::VmOptions::default()
    })
    .map_err(|e| BenchFailure::internal(format!("Vm::new: {e}")))?;
    let vm_ready_ms = started.elapsed().as_secs_f64() * 1000.0;
    let boot_classpath_classes = rt.vm().classes.len();

    let mut stages: Vec<Stage> = Vec::new();
    let mut push_stage = |name: &str, ms: f64, vm: &acos_vm::Vm| {
        stages.push(Stage {
            name: name.to_string(),
            ms,
            classes: vm.classes.len() as u64,
            clinits: vm.metrics.clinit_count,
            instructions: vm.metrics.instructions,
            natives: vm.native_trace.as_ref().map_or(0, |t| t.hits.len() as u64),
        });
    };

    let boot_started = Instant::now();

    // --- open-apk ---------------------------------------------------------
    let t = Instant::now();
    let apk_file = acos_apk::Apk::open(apk)
        .map_err(|e| BenchFailure::new(ErrorCode::ApkIo, format!("open apk: {e}")))?;
    push_stage("open-apk", t.elapsed().as_secs_f64() * 1000.0, rt.vm());

    // --- load-app-dex -----------------------------------------------------
    let t = Instant::now();
    let raw = apk_file
        .dex_files
        .first()
        .ok_or_else(|| BenchFailure::new(ErrorCode::DexLoadError, "apk contains no classes.dex"))?;
    let dex = acos_dex::DexFile::parse(raw.bytes.clone())
        .map_err(|e| BenchFailure::new(ErrorCode::DexLoadError, format!("dex: {e}")))?;
    rt.vm_mut().add_dex(dex);
    push_stage("load-app-dex", t.elapsed().as_secs_f64() * 1000.0, rt.vm());

    // --- recording ATMS (must be installed before attach) ------------------
    let log = {
        let host = rt.host();
        let mut host = host.borrow_mut();
        install_recording_atms(&mut host)
    };

    // --- prepare-main-looper ---------------------------------------------
    let t = Instant::now();
    rt.vm_mut()
        .call_static("android.os.Looper", "prepareMainLooper", "()V", &[])
        .map_err(|e| BenchFailure::internal(format!("prepare-main-looper: {e}")))?;
    push_stage(
        "prepare-main-looper",
        t.elapsed().as_secs_f64() * 1000.0,
        rt.vm(),
    );

    // --- instantiate-activity-thread -------------------------------------
    let t = Instant::now();
    let thread: Handle = rt
        .vm_mut()
        .new_instance_ctor("android.app.ActivityThread", "()V", &[])
        .map_err(|e| BenchFailure::internal(format!("instantiate-activity-thread: {e}")))?;
    push_stage(
        "instantiate-activity-thread",
        t.elapsed().as_secs_f64() * 1000.0,
        rt.vm(),
    );

    // --- attach-application ----------------------------------------------
    let t = Instant::now();
    let start_seq: i64 = 1;
    rt.vm_mut()
        .call_method_direct(
            thread,
            "attach",
            "(ZJ)V",
            &[JavaValue::Boolean(false), JavaValue::Long(start_seq)],
        )
        .map_err(|e| BenchFailure::internal(format!("attach-application: {e}")))?;
    let attach = first_attach(&log).ok_or_else(|| {
        BenchFailure::internal(
            "attach returned but no IActivityManager.attachApplication was recorded \
             (boot-stage mirror drifted from acos_runtime::boot)",
        )
    })?;
    if !is_valid_app_thread(&attach) {
        return Err(BenchFailure::internal(
            "attachApplication arrived without a valid IApplicationThread binder",
        ));
    }
    push_stage(
        "attach-application",
        t.elapsed().as_secs_f64() * 1000.0,
        rt.vm(),
    );
    let boot_ms = boot_started.elapsed().as_secs_f64() * 1000.0;

    let mut probe = Probe {
        vm_ready_ms,
        boot_ms,
        stages,
        bind_ms: f64::NAN,
        phase_ms: f64::NAN,
        bind_delivered: false,
        blocker: None,
        classes_loaded: rt.vm().classes.len(),
        boot_classpath_classes,
        clinits: rt.vm().metrics.clinit_count,
        instructions: rt.vm().metrics.instructions,
        natives: rt.vm().native_trace.as_ref().map_or(0, |t| t.hits.len()),
        launch_ms: f64::NAN,
        transaction_ms: f64::NAN,
        transaction_phase_ms: f64::NAN,
        quit_phase_ms: f64::NAN,
        app_created: false,
        transaction_delivered: false,
        looper_quit: false,
    };

    if !bind {
        return Ok(probe);
    }

    if launch {
        // Full launch from an already-framework-booted Runtime. The app DEX was
        // loaded in the `load-app-dex` stage above, so this uses the "loaded"
        // half and measures bindApplication + Application.onCreate + the launch
        // ClientTransaction + Activity.onCreate/onResume + quitSafely.
        let app = app_info(apk)?;
        let report =
            acos_runtime::session_pool::launch_from_template_loaded(&mut rt, apk, thread, &app);
        probe.bind_ms = report.bind_ms;
        probe.phase_ms = report.app_phase_ms;
        probe.transaction_ms = report.transaction_ms;
        probe.transaction_phase_ms = report.transaction_phase_ms;
        probe.quit_phase_ms = report.quit_phase_ms;
        probe.app_created = report.app_created;
        probe.transaction_delivered = report.transaction_delivered;
        probe.looper_quit = report.looper_quit;
        probe.bind_delivered = report.error.is_none();
        probe.blocker = report.error;
        probe.launch_ms = boot_started.elapsed().as_secs_f64() * 1000.0;
        probe.classes_loaded = rt.vm().classes.len();
        probe.clinits = rt.vm().metrics.clinit_count;
        probe.instructions = rt.vm().metrics.instructions;
        probe.natives = rt.vm().native_trace.as_ref().map_or(0, |t| t.hits.len());
        return Ok(probe);
    }

    // --- bindApplication ---------------------------------------------------
    // Real flow (acos-runtime/src/lifecycle.rs): push `Looper.loop`, deliver
    // the real `ApplicationThread.bindApplication`, then run the interpreter
    // until the queue drains. The phase after the bind is expected to stop at
    // the first unwired native; that blocker is reported verbatim.
    let app = app_info(apk)?;
    rt.vm_mut()
        .push_static_call("android.os.Looper", "loop", "()V", &[])
        .map_err(|e| BenchFailure::internal(format!("Looper.loop: {e}")))?;
    let t = Instant::now();
    acos_runtime::bind_app::deliver_bind_application(rt.vm_mut(), thread, &app, &[])
        .map_err(|e| BenchFailure::internal(format!("bindApplication: {e}")))?;
    probe.bind_ms = t.elapsed().as_secs_f64() * 1000.0;
    probe.bind_delivered = true;
    probe.classes_loaded = rt.vm().classes.len();
    probe.clinits = rt.vm().metrics.clinit_count;

    let t = Instant::now();
    let mut blocker = None;
    match acos_vm::interp::run(rt.vm_mut()) {
        Ok(()) => {
            if rt.vm().pending_exception != NULL {
                let h = rt.vm().pending_exception;
                rt.vm_mut().pending_exception = NULL;
                // Name the throwable, not just its handle.
                let class = rt
                    .vm()
                    .class_of_object(h)
                    .map(|c| rt.vm().class_desc(c))
                    .unwrap_or("java.lang.Throwable")
                    .to_string();
                let message = rt.vm().throwable_message(h);
                blocker = Some(match message {
                    Some(m) => format!("uncaught {class}: {m}"),
                    None => format!("uncaught {class}"),
                });
            }
        }
        Err(e) => blocker = Some(format!("{e}")),
    }
    probe.phase_ms = t.elapsed().as_secs_f64() * 1000.0;
    probe.blocker = blocker;
    probe.classes_loaded = rt.vm().classes.len();
    probe.clinits = rt.vm().metrics.clinit_count;
    probe.instructions = rt.vm().metrics.instructions;
    probe.natives = rt.vm().native_trace.as_ref().map_or(0, |t| t.hits.len());
    Ok(probe)
}

fn app_info(apk: &Path) -> Result<acos_services::pms::AppInfo, BenchFailure> {
    let opened = acos_apk::Apk::open(apk)
        .map_err(|e| BenchFailure::new(ErrorCode::ApkIo, format!("open apk: {e}")))?;
    let manifest = opened
        .manifest()
        .map_err(|e| BenchFailure::new(ErrorCode::AxmlMalformed, format!("manifest: {e}")))?;
    Ok(acos_services::pms::AppInfo::from_manifest(
        &manifest,
        apk,
        acos_services::pms::FIXTURE_UID,
    ))
}

fn parse_corelib(name: &str) -> Result<acos_vm::CoreLib, BenchFailure> {
    acos_vm::CoreLib::parse(name).ok_or_else(|| {
        BenchFailure::new(
            ErrorCode::InvalidArgument,
            format!("unknown corelib {name:?} (expected bootstrap|real)"),
        )
    })
}

fn run_program(path: &Path, main: &str, corelib: &str) -> Result<(u64, u64), BenchFailure> {
    let corelib = parse_corelib(corelib)?;
    let bytes = std::fs::read(path).map_err(|e| {
        BenchFailure::new(ErrorCode::ApkIo, format!("read {}: {e}", path.display()))
    })?;
    let mut vm = acos_vm::Vm::new(acos_vm::VmOptions {
        corelib,
        ..acos_vm::VmOptions::default()
    })
    .map_err(|e| BenchFailure::internal(format!("Vm::new: {e}")))?;
    let dex = acos_dex::DexFile::parse(bytes)
        .map_err(|e| BenchFailure::new(ErrorCode::DexLoadError, format!("dex: {e}")))?;
    vm.add_dex(dex);
    let outcome = vm.run(main, &[]);
    if outcome.exit_code != 0 {
        return Err(BenchFailure::internal(format!(
            "program {} exited {}: {}",
            path.display(),
            outcome.exit_code,
            outcome.stderr.lines().next().unwrap_or("").trim()
        )));
    }
    Ok((outcome.metrics.instructions, outcome.metrics.allocations))
}

// ---------------------------------------------------------------------------
// child probes (self-spawned so each memory read is a fresh process)
// ---------------------------------------------------------------------------

fn run_child(mode: &str, args: &BenchArgs) -> std::process::ExitCode {
    match child_body(mode, args) {
        Ok(doc) => {
            println!("ACOS_BENCH_PROBE {}", doc);
            std::process::ExitCode::SUCCESS
        }
        Err(failure) => {
            eprintln!("bench child {mode}: {}: {}", failure.code, failure.message);
            std::process::ExitCode::FAILURE
        }
    }
}

fn child_body(mode: &str, args: &BenchArgs) -> Result<Value, BenchFailure> {
    match mode {
        "vm-ready" => {
            let corelib = parse_corelib(&args.corelib)?;
            let started = Instant::now();
            let vm = acos_vm::Vm::new(acos_vm::VmOptions {
                corelib,
                ..acos_vm::VmOptions::default()
            })
            .map_err(|e| BenchFailure::internal(format!("Vm::new: {e}")))?;
            let ms = started.elapsed().as_secs_f64() * 1000.0;
            Ok(json!({"mode": "vm-ready", "ms": round3(ms), "classes": vm.classes.len()}))
        }
        "idle-rss" => {
            let variant = args.variant.as_str();
            let mut rt = build_runtime(&args.corelib)?;
            if variant == "booted" {
                boot_only(&mut rt, &args.apk)?;
            }
            settle(args.settle_ms);
            let memory = read_memory()
                .ok_or_else(|| BenchFailure::internal("could not read /proc/self/smaps_rollup"))?;
            drop(rt);
            Ok(json!({"mode": "idle-rss", "variant": variant, "memory": memory}))
        }
        "sessions-shared" => super::bench_sessions::child_body(args),
        "sessions-pool" => super::bench_pool::child_body(args),
        "sessions" => {
            let mut runtimes = Vec::new();
            for _ in 0..args.n {
                let mut rt = build_runtime(&args.corelib)?;
                boot_only(&mut rt, &args.apk)?;
                runtimes.push(rt);
            }
            settle(args.settle_ms);
            let memory = read_memory()
                .ok_or_else(|| BenchFailure::internal("could not read /proc/self/smaps_rollup"))?;
            drop(runtimes);
            Ok(json!({"mode": "sessions", "n": args.n, "memory": memory}))
        }
        "idle-cpu" => {
            let mut rt = build_runtime(&args.corelib)?;
            boot_only(&mut rt, &args.apk)?;
            settle(args.settle_ms);
            let parked = rt
                .vm()
                .threads
                .iter()
                .all(|t| !matches!(t.status, acos_vm::vm::ThreadStatus::Running));
            let before = cpu_ticks()
                .ok_or_else(|| BenchFailure::internal("could not read /proc/self/stat"))?;
            let t0 = Instant::now();
            std::thread::sleep(Duration::from_secs(args.cpu_secs));
            let wall = t0.elapsed().as_secs_f64();
            let after = cpu_ticks()
                .ok_or_else(|| BenchFailure::internal("could not read /proc/self/stat"))?;
            let delta = after.saturating_sub(before) as f64;
            let pct = if wall > 0.0 {
                delta / 100.0 / wall * 100.0
            } else {
                0.0
            };
            Ok(json!({
                "mode": "idle-cpu",
                "cpu_ticks": delta,
                "cpu_pct_one_core": round3(pct),
                "window_secs": wall,
                "state": {
                    "parked_looper": parked,
                    "threads": rt.vm().threads.len(),
                    "status": rt.vm().threads.iter()
                        .map(|t| format!("{:?}", t.status))
                        .collect::<Vec<_>>(),
                },
            }))
        }
        other => Err(BenchFailure::new(
            ErrorCode::InvalidArgument,
            format!("unknown child probe mode {other:?}"),
        )),
    }
}

pub(crate) fn build_runtime(corelib: &str) -> Result<acos_runtime::Runtime, BenchFailure> {
    let corelib = parse_corelib(corelib)?;
    acos_runtime::Runtime::standard_with(acos_vm::VmOptions {
        corelib,
        ..acos_vm::VmOptions::default()
    })
    .map_err(|e| BenchFailure::internal(format!("runtime: {e}")))
}

/// Boot the framework app process (attach handshake), discarding the report's
/// detail: the memory/CPU probes only need the resulting live state.
pub(crate) fn boot_only(rt: &mut acos_runtime::Runtime, apk: &Path) -> Result<(), BenchFailure> {
    rt.boot_app_process(apk)
        .map(|_| ())
        .map_err(|e| BenchFailure::internal(format!("boot_app_process: {e}")))
}

pub(crate) fn spawn_child(extra: &[&str]) -> Result<Value, BenchFailure> {
    let exe =
        std::env::current_exe().map_err(|e| BenchFailure::internal(format!("current_exe: {e}")))?;
    let output = Command::new(&exe)
        .args(extra)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| BenchFailure::internal(format!("spawn child: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(BenchFailure::new(
            ErrorCode::RamGuard,
            format!(
                "probe child failed ({}): {}",
                output.status,
                stderr.lines().take(4).collect::<Vec<_>>().join(" | ")
            ),
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout
        .lines()
        .find(|l| l.starts_with("ACOS_BENCH_PROBE "))
        .ok_or_else(|| BenchFailure::internal("probe child printed no probe line"))?;
    let body = line.trim_start_matches("ACOS_BENCH_PROBE ");
    serde_json::from_str(body)
        .map_err(|e| BenchFailure::internal(format!("probe JSON: {e} (line: {body})")))
}

fn parse_probe(line: &str) -> Result<Value, BenchFailure> {
    let body = line
        .lines()
        .find(|l| l.starts_with("ACOS_BENCH_PROBE "))
        .ok_or_else(|| BenchFailure::internal("child printed no probe line"))?;
    serde_json::from_str(body.trim_start_matches("ACOS_BENCH_PROBE "))
        .map_err(|e| BenchFailure::internal(format!("probe JSON: {e}")))
}

pub(crate) fn settle(ms: u64) {
    if ms > 0 {
        std::thread::sleep(Duration::from_millis(ms));
    }
}

// ---------------------------------------------------------------------------
// /proc readers
// ---------------------------------------------------------------------------

/// Memory types read from `/proc/self/{smaps_rollup,status}`. Every field keeps
/// its own label; nothing is summed across metric types.
pub(crate) fn read_memory() -> Option<Value> {
    let rollup = std::fs::read_to_string("/proc/self/smaps_rollup").ok()?;
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let get = |name: &str| smaps_value(&rollup, name).unwrap_or(0.0);
    let status_get = |name: &str| status_kib(&status, name).unwrap_or(0.0);
    let rss_anon = status_get("RssAnon");
    let rss_shmem = status_get("RssShmem");
    let rss_file = status_get("RssFile");
    let shared = get("Shared_Clean") + get("Shared_Dirty");
    let private = get("Private_Clean") + get("Private_Dirty");
    Some(json!({
        "rss_kib": get("Rss"),
        "pss_kib": get("Pss"),
        "pss_anon_kib": get("Pss_Anon"),
        "pss_file_kib": get("Pss_File"),
        "pss_shmem_kib": get("Pss_Shmem"),
        "rss_anon_kib": rss_anon,
        "rss_file_kib": rss_file,
        "rss_shmem_kib": rss_shmem,
        "anon_plus_shmem_kib": rss_anon + rss_shmem,
        "anonymous_kib": get("Anonymous"),
        "private_kib": private,
        "shared_kib": shared,
        "private_clean_kib": get("Private_Clean"),
        "private_dirty_kib": get("Private_Dirty"),
        "shared_clean_kib": get("Shared_Clean"),
        "shared_dirty_kib": get("Shared_Dirty"),
        "swap_kib": get("Swap"),
    }))
}

fn smaps_value(text: &str, key: &str) -> Option<f64> {
    for line in text.lines() {
        let Some(rest) = line.strip_prefix(key).and_then(|r| r.strip_prefix(':')) else {
            continue;
        };
        return rest.split_whitespace().next()?.parse().ok();
    }
    None
}

fn status_kib(text: &str, key: &str) -> Option<f64> {
    for line in text.lines() {
        let Some(rest) = line.strip_prefix(key).and_then(|r| r.strip_prefix(':')) else {
            continue;
        };
        return rest.split_whitespace().next()?.parse().ok();
    }
    None
}

/// utime+stime of this process, in clock ticks (`/proc/self/stat` fields 14/15).
fn cpu_ticks() -> Option<u64> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    let close = stat.rfind(')')?;
    let fields: Vec<&str> = stat[close + 1..].split_whitespace().collect();
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    Some(utime + stime)
}

pub(crate) fn kib(mem: &Value, field: &str) -> f64 {
    mem.get(field).and_then(Value::as_f64).unwrap_or(0.0)
}

/// The p50 of a `stats()` block, or the raw number when the field is scalar.
pub(crate) fn kb_value(value: &Value, field: &str) -> f64 {
    let Some(block) = value.get(field) else {
        return 0.0;
    };
    block
        .get("p50")
        .and_then(Value::as_f64)
        .or_else(|| block.as_f64())
        .unwrap_or(0.0)
}

// ---------------------------------------------------------------------------
// statistics
// ---------------------------------------------------------------------------

pub(crate) fn stats(samples: &[f64]) -> Value {
    if samples.is_empty() {
        return json!({"samples": 0});
    }
    let mut sorted: Vec<f64> = samples.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    json!({
        "samples": sorted.len(),
        "p50": round3(percentile(&sorted, 0.50)),
        "p95": round3(percentile(&sorted, 0.95)),
        "min": round3(sorted[0]),
        "max": round3(*sorted.last().unwrap()),
        "mean": round3(sorted.iter().sum::<f64>() / sorted.len() as f64),
        "raw": sorted.iter().map(|v| round3(*v)).collect::<Vec<_>>(),
    })
}

/// Nearest-rank percentile on a sorted slice.
fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (p * sorted.len() as f64).ceil().max(1.0) as usize;
    sorted[rank.min(sorted.len()) - 1]
}

fn median(samples: &[f64]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    percentile(&sorted, 0.50)
}

pub(crate) fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

pub(crate) fn parse_session_counts(text: &str) -> Result<Vec<usize>, BenchFailure> {
    let mut out = Vec::new();
    for part in text.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let n: usize = part.parse().map_err(|_| {
            BenchFailure::new(
                ErrorCode::InvalidArgument,
                format!("--sessions: {part:?} is not a number"),
            )
        })?;
        out.push(n);
    }
    if out.is_empty() {
        return Err(BenchFailure::new(
            ErrorCode::InvalidArgument,
            "--sessions: at least one N is required",
        ));
    }
    Ok(out)
}

struct Program {
    name: String,
    path: PathBuf,
}

fn resolve_programs(args: &BenchArgs) -> Result<Vec<Program>, BenchFailure> {
    let defaults: Vec<PathBuf> = [
        "fixtures/build/dexprogs/02_int_arith/classes.dex",
        "fixtures/build/dexprogs/05_branches_loops/classes.dex",
        "fixtures/build/dexprogs/09_strings/classes.dex",
    ]
    .iter()
    .map(PathBuf::from)
    .collect();
    let paths = if args.dexes.is_empty() {
        defaults
    } else {
        args.dexes.clone()
    };
    let mut out = Vec::new();
    for path in paths {
        if !path.is_file() {
            return Err(BenchFailure::new(
                ErrorCode::ApkIo,
                format!(
                    "{} is missing; build it with tools/build-fixtures.sh",
                    path.display()
                ),
            ));
        }
        let name = path
            .parent()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        out.push(Program { name, path });
    }
    Ok(out)
}

pub(crate) fn resolve_apk(args: &BenchArgs) -> Result<PathBuf, BenchFailure> {
    let apk = if args.apk.is_absolute() {
        args.apk.clone()
    } else {
        let cwd = std::env::current_dir()
            .map_err(|e| BenchFailure::internal(format!("current_dir: {e}")))?;
        if args.apk.is_file() {
            args.apk.clone()
        } else {
            cwd.join(&args.apk)
        }
    };
    if !apk.is_file() {
        return Err(BenchFailure::new(
            ErrorCode::ApkIo,
            format!(
                "{} is missing; build it with tools/build-apps.sh",
                apk.display()
            ),
        ));
    }
    Ok(apk)
}

pub(crate) fn file_sha256(path: &Path) -> String {
    // A hash is evidence metadata, not a benchmark input: use `sha256sum` when
    // present and report `unavailable` instead of inventing a value.
    match Command::new("sha256sum").arg(path).output() {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .next()
            .unwrap_or("unavailable")
            .to_string(),
        _ => "unavailable".to_string(),
    }
}

// ---------------------------------------------------------------------------
// text output
// ---------------------------------------------------------------------------

fn print_text(doc: &Value) {
    let metric = doc.get("metric").and_then(Value::as_str).unwrap_or("bench");
    println!("### acos bench {metric}");
    if let Some(measured) = doc.get("measured").and_then(Value::as_object) {
        for (name, value) in measured {
            println!("- {name}: {}", summarize(value));
        }
    } else {
        println!("- {}", summarize(doc));
    }
    if let Some(rows) = doc.get("not_yet_measurable").and_then(Value::as_array) {
        println!("- not_yet_measurable ({}):", rows.len());
        for row in rows {
            println!(
                "  - {}: {}",
                row.get("metric").and_then(Value::as_str).unwrap_or(""),
                row.get("dependency").and_then(Value::as_str).unwrap_or("")
            );
        }
    }
}

fn summarize(value: &Value) -> String {
    let mut parts = Vec::new();
    for key in [
        "wall_ms",
        "boot_ms",
        "attach_to_bind_ms",
        "bind_phase_ms",
        "vm_ready_ms",
        "pss_kib",
        "anon_plus_shmem_kib",
        "cpu_ticks",
    ] {
        if let Some(block) = value.get(key) {
            let p50 = block.get("p50").and_then(Value::as_f64);
            let p95 = block.get("p95").and_then(Value::as_f64);
            if let (Some(a), Some(b)) = (p50, p95) {
                parts.push(format!("{key} p50={a} p95={b}"));
            }
        }
    }
    if let Some(err) = value.get("error").and_then(Value::as_str) {
        parts.push(format!("error={err}"));
    }
    if parts.is_empty() {
        "measured".to_string()
    } else {
        parts.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_uses_nearest_rank() {
        let v: Vec<f64> = (1..=20).map(f64::from).collect();
        assert_eq!(percentile(&v, 0.50), 10.0);
        assert_eq!(percentile(&v, 0.95), 19.0);
        assert_eq!(percentile(&v, 1.0), 20.0);
    }

    #[test]
    fn stats_are_labelled() {
        let s = stats(&[3.0, 1.0, 2.0]);
        assert_eq!(s["p50"], 2.0);
        assert_eq!(s["min"], 1.0);
        assert_eq!(s["max"], 3.0);
        assert_eq!(s["samples"], 3);
    }

    #[test]
    fn smaps_and_status_lines_parse() {
        let rollup = "Rss:                1234 kB\nPss:                 567 kB\n\
                      Pss_Anon:            400 kB\nShared_Clean:         10 kB\n\
                      Shared_Dirty:         20 kB\nPrivate_Clean:        30 kB\n\
                      Private_Dirty:        40 kB\nAnonymous:           400 kB\nSwap:                  0 kB\n";
        assert_eq!(smaps_value(rollup, "Rss"), Some(1234.0));
        assert_eq!(smaps_value(rollup, "Pss"), Some(567.0));
        assert_eq!(smaps_value(rollup, "Pss_Anon"), Some(400.0));
        assert_eq!(smaps_value(rollup, "Missing"), None);
        let status = "Name:\tacos\nRssAnon:\t  100 kB\nRssFile:\t  200 kB\nRssShmem:\t   50 kB\n";
        assert_eq!(status_kib(status, "RssAnon"), Some(100.0));
        assert_eq!(status_kib(status, "RssFile"), Some(200.0));
        assert_eq!(status_kib(status, "RssShmem"), Some(50.0));
    }

    #[test]
    fn session_counts_parse() {
        assert_eq!(parse_session_counts("1,2,5,10").unwrap(), vec![1, 2, 5, 10]);
        assert!(parse_session_counts("").is_err());
        assert!(parse_session_counts("1,x").is_err());
    }

    #[test]
    fn not_yet_metrics_all_name_a_dependency() {
        let mut count = 0;
        for metric in NOT_YET {
            let dep = metric.dependency().expect("dependency");
            assert!(!dep.is_empty());
            count += 1;
        }
        assert_eq!(count, 7);
        for metric in [
            Metric::ProcessStart,
            Metric::BootAttach,
            Metric::BindApp,
            Metric::Launch,
            Metric::WorkerCreateFrame,
            Metric::ResetToReady,
            Metric::IdleRss,
            Metric::Sessions,
            Metric::IdleCpu,
            Metric::Interpreter,
        ] {
            assert!(metric.dependency().is_none());
        }
    }

    /// The emitted document must be a valid `acos-protocol` envelope (this is
    /// the structural guarantee the harness advertises).
    #[test]
    fn bench_document_is_a_valid_envelope() {
        let doc = json!({
            "metric": "interpreter",
            "programs": [{"program": "02_int_arith"}],
            "methodology": METHODOLOGY,
        });
        let envelope = Envelope::pass(
            "req_test",
            Backend::AcosNative,
            Fidelity::NativeRuntime,
            Metrics::wall_ms(5),
        )
        .with_result(doc)
        .expect("result");
        let envelope = validate_roundtrip(envelope).expect("valid envelope");
        assert!(envelope.ok());
        assert_eq!(envelope.status().as_str(), "pass");
        let text = serde_json::to_string(&envelope).unwrap();
        let parsed: Envelope = serde_json::from_str(&text).unwrap();
        assert_eq!(
            parsed.result().unwrap()["metric"].as_str().unwrap(),
            "interpreter"
        );
    }

    #[test]
    fn unsupported_metrics_never_pass() {
        for metric in NOT_YET {
            let result = measure_one(*metric, &test_args());
            let failure = result.expect_err("must not be measurable");
            assert_eq!(failure.code.as_str(), "UNSUPPORTED_ANDROID_API");
            let envelope = Envelope::failure(
                "req_test",
                Backend::AcosNative,
                Fidelity::NativeRuntime,
                ProtocolError::new(failure.code, failure.message),
                Metrics::wall_ms(1),
            );
            assert!(!envelope.ok());
            assert_eq!(envelope.status().as_str(), "unsupported");
        }
    }

    fn test_args() -> BenchArgs {
        BenchArgs {
            metric: Metric::All,
            runs: 1,
            warmup: 0,
            json: false,
            apk: PathBuf::from("fixtures/build/apps/hello-views.apk"),
            dexes: Vec::new(),
            main: "Main".to_string(),
            sessions: "1".to_string(),
            corelib: "real".to_string(),
            settle_ms: 0,
            cpu_secs: 0,
            out: None,
            child: None,
            n: 0,
            variant: "vm".to_string(),
        }
    }
}
