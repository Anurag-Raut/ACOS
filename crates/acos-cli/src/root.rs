//! Runtime root resolution (REL1a).
//!
//! One rule, shared by every `acos` code path that names a repo-relative
//! asset or a writable cache: `$ACOS_HOME` when it is set (an installed ACOS,
//! see `docs/INSTALL.md`), else the developer checkout. The installed layout
//! mirrors the checkout's relative paths (`.cache/art/sysroot`,
//! `.cache/boot/android-34`, `.cache/sessions`, …), so nothing else in this
//! crate needs to know which of the two it is running from.
//!
//! `acos-art` resolves the same way in `acos_art::repo_root`; these helpers
//! exist because `acos-cli` does not link `acos-art` (it spawns it).

use std::path::{Path, PathBuf};

/// `$ACOS_HOME` when set and non-empty.
#[must_use]
pub fn acos_home() -> Option<PathBuf> {
    let raw = std::env::var_os("ACOS_HOME")?;
    if raw.is_empty() {
        return None;
    }
    Some(PathBuf::from(raw))
}

/// The root every runtime path is relative to: `$ACOS_HOME`, else the checkout
/// (`$ACOS_ROOT`, then the running binary's ancestors, then the cwd's).
#[must_use]
pub fn runtime_root() -> Option<PathBuf> {
    runtime_root_with(acos_home())
}

/// [`runtime_root`] with the install root passed in, so tests can prove both
/// arms without mutating the process environment.
#[must_use]
pub fn runtime_root_with(home: Option<PathBuf>) -> Option<PathBuf> {
    if let Some(home) = home {
        return Some(home);
    }
    if let Some(dir) = std::env::var_os("ACOS_ROOT") {
        let dir = PathBuf::from(dir);
        if dir.join("Cargo.toml").is_file() {
            return Some(dir);
        }
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        candidates.extend(exe.ancestors().map(Path::to_path_buf));
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.extend(cwd.ancestors().map(Path::to_path_buf));
    }
    candidates
        .into_iter()
        .find(|dir| dir.join("Cargo.toml").is_file() && dir.join("crates").is_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acos_home_wins() {
        let home = PathBuf::from("/nonexistent/acos-home-test");
        assert_eq!(runtime_root_with(Some(home.clone())), Some(home));
    }

    #[test]
    fn unset_resolves_to_the_checkout() {
        let root = runtime_root_with(None).expect("the test runs inside the checkout");
        assert!(root.join("Cargo.toml").is_file(), "{}", root.display());
        assert!(root.join("crates").is_dir(), "{}", root.display());
    }
}
