//! 16-slot emulator port & flock allocator (§4.4 of docs/DEVICE_PANEL_SPEC.md).

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::net::{Ipv4Addr, TcpListener};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

/// Total emulator slots scanned by the stock adb server (`5554..=5584`).
pub const MAX_ADB_SLOTS: u8 = 16;

/// Base console port for slot 0. Slot `k` uses `5554 + 2*k` (console) and
/// `5555 + 2*k` (adb).
pub const BASE_CONSOLE_PORT: u16 = 5554;

/// Env var overriding the slot lock directory (for isolated tests).
pub const ADB_LOCK_DIR_ENV: &str = "ACOS_ADB_LOCK_DIR";

/// Default lock directory (`$ACOS_ADB_LOCK_DIR`, else `$XDG_RUNTIME_DIR/acos-adb`,
/// else `/tmp/acos-adb-<uid>`).
pub fn lock_dir_from_env() -> PathBuf {
    if let Some(val) = std::env::var_os(ADB_LOCK_DIR_ENV) {
        if !val.is_empty() {
            return PathBuf::from(val);
        }
    }
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
        if !runtime.is_empty() {
            return PathBuf::from(runtime).join("acos-adb");
        }
    }
    let uid = unsafe { libc::getuid() };
    std::env::temp_dir().join(format!("acos-adb-{uid}"))
}

/// Immutable slot metadata for an exposed session.
#[derive(Debug, Clone)]
pub struct SlotInfo {
    pub slot: u8,
    pub console_port: u16,
    pub adb_port: u16,
    pub serial: String,
}

/// An active slot claim: holds the `flock` and TCP listeners until dropped.
pub struct SlotClaim {
    pub info: SlotInfo,
    pub console_listener: TcpListener,
    pub listener: TcpListener,
    _lock_file: File,
    pub lock_path: PathBuf,
}

/// Ensure the lock directory exists and is `0700` owned by the current uid.
fn prepare_lock_dir(dir: &Path) -> std::io::Result<()> {
    if let Some(parent) = dir.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)?;
        }
    }
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    Ok(())
}

/// Claim the first free adb emulator slot (`0..16`), returning `Ok(None)` when
/// all 16 slots are busy (either locked by an ACOS session or already bound on
/// loopback by a real Android emulator).
pub fn claim_slot(lock_dir: &Path) -> std::io::Result<Option<SlotClaim>> {
    prepare_lock_dir(lock_dir)?;
    for slot in 0..MAX_ADB_SLOTS {
        let console_port = BASE_CONSOLE_PORT + 2 * (slot as u16);
        let adb_port = console_port + 1;
        let lock_path = lock_dir.join(format!("slot-{slot}.lock"));
        let file = match OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .custom_flags(libc::O_CLOEXEC)
            .mode(0o600)
            .open(&lock_path)
        {
            Ok(f) => f,
            Err(_) => continue,
        };
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc != 0 {
            continue;
        }
        let console_listener = match TcpListener::bind((Ipv4Addr::LOCALHOST, console_port)) {
            Ok(l) => l,
            Err(_) => continue,
        };
        if console_listener.set_nonblocking(true).is_err() {
            continue;
        }
        let listener = match TcpListener::bind((Ipv4Addr::LOCALHOST, adb_port)) {
            Ok(l) => l,
            Err(_) => continue,
        };
        if listener.set_nonblocking(true).is_err() {
            continue;
        }
        // Best-effort diagnostics: record owning server pid in the lock file.
        if let Ok(mut f_ref) = file.try_clone() {
            let _ = f_ref.set_len(0);
            let _ = writeln!(f_ref, "{}", std::process::id());
        }
        return Ok(Some(SlotClaim {
            info: SlotInfo {
                slot,
                console_port,
                adb_port,
                serial: format!("emulator-{console_port}"),
            },
            console_listener,
            listener,
            _lock_file: file,
            lock_path,
        }));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepare_lock_dir_creates_missing_parents_0700() {
        let base = std::env::temp_dir().join(format!("acos-slots-mode-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let parent = base.join("acos");
        let leaf = parent.join("adb-locks");
        prepare_lock_dir(&leaf).expect("prepare_lock_dir");
        for path in [&parent, &leaf] {
            let mode = std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
            assert_eq!(
                mode,
                0o700,
                "{} created with mode 0{mode:o}, expected 0700",
                path.display()
            );
        }
        let _ = std::fs::remove_dir_all(&base);
    }
}
