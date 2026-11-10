//! Low-level Linux confinement primitives.
//!
//! Every syscall that changes process state lives in this module (and its two
//! submodules). Nothing here is public API: [`crate::confine`] is the single
//! entry point. Each `unsafe` block names the invariant it relies on.

pub mod landlock;
pub mod seccomp;

use std::ffi::CString;
use std::os::raw::{c_int, c_ulong};
use std::path::PathBuf;

use crate::confine::{NamespacePlan, RlimitProfile};
use crate::Layer;

/// Landlock ABI version reported by the kernel, if available.
pub fn landlock_abi() -> Option<u32> {
    landlock::abi()
}

/// `prctl(PR_SET_NO_NEW_PRIVS, 1)`.
pub fn no_new_privs() -> Result<String, String> {
    // SAFETY: `prctl` with PR_SET_NO_NEW_PRIVS takes four integer arguments and
    // returns an int; no pointers are involved.
    let rc = unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1u64, 0u64, 0u64, 0u64) };
    if rc != 0 {
        return Err(format!(
            "prctl(PR_SET_NO_NEW_PRIVS): {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok("PR_SET_NO_NEW_PRIVS=1".to_string())
}

/// Close every inherited file descriptor except those in `keep`.
pub fn close_inherited(keep: &[i32]) -> Result<String, String> {
    let mut keep: Vec<i32> = keep.iter().copied().filter(|&fd| fd >= 3).collect();
    keep.sort_unstable();
    keep.dedup();
    let mut next: u32 = 3;
    for &k in &keep {
        let k = k as u32;
        if k > next {
            close_span(next, k - 1)?;
        }
        next = k.saturating_add(1);
    }
    close_span(next, u32::MAX)?;
    Ok(format!("kept {keep:?}"))
}

fn close_span(first: u32, last: u32) -> Result<(), String> {
    if first > last {
        return Ok(());
    }
    // SAFETY: `close_range` takes three scalar arguments; closing descriptors
    // is idempotent and cannot invalidate memory.
    let rc = unsafe { libc::close_range(first, last, 0) };
    if rc == 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::ENOSYS) {
        // Fallback for kernels without close_range: bounded brute-force close.
        let ceiling = last.min(1 << 20);
        for fd in first..=ceiling {
            // SAFETY: `close` only affects the descriptor table.
            unsafe {
                libc::close(fd as c_int);
            }
        }
        return Ok(());
    }
    Err(format!("close_range({first},{last}): {err}"))
}

/// Clear the environment and install exactly `pairs`.
pub fn sanitize_env(pairs: &[(String, String)]) -> Result<String, String> {
    // SAFETY: `clearenv` replaces the process environment pointer; no Rust
    // borrow of `environ` is live.
    let rc = unsafe { libc::clearenv() };
    if rc != 0 {
        return Err(format!(
            "clearenv: {}",
            std::io::Error::last_os_error()
        ));
    }
    for (key, value) in pairs {
        let ck = CString::new(key.as_str()).map_err(|e| format!("env key {key:?}: {e}"))?;
        let cv = CString::new(value.as_str()).map_err(|e| format!("env value for {key:?}: {e}"))?;
        // SAFETY: both CStrings are NUL-terminated and outlive the call.
        let rc = unsafe { libc::setenv(ck.as_ptr(), cv.as_ptr(), 1) };
        if rc != 0 {
            return Err(format!(
                "setenv({key}): {}",
                std::io::Error::last_os_error()
            ));
        }
    }
    Ok(format!("cleared; set {} vars", pairs.len()))
}

/// Apply the resource limits in `profile`.
pub fn apply_rlimits(profile: &RlimitProfile) -> Result<String, String> {
    let mut applied = Vec::new();
    if let Some(value) = profile.address_space_bytes {
        set_limit(libc::RLIMIT_AS, value)?;
        applied.push(format!("AS={value}"));
    }
    if let Some(value) = profile.nofile {
        set_limit(libc::RLIMIT_NOFILE, value)?;
        applied.push(format!("NOFILE={value}"));
    }
    if let Some(value) = profile.nproc {
        set_limit(libc::RLIMIT_NPROC, value)?;
        applied.push(format!("NPROC={value}"));
    }
    if let Some(value) = profile.cpu_seconds {
        set_limit(libc::RLIMIT_CPU, value)?;
        applied.push(format!("CPU={value}"));
    }
    if let Some(value) = profile.file_size_bytes {
        set_limit(libc::RLIMIT_FSIZE, value)?;
        applied.push(format!("FSIZE={value}"));
    }
    if let Some(value) = profile.stack_bytes {
        set_limit(libc::RLIMIT_STACK, value)?;
        applied.push(format!("STACK={value}"));
    }
    if let Some(value) = profile.memlock_bytes {
        set_limit(libc::RLIMIT_MEMLOCK, value)?;
        applied.push(format!("MEMLOCK={value}"));
    }
    set_limit(libc::RLIMIT_CORE, profile.core)?;
    applied.push(format!("CORE={}", profile.core));
    Ok(applied.join(" "))
}

fn set_limit(resource: u32, value: u64) -> Result<(), String> {
    let res = resource as libc::__rlimit_resource_t;
    let mut current = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `current` is a valid out-parameter of the right type.
    let rc = unsafe { libc::getrlimit(res, &mut current) };
    if rc != 0 {
        return Err(format!(
            "getrlimit({resource}): {}",
            std::io::Error::last_os_error()
        ));
    }
    let want = value as libc::rlim_t;
    let capped = if current.rlim_max == libc::RLIM_INFINITY {
        want
    } else {
        want.min(current.rlim_max)
    };
    let limit = libc::rlimit {
        rlim_cur: capped,
        rlim_max: capped,
    };
    // SAFETY: `limit` is a valid in-parameter; setrlimit copies it.
    let rc = unsafe { libc::setrlimit(res, &limit) };
    if rc != 0 {
        return Err(format!(
            "setrlimit({resource}={capped}): {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

/// Attempt the requested namespaces. Returns one entry per requested layer.
pub fn setup_namespaces(plan: &NamespacePlan) -> Vec<(Layer, Result<String, String>)> {
    let mut out = Vec::new();
    if !plan.any() {
        return out;
    }
    // Capture the caller's credentials before entering the user namespace.
    // SAFETY: `getuid`/`getgid` are trivial, always-successful queries.
    let host_uid = unsafe { libc::getuid() };
    let host_gid = unsafe { libc::getgid() };

    let mut flags: c_int = 0;
    if plan.user {
        flags |= libc::CLONE_NEWUSER;
    }
    if plan.net {
        flags |= libc::CLONE_NEWNET;
    }
    if plan.mount {
        flags |= libc::CLONE_NEWNS;
    }
    // SAFETY: `unshare` takes a flag word; it changes only the current task's
    // namespaces.
    let rc = unsafe { libc::unshare(flags) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        let message = format!("unshare({flags:#x}): {err}");
        for (layer, requested) in [
            (Layer::UserNamespace, plan.user),
            (Layer::NetNamespace, plan.net),
            (Layer::MountNamespace, plan.mount),
        ] {
            if requested {
                out.push((layer, Err(message.clone())));
            }
        }
        return out;
    }

    if plan.user {
        out.push((Layer::UserNamespace, map_user(host_uid, host_gid)));
    }
    if plan.net {
        out.push((Layer::NetNamespace, Ok("CLONE_NEWNET".to_string())));
    }
    if plan.mount {
        out.push((Layer::MountNamespace, make_mounts_private()));
    }
    out
}

fn map_user(uid: u32, gid: u32) -> Result<String, String> {
    // `setgroups` does not exist on very old kernels; ENOENT is harmless.
    let _ = std::fs::write("/proc/self/setgroups", "deny");
    std::fs::write("/proc/self/uid_map", format!("0 {uid} 1\n"))
        .map_err(|e| format!("write uid_map: {e}"))?;
    std::fs::write("/proc/self/gid_map", format!("0 {gid} 1\n"))
        .map_err(|e| format!("write gid_map: {e}"))?;
    Ok(format!("0 {uid} 1 / 0 {gid} 1"))
}

fn make_mounts_private() -> Result<String, String> {
    let target = CString::new("/").expect("static string has no NUL");
    // SAFETY: `target` is a valid C string; the other arguments are NULL as
    // required by MS_REC|MS_PRIVATE (no source, no fstype, no data).
    let rc = unsafe {
        libc::mount(
            std::ptr::null(),
            target.as_ptr(),
            std::ptr::null(),
            (libc::MS_REC | libc::MS_PRIVATE) as c_ulong,
            std::ptr::null(),
        )
    };
    if rc != 0 {
        return Err(format!(
            "mount(NULL, \"/\", MS_REC|MS_PRIVATE): {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok("private mount propagation".to_string())
}

/// Apply a Landlock ruleset from two root lists.
pub fn apply_landlock(ro: &[PathBuf], rw: &[PathBuf]) -> Result<String, String> {
    landlock::restrict(ro, rw)
}

/// Install the seccomp-bpf syscall filter.
pub fn install_seccomp() -> Result<String, String> {
    seccomp::install()
}
