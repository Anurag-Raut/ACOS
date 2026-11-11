//! Private-root validation for the `ACOS_*` directory knobs.
//!
//! Three environment variables redirect where ACOS reads or writes state:
//!
//! * `ACOS_CACHE_DIR` — the on-disk boot-image cache (writable, metadata-bearing),
//! * `ACOS_SESSION_ROOT` — the VFS data root of a session (writable, app data),
//! * `ACOS_BOOT_DIR` — the extracted boot classpath (read).
//!
//! A world- or group-writable root lets another local user pre-seed a cache
//! entry, swap app data under a live session, or replace the boot classpath a
//! confined child loads. The sandbox already makes the *child* unable to write
//! outside its root, but the root itself must still be private. This module is
//! the one place that decides, and it fails closed with a structured
//! [`RootError`] instead of silently tightening (silently `chmod`-ing an
//! attacker's directory would be a lie about who controls it).
//!
//! The rule is deliberately narrower than "must be 0700": an existing `0755`
//! directory (a normal developer checkout, `~/.cache`) is accepted and tightened
//! to `0700`; only group/world **write** bits (`0o022`) are a hard rejection,
//! because those are what let another user swap the contents.

use std::io;
use std::path::Path;

/// Why a root directory was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootError {
    /// The entry exists but is not a real directory (a symlink or a file).
    NotADirectory,
    /// The directory is not owned by the effective user.
    NotOwned,
    /// `mode & 0o022 != 0`: another local user can add/remove/replace entries.
    WritableByOthers,
    /// The metadata could not be read.
    Io,
}

impl RootError {
    /// Stable machine-readable code, used in envelopes and log lines.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            RootError::NotADirectory => "ROOT_NOT_A_DIRECTORY",
            RootError::NotOwned => "ROOT_NOT_OWNED",
            RootError::WritableByOthers => "ROOT_WRITABLE_BY_OTHERS",
            RootError::Io => "ROOT_IO_ERROR",
        }
    }

    /// The error code the agent-facing protocol already uses for an unsafe path.
    #[must_use]
    pub fn protocol_code(self) -> &'static str {
        "UNSAFE_PATH"
    }
}

impl std::fmt::Display for RootError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for RootError {}

/// Validate `dir` as a private root for the knob named `var`.
///
/// * a symlink or non-directory is rejected ([`RootError::NotADirectory`]),
/// * a directory owned by another uid is rejected ([`RootError::NotOwned`]),
/// * group/world-writable directories are rejected
///   ([`RootError::WritableByOthers`]) — the message names the knob and mode,
/// * any other group/other access is tightened to `0700` (an existing `0755`
///   checkout keeps working), and the result is re-checked.
///
/// A missing directory is not an error here: the caller creates it (and the
/// create path must itself be `0700`).
pub fn ensure_private_root(var: &str, dir: &Path) -> Result<(), (RootError, String)> {
    #[cfg(not(unix))]
    {
        let _ = (var, dir);
        return Ok(());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let meta = match std::fs::symlink_metadata(dir) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => {
                return Err((
                    RootError::Io,
                    format!("{var} {}: cannot stat: {e}", dir.display()),
                ))
            }
        };
        if meta.file_type().is_symlink() || !meta.is_dir() {
            return Err((
                RootError::NotADirectory,
                format!("{var} {} is not a real directory", dir.display()),
            ));
        }
        let euid = effective_uid();
        if meta.uid() != euid {
            return Err((
                RootError::NotOwned,
                format!(
                    "{var} {} is owned by uid {} (effective {euid})",
                    dir.display(),
                    meta.uid()
                ),
            ));
        }
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o022 != 0 {
            return Err((
                RootError::WritableByOthers,
                format!(
                    "{var} {} is group/world-writable (mode {mode:o}); it must not be \
                     writable by other users (chmod 700 {})",
                    dir.display(),
                    dir.display()
                ),
            ));
        }
        if mode & 0o077 != 0 {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| {
                (
                    RootError::Io,
                    format!("{var} {}: cannot tighten to 0700: {e}", dir.display()),
                )
            })?;
        }
        let mode = std::fs::metadata(dir)
            .map_err(|e| {
                (
                    RootError::Io,
                    format!("{var} {}: cannot re-stat: {e}", dir.display()),
                )
            })?
            .permissions()
            .mode()
            & 0o777;
        if mode & 0o077 != 0 {
            return Err((
                RootError::Io,
                format!("{var} {} is not private (mode {mode:o})", dir.display()),
            ));
        }
        Ok(())
    }
}

/// The effective uid, read from `/proc/self/status` (no `unsafe`, no libc).
fn effective_uid() -> u32 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines().find_map(|l| {
                l.strip_prefix("Uid:")
                    .and_then(|rest| rest.split_whitespace().nth(1))
                    .and_then(|uid| uid.parse().ok())
            })
        })
        .unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> TempDir {
            use std::sync::atomic::{AtomicU64, Ordering};
            static NONCE: AtomicU64 = AtomicU64::new(0);
            let mut path = std::env::temp_dir();
            path.push(format!(
                "acos-root-{tag}-{}-{}",
                std::process::id(),
                NONCE.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).expect("temp dir");
            TempDir(path)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let mut perms = std::fs::metadata(&self.0).unwrap().permissions();
            perms.set_mode(0o700);
            let _ = std::fs::set_permissions(&self.0, perms);
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn mode(dir: &Path, mode: u32) {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode)).expect("chmod");
    }

    #[test]
    fn a_private_directory_is_accepted() {
        let d = TempDir::new("ok");
        mode(d.path(), 0o700);
        assert_eq!(ensure_private_root("ACOS_CACHE_DIR", d.path()), Ok(()));
    }

    #[test]
    fn a_read_only_for_others_directory_is_tightened() {
        let d = TempDir::new("tighten");
        mode(d.path(), 0o755);
        assert_eq!(ensure_private_root("ACOS_CACHE_DIR", d.path()), Ok(()));
        let m = std::fs::metadata(d.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(m, 0o700, "an existing 0755 root is tightened, not trusted");
    }

    #[test]
    fn a_world_writable_root_is_rejected() {
        let d = TempDir::new("world");
        mode(d.path(), 0o777);
        let err = ensure_private_root("ACOS_SESSION_ROOT", d.path()).expect_err("0o777");
        assert_eq!(err.0, RootError::WritableByOthers);
        assert_eq!(err.0.code(), "ROOT_WRITABLE_BY_OTHERS");
        assert_eq!(err.0.protocol_code(), "UNSAFE_PATH");
        assert!(err.1.contains("ACOS_SESSION_ROOT"), "names the knob: {}", err.1);
        assert!(err.1.contains("777"), "names the mode: {}", err.1);
    }

    #[test]
    fn a_group_writable_root_is_rejected() {
        let d = TempDir::new("group");
        mode(d.path(), 0o770);
        let err = ensure_private_root("ACOS_BOOT_DIR", d.path()).expect_err("0o770");
        assert_eq!(err.0, RootError::WritableByOthers);
        assert!(err.1.contains("ACOS_BOOT_DIR"));
    }

    #[test]
    fn a_symlinked_or_file_root_is_rejected() {
        let d = TempDir::new("link");
        let link = d.path().join("link");
        std::os::unix::fs::symlink(d.path(), &link).expect("symlink");
        assert_eq!(
            ensure_private_root("ACOS_CACHE_DIR", &link).map_err(|e| e.0),
            Err(RootError::NotADirectory)
        );
        let file = d.path().join("file");
        std::fs::write(&file, b"x").expect("file");
        assert_eq!(
            ensure_private_root("ACOS_CACHE_DIR", &file).map_err(|e| e.0),
            Err(RootError::NotADirectory)
        );
    }

    #[test]
    fn a_missing_root_is_left_to_the_caller() {
        let d = TempDir::new("missing");
        let missing = d.path().join("nope");
        assert_eq!(ensure_private_root("ACOS_CACHE_DIR", &missing), Ok(()));
        assert!(!missing.exists(), "validation never creates the directory");
    }
}
