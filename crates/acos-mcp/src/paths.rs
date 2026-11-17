//! Runtime path resolution for the MCP server (REL1c).
//!
//! One rule, the same one the rest of ACOS uses (`acos_art::repo_root`,
//! `acos_cli::root`, `acos_vm::vm::default_boot_dir`):
//!
//! ```text
//! root := $ACOS_HOME            (an installed ACOS, docs/INSTALL.md)
//!       | else the checkout     (the legacy developer behaviour)
//! ```
//!
//! The installed layout mirrors the checkout's relative paths (`.cache/boot/
//! android-34`, `.cache/flutter-1/lib-x86_64`, `crates/acos-jni/tests/data/…`),
//! so with `ACOS_HOME` set the MCP server reads *that* tree and never the
//! checkout it happened to be compiled in. With `ACOS_HOME` unset the legacy
//! lookup is byte-identical: a cwd-relative path that exists wins, then the
//! compile-time checkout root, then the cwd-relative path (which fails later
//! with the runtime's own error).
//!
//! `acos-mcp` does not link `acos-cli`/`acos-art`, so the small rule lives here
//! and every module in this crate goes through it (`rn_backend`,
//! `flutter_backend`, `session_backend`).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The boot-classpath cache, relative to a runtime root.
pub const BOOT_REL: &str = ".cache/boot/android-34";

/// `$ACOS_HOME` parsed, with the empty string treated as unset.
#[must_use]
pub fn acos_home_with(raw: Option<OsString>) -> Option<PathBuf> {
    let raw = raw?;
    if raw.is_empty() {
        return None;
    }
    Some(PathBuf::from(raw))
}

/// `$ACOS_HOME` when this server runs from an installed tree.
#[must_use]
pub fn acos_home() -> Option<PathBuf> {
    acos_home_with(std::env::var_os("ACOS_HOME"))
}

/// The developer checkout, derived from this crate's compile-time manifest
/// directory. Only used when `ACOS_HOME` is unset.
#[must_use]
pub fn dev_root() -> Option<PathBuf> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
}

/// A read-only file that ships with the tree, resolved against the runtime
/// root: `$ACOS_HOME/<rel>` when installed, else the legacy developer order.
#[must_use]
pub fn runtime_path(rel: &str) -> PathBuf {
    runtime_path_with(acos_home().as_deref(), rel)
}

/// [`runtime_path`] with the install root passed in, so tests can prove both
/// arms without mutating the process environment.
#[must_use]
pub fn runtime_path_with(home: Option<&Path>, rel: &str) -> PathBuf {
    if let Some(home) = home {
        return home.join(rel);
    }
    // Legacy developer order, unchanged: a path that exists relative to the
    // current directory wins, then the checkout, then the bare relative path.
    let direct = PathBuf::from(rel);
    if direct.exists() {
        return direct;
    }
    dev_root()
        .map(|root| root.join(rel))
        .filter(|p| p.exists())
        .unwrap_or(direct)
}

/// `<root>/<BOOT_REL>` for an install root, when that directory exists.
#[must_use]
pub fn installed_boot_dir_with(home: Option<&Path>) -> Option<PathBuf> {
    let dir = home?.join(BOOT_REL);
    dir.is_dir().then_some(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = std::env::temp_dir().join(format!(
            "acos-mcp-paths-{}-{tag}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn acos_home_wins_over_the_checkout() {
        let home = unique_dir("home");
        let rel = "crates/acos-jni/tests/data/flutter-probe.dex";
        let got = runtime_path_with(Some(&home), rel);
        assert_eq!(got, home.join(rel), "ACOS_HOME must own the whole path");
        // Not the checkout's copy: the install root is the only root.
        assert!(!got.starts_with(dev_root().unwrap_or_default()));
        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn unset_keeps_the_legacy_developer_lookup() {
        // A cwd-relative file that exists is returned as-is (cargo runs unit
        // tests with the crate directory as the cwd).
        assert_eq!(
            runtime_path_with(None, "Cargo.toml"),
            PathBuf::from("Cargo.toml")
        );
        // A path that only exists in the checkout resolves to the checkout,
        // exactly as before REL1c.
        let checkout_owned = "crates/acos-mcp/Cargo.toml";
        let got = runtime_path_with(None, checkout_owned);
        assert_eq!(got, dev_root().unwrap().join(checkout_owned));
        assert!(got.is_file(), "{}", got.display());
        // A path that exists in neither stays relative (the caller's error
        // message then names the same path it was given).
        let missing = "no/such/asset.bin";
        assert_eq!(runtime_path_with(None, missing), PathBuf::from(missing));
    }

    #[test]
    fn empty_acos_home_is_unset() {
        assert_eq!(acos_home_with(Some(OsString::new())), None);
        assert_eq!(
            acos_home_with(Some(OsString::from("/nonexistent/acos-home"))),
            Some(PathBuf::from("/nonexistent/acos-home"))
        );
        assert_eq!(acos_home_with(None), None);
    }

    #[test]
    fn installed_boot_dir_is_the_home_copy() {
        let home = unique_dir("boot");
        let boot = home.join(BOOT_REL);
        assert_eq!(installed_boot_dir_with(Some(&home)), None);
        std::fs::create_dir_all(&boot).expect("boot dir");
        assert_eq!(installed_boot_dir_with(Some(&home)), Some(boot));
        assert_eq!(installed_boot_dir_with(None), None);
        std::fs::remove_dir_all(&home).ok();
    }
}
