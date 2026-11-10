//! W142-A: Flutter product sessions.
//!
//! [`FlutterSession`] must be created and driven on the confined child's single
//! serving thread: it owns the guest `Vm` and a raw `ALooper`, and the engine's
//! native threads cache the VM pointer. This module holds the per-session state
//! in the confined child's [`crate::session_backend::SessionBackend`]. The
//! parent daemon/MCP process holds only a `ConfinedChild` process and forwards
//! the session tools to it, so a Flutter session is never touched across
//! threads.
//!
//! Nothing here enters confinement; the caller (`SessionBackend::call_run`)
//! either already runs inside the ADR-0022 sandbox or re-execs a confined
//! child before calling [`FlutterSessions::launch`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use acos_jni::flutter::SemanticsNode;
use acos_jni::{FlutterSession, FlutterSessionError, FlutterSessionOptions};

/// One live Flutter engine owned by the confined child.
pub struct FlutterHeld {
    /// The product driver.
    pub session: FlutterSession,
    /// The APK the session was launched from.
    pub apk: PathBuf,
    /// When the launch began.
    pub started: Instant,
    /// Wall-clock the launch itself took.
    pub launch_ms: u128,
}

/// The confined child's Flutter session table (one lock; the server is
/// single-threaded, the mutex only keeps `&mut` access sound).
#[derive(Default)]
pub struct FlutterSessions {
    held: Mutex<HashMap<String, FlutterHeld>>,
}

/// A frame captured by [`FlutterSessions::observe`].
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

/// Default directory holding the extracted engine, overridable by env.
fn default_lib_dir() -> PathBuf {
    std::env::var_os("ACOS_FLUTTER_LIB_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".cache/flutter-1/lib-x86_64"))
}

/// Default Java driver dex, overridable by env.
fn default_driver_dex() -> PathBuf {
    std::env::var_os("ACOS_FLUTTER_DRIVER_DEX")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("crates/acos-jni/tests/data/flutter-probe.dex"))
}

/// Session options for a product launch (geometry from the reference device).
#[must_use]
pub fn flush_options(scratch: PathBuf) -> FlutterSessionOptions {
    FlutterSessionOptions {
        lib_dir: default_lib_dir(),
        scratch,
        driver_dex: Some(default_driver_dex()),
        prefer_probe_snapshot: default_lib_dir().join("libapp_probe.so").is_file(),
        ..FlutterSessionOptions::default()
    }
}

impl FlutterSessions {
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
        self.held
            .lock()
            .map(|mut m| m.remove(id).is_some())
            .unwrap_or(false)
    }

    /// Launch a Flutter engine for `apk` and hold it. The caller must already be
    /// confined (fail closed: `FlutterSession` maps the engine with
    /// `require_confinement = true`).
    pub fn launch(
        &self,
        id: &str,
        apk: &Path,
        scratch: PathBuf,
    ) -> Result<(), FlutterSessionError> {
        let started = Instant::now();
        let session = FlutterSession::launch(apk, flush_options(scratch))?;
        let launch_ms = started.elapsed().as_millis();
        let held = FlutterHeld {
            session,
            apk: apk.to_path_buf(),
            started,
            launch_ms,
        };
        if let Ok(mut map) = self.held.lock() {
            map.insert(id.to_string(), held);
        }
        Ok(())
    }

    /// Pump until at least one frame is present, then return the newest frame
    /// (and optionally its encoded PNG).
    pub fn observe(
        &self,
        id: &str,
        with_png: bool,
    ) -> Result<FlutterObserve, FlutterSessionError> {
        let mut held = self.held.lock().map_err(|_| FlutterSessionError::Setup {
            stage: "flutter-lock",
            detail: "session table poisoned".to_string(),
        })?;
        let h = held.get_mut(id).ok_or_else(|| FlutterSessionError::Setup {
            stage: "flutter-session",
            detail: format!("no Flutter session {id:?}"),
        })?;
        if h.session.frame_count() == 0 {
            h.session.pump_until_frames(1, h.session.opts().max_pump_rounds)?;
        }
        let (seq, width, height, png) = match h.session.frame() {
            Some(frame) => {
                let png = if with_png {
                    Some(frame.png()?)
                } else {
                    None
                };
                (frame.seq, frame.width, frame.height, png)
            }
            None => (0, 0, 0, None),
        };
        let semantics = h.session.semantics()?.len();
        Ok(FlutterObserve {
            seq,
            width,
            height,
            png,
            semantics,
        })
    }

    /// The current semantic tree.
    pub fn semantics(&self, id: &str) -> Result<Vec<SemanticsNode>, FlutterSessionError> {
        let mut held = self.held.lock().map_err(|_| FlutterSessionError::Setup {
            stage: "flutter-lock",
            detail: "session table poisoned".to_string(),
        })?;
        let h = held.get_mut(id).ok_or_else(|| FlutterSessionError::Setup {
            stage: "flutter-session",
            detail: format!("no Flutter session {id:?}"),
        })?;
        // A semantics update only arrives after a frame; pump once so a
        // freshly-launched app publishes it before the first observe.
        if h.session.frame_count() == 0 {
            let _ = h.session.pump_until_frames(1, h.session.opts().max_pump_rounds);
        }
        h.session.semantics()
    }

    /// Tap physical `(x, y)`.
    pub fn tap(&self, id: &str, x: f64, y: f64) -> Result<(), FlutterSessionError> {
        self.mutate(id, |s| s.tap(x, y))
    }

    /// Tap the centre of the node labelled `label`.
    pub fn tap_text(&self, id: &str, label: &str) -> Result<[f64; 2], FlutterSessionError> {
        self.mutate(id, |s| s.tap_text(label))
    }

    /// Status snapshot.
    #[must_use]
    pub fn status(&self, id: &str) -> Option<FlutterStatus> {
        let mut held = self.held.lock().ok()?;
        let h = held.get_mut(id)?;
        Some(FlutterStatus {
            apk: h.apk.clone(),
            frames: h.session.frame_count(),
            semantics: h.session.semantics().map(|s| s.len()).unwrap_or(0),
            elapsed_ms: h.started.elapsed().as_millis(),
            launch_ms: h.launch_ms,
            engine: h.session.engine_stats(),
        })
    }

    /// The driver's captured stdout/stderr lines.
    #[must_use]
    pub fn logs(&self, id: &str) -> Vec<String> {
        let Ok(held) = self.held.lock() else {
            return Vec::new();
        };
        let Some(h) = held.get(id) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for line in h.session.driver_stdout().lines() {
            out.push(format!("stdout: {line}"));
        }
        for line in h.session.driver_stderr().lines() {
            out.push(format!("stderr: {line}"));
        }
        if let Some(err) = h.session.driver_error() {
            out.push(format!("error: {err}"));
        }
        out
    }

    fn mutate<T>(
        &self,
        id: &str,
        f: impl FnOnce(&mut FlutterSession) -> Result<T, FlutterSessionError>,
    ) -> Result<T, FlutterSessionError> {
        let mut held = self.held.lock().map_err(|_| FlutterSessionError::Setup {
            stage: "flutter-lock",
            detail: "session table poisoned".to_string(),
        })?;
        let h = held.get_mut(id).ok_or_else(|| FlutterSessionError::Setup {
            stage: "flutter-session",
            detail: format!("no Flutter session {id:?}"),
        })?;
        f(&mut h.session)
    }
}
