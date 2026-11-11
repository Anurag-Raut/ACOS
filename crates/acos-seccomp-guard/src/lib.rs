//! `LD_PRELOAD` guard for the arm64 qemu transport (W191-A).
//!
//! The arm64 path must `execve` `qemu-aarch64` **after** the other confinement
//! layers are applied, but the ACOS seccomp filter denies `execve` outright
//! (`acos-sandbox/src/linux/seccomp.rs`). The only in-process hook that runs
//! after the exec and before the guest does is the dynamic linker's
//! constructor list, so this crate is a shared object whose `.init_array`
//! constructor installs the filter inside the exec'd `qemu-aarch64` process.
//!
//! The filter is **not** a second copy of the policy: the constructor calls
//! [`acos_sandbox::install_deferred_seccomp`], which is the exact same audited
//! builder `acos_sandbox::confine` installs inline.
//!
//! The transport keeps one pipe write end open across the exec and tells the
//! constructor its number in `ACOS_GUARD_STATUS_FD`; the constructor writes a
//! single `OK ...`/`ERR ...` line and closes it, so the parent can prove the
//! filter really loaded (a failed preload is an EOF, never a fake pass). The
//! process also has `PR_SET_NO_NEW_PRIVS` set and Landlock applied by the
//! launcher before the exec, both of which persist across `execve`.
//!
//! Build it with `tools/build-arm64-guard.sh`; the resulting
//! `libacos_seccomp_guard.so` is then passed as the `SeccompMode::DeferredPreload`
//! guard (or discovered by `acos-arm64` via `ACOS_SECCOMP_GUARD`).
#![deny(unsafe_op_in_unsafe_fn)]

/// The `.init_array` constructor symbol.
///
/// It runs during dynamic-linker initialisation of the preloaded process,
/// after the guest image is loaded but before `main`. It must not unwind: any
/// failure is reported over the status pipe and the process is expected to be
/// treated as untrusted by the parent.
///
/// Excluded from the `cargo test` harness (`cfg(not(test))`): a test binary
/// must not install the deny filter into the test runner.
#[cfg(not(test))]
extern "C" fn acos_seccomp_guard_init() {
    let result = acos_sandbox::install_deferred_seccomp();
    if let Some(fd) = status_fd() {
        let line = match &result {
            Ok(detail) => format!("OK {detail}\n"),
            Err(err) => format!("ERR {err}\n"),
        };
        // SAFETY: `fd` came from `ACOS_GUARD_STATUS_FD` and was deliberately
        // kept open by the launcher; writing to a pipe cannot touch memory we
        // own, and a short write is retried.
        unsafe {
            write_all(fd, line.as_bytes());
            libc::close(fd);
        }
    }
    // `result` is consumed above; keep the binding meaningful if the env var
    // is absent (seccomp is still installed, only the report is skipped).
    let _ = result;
}

/// Parse `ACOS_GUARD_STATUS_FD` into a raw descriptor.
#[cfg(not(test))]
fn status_fd() -> Option<libc::c_int> {
    std::env::var("ACOS_GUARD_STATUS_FD")
        .ok()
        .and_then(|raw| raw.parse::<libc::c_int>().ok())
        .filter(|&fd| fd >= 3)
}

/// `write(2)` until every byte is written or the write fails.
///
/// # Safety
///
/// `fd` must be a valid, writable descriptor for the duration of the call.
#[cfg(not(test))]
unsafe fn write_all(fd: libc::c_int, mut bytes: &[u8]) {
    while !bytes.is_empty() {
        // SAFETY: `bytes` is a live slice and `fd` is a valid descriptor; the
        // kernel only reads from the slice.
        let written = unsafe {
            libc::write(
                fd,
                bytes.as_ptr() as *const libc::c_void,
                bytes.len(),
            )
        };
        if written <= 0 {
            return;
        }
        bytes = &bytes[written as usize..];
    }
}

// Force the constructor into `.init_array`. `#[used]` keeps it from being
// dropped; the section makes the dynamic linker call it. Omitted from test
// harnesses (see the function's `cfg`).
#[cfg(not(test))]
#[used]
#[link_section = ".init_array"]
static ACOS_SECCOMP_GUARD_INIT: extern "C" fn() = acos_seccomp_guard_init;
