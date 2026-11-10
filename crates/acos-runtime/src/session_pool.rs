//! **Session pool: fast session creation from a shared template snapshot, with
//! in-process parallel execution, per-session resets and per-session isolation.**
//!
//! This is the T-44 building block: rather than booting the real Android-14
//! boot classpath for every session, one [`SessionTemplate`] boots a core `Vm`
//! once, snapshots it (`Vm::snapshot`, W19-E/T-42) and then every session is a
//! `Vm::restore` from those bytes. The process-global `acos_dex::cache::BootCache`
//! and `acos_vm::codecache` (W21-B) make both the template boot and each restore
//! share the raw boot DEX, the parsed `DexFile` tables and the decoded
//! `MethodCode` bodies; only the mutable VM state (heap, statics, handles,
//! interner, class table) is per session.
//!
//! # Concurrency model
//!
//! `Vm` carries no `Send`-hostile state in the boot path, but the pool does not
//! rely on that: each worker **thread owns its own `Vm`** and creates/restores
//! it *on that thread* from the shared immutable `Arc<[u8]>` snapshot. Jobs and
//! control commands travel over `std::sync::mpsc` channels, so no `&mut Vm`
//! ever crosses a thread. This is the design the task asked for and the one the
//! `Runtime` wrapper (which owns an `Rc<RefCell<ServiceHost>>`) forces anyway.
//!
//! # Isolation and failure containment
//!
//! * Every job body is wrapped in [`std::panic::catch_unwind`]. A panic drops
//!   the tainted `Vm` and the session is lazily restored from the snapshot; the
//!   other sessions are untouched.
//! * Every session has its own [`HostGuard`] wall clock and a guest heap quota;
//!   a looping session returns `TIMEOUT` from `Vm::run` and is replaced.
//! * Results are keyed by [`SessionJob::id`] and returned sorted, so the output
//!   is deterministic for any worker count `N`.
//!
//! # What is *not* isolated (see `results/bench/POOL_DESIGN.md`)
//!
//! The shared boot DEX/decoded code is immutable research data, but this is an
//! **in-process** pool: a memory-safety bug or an escape in one session could
//! reach another session's memory, and the `HostGuard` RSS ceiling reads the
//! whole process, not one session. **Processes remain the isolation boundary for
//! untrusted APKs.** In-process pooling is for the agent's own trusted
//! workloads; untrusted tenants need one process (or a zygote-forked child) per
//! session with its own capability/transport namespace.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use acos_dex::DexFile;
use acos_vm::limits::HostGuard;
use acos_vm::vm::{CoreLib, RunOutcome, Vm, VmOptions};
use acos_vm::VmError;

/// Structured pool failure.
#[derive(Debug, thiserror::Error)]
pub enum PoolError {
    /// The one-time template boot + snapshot failed.
    #[error("session template boot failed: {0}")]
    Boot(#[source] VmError),
    /// A session could not be restored from the template snapshot.
    #[error("session restore failed: {0}")]
    Restore(#[source] VmError),
    /// A worker thread/channel is gone (it panicked outside the per-job guard).
    #[error("session worker {session} is gone")]
    WorkerGone { session: usize },
    /// The pool was asked to run with zero sessions.
    #[error("the session pool has no sessions")]
    NoSessions,
    /// The job's `main` class is empty.
    #[error("job {job} has no main class")]
    EmptyMain { job: u64 },
}

/// A per-session resource quota.
///
/// `heap_max_bytes` is enforced by the managed heap and is genuinely
/// per-session. `wall_secs` is enforced by the VM's watchdog during `run`.
/// `rss_max_bytes` is checked by [`HostGuard`] against **process** RSS (the
/// kernel does not expose a per-thread RSS), so with `N` sessions it is a
/// process budget, not a per-session one; leave it `0` (disabled) unless the
/// pool is the only load, and read `POOL_DESIGN.md` before tightening it.
#[derive(Clone, Copy, Debug)]
pub struct SessionLimits {
    /// Guest managed-heap ceiling per session (`0` = the template's value).
    pub heap_max_bytes: usize,
    /// Wall-clock ceiling for one `run` (`0` = disabled).
    pub wall_secs: u64,
    /// Optional host-process RSS ceiling sampled by the VM watchdog (`0` = off).
    pub rss_max_bytes: usize,
}

impl Default for SessionLimits {
    fn default() -> Self {
        SessionLimits {
            heap_max_bytes: 0,
            wall_secs: 60,
            rss_max_bytes: 0,
        }
    }
}

/// A deliberately injected session fault, used by the isolation tests (and only
/// by them): `Panic` makes the worker panic inside the job guard, proving the
/// pool survives and replaces the session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionFault {
    /// Panic in the session thread before running the program.
    Panic,
}

/// One unit of work for a pooled session.
#[derive(Clone, Debug)]
pub struct SessionJob {
    /// Stable identifier; results are returned sorted by this.
    pub id: u64,
    /// Fully-qualified main class (e.g. `Main`).
    pub main: String,
    /// Shared parsed application DEX. `Arc` so all sessions reuse one parse.
    pub dex: Option<Arc<DexFile>>,
    /// Restore the session from the template snapshot after the run (a
    /// "run + reset" job). When `false` the session keeps its state, which is
    /// how a caller can observe leaked state on purpose.
    pub reset_after: bool,
    /// Test-only fault injection.
    pub fault: Option<SessionFault>,
}

impl SessionJob {
    /// A run + reset job (the normal pooled workload).
    #[must_use]
    pub fn new(id: u64, main: impl Into<String>, dex: Option<Arc<DexFile>>) -> Self {
        SessionJob {
            id,
            main: main.into(),
            dex,
            reset_after: true,
            fault: None,
        }
    }
}

/// How a job finished.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobStatus {
    /// `Vm::run` returned without a VM-level error (a Java exception with a
    /// non-zero exit code is still `Completed`: it is a deterministic program
    /// result).
    Completed,
    /// The session was stopped by its own wall/heap watchdog (`TIMEOUT` /
    /// `HOST_MEMORY_LIMIT`) and then replaced.
    Killed,
    /// A VM-level structured error other than the watchdogs.
    Error,
    /// The session thread panicked; it was dropped and will be restored lazily.
    Panicked,
    /// The session could not be restored at all.
    RestoreFailed,
}

/// The result of one [`SessionJob`].
#[derive(Clone, Debug)]
pub struct JobResult {
    pub job_id: u64,
    /// The worker/session index that executed it.
    pub session: usize,
    pub status: JobStatus,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    /// Class name of an uncaught Java exception, if the program threw.
    pub uncaught: Option<String>,
    /// Wall time of `Vm::run` (`0` when the job never ran).
    pub run_nanos: u64,
    /// Wall time of the reset restore (`0` when no reset happened).
    pub reset_nanos: u64,
    /// Structured code for `Killed`/`Error`/`Panicked`/`RestoreFailed`.
    pub error: Option<String>,
}

impl JobResult {
    fn panicked(job_id: u64, session: usize, message: String) -> JobResult {
        JobResult {
            job_id,
            session,
            status: JobStatus::Panicked,
            exit_code: -1,
            stdout: String::new(),
            stderr: String::new(),
            uncaught: None,
            run_nanos: 0,
            reset_nanos: 0,
            error: Some(message),
        }
    }

    fn restore_failed(job_id: u64, session: usize, message: String) -> JobResult {
        JobResult {
            job_id,
            session,
            status: JobStatus::RestoreFailed,
            exit_code: -1,
            stdout: String::new(),
            stderr: String::new(),
            uncaught: None,
            run_nanos: 0,
            reset_nanos: 0,
            error: Some(message),
        }
    }

    /// True when the job produced a deterministic program result.
    #[must_use]
    pub fn is_completed(&self) -> bool {
        self.status == JobStatus::Completed
    }
}

/// The result of an explicit [`SessionPool::reset_all`] for one session.
#[derive(Clone, Copy, Debug)]
pub struct ResetReport {
    pub session: usize,
    pub ok: bool,
    pub reset_nanos: u64,
}

/// The result of [`SessionPool::warm`] for one session.
#[derive(Clone, Copy, Debug)]
pub struct WarmReport {
    pub session: usize,
    /// `true` when this call actually created the session (first warm).
    pub created: bool,
    /// Restore wall time when `created`, else `0`.
    pub create_nanos: u64,
}

/// Aggregated worker counters.
#[derive(Clone, Copy, Debug, Default)]
pub struct PoolStats {
    pub sessions: usize,
    pub jobs: u64,
    pub resets: u64,
    pub panics: u64,
    pub timeouts: u64,
    pub restore_failures: u64,
    /// Sum of all measured reset restore times.
    pub reset_nanos_total: u64,
}

/// One immutable template from which every session is restored.
///
/// Cloning is cheap (an `Arc<[u8]>` bump) and lets several pools share the same
/// snapshot bytes.
#[derive(Clone, Debug)]
pub struct SessionTemplate {
    snapshot: Arc<[u8]>,
    corelib: CoreLib,
    heap_max_bytes: usize,
    boot_classes: usize,
    boot_wall: Duration,
}

impl SessionTemplate {
    /// Boot a core `Vm` once and snapshot it. This is the only step that pays
    /// the full boot-classpath cost; every session afterwards is a restore.
    pub fn boot(options: VmOptions) -> Result<SessionTemplate, PoolError> {
        let started = Instant::now();
        let vm = Vm::new(options.clone()).map_err(PoolError::Boot)?;
        let snapshot = vm.snapshot().map_err(PoolError::Boot)?;
        let template = SessionTemplate {
            snapshot: Arc::from(snapshot.into_boxed_slice()),
            corelib: options.corelib,
            heap_max_bytes: options.heap_max_bytes,
            boot_classes: vm.classes.len(),
            boot_wall: started.elapsed(),
        };
        Ok(template)
    }

    /// Restore one fresh session `Vm` from the template snapshot.
    pub fn restore(&self) -> Result<Vm, PoolError> {
        Vm::restore(&self.snapshot).map_err(PoolError::Restore)
    }

    /// Snapshot bytes (the immutable data every session shares).
    #[must_use]
    pub fn snapshot_bytes(&self) -> usize {
        self.snapshot.len()
    }

    #[must_use]
    pub fn corelib(&self) -> CoreLib {
        self.corelib
    }

    #[must_use]
    pub fn boot_classes(&self) -> usize {
        self.boot_classes
    }

    #[must_use]
    pub fn boot_wall(&self) -> Duration {
        self.boot_wall
    }

    #[must_use]
    pub fn heap_max_bytes(&self) -> usize {
        self.heap_max_bytes
    }
}

// ---------------------------------------------------------------------------
// workers
// ---------------------------------------------------------------------------

enum Cmd {
    Job(Box<SessionJob>, Sender<JobResult>),
    Reset(Sender<ResetReport>),
    Warm(Sender<WarmReport>),
    Shutdown,
}

#[derive(Debug, Default)]
struct WorkerStats {
    jobs: AtomicU64,
    resets: AtomicU64,
    panics: AtomicU64,
    timeouts: AtomicU64,
    restore_failures: AtomicU64,
    reset_nanos_total: AtomicU64,
}

#[derive(Debug)]
struct Worker {
    index: usize,
    tx: Sender<Cmd>,
    join: Option<JoinHandle<()>>,
    stats: Arc<WorkerStats>,
}

impl Worker {
    fn spawn(
        index: usize,
        template: Arc<SessionTemplate>,
        limits: SessionLimits,
        stats: Arc<WorkerStats>,
    ) -> Worker {
        let (tx, rx) = channel::<Cmd>();
        let join = {
            let stats = Arc::clone(&stats);
            let template = Arc::clone(&template);
            thread::Builder::new()
                .name(format!("acos-session-{index}"))
                .spawn(move || worker_loop(index, template, limits, stats, rx))
                .expect("spawn session worker thread")
        };
        Worker {
            index,
            tx,
            join: Some(join),
            stats,
        }
    }
}

/// Restore one session and apply its quota. Runs on the owning worker thread.
fn restore_session(
    template: &SessionTemplate,
    limits: &SessionLimits,
) -> Result<Vm, PoolError> {
    let mut vm = template.restore()?;
    vm.host = HostGuard::new(limits.rss_max_bytes, limits.wall_secs);
    if limits.heap_max_bytes != 0 {
        vm.heap.max_bytes = limits.heap_max_bytes;
    }
    Ok(vm)
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "session thread panicked".to_string()
    }
}

fn worker_loop(
    index: usize,
    template: Arc<SessionTemplate>,
    limits: SessionLimits,
    stats: Arc<WorkerStats>,
    rx: Receiver<Cmd>,
) {
    // Each worker owns its Vm here, on this thread. It is created lazily and
    // dropped/replaced on every reset and on every panic.
    let mut vm: Option<Vm> = None;

    while let Ok(cmd) = rx.recv() {
        match cmd {
            Cmd::Job(job, reply) => {
                let job_id = job.id;
                let outcome = catch_unwind(AssertUnwindSafe(|| {
                    run_one_job(index, &template, &limits, &mut vm, &stats, &job)
                }));
                let result = match outcome {
                    Ok(result) => result,
                    Err(payload) => {
                        // The tainted Vm is a local inside the closure and was
                        // dropped while unwinding; force a fresh restore next.
                        vm = None;
                        stats.panics.fetch_add(1, Ordering::Relaxed);
                        JobResult::panicked(job_id, index, panic_message(payload.as_ref()))
                    }
                };
                let _ = reply.send(result);
            }
            Cmd::Reset(reply) => {
                let outcome = catch_unwind(AssertUnwindSafe(|| {
                    let t0 = Instant::now();
                    match restore_session(&template, &limits) {
                        Ok(v) => {
                            vm = Some(v);
                            let nanos = t0.elapsed().as_nanos() as u64;
                            stats.resets.fetch_add(1, Ordering::Relaxed);
                            stats.reset_nanos_total.fetch_add(nanos, Ordering::Relaxed);
                            ResetReport {
                                session: index,
                                ok: true,
                                reset_nanos: nanos,
                            }
                        }
                        Err(e) => {
                            vm = None;
                            stats.restore_failures.fetch_add(1, Ordering::Relaxed);
                            eprintln!("session-pool: session {index} reset failed: {e}");
                            ResetReport {
                                session: index,
                                ok: false,
                                reset_nanos: t0.elapsed().as_nanos() as u64,
                            }
                        }
                    }
                }));
                let report = match outcome {
                    Ok(r) => r,
                    Err(payload) => {
                        vm = None;
                        stats.panics.fetch_add(1, Ordering::Relaxed);
                        let _ = panic_message(payload.as_ref());
                        ResetReport {
                            session: index,
                            ok: false,
                            reset_nanos: 0,
                        }
                    }
                };
                let _ = reply.send(report);
            }
            Cmd::Warm(reply) => {
                let outcome = catch_unwind(AssertUnwindSafe(|| {
                    if vm.is_some() {
                        WarmReport {
                            session: index,
                            created: false,
                            create_nanos: 0,
                        }
                    } else {
                        let t0 = Instant::now();
                        match restore_session(&template, &limits) {
                            Ok(v) => {
                                vm = Some(v);
                                WarmReport {
                                    session: index,
                                    created: true,
                                    create_nanos: t0.elapsed().as_nanos() as u64,
                                }
                            }
                            Err(e) => {
                                vm = None;
                                stats.restore_failures.fetch_add(1, Ordering::Relaxed);
                                let _ = e;
                                WarmReport {
                                    session: index,
                                    created: false,
                                    create_nanos: 0,
                                }
                            }
                        }
                    }
                }));
                let report = match outcome {
                    Ok(r) => r,
                    Err(_) => {
                        vm = None;
                        WarmReport {
                            session: index,
                            created: false,
                            create_nanos: 0,
                        }
                    }
                };
                let _ = reply.send(report);
            }
            Cmd::Shutdown => break,
        }
    }
}

fn run_one_job(
    index: usize,
    template: &SessionTemplate,
    limits: &SessionLimits,
    vm_slot: &mut Option<Vm>,
    stats: &WorkerStats,
    job: &SessionJob,
) -> JobResult {
    if job.fault == Some(SessionFault::Panic) {
        panic!("session {index}: injected panic fault (job {})", job.id);
    }
    if job.main.is_empty() {
        return JobResult::restore_failed(job.id, index, "empty main class".to_string());
    }
    if vm_slot.is_none() {
        match restore_session(template, limits) {
            Ok(v) => *vm_slot = Some(v),
            Err(e) => {
                stats.restore_failures.fetch_add(1, Ordering::Relaxed);
                return JobResult::restore_failed(job.id, index, e.to_string());
            }
        }
    }

    let vm = vm_slot.as_mut().expect("session restored above");
    if let Some(dex) = &job.dex {
        // `add_shared_dex` is idempotent per `DexFile::id`; skip a re-add so a
        // non-reset session does not grow its DEX table.
        if !vm.dexs.iter().any(|ctx| ctx.file.id() == dex.id()) {
            vm.add_shared_dex(Arc::clone(dex));
        }
    }

    let t0 = Instant::now();
    let outcome: RunOutcome = vm.run(&job.main, &[]);
    let run_nanos = t0.elapsed().as_nanos() as u64;
    stats.jobs.fetch_add(1, Ordering::Relaxed);

    let (status, error) = classify(&outcome);
    if status == JobStatus::Killed {
        stats.timeouts.fetch_add(1, Ordering::Relaxed);
    }

    let mut reset_nanos = 0u64;
    if job.reset_after || status != JobStatus::Completed {
        let t_reset = Instant::now();
        match restore_session(template, limits) {
            Ok(v) => {
                // Dropping the old Vm here is the reset.
                *vm_slot = Some(v);
                reset_nanos = t_reset.elapsed().as_nanos() as u64;
                stats.resets.fetch_add(1, Ordering::Relaxed);
                stats
                    .reset_nanos_total
                    .fetch_add(reset_nanos, Ordering::Relaxed);
            }
            Err(e) => {
                *vm_slot = None;
                stats.restore_failures.fetch_add(1, Ordering::Relaxed);
                return JobResult {
                    job_id: job.id,
                    session: index,
                    status: JobStatus::RestoreFailed,
                    exit_code: -1,
                    stdout: outcome.stdout,
                    stderr: outcome.stderr,
                    uncaught: outcome.uncaught.map(|u| u.class),
                    run_nanos,
                    reset_nanos,
                    error: Some(e.to_string()),
                };
            }
        }
    }

    JobResult {
        job_id: job.id,
        session: index,
        status,
        exit_code: outcome.exit_code,
        stdout: outcome.stdout,
        stderr: outcome.stderr,
        uncaught: outcome.uncaught.map(|u| u.class),
        run_nanos,
        reset_nanos,
        error,
    }
}

fn classify(outcome: &RunOutcome) -> (JobStatus, Option<String>) {
    match &outcome.vm_error {
        Some(VmError::Timeout { .. }) => (JobStatus::Killed, Some("TIMEOUT".to_string())),
        Some(VmError::HostMemoryLimit { .. }) => {
            (JobStatus::Killed, Some("HOST_MEMORY_LIMIT".to_string()))
        }
        Some(e) => (JobStatus::Error, Some(e.code().to_string())),
        None => (JobStatus::Completed, None),
    }
}

// ---------------------------------------------------------------------------
// pool
// ---------------------------------------------------------------------------

/// A pool of `N` worker sessions sharing one immutable template snapshot.
#[derive(Debug)]
pub struct SessionPool {
    template: Arc<SessionTemplate>,
    limits: SessionLimits,
    workers: Vec<Worker>,
    next: usize,
}

impl SessionPool {
    /// Start `sessions` worker threads from `template`.
    pub fn start(
        template: SessionTemplate,
        sessions: usize,
        limits: SessionLimits,
    ) -> Result<SessionPool, PoolError> {
        if sessions == 0 {
            return Err(PoolError::NoSessions);
        }
        let template = Arc::new(template);
        let mut workers = Vec::with_capacity(sessions);
        for index in 0..sessions {
            let stats = Arc::new(WorkerStats::default());
            workers.push(Worker::spawn(
                index,
                Arc::clone(&template),
                limits,
                stats,
            ));
        }
        Ok(SessionPool {
            template,
            limits,
            workers,
            next: 0,
        })
    }

    /// Start a pool from a fresh template boot.
    pub fn boot(
        options: VmOptions,
        sessions: usize,
        limits: SessionLimits,
    ) -> Result<SessionPool, PoolError> {
        let template = SessionTemplate::boot(options)?;
        SessionPool::start(template, sessions, limits)
    }

    #[must_use]
    pub fn sessions(&self) -> usize {
        self.workers.len()
    }

    #[must_use]
    pub fn template(&self) -> &SessionTemplate {
        &self.template
    }

    #[must_use]
    pub fn limits(&self) -> SessionLimits {
        self.limits
    }

    /// Create any session that does not exist yet, so the caller can measure the
    /// settled memory of `N` resident sessions. Returns one report per session
    /// with the restore cost of the ones actually created.
    pub fn warm(&mut self) -> Result<Vec<WarmReport>, PoolError> {
        let mut pending = Vec::with_capacity(self.workers.len());
        for worker in &self.workers {
            let (tx, rx) = channel();
            worker
                .tx
                .send(Cmd::Warm(tx))
                .map_err(|_| PoolError::WorkerGone {
                    session: worker.index,
                })?;
            pending.push((worker.index, rx));
        }
        let mut reports = Vec::with_capacity(pending.len());
        for (index, rx) in pending {
            reports.push(rx.recv().map_err(|_| PoolError::WorkerGone { session: index })?);
        }
        reports.sort_by_key(|r| r.session);
        Ok(reports)
    }

    /// Run a batch of jobs across the pool and return their results **sorted by
    /// `job_id`** (deterministic regardless of `N`). Jobs are assigned
    /// round-robin; the call blocks until every job has a result.
    pub fn run_batch(&mut self, jobs: Vec<SessionJob>) -> Result<Vec<JobResult>, PoolError> {
        if self.workers.is_empty() {
            return Err(PoolError::NoSessions);
        }
        let mut pending = Vec::with_capacity(jobs.len());
        for job in jobs {
            let index = self.next % self.workers.len();
            self.next = self.next.wrapping_add(1);
            let (tx, rx) = channel();
            self.workers[index]
                .tx
                .send(Cmd::Job(Box::new(job), tx))
                .map_err(|_| PoolError::WorkerGone { session: index })?;
            pending.push((index, rx));
        }
        let mut results = Vec::with_capacity(pending.len());
        for (index, rx) in pending {
            results.push(rx.recv().map_err(|_| PoolError::WorkerGone { session: index })?);
        }
        results.sort_by_key(|r| r.job_id);
        Ok(results)
    }

    /// Run one job per resident session, concurrently.
    pub fn run_round(
        &mut self,
        main: &str,
        dex: Option<Arc<DexFile>>,
        base_id: u64,
        reset_after: bool,
    ) -> Result<Vec<JobResult>, PoolError> {
        let jobs: Vec<SessionJob> = (0..self.workers.len())
            .map(|i| {
                let mut job = SessionJob::new(base_id + i as u64, main, dex.clone());
                job.reset_after = reset_after;
                job
            })
            .collect();
        self.run_batch(jobs)
    }

    /// Explicitly restore every session to the clean template state.
    pub fn reset_all(&mut self) -> Result<Vec<ResetReport>, PoolError> {
        let mut pending = Vec::with_capacity(self.workers.len());
        for worker in &self.workers {
            let (tx, rx) = channel();
            worker
                .tx
                .send(Cmd::Reset(tx))
                .map_err(|_| PoolError::WorkerGone {
                    session: worker.index,
                })?;
            pending.push((worker.index, rx));
        }
        let mut reports = Vec::with_capacity(pending.len());
        for (index, rx) in pending {
            reports.push(rx.recv().map_err(|_| PoolError::WorkerGone { session: index })?);
        }
        reports.sort_by_key(|r| r.session);
        Ok(reports)
    }

    /// Aggregated worker counters.
    #[must_use]
    pub fn stats(&self) -> PoolStats {
        let mut out = PoolStats {
            sessions: self.workers.len(),
            ..PoolStats::default()
        };
        for worker in &self.workers {
            out.jobs += worker.stats.jobs.load(Ordering::Relaxed);
            out.resets += worker.stats.resets.load(Ordering::Relaxed);
            out.panics += worker.stats.panics.load(Ordering::Relaxed);
            out.timeouts += worker.stats.timeouts.load(Ordering::Relaxed);
            out.restore_failures += worker.stats.restore_failures.load(Ordering::Relaxed);
            out.reset_nanos_total += worker.stats.reset_nanos_total.load(Ordering::Relaxed);
        }
        out
    }
}

impl Drop for SessionPool {
    fn drop(&mut self) {
        for worker in &self.workers {
            let _ = worker.tx.send(Cmd::Shutdown);
        }
        for worker in &mut self.workers {
            if let Some(join) = worker.join.take() {
                let _ = join.join();
            }
        }
    }
}
