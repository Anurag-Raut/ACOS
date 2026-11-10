//! Landlock filesystem confinement via the raw Landlock syscalls.
//!
//! We call the ABI directly rather than pull in the `landlock` crate so that
//! the whole unsafe surface stays in this crate and the exact handled-access
//! mask can be tuned per ABI version.
//!
//! A ruleset with `handled_access_fs` set to every right the kernel supports
//! restricts *all* of them; any path not covered by an added rule is denied.
//! Read-only roots grant `EXECUTE|READ_FILE|READ_DIR`; read-write roots grant
//! the full handled mask.

use std::ffi::CString;
use std::os::raw::c_int;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

const LANDLOCK_CREATE_RULESET_VERSION: u64 = 1;
const LANDLOCK_RULE_PATH_BENEATH: u32 = 1;

const FS_EXECUTE: u64 = 1 << 0;
const FS_WRITE_FILE: u64 = 1 << 1;
const FS_READ_FILE: u64 = 1 << 2;
const FS_READ_DIR: u64 = 1 << 3;
const FS_REMOVE_DIR: u64 = 1 << 4;
const FS_REMOVE_FILE: u64 = 1 << 5;
const FS_MAKE_CHAR: u64 = 1 << 6;
const FS_MAKE_DIR: u64 = 1 << 7;
const FS_MAKE_REG: u64 = 1 << 8;
const FS_MAKE_SOCK: u64 = 1 << 9;
const FS_MAKE_FIFO: u64 = 1 << 10;
const FS_MAKE_BLOCK: u64 = 1 << 11;
const FS_MAKE_SYM: u64 = 1 << 12;
const FS_REFER: u64 = 1 << 13;
const FS_TRUNCATE: u64 = 1 << 14;
const FS_IOCTL_DEV: u64 = 1 << 15;

#[repr(C)]
struct RulesetAttr {
    handled_access_fs: u64,
}

#[repr(C, packed)]
struct PathBeneathAttr {
    allowed_access: u64,
    parent_fd: i32,
}

/// Query the Landlock ABI version.
pub fn abi() -> Option<u32> {
    // SAFETY: passing a NULL attribute pointer with size 0 and the VERSION
    // command is the documented way to query the ABI; no memory is touched.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<RulesetAttr>(),
            0usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    };
    if rc < 0 {
        None
    } else {
        Some(rc as u32)
    }
}

fn handled_fs(abi: u32) -> u64 {
    let mut mask = FS_EXECUTE
        | FS_WRITE_FILE
        | FS_READ_FILE
        | FS_READ_DIR
        | FS_REMOVE_DIR
        | FS_REMOVE_FILE
        | FS_MAKE_CHAR
        | FS_MAKE_DIR
        | FS_MAKE_REG
        | FS_MAKE_SOCK
        | FS_MAKE_FIFO
        | FS_MAKE_BLOCK
        | FS_MAKE_SYM;
    if abi >= 2 {
        mask |= FS_REFER;
    }
    if abi >= 3 {
        mask |= FS_TRUNCATE;
    }
    if abi >= 5 {
        mask |= FS_IOCTL_DEV;
    }
    mask
}

fn read_only_access() -> u64 {
    FS_EXECUTE | FS_READ_FILE | FS_READ_DIR
}

/// Create and enforce a ruleset. Missing roots are skipped (never widened).
pub fn restrict(read_only: &[PathBuf], read_write: &[PathBuf]) -> Result<String, String> {
    let abi = abi().ok_or_else(|| "landlock_create_ruleset(VERSION) unsupported".to_string())?;
    let handled = handled_fs(abi);
    let attr = RulesetAttr {
        handled_access_fs: handled,
    };
    // SAFETY: `attr` is a valid, initialised ruleset attribute of the size we
    // pass; the kernel copies it before returning.
    let fd = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            &attr as *const RulesetAttr,
            std::mem::size_of::<RulesetAttr>(),
            0u64,
        )
    };
    if fd < 0 {
        return Err(format!(
            "landlock_create_ruleset: {}",
            std::io::Error::last_os_error()
        ));
    }
    let ruleset_fd = fd as c_int;

    let mut rules = 0usize;
    let mut skipped = Vec::new();
    for path in read_only {
        match add_path_rule(ruleset_fd, path, read_only_access()) {
            Ok(true) => rules += 1,
            Ok(false) => skipped.push(path.display().to_string()),
            Err(e) => {
                // SAFETY: `ruleset_fd` is a live descriptor returned above.
                unsafe {
                    libc::close(ruleset_fd);
                }
                return Err(e);
            }
        }
    }
    for path in read_write {
        match add_path_rule(ruleset_fd, path, handled) {
            Ok(true) => rules += 1,
            Ok(false) => skipped.push(path.display().to_string()),
            Err(e) => {
                // SAFETY: see above.
                unsafe {
                    libc::close(ruleset_fd);
                }
                return Err(e);
            }
        }
    }

    // SAFETY: `ruleset_fd` refers to the ruleset built above.
    let rc = unsafe { libc::syscall(libc::SYS_landlock_restrict_self, ruleset_fd, 0u64) };
    let restrict_err = if rc != 0 {
        Some(std::io::Error::last_os_error())
    } else {
        None
    };
    // SAFETY: `ruleset_fd` is still live and no longer needed.
    unsafe {
        libc::close(ruleset_fd);
    }
    if let Some(err) = restrict_err {
        return Err(format!("landlock_restrict_self: {err}"));
    }

    let mut detail = format!("abi {abi}, {rules} rules");
    if !skipped.is_empty() {
        detail.push_str(&format!(", skipped missing {skipped:?}"));
    }
    Ok(detail)
}

/// Add one path-beneath rule. Returns `Ok(false)` for a missing root.
fn add_path_rule(ruleset_fd: c_int, path: &Path, access: u64) -> Result<bool, String> {
    let cpath = CString::new(path.as_os_str().as_bytes())
        .map_err(|e| format!("path {} contains NUL: {e}", path.display()))?;
    // SAFETY: `cpath` is a valid NUL-terminated path; O_PATH|O_CLOEXEC is a
    // read-only lookup that cannot create or modify anything.
    let path_fd = unsafe { libc::open(cpath.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
    if path_fd < 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ENOENT) {
            return Ok(false);
        }
        return Err(format!("open({}) O_PATH: {err}", path.display()));
    }
    let attr = PathBeneathAttr {
        allowed_access: access,
        parent_fd: path_fd,
    };
    // SAFETY: `attr` is a valid packed path-beneath attribute; the path_fd
    // inside it is live for the duration of the call.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_landlock_add_rule,
            ruleset_fd,
            LANDLOCK_RULE_PATH_BENEATH as u64,
            &attr as *const PathBeneathAttr,
            0u64,
        )
    };
    let add_err = if rc != 0 {
        Some(std::io::Error::last_os_error())
    } else {
        None
    };
    // SAFETY: `path_fd` was opened above and is no longer needed.
    unsafe {
        libc::close(path_fd);
    }
    if let Some(err) = add_err {
        return Err(format!("landlock_add_rule({}): {err}", path.display()));
    }
    Ok(true)
}
