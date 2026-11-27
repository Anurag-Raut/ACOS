//! W162-A: React Native product sessions on a dedicated worker thread.
//!
//! Mirrors [`crate::flutter_backend`]: an [`RnSession`] owns a `Vm` and a JNI
//! runtime whose raw owner pointer is pinned by `Box`, and the RN Paper bridge
//! is single-threaded (UI, JS and native modules share the main looper). The MCP
//! server dispatches every `tools/call` on a fresh thread, so a session held
//! directly in the table would be driven from a different thread each call.
//!
//! Each RN session therefore gets a long-lived worker thread (64 MiB stack) that
//! creates and drives the [`RnSession`]; the parent ([`RnSessions`]) marshals
//! commands over a channel with a bounded timeout. Nothing here enters
//! confinement: the caller ([`crate::session_backend::SessionBackend`]) already
//! runs inside the ADR-0022 confined child.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Mutex;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use acos_jni::react_native::RnTree;
use acos_runtime::rn_session::CoreLib;
use acos_runtime::{RnSession, RnSessionError, RnSessionOptions};

/// Explicit worker stack size (64 MiB), matching the Flutter worker.
const WORKER_STACK_BYTES: usize = 64 * 1024 * 1024;

/// Default wall-clock bound for a single marshalled command.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(180);

/// The host Android ABI name (the APK directory spelling).
#[must_use]
pub fn host_abi() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64-v8a",
        "arm" => "armeabi-v7a",
        "x86" => "x86",
        _ => "x86_64",
    }
}

/// Whether `apk` is structurally a React Native APK (runtime + JS bundle).
#[must_use]
pub fn is_react_native(apk: &Path) -> bool {
    matches!(acos_jni::react_native::detect(apk), Ok(Some(_)))
}

/// Resolve a repo-relative asset through the shared rule: `$ACOS_HOME/<rel>`
/// when installed, else the legacy checkout lookup (`crate::paths`).
fn repo_relative(rel: &str) -> PathBuf {
    crate::paths::runtime_path(rel)
}

/// Default Java driver dex, overridable by env (`--rn-dex` re-exports it).
#[must_use]
pub fn default_driver_dex() -> PathBuf {
    std::env::var_os("ACOS_RN_DRIVER_DEX")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_relative("crates/acos-jni/tests/data/rn-bridge-probe.dex"))
}

/// Boot-classpath directory, when present.
///
/// `ACOS_BOOT_DIR` wins, then an installed ACOS reads exactly
/// `$ACOS_HOME/.cache/boot/android-34` (never the checkout). With `ACOS_HOME`
/// unset the legacy lookup is unchanged: walk up from the process cwd (and the
/// manifest root) so a session running from a linked worktree still finds the
/// checkout's `.cache/boot/android-34`.
fn default_boot_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("ACOS_BOOT_DIR") {
        let path = PathBuf::from(dir);
        if path.join("BOOTCLASSPATH").is_file() {
            return Some(path);
        }
    }
    if let Some(home) = crate::paths::acos_home() {
        return crate::paths::installed_boot_dir_with(Some(&home))
            .filter(|dir| dir.join("BOOTCLASSPATH").is_file());
    }
    let mut bases: Vec<PathBuf> = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        bases.push(cwd);
    }
    if let Some(root) = crate::paths::dev_root() {
        bases.push(root);
    }
    for base in bases {
        let mut dir = Some(base.as_path());
        while let Some(current) = dir {
            let candidate = current.join(".cache/boot/android-34");
            if candidate.join("BOOTCLASSPATH").is_file() {
                return Some(candidate);
            }
            dir = current.parent();
        }
    }
    None
}

/// A React Native session's status snapshot.
#[derive(Debug, Clone)]
pub struct RnStatus {
    /// The APK path.
    pub apk: PathBuf,
    /// Nodes in the current agent tree.
    pub nodes: usize,
    /// UIManager ops replayed into the tree.
    pub ops: usize,
    /// Registered JNI natives (diagnostic).
    pub natives: usize,
    /// Whether the session was built against the real boot classpath.
    pub real_boot: bool,
    /// Wall-clock of the launch (extraction + VM boot + mount).
    pub launch_ms: u128,
    /// Wall-clock since launch.
    pub elapsed_ms: u128,
}

/// The operations the worker thread performs on a live session.
///
/// The trait keeps the thread-affinity contract unit-testable with a fake
/// driver, without a real RN mount.
pub trait RnDriver {
    /// Status snapshot.
    fn status(&mut self) -> RnStatus;
    /// The current agent tree.
    fn tree(&mut self) -> RnTree;
    /// Tap the touchable for `text`; returns the resolved tag.
    fn tap_text(&mut self, text: &str) -> Result<i32, RnSessionError>;
    /// Tap the touchable for `testID`; returns the resolved tag.
    fn tap_test_id(&mut self, id: &str) -> Result<i32, RnSessionError>;
    /// The pixel frame; always `UNSUPPORTED_RN_NO_RENDERER` today.
    fn frame(&mut self) -> Result<(), RnSessionError>;
    /// Captured driver output.
    fn logs(&mut self) -> Vec<String>;
}

/// A [`RnDriver`] over the real [`RnSession`].
struct RealDriver {
    session: RnSession,
    apk: PathBuf,
    started: Instant,
    launch_ms: u128,
}

impl RealDriver {
    fn wrap(session: RnSession, launch_ms: u128) -> RealDriver {
        let apk = session.host_apk().to_path_buf();
        RealDriver {
            session,
            apk,
            started: Instant::now(),
            launch_ms,
        }
    }
}

impl RnDriver for RealDriver {
    fn status(&mut self) -> RnStatus {
        RnStatus {
            apk: self.apk.clone(),
            nodes: self.session.node_count(),
            ops: self.session.op_count(),
            natives: self.session.registered_natives(),
            real_boot: self.session.real_boot(),
            launch_ms: self.launch_ms,
            elapsed_ms: self.started.elapsed().as_millis(),
        }
    }

    fn tree(&mut self) -> RnTree {
        self.session.tree().clone()
    }

    fn tap_text(&mut self, text: &str) -> Result<i32, RnSessionError> {
        let tag =
            self.session
                .tree()
                .tag_for_text(text)
                .ok_or_else(|| RnSessionError::NoTarget {
                    query: format!("text={text}"),
                })?;
        self.session.tap_tag(tag)?;
        Ok(tag)
    }

    fn tap_test_id(&mut self, id: &str) -> Result<i32, RnSessionError> {
        let tag =
            self.session
                .tree()
                .tag_for_test_id(id)
                .ok_or_else(|| RnSessionError::NoTarget {
                    query: format!("testID={id}"),
                })?;
        self.session.tap_tag(tag)?;
        Ok(tag)
    }

    fn frame(&mut self) -> Result<(), RnSessionError> {
        self.session.frame()
    }

    fn logs(&mut self) -> Vec<String> {
        self.session.logs()
    }
}

/// Build the launch options for `apk`, extracting its RN runtime libraries into
/// `scratch` (the confined child's only writable tree).
fn prepare_options(apk: &Path, scratch: &Path) -> Result<RnSessionOptions, RnSessionError> {
    let detected = acos_jni::react_native::detect(apk)
        .map_err(|e| RnSessionError::Setup {
            stage: "detect",
            detail: e.to_string(),
        })?
        .ok_or_else(|| RnSessionError::NotReactNative {
            apk: apk.display().to_string(),
        })?;
    let abi = host_abi();
    if !detected.native_abis.iter().any(|a| a == abi) {
        return Err(RnSessionError::UnsupportedAbi {
            apk: apk.display().to_string(),
            abis: detected.native_abis,
        });
    }
    let lib_dir = scratch.join("lib");
    let extracted = RnSession::extract_native_libs(apk, abi, &lib_dir)?;
    if extracted.is_empty() {
        return Err(RnSessionError::UnsupportedAbi {
            apk: apk.display().to_string(),
            abis: detected.native_abis,
        });
    }
    let driver_dex = default_driver_dex();
    if !driver_dex.is_file() {
        return Err(RnSessionError::Setup {
            stage: "driver-dex",
            detail: format!(
                "{} is missing (`rn-bridge-probe.dex` is only used by the legacy `engine: \"rust-vm\"` \
                 route / `ACOS_MCP_ENGINE=legacy`; React Native APKs boot natively on the default ART \
                 engine with plain `acos_launch {{\"apk\": ...}}`)",
                driver_dex.display()
            ),
        });
    }
    let boot_dir = default_boot_dir();
    let real_boot = boot_dir.is_some();
    Ok(RnSessionOptions {
        lib_dir,
        driver_dex,
        scratch: scratch.join("session-scratch"),
        boot_dir,
        corelib: if real_boot {
            CoreLib::Real
        } else {
            CoreLib::Bootstrap
        },
        ..RnSessionOptions::default()
    })
}

/// A command marshalled to the session's worker thread.
enum RnCommand {
    Tree {
        reply: Sender<RnTree>,
    },
    TapText {
        text: String,
        reply: Sender<Result<i32, RnSessionError>>,
    },
    TapTestId {
        id: String,
        reply: Sender<Result<i32, RnSessionError>>,
    },
    Frame {
        reply: Sender<Result<(), RnSessionError>>,
    },
    Status {
        reply: Sender<RnStatus>,
    },
    Logs {
        reply: Sender<Vec<String>>,
    },
    Shutdown {
        reply: Sender<()>,
    },
}

/// The worker loop: run every command on this thread, in order.
fn worker_loop(rx: Receiver<RnCommand>, mut driver: Box<dyn RnDriver>) {
    while let Ok(cmd) = rx.recv() {
        match cmd {
            RnCommand::Tree { reply } => {
                let _ = reply.send(driver.tree());
            }
            RnCommand::TapText { text, reply } => {
                let _ = reply.send(driver.tap_text(&text));
            }
            RnCommand::TapTestId { id, reply } => {
                let _ = reply.send(driver.tap_test_id(&id));
            }
            RnCommand::Frame { reply } => {
                let _ = reply.send(driver.frame());
            }
            RnCommand::Status { reply } => {
                let _ = reply.send(driver.status());
            }
            RnCommand::Logs { reply } => {
                let _ = reply.send(driver.logs());
            }
            RnCommand::Shutdown { reply } => {
                let _ = reply.send(());
                break;
            }
        }
    }
}

/// A live RN session: commands for the worker thread plus launch metadata.
struct RnWorker {
    tx: Sender<RnCommand>,
    join: Option<JoinHandle<()>>,
    timeout: Duration,
}

impl RnWorker {
    fn send<T>(&self, make: impl FnOnce(Sender<T>) -> RnCommand) -> Result<T, RnSessionError> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.tx
            .send(make(reply_tx))
            .map_err(|_| RnSessionError::Setup {
                stage: "rn-worker",
                detail: "React Native session worker is gone".to_string(),
            })?;
        match reply_rx.recv_timeout(self.timeout) {
            Ok(value) => Ok(value),
            Err(RecvTimeoutError::Timeout) => Err(RnSessionError::Setup {
                stage: "rn-worker",
                detail: format!("no reply within {:?}", self.timeout),
            }),
            Err(RecvTimeoutError::Disconnected) => Err(RnSessionError::Setup {
                stage: "rn-worker",
                detail: "React Native session worker dropped the reply".to_string(),
            }),
        }
    }

    fn tree(&self) -> Result<RnTree, RnSessionError> {
        self.send(|reply| RnCommand::Tree { reply })
    }

    fn tap_text(&self, text: &str) -> Result<i32, RnSessionError> {
        let text = text.to_string();
        self.send(|reply| RnCommand::TapText { text, reply })?
    }

    fn tap_test_id(&self, id: &str) -> Result<i32, RnSessionError> {
        let id = id.to_string();
        self.send(|reply| RnCommand::TapTestId { id, reply })?
    }

    fn frame(&self) -> Result<(), RnSessionError> {
        self.send(|reply| RnCommand::Frame { reply })?
    }

    fn status(&self) -> Result<RnStatus, RnSessionError> {
        self.send(|reply| RnCommand::Status { reply })
    }

    fn logs(&self) -> Vec<String> {
        self.send(|reply| RnCommand::Logs { reply })
            .unwrap_or_default()
    }

    /// Ask the worker to drop the session, with a bounded wait. A wedged session
    /// is detached rather than joined.
    fn shutdown(mut self) {
        let (reply_tx, reply_rx) = mpsc::channel();
        if self
            .tx
            .send(RnCommand::Shutdown { reply: reply_tx })
            .is_ok()
        {
            let _ = reply_rx.recv_timeout(Duration::from_secs(10));
        }
        if let Some(join) = self.join.take() {
            if join.is_finished() {
                let _ = join.join();
            } else {
                drop(join);
            }
        }
    }
}

/// Spawn a worker for an already-live driver (tests only).
#[cfg(test)]
fn spawn_worker<D: RnDriver + Send + 'static>(
    driver: D,
) -> Result<(Sender<RnCommand>, JoinHandle<()>), RnSessionError> {
    let (tx, rx) = mpsc::channel();
    let join = thread::Builder::new()
        .name("acos-rn-session".to_string())
        .stack_size(WORKER_STACK_BYTES)
        .spawn(move || worker_loop(rx, Box::new(driver)))
        .map_err(|e| RnSessionError::Setup {
            stage: "rn-worker-spawn",
            detail: e.to_string(),
        })?;
    Ok((tx, join))
}

/// Spawn the worker and run detection + `RnSession::launch` **on it**.
fn spawn_real_worker(apk: PathBuf, scratch: PathBuf) -> Result<RnWorker, RnSessionError> {
    let (tx, rx) = mpsc::channel();
    let (ready_tx, ready_rx) = mpsc::channel::<Result<u128, RnSessionError>>();
    let timeout = COMMAND_TIMEOUT + Duration::from_secs(30);
    let started = Instant::now();
    let join = thread::Builder::new()
        .name("acos-rn-session".to_string())
        .stack_size(WORKER_STACK_BYTES)
        .spawn(move || {
            let launched = prepare_options(&apk, &scratch)
                .and_then(|options| RnSession::launch(&apk, options));
            match launched {
                Ok(session) => {
                    let launch_ms = started.elapsed().as_millis();
                    let _ = ready_tx.send(Ok(launch_ms));
                    worker_loop(rx, Box::new(RealDriver::wrap(session, launch_ms)));
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            }
        })
        .map_err(|e| RnSessionError::Setup {
            stage: "rn-worker-spawn",
            detail: e.to_string(),
        })?;
    match ready_rx.recv_timeout(timeout) {
        Ok(Ok(_launch_ms)) => Ok(RnWorker {
            tx,
            join: Some(join),
            timeout,
        }),
        Ok(Err(e)) => {
            let _ = join.join();
            Err(e)
        }
        Err(RecvTimeoutError::Timeout) => Err(RnSessionError::Setup {
            stage: "rn-launch",
            detail: "RnSession::launch exceeded its bound".to_string(),
        }),
        Err(RecvTimeoutError::Disconnected) => Err(RnSessionError::Setup {
            stage: "rn-launch",
            detail: "launch worker exited without a reply".to_string(),
        }),
    }
}

/// The confined child's React Native session table.
#[derive(Default)]
pub struct RnSessions {
    held: Mutex<HashMap<String, RnWorker>>,
}

impl RnSessions {
    /// The ids of every held RN session.
    #[must_use]
    pub fn ids(&self) -> Vec<String> {
        self.held
            .lock()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Number of held RN sessions.
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

    /// Launch the RN bridge for `apk` on a dedicated worker thread. The caller
    /// must already be confined: the guest's native RN/Hermes runtime is host
    /// code.
    pub fn launch(&self, id: &str, apk: &Path, scratch: PathBuf) -> Result<(), RnSessionError> {
        let _ = self.remove(id);
        let worker = spawn_real_worker(apk.to_path_buf(), scratch)?;
        if let Ok(mut map) = self.held.lock() {
            map.insert(id.to_string(), worker);
        }
        Ok(())
    }

    /// The current agent tree.
    pub fn tree(&self, id: &str) -> Result<RnTree, RnSessionError> {
        self.worker(id)?.tree()
    }

    /// Tap the touchable for `text`; returns the resolved tag.
    pub fn tap_text(&self, id: &str, text: &str) -> Result<i32, RnSessionError> {
        self.worker(id)?.tap_text(text)
    }

    /// Tap the touchable for `testID`; returns the resolved tag.
    pub fn tap_test_id(&self, id: &str, test_id: &str) -> Result<i32, RnSessionError> {
        self.worker(id)?.tap_test_id(test_id)
    }

    /// The pixel frame (always `UNSUPPORTED_RN_NO_RENDERER` today).
    pub fn frame(&self, id: &str) -> Result<(), RnSessionError> {
        self.worker(id)?.frame()
    }

    /// Status snapshot.
    pub fn status(&self, id: &str) -> Result<RnStatus, RnSessionError> {
        self.worker(id)?.status()
    }

    /// The captured driver output.
    pub fn logs(&self, id: &str) -> Vec<String> {
        self.worker(id).map(|w| w.logs()).unwrap_or_default()
    }

    fn worker(&self, id: &str) -> Result<WorkerGuard<'_>, RnSessionError> {
        let guard = self.held.lock().map_err(|_| RnSessionError::Setup {
            stage: "rn-lock",
            detail: "session table poisoned".to_string(),
        })?;
        if !guard.contains_key(id) {
            return Err(RnSessionError::Setup {
                stage: "rn-session",
                detail: format!("no React Native session {id:?}"),
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
    guard: std::sync::MutexGuard<'a, HashMap<String, RnWorker>>,
    id: String,
}

impl std::ops::Deref for WorkerGuard<'_> {
    type Target = RnWorker;
    fn deref(&self) -> &RnWorker {
        self.guard
            .get(&self.id)
            .expect("checked in RnSessions::worker")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    struct FakeDriver {
        threads: Arc<Mutex<Vec<thread::ThreadId>>>,
        calls: Arc<AtomicUsize>,
    }

    impl FakeDriver {
        fn record(&self) {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.threads.lock().unwrap().push(thread::current().id());
        }
    }

    impl RnDriver for FakeDriver {
        fn status(&mut self) -> RnStatus {
            self.record();
            RnStatus {
                apk: PathBuf::from("/fake.apk"),
                nodes: 1,
                ops: 1,
                natives: 0,
                real_boot: false,
                launch_ms: 0,
                elapsed_ms: 0,
            }
        }
        fn tree(&mut self) -> RnTree {
            self.record();
            RnTree {
                roots: Vec::new(),
                op_count: 0,
            }
        }
        fn tap_text(&mut self, _text: &str) -> Result<i32, RnSessionError> {
            self.record();
            Ok(17)
        }
        fn tap_test_id(&mut self, _id: &str) -> Result<i32, RnSessionError> {
            self.record();
            Ok(17)
        }
        fn frame(&mut self) -> Result<(), RnSessionError> {
            self.record();
            Err(RnSessionError::Unsupported {
                feature: "renderer",
                detail: "fake".to_string(),
            })
        }
        fn logs(&mut self) -> Vec<String> {
            self.record();
            Vec::new()
        }
    }

    #[test]
    fn all_commands_run_on_one_stable_worker_thread() {
        let main_id = thread::current().id();
        let threads = Arc::new(Mutex::new(Vec::new()));
        let calls = Arc::new(AtomicUsize::new(0));
        let driver = FakeDriver {
            threads: Arc::clone(&threads),
            calls: Arc::clone(&calls),
        };
        let (tx, join) = spawn_worker(driver).expect("spawn fake worker");
        let worker = RnWorker {
            tx,
            join: Some(join),
            timeout: COMMAND_TIMEOUT,
        };
        worker.tree().unwrap();
        worker.tap_text("TAP ME").unwrap();
        worker.tap_test_id("hello-button").unwrap();
        assert_eq!(
            worker.frame().unwrap_err().code(),
            "UNSUPPORTED_RN_NO_RENDERER"
        );
        worker.status().unwrap();
        assert!(worker.logs().is_empty());
        worker.shutdown();

        let seen = threads.lock().unwrap().clone();
        assert_eq!(seen.len(), 6, "all commands recorded: {seen:?}");
        let ids: std::collections::HashSet<_> = seen.iter().copied().collect();
        assert_eq!(ids.len(), 1, "every command must run on one thread");
        assert_ne!(ids.iter().next().copied(), Some(main_id));
        assert_eq!(calls.load(Ordering::SeqCst), seen.len());
    }

    #[test]
    fn unknown_session_is_a_setup_error() {
        let sessions = RnSessions::default();
        let err = sessions.tree("nope").unwrap_err();
        assert_eq!(err.code(), "UNSUPPORTED_RN_SETUP");
        assert!(!sessions.contains("nope"));
        assert!(!sessions.remove("nope"));
        assert!(sessions.logs("nope").is_empty());
    }

    #[test]
    fn detection_is_structural() {
        assert!(!is_react_native(Path::new("/nonexistent.apk")));
    }
}
