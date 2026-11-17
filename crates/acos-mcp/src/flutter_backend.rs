//! W142-A/W149-A: Flutter product sessions on a dedicated worker thread.
//!
//! A [`FlutterSession`] must be created and driven on **one stable thread**:
//! the engine caches its platform/UI thread identity when `nativeAttach` runs
//! and its `ALooper` must be polled by that thread. The MCP server dispatches
//! every `tools/call` on a fresh thread, so holding a `FlutterSession` in the
//! session table directly meant `launch` and `observe` landed on different
//! threads and the engine's looper thread was never driven (observe timed out).
//!
//! This module gives each Flutter session a long-lived worker thread:
//!
//! * the worker is spawned with an explicit **64 MiB** stack, so
//!   `pthread_getattr_np`/`RLIMIT_STACK` bounds are well-defined for the Dart
//!   VM even when the VM is created on it;
//! * `FlutterSession::launch` and every later command run **on that thread**;
//! * the parent ([`FlutterSessions`]) marshals commands over a channel and waits
//!   with a bounded timeout, so a wedged pump is reported as `TIMEOUT` instead
//!   of blocking the server forever.
//!
//! Nothing here enters confinement; the caller (`SessionBackend`) either
//! already runs inside the ADR-0022 sandbox (the confined child) or re-execs one.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Mutex;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use acos_jni::flutter::SemanticsNode;
use acos_jni::{FlutterSession, FlutterSessionError, FlutterSessionOptions};

/// Explicit worker stack size (64 MiB). A defined mapping keeps
/// `pthread_getattr_np` bounds usable for the Dart VM.
const WORKER_STACK_BYTES: usize = 64 * 1024 * 1024;

/// Default wall-clock bound for a single marshalled command.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(180);

/// A frame captured by a Flutter session.
#[derive(Debug, Clone)]
pub struct FlutterObserve {
    /// Sink frame sequence.
    pub seq: u64,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Encoded PNG, when requested.
    pub png: Option<Vec<u8>>,
    /// Number of semantics nodes currently known.
    pub semantics: usize,
    /// Frames observed so far.
    pub frames: usize,
}

/// A Flutter session's status snapshot.
#[derive(Debug, Clone)]
pub struct FlutterStatus {
    /// The APK path.
    pub apk: PathBuf,
    /// Frames observed so far.
    pub frames: usize,
    /// Semantics nodes observed so far.
    pub semantics: usize,
    /// Wall-clock since launch.
    pub elapsed_ms: u128,
    /// Launch wall-clock.
    pub launch_ms: u128,
    /// Engine import/relocation diagnostics.
    pub engine: (usize, usize, usize),
}

/// Whether `apk` is structurally a Flutter APK (engine + Flutter assets).
#[must_use]
pub fn is_flutter(apk: &Path) -> bool {
    matches!(acos_jni::flutter::detect(apk), Ok(Some(_)))
}

/// Resolve a repo-relative asset through the shared rule: `$ACOS_HOME/<rel>`
/// when installed, else the legacy checkout lookup (`crate::paths`).
fn repo_relative(rel: &str) -> PathBuf {
    crate::paths::runtime_path(rel)
}

/// Directory holding the extracted engine, overridable by env.
#[must_use]
pub fn default_lib_dir() -> PathBuf {
    std::env::var_os("ACOS_FLUTTER_LIB_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_relative(".cache/flutter-1/lib-x86_64"))
}

/// Default Java driver dex, overridable by env.
fn default_driver_dex() -> PathBuf {
    std::env::var_os("ACOS_FLUTTER_DRIVER_DEX")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_relative("crates/acos-jni/tests/data/flutter-probe.dex"))
}

/// Session options for a product launch (geometry from the reference device).
#[must_use]
pub fn flush_options(apk: &Path, scratch: PathBuf) -> FlutterSessionOptions {
    let lib_dir = default_lib_dir();
    FlutterSessionOptions {
        // W189-A: a real APK is always authoritative — stage its own
        // `lib/<abi>/libapp.so`, never the shared engine dir's snapshot (which
        // may be a different app's, or the W93-A instrumented probe). The one
        // exemption is the raw probe fixture itself (`flutter-hello.apk`), which
        // has no semantics in its shipped snapshot and is exercised through the
        // instrumented `libapp_probe.so` when it is present.
        prefer_probe_snapshot: apk.file_stem().and_then(|s| s.to_str()) == Some("flutter-hello")
            && lib_dir.join("libapp_probe.so").is_file(),
        lib_dir,
        scratch,
        driver_dex: Some(default_driver_dex()),
        ..FlutterSessionOptions::default()
    }
}

/// The operations the worker thread can perform on a live session.
///
/// The trait exists so the marshalling/thread-affinity contract can be unit
/// tested with a fake driver, without a real engine.
pub trait FlutterDriver: Send {
    /// The status snapshot.
    fn status(&mut self) -> FlutterStatus;
    /// Pump a frame (and optionally encode its PNG) and report it.
    fn observe(&mut self, with_png: bool) -> Result<FlutterObserve, FlutterSessionError>;
    /// The current semantic tree.
    fn semantics(&mut self) -> Result<Vec<SemanticsNode>, FlutterSessionError>;
    /// Tap physical `(x, y)`.
    fn tap(&mut self, x: f64, y: f64) -> Result<(), FlutterSessionError>;
    /// Tap the centre of the node labelled `label`.
    fn tap_text(&mut self, label: &str) -> Result<[f64; 2], FlutterSessionError>;
    /// The driver's captured stdout/stderr lines.
    fn logs(&mut self) -> Vec<String>;
}

/// A [`FlutterDriver`] over the real [`FlutterSession`].
struct RealDriver {
    session: FlutterSession,
    apk: PathBuf,
    started: Instant,
    launch_ms: u128,
}

impl RealDriver {
    fn wrap(session: FlutterSession, launch_ms: u128) -> RealDriver {
        let apk = session.apk().to_path_buf();
        RealDriver {
            session,
            apk,
            started: Instant::now(),
            launch_ms,
        }
    }
}

impl FlutterDriver for RealDriver {
    fn status(&mut self) -> FlutterStatus {
        FlutterStatus {
            apk: self.apk.clone(),
            frames: self.session.frame_count(),
            semantics: self.session.semantics().map(|s| s.len()).unwrap_or(0),
            elapsed_ms: self.started.elapsed().as_millis(),
            launch_ms: self.launch_ms,
            engine: self.session.engine_stats(),
        }
    }

    fn observe(&mut self, with_png: bool) -> Result<FlutterObserve, FlutterSessionError> {
        if self.session.frame_count() == 0 {
            self.session
                .pump_until_frames(1, self.session.opts().max_pump_rounds)?;
        }
        let (seq, width, height, png) = match self.session.frame() {
            Some(frame) => {
                let png = if with_png { Some(frame.png()?) } else { None };
                (frame.seq, frame.width, frame.height, png)
            }
            None => (0, 0, 0, None),
        };
        let semantics = self.session.semantics()?.len();
        Ok(FlutterObserve {
            seq,
            width,
            height,
            png,
            semantics,
            frames: self.session.frame_count(),
        })
    }

    fn semantics(&mut self) -> Result<Vec<SemanticsNode>, FlutterSessionError> {
        if self.session.frame_count() == 0 {
            let _ = self
                .session
                .pump_until_frames(1, self.session.opts().max_pump_rounds);
        }
        let mut nodes = self.session.semantics()?;
        // `setSemanticsEnabled` reaches Dart on a later frame; the app then calls
        // `updateSemantics`. Pump a bounded number of rounds until the stream is
        // non-empty so `observe --tree` does not race the first update.
        if nodes.is_empty() {
            for _ in 0..self.session.opts().max_pump_rounds.min(120) {
                let _ = self.session.pump();
                nodes = self.session.semantics()?;
                if !nodes.is_empty() {
                    break;
                }
            }
        }
        Ok(nodes)
    }

    fn tap(&mut self, x: f64, y: f64) -> Result<(), FlutterSessionError> {
        self.session.tap(x, y)
    }

    fn tap_text(&mut self, label: &str) -> Result<[f64; 2], FlutterSessionError> {
        self.session.tap_text(label)
    }

    fn logs(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        for line in self.session.driver_stdout().lines() {
            out.push(format!("stdout: {line}"));
        }
        for line in self.session.driver_stderr().lines() {
            out.push(format!("stderr: {line}"));
        }
        if let Some(err) = self.session.driver_error() {
            out.push(format!("error: {err}"));
        }
        // W189-A diagnostics: whether the engine was asked for semantics and
        // how many updates it delivered (a real framework app only builds a
        // tree once `setSemanticsEnabled(true)` reaches Dart).
        out.push(format!(
            "diag: semantics_requested={} seq={} java={}",
            self.session.semantics_requested(),
            self.session.semantics_seq(),
            self.session.semantics_debug()
        ));
        out
    }
}

/// A command marshalled to the session's worker thread.
enum FlutterCommand {
    Observe {
        with_png: bool,
        reply: Sender<Result<FlutterObserve, FlutterSessionError>>,
    },
    Semantics {
        reply: Sender<Result<Vec<SemanticsNode>, FlutterSessionError>>,
    },
    Tap {
        x: f64,
        y: f64,
        reply: Sender<Result<(), FlutterSessionError>>,
    },
    TapText {
        label: String,
        reply: Sender<Result<[f64; 2], FlutterSessionError>>,
    },
    Status {
        reply: Sender<FlutterStatus>,
    },
    Logs {
        reply: Sender<Vec<String>>,
    },
    Shutdown {
        reply: Sender<()>,
    },
}

/// The worker loop: run every command on this thread, in order.
fn worker_loop(rx: Receiver<FlutterCommand>, mut driver: Box<dyn FlutterDriver>) {
    while let Ok(cmd) = rx.recv() {
        match cmd {
            FlutterCommand::Observe { with_png, reply } => {
                let _ = reply.send(driver.observe(with_png));
            }
            FlutterCommand::Semantics { reply } => {
                let _ = reply.send(driver.semantics());
            }
            FlutterCommand::Tap { x, y, reply } => {
                let _ = reply.send(driver.tap(x, y));
            }
            FlutterCommand::TapText { label, reply } => {
                let _ = reply.send(driver.tap_text(&label));
            }
            FlutterCommand::Status { reply } => {
                let _ = reply.send(driver.status());
            }
            FlutterCommand::Logs { reply } => {
                let _ = reply.send(driver.logs());
            }
            FlutterCommand::Shutdown { reply } => {
                let _ = reply.send(());
                break;
            }
        }
    }
}

/// A live Flutter session: commands for the worker thread plus the launch
/// metadata read by the parent.
struct FlutterWorker {
    tx: Sender<FlutterCommand>,
    join: Option<JoinHandle<()>>,
    /// Wall-clock bound for one marshalled command.
    timeout: Duration,
}

impl FlutterWorker {
    fn send<T>(
        &self,
        make: impl FnOnce(Sender<T>) -> FlutterCommand,
    ) -> Result<T, FlutterSessionError> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.tx
            .send(make(reply_tx))
            .map_err(|_| FlutterSessionError::Setup {
                stage: "flutter-worker",
                detail: "Flutter session worker is gone".to_string(),
            })?;
        match reply_rx.recv_timeout(self.timeout) {
            Ok(value) => Ok(value),
            Err(RecvTimeoutError::Timeout) => Err(FlutterSessionError::Budget {
                kind: "command",
                detail: format!("no reply within {:?}", self.timeout),
            }),
            Err(RecvTimeoutError::Disconnected) => Err(FlutterSessionError::Setup {
                stage: "flutter-worker",
                detail: "Flutter session worker dropped the reply".to_string(),
            }),
        }
    }

    fn observe(&self, with_png: bool) -> Result<FlutterObserve, FlutterSessionError> {
        self.send(|reply| FlutterCommand::Observe { with_png, reply })?
    }

    fn semantics(&self) -> Result<Vec<SemanticsNode>, FlutterSessionError> {
        self.send(|reply| FlutterCommand::Semantics { reply })?
    }

    fn tap(&self, x: f64, y: f64) -> Result<(), FlutterSessionError> {
        self.send(|reply| FlutterCommand::Tap { x, y, reply })?
    }

    fn tap_text(&self, label: &str) -> Result<[f64; 2], FlutterSessionError> {
        let label = label.to_string();
        self.send(|reply| FlutterCommand::TapText { label, reply })?
    }

    fn status(&self) -> Result<FlutterStatus, FlutterSessionError> {
        self.send(|reply| FlutterCommand::Status { reply })
    }

    fn logs(&self) -> Vec<String> {
        self.send(|reply| FlutterCommand::Logs { reply })
            .unwrap_or_default()
    }

    /// Ask the worker to drop the session, with a bounded wait. A wedged engine
    /// is detached rather than joined (the process is exiting or the caller
    /// explicitly destroyed the session).
    fn shutdown(mut self) {
        let (reply_tx, reply_rx) = mpsc::channel();
        if self
            .tx
            .send(FlutterCommand::Shutdown { reply: reply_tx })
            .is_ok()
        {
            let _ = reply_rx.recv_timeout(Duration::from_secs(10));
        }
        if let Some(join) = self.join.take() {
            if join.is_finished() {
                let _ = join.join();
            } else {
                // Never block the caller on a wedged engine; detach the thread.
                drop(join);
            }
        }
    }
}

/// Spawn a worker for an already-live driver (tests only).
#[cfg(test)]
fn spawn_worker(
    driver: Box<dyn FlutterDriver>,
) -> Result<(Sender<FlutterCommand>, JoinHandle<()>), FlutterSessionError> {
    let (tx, rx) = mpsc::channel();
    let join = thread::Builder::new()
        .name("acos-flutter-session".to_string())
        .stack_size(WORKER_STACK_BYTES)
        .spawn(move || worker_loop(rx, driver))
        .map_err(|e| FlutterSessionError::Setup {
            stage: "flutter-worker-spawn",
            detail: e.to_string(),
        })?;
    Ok((tx, join))
}

/// Spawn the worker and run `FlutterSession::launch` **on it**, returning the
/// parent-side [`FlutterWorker`] once launch succeeds.
fn spawn_real_worker(
    apk: PathBuf,
    opts: FlutterSessionOptions,
) -> Result<FlutterWorker, FlutterSessionError> {
    let (tx, rx) = mpsc::channel();
    let (ready_tx, ready_rx) = mpsc::channel::<Result<u128, FlutterSessionError>>();
    let timeout = opts.time_budget.max(COMMAND_TIMEOUT) + Duration::from_secs(30);
    let started = Instant::now();
    let join = thread::Builder::new()
        .name("acos-flutter-session".to_string())
        .stack_size(WORKER_STACK_BYTES)
        .spawn(move || match FlutterSession::launch(&apk, opts) {
            Ok(session) => {
                let launch_ms = started.elapsed().as_millis();
                let _ = ready_tx.send(Ok(launch_ms));
                worker_loop(rx, Box::new(RealDriver::wrap(session, launch_ms)));
            }
            Err(e) => {
                let _ = ready_tx.send(Err(e));
            }
        })
        .map_err(|e| FlutterSessionError::Setup {
            stage: "flutter-worker-spawn",
            detail: e.to_string(),
        })?;
    match ready_rx.recv_timeout(timeout) {
        Ok(Ok(_launch_ms)) => Ok(FlutterWorker {
            tx,
            join: Some(join),
            timeout,
        }),
        Ok(Err(e)) => {
            let _ = join.join();
            Err(e)
        }
        Err(RecvTimeoutError::Timeout) => Err(FlutterSessionError::Budget {
            kind: "launch",
            detail: "FlutterSession::launch exceeded its bound".to_string(),
        }),
        Err(RecvTimeoutError::Disconnected) => Err(FlutterSessionError::Setup {
            stage: "flutter-launch",
            detail: "launch worker exited without a reply".to_string(),
        }),
    }
}

/// The confined child's Flutter session table.
#[derive(Default)]
pub struct FlutterSessions {
    held: Mutex<HashMap<String, FlutterWorker>>,
}

impl FlutterSessions {
    /// The ids of every held Flutter session.
    #[must_use]
    pub fn ids(&self) -> Vec<String> {
        self.held
            .lock()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Number of held Flutter sessions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.held.lock().map(|m| m.len()).unwrap_or(0)
    }

    /// Whether no session is held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether `id` is held.
    #[must_use]
    pub fn contains(&self, id: &str) -> bool {
        self.held
            .lock()
            .map(|m| m.contains_key(id))
            .unwrap_or(false)
    }

    /// Drop a held session, returning whether one existed.
    pub fn remove(&self, id: &str) -> bool {
        let worker = self.held.lock().map(|mut m| m.remove(id)).unwrap_or(None);
        match worker {
            Some(worker) => {
                worker.shutdown();
                true
            }
            None => false,
        }
    }

    /// Launch a Flutter engine for `apk` on a dedicated worker thread. The
    /// caller must already be confined (fail closed: `FlutterSession` maps the
    /// engine with `require_confinement = true`).
    pub fn launch(
        &self,
        id: &str,
        apk: &Path,
        scratch: PathBuf,
    ) -> Result<(), FlutterSessionError> {
        let _ = self.remove(id);
        let worker = spawn_real_worker(apk.to_path_buf(), flush_options(apk, scratch))?;
        if let Ok(mut map) = self.held.lock() {
            map.insert(id.to_string(), worker);
        }
        Ok(())
    }

    /// Pump until at least one frame is present, then return the newest frame
    /// (and optionally its encoded PNG).
    pub fn observe(&self, id: &str, with_png: bool) -> Result<FlutterObserve, FlutterSessionError> {
        self.worker(id)?.observe(with_png)
    }

    /// The current semantic tree.
    pub fn semantics(&self, id: &str) -> Result<Vec<SemanticsNode>, FlutterSessionError> {
        self.worker(id)?.semantics()
    }

    /// Tap physical `(x, y)`.
    pub fn tap(&self, id: &str, x: f64, y: f64) -> Result<(), FlutterSessionError> {
        self.worker(id)?.tap(x, y)
    }

    /// Tap the centre of the node labelled `label`.
    pub fn tap_text(&self, id: &str, label: &str) -> Result<[f64; 2], FlutterSessionError> {
        self.worker(id)?.tap_text(label)
    }

    /// Status snapshot.
    pub fn status(&self, id: &str) -> Result<FlutterStatus, FlutterSessionError> {
        self.worker(id)?.status()
    }

    /// The driver's captured stdout/stderr lines.
    pub fn logs(&self, id: &str) -> Vec<String> {
        self.worker(id).map(|w| w.logs()).unwrap_or_default()
    }

    fn worker(&self, id: &str) -> Result<WorkerGuard<'_>, FlutterSessionError> {
        // A guard keeps the map lock held for the duration of the command so
        // two tools cannot interleave on one session, and so a concurrent
        // destroy cannot pull the worker out from under an in-flight call.
        let guard = self.held.lock().map_err(|_| FlutterSessionError::Setup {
            stage: "flutter-lock",
            detail: "session table poisoned".to_string(),
        })?;
        if !guard.contains_key(id) {
            return Err(FlutterSessionError::Setup {
                stage: "flutter-session",
                detail: format!("no Flutter session {id:?}"),
            });
        }
        Ok(WorkerGuard {
            guard,
            id: id.to_string(),
        })
    }
}

/// A borrowed worker under the sessions lock.
struct WorkerGuard<'a> {
    guard: std::sync::MutexGuard<'a, HashMap<String, FlutterWorker>>,
    id: String,
}

impl std::ops::Deref for WorkerGuard<'_> {
    type Target = FlutterWorker;
    fn deref(&self) -> &FlutterWorker {
        self.guard
            .get(&self.id)
            .expect("checked in FlutterSessions::worker")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// A fake driver that records the thread it ran on and can be made slow.
    struct FakeDriver {
        threads: Arc<Mutex<Vec<(String, thread::ThreadId)>>>,
        delay: Duration,
        calls: Arc<AtomicUsize>,
    }

    impl FakeDriver {
        fn record(&self, op: &str) {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.threads
                .lock()
                .unwrap()
                .push((op.to_string(), thread::current().id()));
            if !self.delay.is_zero() {
                thread::sleep(self.delay);
            }
        }
    }

    impl FlutterDriver for FakeDriver {
        fn status(&mut self) -> FlutterStatus {
            self.record("status");
            FlutterStatus {
                apk: PathBuf::from("/fake.apk"),
                frames: 1,
                semantics: 0,
                elapsed_ms: 0,
                launch_ms: 0,
                engine: (0, 0, 0),
            }
        }
        fn observe(&mut self, _with_png: bool) -> Result<FlutterObserve, FlutterSessionError> {
            self.record("observe");
            Ok(FlutterObserve {
                seq: 1,
                width: 1080,
                height: 2400,
                png: None,
                semantics: 0,
                frames: 1,
            })
        }
        fn semantics(&mut self) -> Result<Vec<SemanticsNode>, FlutterSessionError> {
            self.record("semantics");
            Ok(Vec::new())
        }
        fn tap(&mut self, _x: f64, _y: f64) -> Result<(), FlutterSessionError> {
            self.record("tap");
            Ok(())
        }
        fn tap_text(&mut self, _label: &str) -> Result<[f64; 2], FlutterSessionError> {
            self.record("tap_text");
            Ok([0.0, 0.0])
        }
        fn logs(&mut self) -> Vec<String> {
            self.record("logs");
            Vec::new()
        }
    }

    type ThreadLog = Arc<Mutex<Vec<(String, thread::ThreadId)>>>;

    fn fake_worker(delay: Duration) -> (FlutterWorker, ThreadLog, Arc<AtomicUsize>) {
        let threads = Arc::new(Mutex::new(Vec::new()));
        let calls = Arc::new(AtomicUsize::new(0));
        let driver = FakeDriver {
            threads: Arc::clone(&threads),
            delay,
            calls: Arc::clone(&calls),
        };
        let (tx, join) = spawn_worker(Box::new(driver)).expect("spawn fake worker");
        (
            FlutterWorker {
                tx,
                join: Some(join),
                timeout: COMMAND_TIMEOUT,
            },
            threads,
            calls,
        )
    }

    #[test]
    fn all_commands_run_on_one_stable_worker_thread() {
        let main_id = thread::current().id();
        let (worker, threads, calls) = fake_worker(Duration::ZERO);
        // Drive several different commands, some from fresh caller threads.
        let callers: Vec<_> = (0..3).map(|_| thread::spawn(|| ())).collect();
        for handle in callers {
            handle.join().unwrap();
        }
        worker.observe(false).unwrap();
        worker.semantics().unwrap();
        worker.tap(1.0, 2.0).unwrap();
        worker.tap_text("Increment").unwrap();
        worker.status().unwrap();
        assert_eq!(worker.logs().len(), 0);
        worker.shutdown();

        let seen = threads.lock().unwrap().clone();
        assert_eq!(seen.len(), 6, "all commands recorded: {seen:?}");
        let ids: std::collections::HashSet<_> = seen.iter().map(|(_, id)| *id).collect();
        assert_eq!(
            ids.len(),
            1,
            "every command must run on one thread; got {ids:?}"
        );
        assert_ne!(
            ids.iter().next().copied(),
            Some(main_id),
            "worker must not be the caller"
        );
        assert_eq!(calls.load(Ordering::SeqCst), seen.len());
    }

    #[test]
    fn a_slow_command_times_out_without_hanging_the_caller() {
        let (mut worker, _threads, _calls) = fake_worker(Duration::from_millis(500));
        worker.timeout = Duration::from_millis(50);
        let start = Instant::now();
        let err = worker.observe(false).unwrap_err();
        assert!(
            start.elapsed() < Duration::from_millis(400),
            "must not block"
        );
        assert_eq!(err.code(), "TIMEOUT");
        // The worker thread itself is still alive; destroying it is bounded.
        worker.shutdown();
    }

    #[test]
    fn unknown_session_is_a_setup_error() {
        let sessions = FlutterSessions::default();
        let err = sessions.observe("nope", false).unwrap_err();
        assert_eq!(err.code(), "UNSUPPORTED_BACKEND");
        assert!(!sessions.contains("nope"));
        assert!(!sessions.remove("nope"));
        assert_eq!(sessions.logs("nope").len(), 0);
    }
}
