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

use std::cell::RefCell;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use acos_dex::DexFile;
use acos_parcel::{set_process_identity, BinderId, BinderKind};
use acos_services::ServiceHost;
use acos_vm::javabridge::JavaValue;
use acos_vm::limits::HostGuard;
use acos_vm::natives::binder::install_transport;
use acos_vm::vm::{CoreLib, RunOutcome, Vm, VmOptions};
use acos_vm::VmError;

use acos_vm::class::InitState;

use crate::atms_boot::{first_attach, install_recording_atms, is_valid_app_thread};

// ---------------------------------------------------------------------------
// Deterministic framework `<clinit>` priming (W111-A)
// ---------------------------------------------------------------------------

/// Framework classes whose `<clinit>` is a pure function of the device image:
/// it reads only the boot classpath, system properties and `/system` files, and
/// captures no per-app, per-session or per-launch state.
///
/// Their initialisation is therefore lifted out of the per-launch
/// `bindApplication` into the framework-init template snapshot. A launch that
/// restores the template finds these classes already `Init`, so
/// `initialize_class` returns immediately and the cost is paid once when the
/// template is booted (and never again when it is restored from the on-disk
/// boot cache).
///
/// `Typeface` used to be deliberately absent: its `<clinit>` fills the
/// process-global font handle tables (`natives::fwk_fonts` / `gfx_text`), which
/// the `ACOSSNAP` stream did not carry. W115-A now serialises those tables
/// (handles + file paths + family indices; resolved faces are rebuilt lazily),
/// so a restored process re-imports them on first font use and the handles the
/// restored Java fields hold stay valid. `Typeface` and the `Font`/`FontFamily`
/// helpers its `<clinit>` drives are therefore primed.
///
/// A candidate is included only after it initialises successfully during the
/// template boot; a failure reverts the class to `NotInit` so the launch-time
/// first use behaves exactly as it did before priming.
pub const DETERMINISTIC_CLINIT_CLASSES: &[&str] = &[
    "Landroid/content/pm/PackagePartitions;",
    "Lsun/security/jca/Providers;",
    "Landroid/app/SystemServiceRegistry;",
    // NOTE: `android.provider.Settings$Global` is intentionally excluded: its
    // `<clinit>` installs an `android.provider.$Proxy0` dynamic proxy class.
    // The snapshot can carry it but `Vm::restore` cannot relink a class that
    // has no boot-DEX body, so priming it makes every warm restore fail.
    "Landroid/content/res/Configuration;",
    "Landroid/os/LocaleList;",
    "Ljava/util/Locale;",
    // Font stack driven by `Typeface.<clinit>` (W115-A). The native font
    // tables it fills are snapshotted and re-seeded lazily on restore.
    "Landroid/graphics/Typeface;",
    "Landroid/graphics/fonts/Font;",
    "Landroid/graphics/fonts/FontFamily;",
    "Landroid/graphics/fonts/FontFamily$Builder;",
];

/// Initialise the deterministic framework classes in [`DETERMINISTIC_CLINIT_CLASSES`].
///
/// Returns the descriptors that ended `Init`. A class that fails to load or
/// initialise is reverted to `NotInit` and omitted, so priming can never make a
/// class permanently `Failed` where the unprimed launch would still try.
fn prime_deterministic_classes(vm: &mut Vm) -> Vec<String> {
    // Benchmark/AB escape hatch: `ACOS_CLINIT_PRIME=0` disables priming so a
    // single binary can measure the before/after in interleaved rounds.
    if std::env::var("ACOS_CLINIT_PRIME")
        .is_ok_and(|v| v == "0" || v.eq_ignore_ascii_case("off"))
    {
        return Vec::new();
    }
    let trace = std::env::var_os("ACOS_BOOT_TRACE").is_some();
    let mut primed = Vec::new();
    for &desc in DETERMINISTIC_CLINIT_CLASSES {
        let cid = match vm.load_class(desc) {
            Ok(c) => c,
            Err(e) => {
                if trace {
                    eprintln!("[prime] {desc} load failed: {e}");
                }
                continue;
            }
        };
        if vm.classes[cid as usize].init == InitState::Init {
            primed.push(desc.to_string());
            continue;
        }
        match vm.initialize_class(cid) {
            Ok(()) if vm.classes[cid as usize].init == InitState::Init => {
                if trace {
                    eprintln!("[prime] {desc} initialised");
                }
                primed.push(desc.to_string());
            }
            Ok(()) => {
                if trace {
                    eprintln!("[prime] {desc} left {:?}", vm.classes[cid as usize].init);
                }
            }
            Err(e) => {
                // Revert so the first launch-time use sees `NotInit`, exactly
                // as it would without priming.
                vm.classes[cid as usize].init = InitState::NotInit;
                if trace {
                    eprintln!("[prime] {desc} failed: {e}");
                }
            }
        }
    }
    primed
}
use crate::boot_image_cache::{BootImageCache, LoadResult};
use crate::transport::ServiceTransport;
use crate::{Runtime, FIXTURE_UID};

/// Magic for the framed boot-cache payload (VM snapshot + binder layout).
const BOOT_PAYLOAD_MAGIC: u32 = 0x314C_4241;
/// Sanity bound on the binder-layout entry count decoded from a cache file.
const MAX_BOOT_PAYLOAD_ENTRIES: usize = 1 << 20;

/// A decoded boot-cache payload: `(vm snapshot, binder layout, attach record,
/// linked boot image)`. The image borrows the payload buffer (zero-copy);
/// `None`/empty means a legacy entry with no linked image.
type BootPayload<'a> = (
    Vec<u8>,
    Vec<(u64, String, BinderKind)>,
    Option<(u64, i64, String)>,
    &'a [u8],
);

fn kind_code(kind: BinderKind) -> u8 {
    match kind {
        BinderKind::Local => 0,
        BinderKind::Proxy => 1,
        BinderKind::WeakLocal => 2,
        BinderKind::WeakProxy => 3,
    }
}

fn kind_from_code(code: u8) -> Option<BinderKind> {
    Some(match code {
        0 => BinderKind::Local,
        1 => BinderKind::Proxy,
        2 => BinderKind::WeakLocal,
        3 => BinderKind::WeakProxy,
        _ => return None,
    })
}

/// Frame an `ACOSSNAP` snapshot together with the binder-id layout the restored
/// heap depends on. The cache stores this single opaque payload (T-43).
fn encode_boot_payload(
    snapshot: &[u8],
    layout: &[(u64, String, BinderKind)],
    attach: Option<&(u64, i64, String)>,
    image: &[u8],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(snapshot.len() + image.len() + 104 + layout.len() * 48);
    out.extend_from_slice(&BOOT_PAYLOAD_MAGIC.to_le_bytes());
    out.extend_from_slice(&(layout.len() as u32).to_le_bytes());
    for (id, desc, kind) in layout {
        out.extend_from_slice(&id.to_le_bytes());
        let bytes = desc.as_bytes();
        out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(bytes);
        out.push(kind_code(*kind));
    }
    match attach {
        Some((app_thread, seq, package)) => {
            out.push(1);
            out.extend_from_slice(&app_thread.to_le_bytes());
            out.extend_from_slice(&seq.to_le_bytes());
            out.extend_from_slice(&(package.len() as u32).to_le_bytes());
            out.extend_from_slice(package.as_bytes());
        }
        None => out.push(0),
    }
    out.extend_from_slice(&(snapshot.len() as u64).to_le_bytes());
    out.extend_from_slice(snapshot);
    // The fully linked BootImage (class/method tables + decoded bodies). A
    // legacy entry without this segment decodes as an empty image and the
    // restore still works, just paying the relink.
    out.extend_from_slice(&(image.len() as u64).to_le_bytes());
    out.extend_from_slice(image);
    out
}

/// Decode a [`encode_boot_payload`] frame. Every field is length-checked, so a
/// corrupt cache file is a clean `Err` and the caller falls back to a boot.
///
/// The linked-image segment is returned as a **borrow of `bytes`** rather than
/// a copy: it is only needed for the duration of the `Vm::restore` that follows
/// in [`RuntimeTemplate::from_cached`], so the old `image.to_vec()` was a pure
/// transient duplicate of up to ~25 MiB. The snapshot is copied out because the
/// template retains it after the payload buffer is dropped.
fn decode_boot_payload(bytes: &[u8]) -> Result<BootPayload<'_>, String> {
    if bytes.len() < 8 {
        return Err("truncated payload header".to_string());
    }
    if u32::from_le_bytes(bytes[0..4].try_into().unwrap_or_default()) != BOOT_PAYLOAD_MAGIC {
        return Err("bad payload magic".to_string());
    }
    let count = u32::from_le_bytes(bytes[4..8].try_into().unwrap_or_default()) as usize;
    if count > MAX_BOOT_PAYLOAD_ENTRIES {
        return Err(format!("binder layout entry count {count} too large"));
    }
    // Every length below is a full `u64` from the untrusted frame. Use checked
    // arithmetic and a `remaining = len - pos` comparison so a crafted length
    // (including `u64::MAX`) is a clean `Err`, never a wrap or a slice panic.
    let take = |pos: usize, n: usize| -> Result<usize, String> {
        pos.checked_add(n)
            .filter(|&end| end <= bytes.len())
            .ok_or_else(|| "truncated or overflowing payload".to_string())
    };
    let mut pos = 8usize;
    let mut layout = Vec::with_capacity(count);
    for _ in 0..count {
        pos = take(pos, 12)?;
        let id = u64::from_le_bytes(bytes[pos - 12..pos - 4].try_into().unwrap_or_default());
        let dlen = u32::from_le_bytes(bytes[pos - 4..pos].try_into().unwrap_or_default()) as usize;
        pos = take(pos, dlen)?;
        let desc = std::str::from_utf8(&bytes[pos - dlen..pos])
            .map_err(|_| "binder descriptor is not utf-8".to_string())?
            .to_string();
        pos = take(pos, 1)?;
        let kind = kind_from_code(bytes[pos - 1]).ok_or_else(|| "bad binder kind".to_string())?;
        layout.push((id, desc, kind));
    }
    pos = take(pos, 1)?;
    let attach = match bytes[pos - 1] {
        0 => None,
        1 => {
            pos = take(pos, 20)?;
            let app_thread =
                u64::from_le_bytes(bytes[pos - 20..pos - 12].try_into().unwrap_or_default());
            let seq = i64::from_le_bytes(bytes[pos - 12..pos - 4].try_into().unwrap_or_default());
            let plen = u32::from_le_bytes(bytes[pos - 4..pos].try_into().unwrap_or_default()) as usize;
            pos = take(pos, plen)?;
            let package = std::str::from_utf8(&bytes[pos - plen..pos])
                .map_err(|_| "attach package is not utf-8".to_string())?
                .to_string();
            Some((app_thread, seq, package))
        }
        other => return Err(format!("bad attach marker {other}")),
    };
    pos = take(pos, 8)?;
    let snap_len = u64::from_le_bytes(bytes[pos - 8..pos].try_into().unwrap_or_default()) as usize;
    pos = take(pos, snap_len)?;
    let snapshot = bytes[pos - snap_len..pos].to_vec();
    pos = take(pos, 8)?;
    let img_len = u64::from_le_bytes(bytes[pos - 8..pos].try_into().unwrap_or_default()) as usize;
    let end = pos
        .checked_add(img_len)
        .filter(|&end| end == bytes.len())
        .ok_or_else(|| "linked-image length mismatch".to_string())?;
    acos_vm::profile::note_alloc("boot_payload_snapshot", snapshot.len() as u64);
    acos_vm::profile::note_alloc("boot_payload_image", img_len as u64);
    Ok((snapshot, layout, attach, &bytes[pos..end]))
}

/// Structured pool failure.
#[derive(Debug, thiserror::Error)]
pub enum PoolError {
    /// The one-time template boot + snapshot failed.
    #[error("session template boot failed: {0}")]
    Boot(#[source] VmError),
    /// A framework-boot bridge call (Looper/ActivityThread/attach) failed.
    #[error("framework boot bridge failed: {0}")]
    Bridge(#[from] acos_vm::javabridge::BridgeError),
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
    from_cache: bool,
}

impl SessionTemplate {
    /// Boot a core `Vm` once and snapshot it. This is the only step that pays
    /// the full boot-classpath cost; every session afterwards is a restore.
    ///
    /// When the on-disk boot cache is enabled a warm bare-VM snapshot is loaded
    /// instead of booting; a miss boots once and publishes the snapshot. Set
    /// `ACOS_NO_BOOT_CACHE=1` (or `ACOS_BOOT_CACHE=0`) to opt out.
    pub fn boot(options: VmOptions) -> Result<SessionTemplate, PoolError> {
        let cache = crate::boot_image_cache::from_env();
        let boot_dir = acos_vm::vm::default_boot_dir(options.boot_dir.as_deref());
        let key = cache.as_ref().map(|_| {
            crate::boot_image_cache::salted_key(
                "session-template",
                boot_dir.as_deref(),
                options.corelib,
            )
        });
        if let (Some(cache), Some(key)) = (cache.as_ref(), key) {
            if let LoadResult::Hit(bytes) = cache.load(key) {
                if let Ok(vm) = Vm::restore(&bytes) {
                    return Ok(SessionTemplate {
                        snapshot: Arc::from(bytes.into_boxed_slice()),
                        corelib: options.corelib,
                        heap_max_bytes: options.heap_max_bytes,
                        boot_classes: vm.classes.len(),
                        boot_wall: Duration::ZERO,
                        from_cache: true,
                    });
                }
            }
        }
        let started = Instant::now();
        let vm = Vm::new(options.clone()).map_err(PoolError::Boot)?;
        let snapshot = vm.snapshot().map_err(PoolError::Boot)?;
        if let (Some(cache), Some(key)) = (cache.as_ref(), key) {
            let _ = cache.store(key, &snapshot);
        }
        let template = SessionTemplate {
            snapshot: Arc::from(snapshot.into_boxed_slice()),
            corelib: options.corelib,
            heap_max_bytes: options.heap_max_bytes,
            boot_classes: vm.classes.len(),
            boot_wall: started.elapsed(),
            from_cache: false,
        };
        Ok(template)
    }

    /// Whether the template snapshot came from the on-disk boot cache.
    #[must_use]
    pub fn from_cache(&self) -> bool {
        self.from_cache
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


// ---------------------------------------------------------------------------
// Framework-init runtime template (W45-A)
// ---------------------------------------------------------------------------

/// A framework-init snapshot: one `Runtime` is booted to the
/// `ActivityThread`-attached state, the `Vm` is snapshotted (`ACOSSNAP`), and
/// every launch restores that `Vm` instead of re-running
/// `Looper.prepareMainLooper` + `new ActivityThread` + `attach`.
///
/// The app DEX is **not** part of the template (the T-42 preconditions refuse
/// application DEX); it is loaded per launch after the `Vm` restore. The
/// `activity_thread` handle is preserved by the slot-preserving snapshot.
#[derive(Debug, Clone)]
pub struct RuntimeTemplate {
    snapshot: Arc<[u8]>,
    /// The boot process's binder-id layout, re-applied on every restore so the
    /// restored heap's raw `mObject`/`mNativeData` handles resolve (T-43).
    binder_layout: Arc<[(u64, String, BinderKind)]>,
    /// The boot process's `attachApplication` record `(app thread binder id,
    /// attach seq, package)`, replayed into a fresh ATMS so the restored
    /// framework's `finishAttachApplication` finds its process (T-43).
    attach: Option<(u64, i64, String)>,
    activity_thread: u32,
    boot_ms: f64,
    snapshot_ms: f64,
    classes: usize,
    from_cache: bool,
    /// Deterministic classes successfully primed before the snapshot.
    primed: Arc<[String]>,
}

impl RuntimeTemplate {
    /// Boot the framework once and capture the snapshot.
    ///
    /// This is the only step that pays the framework boot cost; it must run on
    /// the thread that will own the `Runtime` (the `Runtime` is not `Send`).
    pub fn boot() -> Result<RuntimeTemplate, PoolError> {
        let cache = crate::boot_image_cache::from_env();
        Self::boot_with_options(
            VmOptions {
                corelib: CoreLib::Real,
                ..VmOptions::default()
            },
            cache.as_ref(),
        )
    }

    /// [`RuntimeTemplate::boot`] with explicit VM options and an optional cache.
    ///
    /// The on-disk lookup is keyed by `hash(boot classpath + ACOS version +
    /// format version)`. Any cache problem (miss, corrupt/oversized file,
    /// fingerprint or version mismatch, failed restore) falls back to the real
    /// framework boot and is never surfaced as a template failure.
    pub fn boot_with_options(
        options: VmOptions,
        cache: Option<&BootImageCache>,
    ) -> Result<RuntimeTemplate, PoolError> {
        seed_device_environment();
        let boot_dir = acos_vm::vm::default_boot_dir(options.boot_dir.as_deref());
        let cache_key = cache.map(|c| c.image_key(boot_dir.as_deref(), options.corelib));
        let mut guard = None;

        if let (Some(cache), Some(key)) = (cache, cache_key) {
            let started = Instant::now();
            let hit = Self::cached_hit(cache, key, started);
            if std::env::var_os("ACOS_BOOT_TRACE").is_some() {
                eprintln!(
                    "[cache] key={key:016x} hit={} dir={}",
                    hit.is_some(),
                    cache.dir().display()
                );
            }
            if let Some(template) = hit {
                return Ok(template);
            }
            // Only one cold process should pay the boot; the losers wait.
            if let Some(lock) = cache.lock(key) {
                if let Some(template) = Self::cached_hit(cache, key, started) {
                    return Ok(template);
                }
                guard = Some(lock);
            } else if let Some(template) = Self::cached_wait(cache, key) {
                return Ok(template);
            }
        }

        let started = Instant::now();
        let mut rt = Runtime::standard_with(options).map_err(PoolError::Boot)?;
        let log = {
            let host = rt.host();
            let mut host = host.borrow_mut();
            install_recording_atms(&mut host)
        };

        rt.vm_mut()
            .call_static("android.os.Looper", "prepareMainLooper", "()V", &[])?;
        let thread = rt
            .vm_mut()
            .new_instance_ctor("android.app.ActivityThread", "()V", &[])?;
        rt.vm_mut().call_method_direct(
            thread,
            "attach",
            "(ZJ)V",
            &[JavaValue::Boolean(false), JavaValue::Long(1)],
        )?;
        let attach = first_attach(&log).ok_or_else(|| {
            PoolError::Boot(VmError::SnapshotPrecondition(
                "attach returned but no attachApplication was recorded".into(),
            ))
        })?;
        if !is_valid_app_thread(&attach) {
            return Err(PoolError::Boot(VmError::SnapshotPrecondition(
                "attachApplication arrived without a valid IApplicationThread binder".into(),
            )));
        }

        // Lift deterministic framework `<clinit>` into the snapshot so every
        // launch restoring this template skips it.
        let primed = prime_deterministic_classes(rt.vm_mut());

        let boot_ms = started.elapsed().as_secs_f64() * 1000.0;
        // Capture the binder id layout *before* the snapshot bytes leave the
        // process: the restored heap references these raw ids.
        let binder_layout: Vec<(u64, String, BinderKind)> =
            rt.host().borrow().export_binder_layout();
        // The ATMS attach record is per-process state the restored heap has
        // already executed; replay it into the fresh host on restore.
        let attach_seed: Option<(u64, i64, String)> = rt
            .host()
            .borrow_mut()
            .attached_process()
            .or_else(|| attach.app_thread.map(|b| (b.0, attach.start_seq, String::new())));
        let t = Instant::now();
        let snapshot = rt.vm().snapshot().map_err(PoolError::Boot)?;
        let snapshot_ms = t.elapsed().as_secs_f64() * 1000.0;
        let classes = rt.vm().classes.len();
        let activity_thread = rt
            .vm()
            .find_instance_of_class("Landroid/app/ActivityThread;")
            .ok_or_else(|| {
                PoolError::Boot(VmError::SnapshotPrecondition(
                    "ActivityThread instance missing from the snapshot".into(),
                ))
            })?;
        if let (Some(cache), Some(key)) = (cache, cache_key) {
            // Persist the fully linked BootImage alongside the snapshot so a
            // cold process clones the class tables and decoded bodies instead
            // of re-parsing + re-linking the boot classpath. A capture failure
            // only loses the acceleration, never the boot.
            // Diagnostic A/B escape: store an entry without the linked image so
            // the same binary can measure the pre-serialization relink path.
            let image = if std::env::var_os("ACOS_BOOT_CACHE_NO_IMAGE").is_some() {
                Vec::new()
            } else {
                rt.vm().boot_image_bytes().unwrap_or_default()
            };
            let payload = encode_boot_payload(
                &snapshot,
                &binder_layout,
                attach_seed.as_ref(),
                &image,
            );
            let _ = cache.store(key, &payload);
        }
        drop(guard);
        Ok(RuntimeTemplate {
            snapshot: Arc::from(snapshot),
            binder_layout: Arc::from(binder_layout),
            attach: attach_seed,
            activity_thread,
            boot_ms,
            snapshot_ms,
            classes,
            from_cache: false,
            primed: Arc::from(primed),
        })
    }

    /// Validate a loaded entry and rebuild the template without booting.
    fn cached_hit(
        cache: &BootImageCache,
        key: u64,
        started: Instant,
    ) -> Option<RuntimeTemplate> {
        match cache.load(key) {
            LoadResult::Hit(bytes) => match Self::from_cached(bytes, started) {
                Ok(t) => Some(t),
                Err(e) => {
                    if std::env::var_os("ACOS_BOOT_TRACE").is_some() {
                        eprintln!("[cache] from_cached failed: {e}");
                    }
                    None
                }
            },
            LoadResult::Invalid(_) => {
                // A rejected (corrupt/version-mismatched) entry is removed so
                // the next process does not retry it. The snapshot's own
                // validation is what ultimately decides.
                let _ = std::fs::remove_file(cache.entry_path(key));
                None
            }
            LoadResult::Miss => None,
        }
    }

    fn cached_wait(cache: &BootImageCache, key: u64) -> Option<RuntimeTemplate> {
        let started = Instant::now();
        match cache.wait_for(key, Duration::from_secs(30)) {
            LoadResult::Hit(bytes) => Self::from_cached(bytes, started).ok(),
            _ => None,
        }
    }

    /// Build a template from an on-disk snapshot.
    ///
    /// `Vm::restore` re-validates the header, boot fingerprint, property hash
    /// and class layout, so a stale or tampered cache entry cannot become a
    /// launch; the restored `Vm` is dropped after recovering the preserved
    /// `ActivityThread` handle.
    fn from_cached(bytes: Vec<u8>, started: Instant) -> Result<RuntimeTemplate, PoolError> {
        let (snapshot, binder_layout, attach, image) = decode_boot_payload(&bytes).map_err(|e| {
            PoolError::Restore(VmError::SnapshotFormat(format!(
                "boot cache payload: {e}"
            )))
        })?;
        let vm = if image.is_empty() {
            Vm::restore(&snapshot).map_err(PoolError::Restore)?
        } else {
            // `image` borrows `bytes`; both stay live only through this restore.
            Vm::restore_with_boot_image(&snapshot, image).map_err(PoolError::Restore)?
        };
        let activity_thread = vm
            .find_instance_of_class("Landroid/app/ActivityThread;")
            .ok_or_else(|| {
                PoolError::Restore(VmError::SnapshotPrecondition(
                    "ActivityThread instance missing from the cached snapshot".into(),
                ))
            })?;
        let classes = vm.classes.len();
        drop(vm);
        let load_ms = started.elapsed().as_secs_f64() * 1000.0;
        Ok(RuntimeTemplate {
            snapshot: Arc::from(snapshot),
            binder_layout: Arc::from(binder_layout),
            attach,
            activity_thread,
            boot_ms: 0.0,
            snapshot_ms: load_ms,
            classes,
            from_cache: true,
            primed: Arc::from(Vec::new()),
        })
    }

    /// Whether this template came from the on-disk cache (no framework boot).
    #[must_use]
    pub fn from_cache(&self) -> bool {
        self.from_cache
    }

    /// The deterministic classes primed into the snapshot (empty for a
    /// template loaded from an older on-disk entry).
    #[must_use]
    pub fn primed_classes(&self) -> &[String] {
        &self.primed
    }

    /// Correctness guard: whether `desc` is `Init` in a fresh restore of this
    /// template. Used by the priming fidelity test to prove that a restored
    /// template carries the deterministic `<clinit>` state, not merely the
    /// bytes that would let it re-run.
    pub fn restored_class_initialised(&self, desc: &str) -> Result<bool, PoolError> {
        let vm = Vm::restore(&self.snapshot).map_err(PoolError::Restore)?;
        Ok(match vm.find_class(desc) {
            Some(cid) => vm.classes[cid as usize].init == InitState::Init,
            None => false,
        })
    }

    /// The template boot wall time (paid once).
    #[must_use]
    pub fn boot_ms(&self) -> f64 {
        self.boot_ms
    }

    /// The snapshot capture wall time (paid once).
    #[must_use]
    pub fn snapshot_ms(&self) -> f64 {
        self.snapshot_ms
    }

    /// Snapshot size in bytes.
    #[must_use]
    pub fn snapshot_bytes(&self) -> usize {
        self.snapshot.len()
    }

    /// Classes loaded in the framework snapshot.
    #[must_use]
    pub fn classes(&self) -> usize {
        self.classes
    }

    /// Restore the framework `Vm` and wrap it in a fresh `Runtime`.
    ///
    /// The `Runtime` owns a fresh `ServiceHost`; the `Vm` state (including the
    /// `ActivityThread` and its `Looper`) comes from the snapshot. The app DEX
    /// is added by the caller before [`RuntimeLaunch::run`].
    pub fn restore_runtime(&self) -> Result<Runtime, PoolError> {
        let mut vm = Vm::restore(&self.snapshot).map_err(PoolError::Restore)?;
        // Looper/parcel/binder identity are process state the snapshot does not
        // carry; rebuild them before any framework bytecode runs (T-43).
        let seed = vm.reseed_after_restore();
        let mut host = ServiceHost::standard();
        host.import_binder_layout(&self.binder_layout);
        // Re-create the ATMS attach record the restored ActivityThread already
        // performed, so `finishAttachApplication` finds its process.
        if let Some((app_thread, seq, package)) = &self.attach {
            if let Err(e) = host.seed_attached_process(*app_thread, *seq, package) {
                // A missing activity service means the launch will surface the
                // same error later; do not fail the restore here.
                let _ = e;
            }
        }
        let rt = Runtime::with_vm(vm, host);
        for &(id, handle) in &seed.local_binders {
            acos_vm::natives::binder::seed_local_binder(id, handle);
        }
        for &(id, handle) in &seed.binder_proxies {
            acos_vm::natives::binder::seed_binder_proxy(id, handle);
        }
        Ok(rt)
    }

    /// The preserved `android.app.ActivityThread` handle in the restored `Vm`.
    #[must_use]
    pub fn activity_thread(&self) -> u32 {
        self.activity_thread
    }
}

/// Timings and outcome of one launch from a [`RuntimeTemplate`].
#[derive(Debug, Clone, Default)]
pub struct RuntimeLaunchReport {
    /// `Vm::restore` wall time.
    pub restore_ms: f64,
    /// `Runtime::with_vm` + fresh `ServiceHost` wall time.
    pub runtime_ms: f64,
    /// APK open + app DEX parse + `add_dex` wall time.
    pub app_load_ms: f64,
    /// `bindApplication` delivery wall time.
    pub bind_ms: f64,
    /// The `Application.onCreate` interpreter phase wall time.
    pub app_phase_ms: f64,
    /// The `ClientTransaction` delivery wall time.
    pub transaction_ms: f64,
    /// The `Activity.onCreate` interpreter phase wall time.
    pub transaction_phase_ms: f64,
    /// `quitSafely` + final interpreter phase wall time.
    pub quit_phase_ms: f64,
    /// Total launch wall time from restore to the last interpreter phase.
    pub total_ms: f64,
    /// `Application.attach`/`onCreate` ran (`mInitialApplication != null`).
    pub app_created: bool,
    /// `Activity.onCreate` completed (`ActivityClientRecord.activity != null`).
    pub transaction_delivered: bool,
    /// The looper was asked to quit and returned.
    pub looper_quit: bool,
    /// The first structured failure, if any.
    pub error: Option<String>,
}

impl Runtime {
    /// Build a `Runtime` around an already-restored `Vm` and a fresh service host.
    ///
    /// This is the restore half of [`RuntimeTemplate`]; it mirrors
    /// `Runtime::with_host` but takes the `Vm` instead of constructing a new one.
    pub fn with_vm(vm: Vm, host: ServiceHost) -> Runtime {
        let host = Rc::new(RefCell::new(host));
        let context = host
            .borrow()
            .binder_for_descriptor("android.os.IServiceManager")
            .unwrap_or(BinderId(0));
        // Match the kernel pid so `Binder.getCallingPid()` and
        // `Process.myPid()` agree (the same-process short-circuits in the
        // framework's permission checks rely on it).
        let pid = std::process::id() as i32;
        set_process_identity(FIXTURE_UID, pid);
        install_transport(Box::new(ServiceTransport::new(
            Rc::clone(&host),
            context,
            FIXTURE_UID,
            pid,
        )));
        crate::input_java::install();
        Runtime {
            vm,
            host,
            native: None,
        }
    }
}

/// Run the app launch on an already-framework-booted `Runtime`.
///
/// The caller has restored the framework `Vm` (or booted it); this loads the
/// app DEX, stages the `LoadedApk`, and drives the same real framework bytecode
/// as `Runtime::boot_and_run_lifecycle`, minus the framework boot/attach that
/// the template already did: `bindApplication`, `Application.onCreate`, the
/// launch `ClientTransaction`, `Activity.onCreate`/`onResume`, then
/// `quitSafely`.
pub fn launch_from_template(
    rt: &mut Runtime,
    apk: &std::path::Path,
    activity_thread: u32,
) -> RuntimeLaunchReport {
    let started = Instant::now();
    let mut report = RuntimeLaunchReport::default();
    match launch_inner(rt, apk, activity_thread, &mut report) {
        Ok(()) => {}
        Err(e) => report.error = Some(e),
    }
    report.total_ms = started.elapsed().as_secs_f64() * 1000.0;
    report
}

/// Run the launch on a `Runtime` that already has the app DEX loaded.
///
/// This is the half used by the `before` profile: the boot probe has already
/// loaded the app DEX as part of the measured boot path. It skips the app DEX
/// load and drives the real `bindApplication` + `ClientTransaction` + looper.
pub fn launch_from_template_loaded(
    rt: &mut Runtime,
    apk: &std::path::Path,
    activity_thread: u32,
    app: &acos_services::pms::AppInfo,
) -> RuntimeLaunchReport {
    let started = Instant::now();
    let mut report = RuntimeLaunchReport::default();
    match launch_loaded_inner(rt, apk, activity_thread, app, &mut report) {
        Ok(()) => {}
        Err(e) => report.error = Some(e),
    }
    report.total_ms = started.elapsed().as_secs_f64() * 1000.0;
    report
}

fn launch_inner(
    rt: &mut Runtime,
    apk: &std::path::Path,
    activity_thread: u32,
    report: &mut RuntimeLaunchReport,
) -> Result<(), String> {
    let opened = acos_apk::Apk::open(apk).map_err(|e| format!("open apk: {e}"))?;
    let raw = opened
        .dex_files
        .first()
        .ok_or_else(|| "apk contains no classes.dex".to_string())?;
    let dex = DexFile::parse(raw.bytes.clone()).map_err(|e| format!("dex: {e}"))?;
    let t = Instant::now();
    rt.vm_mut().add_dex(dex);
    report.app_load_ms = t.elapsed().as_secs_f64() * 1000.0;

    let manifest = opened.manifest().map_err(|e| format!("manifest: {e}"))?;
    let mut app = acos_services::pms::AppInfo::from_manifest(
        &manifest,
        apk,
        acos_services::pms::FIXTURE_UID,
    );
    crate::apk_loader::stage(rt.vm(), apk, &mut app)
        .map_err(|e| format!("stage apk: {e}"))?;
    launch_loaded_inner(rt, apk, activity_thread, &app, report)
}

fn launch_loaded_inner(
    rt: &mut Runtime,
    apk: &std::path::Path,
    activity_thread: u32,
    app: &acos_services::pms::AppInfo,
    report: &mut RuntimeLaunchReport,
) -> Result<(), String> {
    rt.vm_mut()
        .push_static_call("android.os.Looper", "loop", "()V", &[])
        .map_err(|e| format!("Looper.loop: {e}"))?;

    let t = Instant::now();
    let app_thread =
        crate::bind_app::deliver_bind_application(rt.vm_mut(), activity_thread, app, &[])
            .map_err(|e| format!("bindApplication: {e}"))?;
    report.bind_ms = t.elapsed().as_secs_f64() * 1000.0;

    let t = Instant::now();
    run_phase(rt.vm_mut())?;
    report.app_phase_ms = t.elapsed().as_secs_f64() * 1000.0;
    report.app_created = initial_application_non_null(rt.vm_mut(), activity_thread);

    let t = Instant::now();
    let tx = crate::client_transaction::deliver_launch(rt.vm_mut(), app_thread, app, apk)
        .map_err(|e| format!("client transaction: {e}"))?;
    report.transaction_ms = t.elapsed().as_secs_f64() * 1000.0;

    let t = Instant::now();
    run_phase(rt.vm_mut())?;
    report.transaction_phase_ms = t.elapsed().as_secs_f64() * 1000.0;
    report.transaction_delivered = activity_created(rt.vm_mut(), activity_thread, tx.token);

    let t = Instant::now();
    quit_main_looper(rt.vm_mut())?;
    run_phase(rt.vm_mut())?;
    report.quit_phase_ms = t.elapsed().as_secs_f64() * 1000.0;
    report.looper_quit = true;
    Ok(())
}

fn seed_device_environment() {
    if std::env::var_os("ANDROID_I18N_ROOT").is_none() {
        std::env::set_var("ANDROID_I18N_ROOT", "/system/apex/com.android.i18n");
    }
    if std::env::var_os("ANDROID_TZDATA_ROOT").is_none() {
        std::env::set_var("ANDROID_TZDATA_ROOT", "/apex/com.android.tzdata");
    }
}

fn run_phase(vm: &mut Vm) -> Result<(), String> {
    acos_vm::interp::run(vm).map_err(|e| format!("interp: {e}"))?;
    if vm.pending_exception != acos_vm::heap::NULL {
        let h = vm.pending_exception;
        vm.pending_exception = acos_vm::heap::NULL;
        let class = vm
            .class_of_object(h)
            .map(|c| vm.class_desc_of_id(c))
            .unwrap_or_else(|| "java.lang.Throwable".to_string());
        let message = vm.throwable_message(h);
        return Err(match message {
            Some(m) => format!("uncaught {class}: {m}"),
            None => format!("uncaught {class}"),
        });
    }
    Ok(())
}

fn initial_application_non_null(vm: &mut Vm, thread: u32) -> bool {
    matches!(
        vm.get_field(thread, "mInitialApplication"),
        Ok(JavaValue::Object(h)) if h != acos_vm::heap::NULL
    )
}

fn activity_created(vm: &mut Vm, thread: u32, token: u32) -> bool {
    let record = match vm.call_method(
        thread,
        "getActivityClient",
        "(Landroid/os/IBinder;)Landroid/app/ActivityThread$ActivityClientRecord;",
        &[JavaValue::Object(token)],
    ) {
        Ok(JavaValue::Object(h)) if h != acos_vm::heap::NULL => h,
        _ => return false,
    };
    matches!(
        vm.get_field(record, "activity"),
        Ok(JavaValue::Object(h)) if h != acos_vm::heap::NULL
    )
}

fn quit_main_looper(vm: &mut Vm) -> Result<(), String> {
    let parked = vm
        .threads
        .first()
        .is_some_and(|t| t.status == acos_vm::vm::ThreadStatus::Parked);
    if !parked {
        return Ok(());
    }
    let looper = match vm.call_static(
        "android.os.Looper",
        "myLooper",
        "()Landroid/os/Looper;",
        &[],
    ) {
        Ok(JavaValue::Object(h)) if h != acos_vm::heap::NULL => h,
        _ => return Ok(()),
    };
    vm.call_method(looper, "quitSafely", "()V", &[])
        .map_err(|e| format!("quitSafely: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod payload_tests {
    use super::*;

    /// Build a minimal valid frame: no binder entries, no attach record, one
    /// snapshot byte and one image byte.
    fn valid_frame() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&BOOT_PAYLOAD_MAGIC.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.push(0); // attach marker: none
        out.extend_from_slice(&1u64.to_le_bytes());
        out.push(b'S');
        out.extend_from_slice(&1u64.to_le_bytes());
        out.push(b'I');
        out
    }

    #[test]
    fn valid_frame_round_trips() {
        let frame = valid_frame();
        let (snapshot, layout, attach, image) =
            decode_boot_payload(&frame).expect("valid frame");
        assert_eq!(snapshot, b"S");
        assert!(layout.is_empty());
        assert!(attach.is_none());
        assert_eq!(image, &b"I"[..]);
    }

    #[test]
    fn crafted_lengths_are_errors_not_panics() {
        // Every crafted length must return `Err`, never wrap or slice-panic.
        // The snapshot length sits at a known offset in this frame; find it by
        // rebuilding the prefix so the mutation is unambiguous.
        let prefix_len = 8 + 1; // magic+count, attach marker
        let snap_len_off = prefix_len;
        let img_len_off = prefix_len + 8 + 1;

        for &(off, name) in &[(snap_len_off, "snap_len"), (img_len_off, "img_len")] {
            for &value in &[u64::MAX, u64::MAX - 7, u32::MAX as u64, 1u64 << 62] {
                let mut frame = valid_frame();
                frame[off..off + 8].copy_from_slice(&value.to_le_bytes());
                let err = decode_boot_payload(&frame)
                    .expect_err(&format!("crafted {name}={value} must be rejected"));
                assert!(!err.is_empty());
            }
        }

        // A crafted binder descriptor length must also be rejected cleanly.
        let mut frame = Vec::new();
        frame.extend_from_slice(&BOOT_PAYLOAD_MAGIC.to_le_bytes());
        frame.extend_from_slice(&1u32.to_le_bytes());
        frame.extend_from_slice(&7u64.to_le_bytes());
        frame.extend_from_slice(&u32::MAX.to_le_bytes());
        let err = decode_boot_payload(&frame).expect_err("descriptor length must be rejected");
        assert!(!err.is_empty());
    }

    #[test]
    fn truncated_frames_are_errors() {
        for cut in 0..valid_frame().len() {
            let frame = &valid_frame()[..cut];
            assert!(
                decode_boot_payload(frame).is_err(),
                "truncated frame of {cut} bytes must be rejected"
            );
        }
    }
}
