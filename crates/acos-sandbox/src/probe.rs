//! Unprivileged capability probe: what can this host actually support.
//!
//! Every field is derived from read-only kernel interfaces
//! (`/proc/sys`, `/sys/fs/cgroup`) or a version syscall. Nothing here changes
//! process state, so it is safe to call before a decision to confine.

use serde::{Deserialize, Serialize};

use crate::linux;

/// What the running kernel/host can support, independent of any profile.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Capabilities {
    /// `uname -r`.
    pub kernel_release: String,
    /// `std::env::consts::ARCH` (e.g. `x86_64`).
    pub arch: String,
    /// Landlock ABI version, or `None` when Landlock is unavailable.
    pub landlock_abi: Option<u32>,
    /// Whether the kernel exposes the seccomp-bpf interface.
    pub seccomp: bool,
    /// Whether unprivileged user namespaces can be created.
    pub unprivileged_userns: bool,
    /// Whether a cgroup-v2 hierarchy is mounted.
    pub cgroup_v2: bool,
}

/// Probe the host. Does not modify the process.
pub fn probe() -> Capabilities {
    Capabilities {
        kernel_release: kernel_release(),
        arch: std::env::consts::ARCH.to_string(),
        landlock_abi: linux::landlock_abi(),
        seccomp: seccomp_available(),
        unprivileged_userns: unprivileged_userns_available(),
        cgroup_v2: std::path::Path::new("/sys/fs/cgroup/cgroup.controllers").exists(),
    }
}

fn kernel_release() -> String {
    std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| std::env::consts::OS.to_string())
}

fn seccomp_available() -> bool {
    // `actions_avail` exists on every kernel that supports seccomp filters;
    // the directory itself is the fallback for older exports.
    std::path::Path::new("/proc/sys/kernel/seccomp/actions_avail").exists()
        || std::path::Path::new("/proc/sys/kernel/seccomp").is_dir()
}

fn unprivileged_userns_available() -> bool {
    // The `unprivileged_userns_clone` knob (Debian/Ubuntu) gates it explicitly.
    if let Ok(value) = std::fs::read_to_string("/proc/sys/kernel/unprivileged_userns_clone") {
        if value.trim() == "0" {
            return false;
        }
    }
    // `max_user_namespaces` is the upstream knob: 0 disables unprivileged
    // creation.
    match std::fs::read_to_string("/proc/sys/user/max_user_namespaces") {
        Ok(value) => value.trim().parse::<u64>().map(|n| n > 0).unwrap_or(true),
        Err(_) => true,
    }
}

/// Probe whether a transient systemd user scope (`systemd-run --user --scope`)
/// can be created for optional cgroup-v2 memory/PID limits.
///
/// This *does* spawn a helper process, so it is deliberately not part of
/// [`probe`]; it is reported by `acos sandbox-probe` and is never required.
pub fn systemd_user_scope_available() -> bool {
    std::process::Command::new("systemd-run")
        .args(["--user", "--scope", "-q", "true"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
