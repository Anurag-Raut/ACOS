#![allow(clippy::missing_safety_doc)]
//! # `acos-native-loader`
//!
//! Rust-owned native loader for ACOS: it maps Bionic-targeted x86_64 Android
//! shared objects into the process, applies their relocations, binds their
//! imports against a Rust bionic compatibility table, runs constructors, and
//! exposes exported symbols (`JNI_OnLoad`, `Java_*`, ...).
//!
//! ```
//! use acos_native_loader::{Loader, LoaderConfig};
//!
//! let loader = Loader::new(LoaderConfig::default());
//! // `loader.load_file(std::path::Path::new("libhellojni.so"))`
//! let _ = loader.report();
//! ```
//!
//! ## Never fake
//!
//! An import absent from the bionic table is never silently stubbed. In the
//! default [`MissingPolicy::Strict`] mode, load fails with
//! `UNSUPPORTED_NATIVE_SYMBOL` listing every missing symbol and the library
//! that needs it. In [`MissingPolicy::LazyTrap`] mode it is bound to a
//! per-symbol aborting trap that names the exact symbol when called.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

pub mod bionic;
pub mod dyninfo;
pub mod elf;
pub mod error;
pub mod loader;
pub mod report;
pub mod testjni;
pub mod tls;

pub use error::{LibraryMissing, LoaderError, Result};
pub use loader::{LibraryHandle, Loader, NativeLibrary};
pub use report::Report;

/// Sink for `__android_log_*` output. The runtime installs a sink to capture
/// logcat-style lines without touching the host's real log.
pub trait LogSink: Send + Sync {
    /// Called for every guest log line.
    fn log(&self, priority: i32, tag: &str, message: &str);
}

/// What to do when a guest import is not provided by the bionic table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MissingPolicy {
    /// Fail the load with `UNSUPPORTED_NATIVE_SYMBOL` (default).
    #[default]
    Strict,
    /// Bind the symbol to a per-symbol aborting trap (opt-in diagnostics).
    LazyTrap,
}

/// Loader configuration.
#[derive(Clone, Default)]
pub struct LoaderConfig {
    /// Directories searched for `DT_NEEDED` libraries (e.g. the extracted
    /// APK `lib/x86_64/` directory).
    pub search_paths: Vec<PathBuf>,
    /// Missing-import policy.
    pub missing_policy: MissingPolicy,
    /// Optional log sink for `__android_log_*`.
    pub log_sink: Option<Arc<dyn LogSink>>,
    /// Overrides / additions to the default Android system properties.
    pub system_properties: HashMap<String, String>,
}

impl std::fmt::Debug for LoaderConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoaderConfig")
            .field("search_paths", &self.search_paths)
            .field("missing_policy", &self.missing_policy)
            .field("log_sink", &self.log_sink.is_some())
            .field("system_properties", &self.system_properties.len())
            .finish()
    }
}

impl LoaderConfig {
    /// Convenience builder: add a search path.
    pub fn with_search_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.search_paths.push(path.into());
        self
    }

    /// Convenience builder: choose the missing-symbol policy.
    pub fn with_missing_policy(mut self, policy: MissingPolicy) -> Self {
        self.missing_policy = policy;
        self
    }

    /// Convenience builder: install a guest log sink.
    pub fn with_log_sink(mut self, sink: Arc<dyn LogSink>) -> Self {
        self.log_sink = Some(sink);
        self
    }
}
