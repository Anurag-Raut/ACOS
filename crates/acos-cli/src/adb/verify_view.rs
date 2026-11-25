//! Watch-only exposure of an `acos verify --adb` run (docs/DEVICE_PANEL_SPEC.md §3).
//!
//! A verify run's session is owned by the verify driver, which holds it
//! exclusively for the whole scenario. The device panel therefore sees it only
//! through the session's own live-view stream socket (frames), plus the facts
//! captured when it became ready. Everything that would touch the guest (input,
//! rotation, tree, host screenshots) answers honestly: the run is watch-only and a
//! verdict is never influenced by a human.

use std::io::Write as _;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use acos_protocol::ui::UiTree;

use super::shell::{AdbSessionOps, AdbSessionSnapshot};

const WATCH_ONLY: &str = "panel input ignored: verify run";
const ADB_WATCH_ONLY: &str = "adb input ignored: verify run";
const NOT_AVAILABLE: &str = "not available during a verify run (watch-only)";
/// At most one feed note per this long, so a drag does not flood the feed.
const NOTE_INTERVAL: Duration = Duration::from_millis(1000);

/// Read-only [`AdbSessionOps`] for a verify run.
pub struct VerifyViewOps {
    snapshot: AdbSessionSnapshot,
    stream_socket: Option<PathBuf>,
    last_note: Mutex<Option<Instant>>,
}

impl VerifyViewOps {
    #[must_use]
    pub fn new(snapshot: AdbSessionSnapshot, stream_socket: Option<PathBuf>) -> Self {
        Self {
            snapshot,
            stream_socket,
            last_note: Mutex::new(None),
        }
    }

    fn emit_input_dropped_note(&self, payload: &[u8]) {
        let Some(path) = self.stream_socket.as_ref() else {
            return;
        };
        {
            let mut last = self.last_note.lock().unwrap_or_else(|e| e.into_inner());
            if last.is_some_and(|at| at.elapsed() < NOTE_INTERVAL) {
                return;
            }
            *last = Some(Instant::now());
        }
        if let Ok(mut stream) = UnixStream::connect(path) {
            let _ = stream.set_write_timeout(Some(Duration::from_millis(200)));
            let _ = stream.write_all(payload);
        }
    }
}

impl AdbSessionOps for VerifyViewOps {
    fn snapshot(&self) -> Result<AdbSessionSnapshot, String> {
        Ok(self.snapshot.clone())
    }

    fn rotate(&self, _rotation: u8) -> Result<(), String> {
        Err(NOT_AVAILABLE.to_string())
    }

    fn dump_tree(&self) -> Result<(UiTree, u8), String> {
        Err(NOT_AVAILABLE.to_string())
    }

    fn screenshot_png(&self) -> Result<Vec<u8>, String> {
        Err(NOT_AVAILABLE.to_string())
    }

    fn raw_screenshot(&self, _out_path: &Path, _format: &str) -> Result<(u32, u32, u8), String> {
        Err(NOT_AVAILABLE.to_string())
    }

    fn panel_touch(
        &self,
        _action: i32,
        _down_time: i64,
        _event_time: i64,
        _pointers: &[(i32, f32, f32)],
    ) -> Result<(), String> {
        Err(WATCH_ONLY.to_string())
    }

    fn panel_key(
        &self,
        _code: i32,
        _event_type: &str,
        _meta_state: i32,
        _device_id: i32,
    ) -> Result<(), String> {
        Err(WATCH_ONLY.to_string())
    }

    fn panel_text(&self, _text: &str) -> Result<(), String> {
        Err(WATCH_ONLY.to_string())
    }

    fn adb_tap(&self, _x: i32, _y: i32) -> Result<(), String> {
        Err(ADB_WATCH_ONLY.to_string())
    }

    fn adb_swipe(
        &self,
        _x1: i32,
        _y1: i32,
        _x2: i32,
        _y2: i32,
        _duration_ms: u64,
    ) -> Result<(), String> {
        Err(ADB_WATCH_ONLY.to_string())
    }

    fn adb_key(&self, _code: i32, _longpress: bool) -> Result<(), String> {
        Err(ADB_WATCH_ONLY.to_string())
    }

    fn adb_text(&self, _text: &str) -> Result<(), String> {
        Err(ADB_WATCH_ONLY.to_string())
    }

    fn stream_socket(&self) -> Option<PathBuf> {
        self.stream_socket.clone()
    }

    fn is_verify_run(&self) -> bool {
        true
    }

    fn panel_input_dropped(&self) {
        self.emit_input_dropped_note(b"{\"v\":1,\"panel_input_ignored\":true}\n");
    }

    fn adb_input_dropped(&self) {
        self.emit_input_dropped_note(b"{\"v\":1,\"adb_input_ignored\":true}\n");
    }
}
