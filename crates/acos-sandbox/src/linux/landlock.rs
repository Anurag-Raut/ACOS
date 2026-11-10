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

/// Rights that only apply to a directory. The kernel's `landlock_add_rule`
/// rejects a path-beneath rule for a non-directory path when the mask carries
/// any of these (`EINVAL`), so they must be masked out per path type. A
/// non-directory path may only be granted `EXECUTE|WRITE_FILE|READ_FILE|
/// TRUNCATE|IOCTL_DEV`.
const DIRECTORY_ONLY_RIGHTS: u64 = FS_READ_DIR
    | FS_REMOVE_DIR
    | FS_REMOVE_FILE
    | FS_MAKE_CHAR
    | FS_MAKE_DIR
    | FS_MAKE_REG
    | FS_MAKE_SOCK
    | FS_MAKE_FIFO
    | FS_MAKE_BLOCK
    | FS_MAKE_SYM
    | FS_REFER;

// Landlock ABI 6 scopes: restrict IPCs (signals, abstract UNIX sockets) to the
// sandbox domain. `landlock_ruleset_attr.scoped`.
const LANDLOCK_SCOPE_ABSTRACT_UNIX_SOCKET: u64 = 1 << 0;
const LANDLOCK_SCOPE_SIGNAL: u64 = 1 << 1;

#[repr(C)]
struct RulesetAttr {
    handled_access_fs: u64,
    handled_access_net: u64,
    scoped: u64,
}

/// The `landlock_ruleset_attr` prefix length understood by `abi`. Passing a
/// shorter prefix keeps the call valid on older kernels.
fn ruleset_attr_size(abi: u32) -> usize {
    if abi >= 6 {
        std::mem::size_of::<RulesetAttr>()
    } else if abi >= 4 {
        std::mem::size_of::<RulesetAttr>() - 8
    } else {
        8
    }
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

/// Create a ruleset for `abi`, returning its fd and the IPC scope mask.
fn create_ruleset(abi: u32) -> Result<(c_int, u64), String> {
    let handled = handled_fs(abi);
    // Landlock ABI 6 extends the ruleset with `scoped`, which confines signals
    // and abstract UNIX sockets to the sandbox domain: a confined guest then
    // cannot `kill` a host same-uid process or `connect` to a host abstract
    // socket even if seccomp were bypassed (W67A #5). Older kernels ignore it.
    let scope = if abi >= 6 {
        LANDLOCK_SCOPE_ABSTRACT_UNIX_SOCKET | LANDLOCK_SCOPE_SIGNAL
    } else {
        0
    };
    let attr = RulesetAttr {
        handled_access_fs: handled,
        handled_access_net: 0,
        scoped: scope,
    };
    let attr_size = ruleset_attr_size(abi);
    // SAFETY: `attr` is a valid, initialised ruleset attribute; only the
    // documented `attr_size` prefix is read by the kernel, which copies it
    // before returning.
    let fd = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            &attr as *const RulesetAttr,
            attr_size,
            0u64,
        )
    };
    if fd < 0 {
        return Err(format!(
            "landlock_create_ruleset: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok((fd as c_int, scope))
}

/// Create and enforce a ruleset. Missing roots are skipped (never widened).
pub fn restrict(read_only: &[PathBuf], read_write: &[PathBuf]) -> Result<String, String> {
    let abi = abi().ok_or_else(|| "landlock_create_ruleset(VERSION) unsupported".to_string())?;
    let (ruleset_fd, scope) = create_ruleset(abi)?;

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
        match add_path_rule(ruleset_fd, path, handled_fs(abi)) {
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
    if scope != 0 {
        detail.push_str(", scoped abstract_unix_socket+signal");
    } else {
        detail.push_str(", scopes unsupported (abi < 6)");
    }
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
    // `landlock_add_rule` rejects directory-only rights on a non-directory
    // path (`EINVAL`). Classify the opened object rather than the spelling so
    // symlinks are classified by their target.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `path_fd` is a live O_PATH descriptor; `fstat` only fills `st`.
    let is_dir = unsafe { libc::fstat(path_fd, &mut st) } == 0
        && (st.st_mode & libc::S_IFMT) == libc::S_IFDIR;
    let allowed_access = if is_dir {
        access
    } else {
        access & !DIRECTORY_ONLY_RIGHTS
    };
    let attr = PathBeneathAttr {
        allowed_access,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// W106-A: a read-only grant for the device nodes Hermes opens
    /// (`/dev/urandom`, `/dev/null`) must not carry directory-only rights.
    /// The kernel rejects those on non-directories with `EINVAL`; this test
    /// builds a ruleset and adds the rules without applying it to the test
    /// process, so it exercises the real `landlock_add_rule` call.
    #[test]
    fn device_nodes_accept_read_only_path_rules() {
        let Some(abi) = abi() else {
            eprintln!("SKIP landlock device-node rule test: Landlock unavailable");
            return;
        };
        let (ruleset_fd, _scope) = create_ruleset(abi).expect("create landlock ruleset");
        for raw in ["/dev/urandom", "/dev/null"] {
            let path = Path::new(raw);
            if !path.exists() {
                eprintln!("SKIP {raw}: missing on this host");
                continue;
            }
            match add_path_rule(ruleset_fd, path, read_only_access()) {
                Ok(true) => {}
                Ok(false) => panic!("{raw} disappeared between exists() and add_path_rule"),
                Err(e) => panic!("landlock_add_rule({raw}): {e}"),
            }
        }
        // SAFETY: the ruleset fd was returned by create_ruleset and is no
        // longer needed. The ruleset is never installed via
        // `landlock_restrict_self`, so this test does not confine the runner.
        unsafe {
            libc::close(ruleset_fd);
        }
    }

    #[test]
    fn directory_only_rights_are_masked_for_files() {
        assert_eq!(read_only_access() & DIRECTORY_ONLY_RIGHTS, FS_READ_DIR);
        // A directory keeps the directory-only rights; a file loses them.
        assert_eq!(read_only_access() & !DIRECTORY_ONLY_RIGHTS, FS_EXECUTE | FS_READ_FILE);
    }
}
