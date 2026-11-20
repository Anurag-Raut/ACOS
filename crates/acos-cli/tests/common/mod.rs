//! Helpers shared by the `acos serve` integration tests in this directory.
//!
//! Cargo compiles this module into each test binary that declares `mod common;`,
//! so unused items here are expected rather than a smell.

#![allow(dead_code)]

use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Bytes a pathname `AF_UNIX` address may use on Linux: `sun_path` is 108 bytes
/// including the terminating NUL (`.cache/g3f1/sun-len-evidence.txt` measured
/// 107 binding and 108 failing with `AF_UNIX path too long`).
pub const SUN_PATH_MAX: usize = 107;

/// A scratch directory whose own path — and therefore every socket path in it —
/// stays short enough for `sun_path`.
///
/// A daemon socket used to live in `<worktree>/.cache/<test>-<tag>-<pid>/`, which
/// is short in a short checkout and over the limit in a long worktree: G3F1's
/// worktree measured `<WT>/.cache/w50-test-unknown-session-<pid>/serve.sock` at
/// exactly 108 bytes, so `acos serve` could not bind and
/// `an_unknown_session_is_not_routed_to_the_frozen_rust_vm` failed for a reason
/// that had nothing to do with what it asserts. The socket now lives in
/// `$XDG_RUNTIME_DIR/acos-test-<pid>-<n>-<tag>` (falling back to `/tmp` only when
/// `XDG_RUNTIME_DIR` is unset or unusable), which is independent of the checkout
/// path, and the directory is removed on drop.
///
/// Scratch that is not a socket (runtime dir, cache dir, logs) stays under the
/// worktree: this helper is only for the short path names.
pub struct ShortSocketDir {
    dir: PathBuf,
}

impl ShortSocketDir {
    pub fn new(tag: &str) -> ShortSocketDir {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let base = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|dir| dir.is_dir())
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        let tag: String = tag
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
            .take(20)
            .collect();
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = base.join(format!("acos-test-{}-{seq}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)
            .unwrap_or_else(|e| panic!("create short socket dir {}: {e}", dir.display()));
        ShortSocketDir { dir }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Path of a socket in this directory. Asserts the `sun_path` budget so a
    /// future edit (a longer tag, a longer base) cannot silently reintroduce the
    /// long-path failure — it becomes a one-line assertion instead of an
    /// `ENAMETOOLONG` from inside the daemon.
    pub fn socket(&self, name: &str) -> PathBuf {
        let path = self.dir.join(name);
        let len = path.as_os_str().as_bytes().len();
        assert!(
            len <= SUN_PATH_MAX,
            "socket path {} is {len} bytes; sun_path allows {SUN_PATH_MAX} (+NUL)",
            path.display()
        );
        path
    }
}

impl Drop for ShortSocketDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of the helper: a socket path is short whatever the tag
    /// and whatever the checkout path is.
    #[test]
    fn socket_paths_stay_inside_sun_path() {
        let dir = ShortSocketDir::new("a_deliberately_long_tag_name_that_would_never_fit");
        let path = dir.socket("serve.sock");
        let len = path.as_os_str().as_bytes().len();
        assert!(len <= SUN_PATH_MAX, "{} is {len} bytes", path.display());
        assert!(path.parent().unwrap().is_dir(), "socket dir must exist");
        let parent = path.parent().unwrap().to_path_buf();
        drop(dir);
        assert!(!parent.exists(), "socket dir must be removed on drop");
    }

    /// The budget assertion has teeth: an over-long name panics here instead of
    /// surfacing as `ENAMETOOLONG` from inside `acos serve`.
    #[test]
    #[should_panic(expected = "sun_path allows")]
    fn a_name_over_the_budget_is_rejected() {
        let dir = ShortSocketDir::new("budget");
        let _ = dir.socket(&"s".repeat(200));
    }
}
