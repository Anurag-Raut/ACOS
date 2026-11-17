//! # `acos-sandbox`
//!
//! The ACOS confinement layer. It is the **only** ACOS crate allowed to hold
//! the `unsafe`/`libc` calls needed to enter a sandbox (see
//! `DECISIONS.md` ADR-0022). No sudo and no kernel/system changes are required:
//! every layer is an unprivileged Linux facility.
//!
//! [`confine`] applies, in order, the layers an untrusted APK native library
//! needs to be contained inside the ACOS session process:
//!
//! 1. `prctl(PR_SET_NO_NEW_PRIVS)` — makes the following layers un-removable;
//! 2. close inherited file descriptors (except an explicit keep list);
//! 3. a sanitised environment (`clearenv` + a minimal set);
//! 4. resource limits (address space, `NOFILE`, `NPROC`, CPU, `CORE=0`,
//!    `FSIZE`, `STACK`, `MEMLOCK`);
//! 5. optional unprivileged user/network/mount namespaces;
//! 6. a Landlock filesystem ruleset: read-only for system VFS roots and the
//!    APK directory, read-write only for the app data directory, nothing else;
//! 7. a seccomp-bpf filter that denies `exec*`, `ptrace`, `process_vm_*`,
//!    `mount`/`unshare`/`setns`, the new mount API, `open_by_handle_at`,
//!    `io_uring_*`, `userfaultfd`, the pidfd family, `kcmp`, `perf_event_open`,
//!    `keyctl`, module ops, `setuid`/`capset`, and every socket family except
//!    `AF_UNIX`; `clone` namespace flags, foreign-process signals and
//!    `personality` are argument-filtered, while futex, thread creation,
//!    `mmap`/`mprotect` (including `PROT_EXEC`), clocks, eventfd and epoll stay
//!    available. Optional Landlock ABI>=6 scopes additionally confine signals
//!    and abstract UNIX sockets to the sandbox domain.
//!
//! It **fails closed**: if any layer listed in [`ConfineProfile::required`]
//! cannot be established, [`confine`] returns a structured
//! [`ConfineError`] with code [`CONFINEMENT_UNAVAILABLE`]. The
//! [`ConfinementReport`] always says exactly which layers are active.
//!
//! Optional cgroup-v2 memory/PID limits via `systemd-run --user --scope` are
//! *probed* ([`Capabilities`]) but never required and never part of the fatal
//! path.
//!
//! ## Never fake
//!
//! [`confine`] never reports a layer as active unless the kernel accepted it.
//! A process that has not called [`confine`] reports
//! [`confinement_active`]`() == false`, which is what the native loader checks
//! before it maps an APK `.so`.
#![deny(unsafe_op_in_unsafe_fn)]
#![allow(clippy::missing_safety_doc)]

use serde::{Deserialize, Serialize};

mod confine;
pub mod error;
mod linux;
mod live;
mod probe;

pub use confine::{
    confine, confinement_active, ConfineProfile, ConfinementReport, LayerReport, LayerStatus,
    NamespacePlan, RlimitProfile, SeccompMode,
};
pub use error::{ConfineError, CONFINEMENT_FAILED, CONFINEMENT_UNAVAILABLE};
pub use live::probe_live;
pub use probe::{probe, systemd_user_scope_available, Capabilities};

/// The kernel Landlock ABI this host supports (`None` when Landlock is absent).
///
/// Callers that install a ruleset *outside* this process — the deferred
/// in-guest install in `acos-art` — use this to decide whether the layer is
/// required (kernel supports it ⇒ fail closed) or must fall back to the
/// namespace/mount view with the gap documented.
pub fn landlock_abi() -> Option<u32> {
    linux::landlock_abi()
}

/// Serialise a Landlock ruleset for a deferred install in another process.
///
/// The returned plan is the crate's single Landlock policy expressed as the
/// line-oriented guest format the freestanding applier reads (see
/// `linux::landlock::plan`). Fails when the host has no Landlock at all; a
/// caller with no in-process `confine` (the ART guest) must then document the
/// fallback instead of pretending the layer is active.
pub fn landlock_guest_plan(read_only: &[&str], read_write: &[&str]) -> Result<Vec<u8>, String> {
    linux::landlock_plan(read_only, read_write)
}

/// Install the audited seccomp-bpf filter in the current process.
///
/// This is the entry point an `LD_PRELOAD` guard calls from its constructor
/// after `execve` when the session runs with
/// [`SeccompMode::DeferredPreload`](confine::SeccompMode::DeferredPreload). It
/// installs exactly the same filter [`confine`] installs inline — there is one
/// policy, not a second copy — and returns the same human-readable detail
/// string. The caller must arrange for the process to have
/// `PR_SET_NO_NEW_PRIVS` set and Landlock applied first (which [`confine`]
/// already did in the parent before the `execve`).
///
/// Fails closed: an error return means the filter was **not** installed and
/// the process must not be trusted.
pub fn install_deferred_seccomp() -> Result<String, String> {
    linux::install_seccomp()
}

/// A confinement layer that can be independently probed and reported.
///
/// The `snake_case` serialisation is the stable wire name used by
/// `acos sandbox-probe` and by the ADR-0022 report.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    /// `prctl(PR_SET_NO_NEW_PRIVS)`.
    NoNewPrivs,
    /// Closing inherited file descriptors.
    CloseFds,
    /// Clearing and re-seeding the process environment.
    SanitizeEnv,
    /// Address-space / file / process / CPU / core resource limits.
    Rlimits,
    /// Unprivileged user namespace.
    UserNamespace,
    /// Network namespace.
    NetNamespace,
    /// Mount namespace.
    MountNamespace,
    /// Landlock filesystem access ruleset.
    Landlock,
    /// seccomp-bpf syscall filter.
    Seccomp,
}

impl Layer {
    /// Stable machine-readable name (`snake_case`).
    pub fn as_str(self) -> &'static str {
        match self {
            Layer::NoNewPrivs => "no_new_privs",
            Layer::CloseFds => "close_fds",
            Layer::SanitizeEnv => "sanitize_env",
            Layer::Rlimits => "rlimits",
            Layer::UserNamespace => "user_namespace",
            Layer::NetNamespace => "net_namespace",
            Layer::MountNamespace => "mount_namespace",
            Layer::Landlock => "landlock",
            Layer::Seccomp => "seccomp",
        }
    }

    /// The layers that must all be active for a process to count as confined.
    pub const BASE: [Layer; 3] = [Layer::NoNewPrivs, Layer::Landlock, Layer::Seccomp];

    /// The full fail-closed layer set: process hygiene plus the two kernel
    /// enforcement layers. [`confinement_active`](crate::confinement_active)
    /// stays false unless every one of these is active, so a profile whose
    /// hygiene steps failed (or a probe with no `required` layers) can never
    /// make the native loader trust an untrusted `.so` (W67A #9).
    pub const TRUSTED: [Layer; 6] = [
        Layer::NoNewPrivs,
        Layer::CloseFds,
        Layer::SanitizeEnv,
        Layer::Rlimits,
        Layer::Landlock,
        Layer::Seccomp,
    ];
}

impl std::fmt::Display for Layer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
