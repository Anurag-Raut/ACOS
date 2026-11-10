//! Fork-and-confine live probe, used by `acos sandbox-probe`.
//!
//! Forks a child that applies the profile and reports the resulting layer
//! statuses back over a pipe. The parent is never confined, so the probe can
//! run inside a normal process. This is safe to call while single-threaded;
//! callers that are already multi-threaded should rely on the static
//! [`probe`](crate::probe) instead.

use std::os::raw::{c_int, c_void};

use serde::{Deserialize, Serialize};

use crate::confine::{confine, ConfinementReport};
use crate::error::ConfineError;
use crate::ConfineProfile;

#[derive(Serialize, Deserialize)]
enum LiveOutcome {
    Report(ConfinementReport),
    Error { code: String, message: String },
}

/// Apply `profile` in a forked child and return the child's report.
///
/// The child's `keep_fds` is extended with the report pipe so fd hygiene in
/// the child cannot sever the report channel.
pub fn probe_live(profile: &ConfineProfile) -> Result<ConfinementReport, ConfineError> {
    let mut fds = [0 as c_int; 2];
    // SAFETY: `fds` is a valid two-element out-parameter array.
    let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
    if rc != 0 {
        return Err(unavailable(format!(
            "pipe: {}",
            std::io::Error::last_os_error()
        )));
    }
    let (read_fd, write_fd) = (fds[0], fds[1]);

    // SAFETY: `fork` has no arguments. The child runs a bounded code path and
    // terminates with `_exit`; it never returns into the parent's stack.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        // SAFETY: both descriptors are live and owned here.
        unsafe {
            libc::close(read_fd);
            libc::close(write_fd);
        }
        return Err(unavailable(format!(
            "fork: {}",
            std::io::Error::last_os_error()
        )));
    }
    if pid == 0 {
        // Child: report through the pipe, then leave immediately.
        // SAFETY: the read end is not needed in the child.
        unsafe {
            libc::close(read_fd);
        }
        let mut child_profile = profile.clone();
        if !child_profile.keep_fds.contains(&write_fd) {
            child_profile.keep_fds.push(write_fd);
        }
        let outcome = match confine(&child_profile) {
            Ok(report) => LiveOutcome::Report(report),
            Err(e) => LiveOutcome::Error {
                code: e.code().to_string(),
                message: e.to_string(),
            },
        };
        let payload = serde_json::to_vec(&outcome).unwrap_or_default();
        write_all(write_fd, &payload);
        // SAFETY: `_exit` terminates without running destructors or atexit.
        unsafe {
            libc::_exit(0);
        }
    }

    // Parent.
    // SAFETY: the write end is closed in the parent; the child owns its copy.
    unsafe {
        libc::close(write_fd);
    }
    let payload = read_all(read_fd);
    // SAFETY: `read_fd` is live.
    unsafe {
        libc::close(read_fd);
    }
    let mut status: c_int = 0;
    // SAFETY: `pid` is a live child and `status` is a valid out-parameter.
    unsafe {
        libc::waitpid(pid, &mut status, 0);
    }

    match serde_json::from_slice::<LiveOutcome>(&payload) {
        Ok(LiveOutcome::Report(report)) => Ok(report),
        Ok(LiveOutcome::Error { code, message }) => Err(ConfineError::Unavailable {
            missing: Vec::new(),
            message: format!("{code}: {message}"),
        }),
        Err(e) => Err(unavailable(format!(
            "invalid live probe output: {e} ({})",
            String::from_utf8_lossy(&payload)
        ))),
    }
}

fn unavailable(message: String) -> ConfineError {
    ConfineError::Unavailable {
        missing: Vec::new(),
        message,
    }
}

fn write_all(fd: c_int, mut buf: &[u8]) {
    while !buf.is_empty() {
        // SAFETY: `buf` is a valid slice and `fd` is a live descriptor.
        let n = unsafe { libc::write(fd, buf.as_ptr() as *const c_void, buf.len()) };
        if n <= 0 {
            break;
        }
        buf = &buf[n as usize..];
    }
}

fn read_all(fd: c_int) -> Vec<u8> {
    let mut out = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        // SAFETY: `chunk` is a valid mutable buffer and `fd` is live.
        let n = unsafe { libc::read(fd, chunk.as_mut_ptr() as *mut c_void, chunk.len()) };
        if n <= 0 {
            break;
        }
        out.extend_from_slice(&chunk[..n as usize]);
    }
    out
}
