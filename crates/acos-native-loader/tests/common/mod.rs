//! Shared helpers for the native-loader integration tests.
//!
//! The loader's `dl*` shims and the test JNI harness keep process-global state,
//! so tests in a single binary must not run concurrently. Every test takes
//! [`serial`].
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

static SERIAL: Mutex<()> = Mutex::new(());

/// Serialise tests within one test binary.
pub fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// Absolute path of a checked-in test data file.
pub fn data(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

/// The test data directory (used as a loader search path).
pub fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data")
}

/// Transmute a symbol pointer to a function pointer type.
///
/// # Safety
/// `p` must point at code with the ABI described by `R`.
pub unsafe fn sym<R>(p: *const std::os::raw::c_void) -> R {
    assert!(!p.is_null(), "symbol pointer was null");
    std::mem::transmute_copy::<*const std::os::raw::c_void, R>(&p)
}

use std::os::raw::c_char;
use std::sync::Arc;

/// A [`LogSink`](acos_native_loader::LogSink) that records every guest log line.
#[derive(Default)]
pub struct RecordingSink {
    pub lines: Mutex<Vec<String>>,
}

impl acos_native_loader::LogSink for RecordingSink {
    fn log(&self, priority: i32, tag: &str, message: &str) {
        self.lines
            .lock()
            .unwrap()
            .push(format!("[{priority}] {tag}: {message}"));
    }
}

impl RecordingSink {
    pub fn lines(&self) -> Vec<String> {
        self.lines.lock().unwrap().clone()
    }

    pub fn new_arc() -> Arc<Self> {
        Arc::new(Self::default())
    }
}

/// Read a NUL-terminated C string.
pub fn cstr(p: *const c_char) -> String {
    unsafe { std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned() }
}
