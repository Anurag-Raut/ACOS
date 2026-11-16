//! Guest-side seccomp-bpf: the syscall filter ART runs under (S5).
//!
//! `results/art-runtime/S1-sandbox-review.md` §5 #5: the bwrap route applied no
//! syscall filter, so a guest `.so` could reach `ptrace`, `mount`, namespace
//! creation, `io_uring`, `bpf` and non-`AF_UNIX` sockets. The filter cannot be
//! installed in the launcher's `pre_exec` (bwrap itself needs `mount`,
//! `unshare`, `execve`), so it is installed **inside the guest**, after bwrap's
//! mount setup and immediately before ART's `main`, by a freestanding
//! `LD_PRELOAD` constructor — the same deferred-preload pattern
//! `crates/acos-seccomp-guard` uses for the arm64 qemu transport.
//!
//! Two deliberate differences from that guard, both forced by the ART guest:
//!
//! * **bwrap installs the filter, the preload only logs denials.** The guest's
//!   dynamic linker is bionic (`/system/apex/com.android.runtime/bin/linker64`),
//!   so a Rust `cdylib` (host glibc) cannot be preloaded; the logging stub is
//!   freestanding C (the shape of `shim/artshim.c`). Installing the filter
//!   itself through `seccomp(2)` from that stub is not needed either: bwrap
//!   0.13.1 supports `--seccomp FD`, which it applies in the sandbox child
//!   after the mounts and immediately before the `exec` of the guest — the same
//!   deferred point, verified to accept the raw `struct sock_filter[]` blob
//!   (an 8-byte multiple; a length prefix is rejected).
//! * **The policy lives here, in Rust, not in the C file.** [`build_blob`]
//!   assembles the classic-BPF program from [`deny_list`] with the same
//!   semantics as `acos-sandbox/src/linux/seccomp.rs` (which the ART path
//!   cannot reuse: it is installed by its `confine` profile, before bwrap's own
//!   `mount`/`unshare`/`execve`). `cargo test` covers the policy; the C stub is
//!   a SIGSYS handler plus an append-only log.
//!
//! ## Deny action and logging (measured, see S5.md)
//!
//! The deny action is `SECCOMP_RET_ERRNO | EACCES`: a forbidden syscall returns
//! `EACCES` to the guest, which can handle it, and the launcher reports the
//! policy (`seccomp: guard=active insns=N`) in the run diagnostics.
//!
//! `SECCOMP_RET_TRAP` + a SIGSYS handler that appends `DENY syscall=<nr>` to
//! `/data/out/seccomp.log` was implemented first and **rejected on evidence**:
//! the handler installed from the preload constructor is not in effect when the
//! denial fires (a probe that installs its own SIGSYS handler sees the trap and
//! the correct syscall number; the same probe without that handler dies of
//! `SIGSEGV`, and no `DENY` line is written). `RET_ERRNO` is the only action
//! whose effect is observable without `CAP_SYS_ADMIN` (a bwrap userns has none,
//! so `SECCOMP_FILTER_FLAG_LOG` and the audit log are unavailable). Denials are
//! logged *by the guest* (the adversarial probe records the errno; that text is
//! part of the run result), not by the kernel.
//!
//! The `acos-sandbox` filter also argument-filters `tgkill`/`rt_*sigqueueinfo`
//! against the pid captured at filter-build time. That is impossible here: the
//! program is assembled by the **host** launcher, before the guest exists, so
//! the guest's pid is unknowable. `kill`/`tkill` stay denied outright (so no
//! process outside the thread group can be signalled through them), `tgkill` is
//! allowed for the SIGSYS re-raise, and the guest pid namespace (`--unshare-pid`)
//! is what bounds signalling in the first place.
//!
//! The filter **fails closed**: if the kernel rejects the program, bwrap exits
//! with an error and the guest never starts; if the logging stub was not
//! accepted by bionic, `/data/out/seccomp.log` has no `OK guard=active` line
//! and the launcher turns that into [`crate::Error::SeccompUnavailable`].
//! Tamper caveat: the log lives in the run root, which the guest can write; it
//! is evidence of *observed denials*, not a tamper-proof audit trail.
//!
//! ## S6: the same constructor applies Landlock first
//!
//! [`crate::landlock`] writes the guest ruleset plan (assembled by
//! `acos-sandbox`, one policy in the tree) into the same `<data>/seccomp`
//! directory. The constructor applies it with the raw Landlock syscalls
//! **before** installing this filter, so after the constructor returns both
//! kernel layers are in effect and the guest cannot widen either. The evidence
//! line is `OK landlock=active abi=N rules=M`; [`require_guard`] fails closed
//! when it is missing on a kernel that supports Landlock.
//!
//! ## Audit mode (`ACOS_ART_SECCOMP_AUDIT=kill`)
//!
//! The production deny action is `EACCES`. For the denial audit in
//! `results/art-runtime/S6.md`, setting this variable rebuilds the blob with
//! `SECCOMP_RET_KILL_PROCESS` for the same deny list: a run that completes
//! normally then proves that **no** denied syscall was attempted at all (a
//! single attempt would die of `SIGSYS` instead). It is an audit instrument,
//! not a production default, and it is part of the artefact hash so the two
//! blobs never alias in the cache.

use crate::{Error, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Guest path of the Landlock plan bound alongside the seccomp artefacts.
pub use crate::landlock::GUEST_PLAN;

/// Freestanding guest stub, compiled on demand into the data dir.
pub const GUEST_SOURCE: &str = include_str!("seccomp_guest.c");

/// Guest paths of the two artefacts (bound read-only into every filtered run).
pub const GUEST_SO: &str = "/data/seccomp/libacos_seccomp_guest.so";
pub const GUEST_BLOB: &str = "/data/seccomp/seccomp.bpf";

// Classic-BPF opcodes.
const LD_ABS_W: u16 = 0x20;
const JEQ_K: u16 = 0x15;
const JA: u16 = 0x05;
const RET_K: u16 = 0x06;
const AND_K: u16 = 0x54;

const RET_KILL_PROCESS: u32 = 0x8000_0000;
const RET_ERRNO: u32 = 0x0005_0000;
const RET_ALLOW: u32 = 0x7fff_0000;

// `struct seccomp_data` offsets.
const OFF_NR: u32 = 0;
const OFF_ARCH: u32 = 4;
const OFF_ARG0: u32 = 16;

#[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
const AUDIT_ARCH: u32 = 0xC000_003E;
#[cfg(target_arch = "aarch64")]
const AUDIT_ARCH: u32 = 0xC000_00B7;
#[cfg(not(any(target_arch = "x86_64", target_arch = "x86", target_arch = "aarch64")))]
compile_error!("acos-art: no AUDIT_ARCH for this target_arch (seccomp_guest.c is x86_64-only)");

/// `CLONE_NEWNS|NEWCGROUP|NEWUTS|NEWIPC|NEWUSER|NEWPID|NEWNET`.
const CLONE_NEW_MASK: u32 = 0x0002_0000
    | 0x0200_0000
    | 0x0400_0000
    | 0x0800_0000
    | 0x1000_0000
    | 0x2000_0000
    | 0x4000_0000;

const PER_LINUX: u32 = 0;
const PERSONALITY_QUERY: u32 = 0xffff_ffff;

#[cfg(target_arch = "aarch64")]
const SYS_KEXEC_FILE_LOAD: u32 = 294;
#[cfg(not(target_arch = "aarch64"))]
const SYS_KEXEC_FILE_LOAD: u32 = libc::SYS_kexec_file_load as u32;

/// Syscalls denied outright. Mirrors `acos-sandbox`'s audited list; everything
/// ART needs (`futex`, `mmap`/`mprotect` with `PROT_EXEC`, `memfd_create`,
/// `madvise`, `prctl`, `open*`, `read`/`write`, `epoll`, `AF_UNIX` sockets,
/// thread-creating `clone`) stays available on purpose.
pub fn deny_list() -> Vec<u32> {
    let mut list = vec![
        // NOTE: `execve`/`execveat` are *not* denied here. bwrap applies this
        // filter in the sandbox child immediately before it `exec`s the guest,
        // so denying exec would kill bwrap itself. Exec'ing another program
        // inside the mount view is not a host escape (the new image inherits
        // the filter, the mount view, the cgroup and the namespaces); the
        // requirement list for the ART route does not ask for it either.
        libc::SYS_ptrace as u32,
        libc::SYS_process_vm_readv as u32,
        libc::SYS_process_vm_writev as u32,
        libc::SYS_process_madvise as u32,
        libc::SYS_kcmp as u32,
        libc::SYS_mount as u32,
        libc::SYS_umount2 as u32,
        libc::SYS_pivot_root as u32,
        libc::SYS_chroot as u32,
        libc::SYS_unshare as u32,
        libc::SYS_setns as u32,
        libc::SYS_mount_setattr as u32,
        libc::SYS_fsopen as u32,
        libc::SYS_fsconfig as u32,
        libc::SYS_fsmount as u32,
        libc::SYS_fspick as u32,
        libc::SYS_move_mount as u32,
        libc::SYS_open_tree as u32,
        libc::SYS_open_by_handle_at as u32,
        libc::SYS_name_to_handle_at as u32,
        libc::SYS_io_uring_setup as u32,
        libc::SYS_io_uring_enter as u32,
        libc::SYS_io_uring_register as u32,
        libc::SYS_userfaultfd as u32,
        libc::SYS_bpf as u32,
        libc::SYS_perf_event_open as u32,
        libc::SYS_kexec_load as u32,
        SYS_KEXEC_FILE_LOAD,
        libc::SYS_init_module as u32,
        libc::SYS_finit_module as u32,
        libc::SYS_delete_module as u32,
        libc::SYS_reboot as u32,
        libc::SYS_keyctl as u32,
        libc::SYS_add_key as u32,
        libc::SYS_request_key as u32,
        libc::SYS_setuid as u32,
        libc::SYS_setgid as u32,
        libc::SYS_setreuid as u32,
        libc::SYS_setregid as u32,
        libc::SYS_setresuid as u32,
        libc::SYS_setresgid as u32,
        libc::SYS_setfsuid as u32,
        libc::SYS_setfsgid as u32,
        libc::SYS_setgroups as u32,
        libc::SYS_capset as u32,
        libc::SYS_pidfd_open as u32,
        libc::SYS_pidfd_getfd as u32,
        libc::SYS_pidfd_send_signal as u32,
        libc::SYS_kill as u32,
        libc::SYS_tkill as u32,
        libc::SYS_lookup_dcookie as u32,
        libc::SYS_quotactl as u32,
        libc::SYS_swapon as u32,
        libc::SYS_swapoff as u32,
        libc::SYS_syslog as u32,
        libc::SYS_acct as u32,
    ];
    #[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
    {
        list.push(libc::SYS_ioperm as u32);
        list.push(libc::SYS_iopl as u32);
        list.push(libc::SYS_modify_ldt as u32);
    }
    list.sort_unstable();
    list.dedup();
    list
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Insn {
    code: u16,
    jt: u8,
    jf: u8,
    k: u32,
}

type Label = usize;

enum Op {
    Ld(u32),
    And(u32),
    Jeq { k: u32, t: Label, f: Label },
    Ja(Label),
    Ret(u32),
}

struct Builder {
    ops: Vec<Op>,
    labels: Vec<Option<usize>>,
}

impl Builder {
    fn new() -> Self {
        Self { ops: Vec::new(), labels: Vec::new() }
    }
    fn label(&mut self) -> Label {
        self.labels.push(None);
        self.labels.len() - 1
    }
    fn mark(&mut self, l: Label) {
        debug_assert!(self.labels[l].is_none());
        self.labels[l] = Some(self.ops.len());
    }
    fn ld_abs(&mut self, off: u32) {
        self.ops.push(Op::Ld(off));
    }
    fn and(&mut self, k: u32) {
        self.ops.push(Op::And(k));
    }
    fn jeq(&mut self, k: u32, t: Label, f: Label) {
        self.ops.push(Op::Jeq { k, t, f });
    }
    fn ja(&mut self, t: Label) {
        self.ops.push(Op::Ja(t));
    }
    fn ret(&mut self, k: u32) {
        self.ops.push(Op::Ret(k));
    }
    fn finish(self) -> Vec<Insn> {
        let target = |l: Label| self.labels[l].expect("label never marked");
        self.ops
            .iter()
            .enumerate()
            .map(|(pc, op)| match *op {
                Op::Ld(off) => Insn { code: LD_ABS_W, jt: 0, jf: 0, k: off },
                Op::And(k) => Insn { code: AND_K, jt: 0, jf: 0, k },
                Op::Jeq { k, t, f } => {
                    let jt = target(t) - pc - 1;
                    let jf = target(f) - pc - 1;
                    debug_assert!(target(t) > pc && target(f) > pc, "backward jump");
                    Insn { code: JEQ_K, jt: jt as u8, jf: jf as u8, k }
                }
                Op::Ja(t) => {
                    debug_assert!(target(t) > pc);
                    Insn { code: JA, jt: 0, jf: 0, k: (target(t) - pc - 1) as u32 }
                }
                Op::Ret(k) => Insn { code: RET_K, jt: 0, jf: 0, k },
            })
            .collect()
    }
}

/// The deny action: `EACCES` in production, `KILL_PROCESS` for the S6 audit.
///
/// `Kill(Some(nrs))` kills only the listed syscall numbers and keeps `EACCES`
/// for the rest, which is what lets the audit attribute a single denial to one
/// syscall (kill one candidate at a time; a run that still completes proves
/// that syscall was not attempted).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DenyAction {
    /// `SECCOMP_RET_ERRNO | EACCES` (production).
    Errno,
    /// `SECCOMP_RET_KILL_PROCESS` (audit only: a denial is fatal and loud).
    Kill(Option<Vec<u32>>),
}

impl DenyAction {
    /// Stable hash component so two audit blobs never alias in the cache.
    fn to_u64(&self) -> u64 {
        match self {
            DenyAction::Errno => 1,
            DenyAction::Kill(None) => 2,
            DenyAction::Kill(Some(list)) => {
                3 ^ list.iter().fold(0u64, |acc, nr| acc.wrapping_mul(31).wrapping_add(*nr as u64))
            }
        }
    }

    fn kills(&self, nr: u32) -> bool {
        match self {
            DenyAction::Errno => false,
            DenyAction::Kill(None) => true,
            DenyAction::Kill(Some(list)) => list.contains(&nr),
        }
    }
}

/// `ACOS_ART_SECCOMP_AUDIT` selects the audit action; anything else (or unset)
/// keeps production `EACCES`:
///
/// * `kill` — every deny-listed syscall is `KILL_PROCESS` (whole-list audit);
/// * `kill:101,157` — only those syscall numbers die, the rest return `EACCES`
///   (used to attribute a denial to one syscall without ptrace: an
///   unprivileged userns has no seccomp audit log, see S6.md).
fn audit_action() -> DenyAction {
    let Ok(raw) = std::env::var("ACOS_ART_SECCOMP_AUDIT") else {
        return DenyAction::Errno;
    };
    let raw = raw.trim();
    if matches!(raw, "kill" | "1" | "kill_process") {
        return DenyAction::Kill(None);
    }
    if let Some(list) = raw.strip_prefix("kill:") {
        let nrs: Vec<u32> = list
            .split(',')
            .filter_map(|p| p.trim().parse::<u32>().ok())
            .collect();
        if !nrs.is_empty() {
            return DenyAction::Kill(Some(nrs));
        }
    }
    DenyAction::Errno
}

/// Assemble the filter program.
///
/// SPIKE-NET: `allow_inet` also permits `AF_INET`/`AF_INET6` sockets. It is
/// only ever set when `ACOS_SPIKE_NET=1`, i.e. when the session really has a
/// network namespace with an interface in it (ADR-0030 §2: the address
/// families are allowed only when the network module is attached).
fn build_program_with(action: &DenyAction, allow_inet: bool) -> Vec<Insn> {
    let mut b = Builder::new();
    let deny = b.label();
    let kill = b.label();
    let kill_arch = b.label();
    let arch_ok = b.label();
    let list = b.label();
    let socket_check = b.label();
    let not_socket = b.label();
    let not_socket2 = b.label();
    let allow = b.label();
    let clone_check = b.label();
    let clone3_deny = b.label();
    let personality_check = b.label();
    let personality_query = b.label();

    b.ld_abs(OFF_ARCH);
    b.jeq(AUDIT_ARCH, arch_ok, kill_arch);
    b.mark(kill_arch);
    b.ret(RET_KILL_PROCESS);
    b.mark(arch_ok);

    b.ld_abs(OFF_NR);
    b.jeq(libc::SYS_socket as u32, socket_check, not_socket);
    b.mark(not_socket);
    b.jeq(libc::SYS_socketpair as u32, socket_check, not_socket2);
    b.mark(not_socket2);
    b.ja(list);

    b.mark(socket_check);
    b.ld_abs(OFF_ARG0);
    b.jeq(libc::AF_UNIX as u32, list, deny);
    if allow_inet {
        let inet4 = b.label();
        let inet6 = b.label();
        b.jeq(libc::AF_INET as u32, inet4, inet6);
        b.mark(inet4);
        b.ja(list);
        b.mark(inet6);
        b.jeq(libc::AF_INET6 as u32, list, deny);
    }

    b.mark(list);
    b.ld_abs(OFF_NR);
    for nr in deny_list() {
        let next = b.label();
        // The audit can make an individual syscall fatal; everything else keeps
        // the production EACCES action.
        b.jeq(nr, if action.kills(nr) { kill } else { deny }, next);
        b.mark(next);
    }
    let after_clone = b.label();
    b.jeq(libc::SYS_clone as u32, clone_check, after_clone);
    b.mark(after_clone);
    let after_clone3 = b.label();
    b.jeq(libc::SYS_clone3 as u32, clone3_deny, after_clone3);
    b.mark(after_clone3);
    let after_personality = b.label();
    b.jeq(libc::SYS_personality as u32, personality_check, after_personality);
    b.mark(after_personality);
    b.ja(allow);

    // clone3's flags live behind a pointer; refuse it so bionic falls back to
    // the (filtered) clone and thread creation keeps working.
    b.mark(clone3_deny);
    b.ret(RET_ERRNO | (libc::ENOSYS as u32 & 0xffff));

    b.mark(clone_check);
    b.ld_abs(OFF_ARG0);
    b.and(CLONE_NEW_MASK);
    b.jeq(0, allow, deny);

    b.mark(personality_check);
    b.ld_abs(OFF_ARG0);
    b.jeq(PER_LINUX, allow, personality_query);
    b.mark(personality_query);
    b.jeq(PERSONALITY_QUERY, allow, deny);

    // The kill terminal is emitted for every build (dead code in production,
    // and it keeps the production layout stable for the tests below).
    b.mark(kill);
    b.ret(RET_KILL_PROCESS);
    b.mark(allow);
    b.ret(RET_ALLOW);
    b.mark(deny);
    b.ret(RET_ERRNO | (libc::EACCES as u32 & 0xffff));

    b.finish()
}

/// The on-disk blob bwrap reads: `count` × 8-byte `struct sock_filter`
/// records, little-endian, no length prefix (bwrap derives the count from the
/// file size; measured — a prefix is rejected with "must be multiple of 8").
pub fn build_blob() -> Vec<u8> {
    build_blob_with(&audit_action())
}

/// The blob for an explicit deny action (the audit uses [`DenyAction::Kill`]).
pub fn build_blob_with(action: &DenyAction) -> Vec<u8> {
    let prog = build_program_with(action, crate::spike_net::enabled());
    let mut out = Vec::with_capacity(prog.len() * 8);
    for i in &prog {
        out.extend_from_slice(&i.code.to_le_bytes());
        out.push(i.jt);
        out.push(i.jf);
        out.extend_from_slice(&i.k.to_le_bytes());
    }
    out
}

/// Build (or reuse) the guard artefacts for `root`. Fail closed: a host without
/// a working `cc` cannot run filtered guest code.
pub fn enable(root: &crate::ArtRoot) -> Result<()> {
    build_guest(&root.data.join("seccomp")).map(|_| ())
}

/// Read the guest guard's evidence from a finished run root. Fail closed: a run
/// whose filter cannot be proven (`OK guard=active` absent) is not a run the
/// product reports as successful; callers that already reported a watchdog or
/// cgroup kill may skip this.
pub fn require_guard(run_dir: &Path) -> Result<String> {
    let path = run_dir.join("out/seccomp.log");
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let guard = text
        .lines()
        .find(|l| l.starts_with("OK guard=active"))
        .map(str::to_string)
        .ok_or_else(|| {
            Error::SeccompUnavailable(format!(
                "the guest seccomp guard did not report in {}; a run whose filter cannot be proven is refused",
                path.display()
            ))
        })?;
    // S6: the same constructor applies Landlock before the filter. Its evidence
    // is mandatory on a kernel that supports Landlock (fail closed), and an
    // explicit `SKIP landlock=kernel-unsupported` is the only accepted fallback.
    let landlock = crate::landlock::require_from_log(&text, &path)?;
    Ok(format!("{guard} / {landlock}"))
}

/// Compile the freestanding stub and write the blob into `<data>/seccomp/`.
/// The compile and the blob are built **once** in the shared content-addressed
/// build cache (key: stub source, blob bytes, audit action, compiler); a launch
/// only installs the two artefacts.
pub fn build_guest(dest_dir: &Path) -> Result<(PathBuf, PathBuf)> {
    std::fs::create_dir_all(dest_dir)?;
    let action = audit_action();
    // S6: the Landlock plan is written every time (it is a text file, not a
    // compiled artefact) and removed when this kernel has no Landlock, so a
    // run never ships a plan the kernel cannot enforce.
    let landlock = crate::landlock::write_plan(dest_dir)?;
    let blob_bytes = build_blob_with(&action);
    let cc = std::env::var("CC").unwrap_or_else(|_| "clang".to_string());
    let mut parts: Vec<Vec<u8>> = vec![
        b"acos-art/seccomp-guest/v2".to_vec(),
        GUEST_SOURCE.as_bytes().to_vec(),
        blob_bytes.clone(),
        format!("action={}", action.to_u64()).into_bytes(),
        format!("landlock={landlock}").into_bytes(),
        crate::build_cache::file_id(Path::new(&cc)).into_bytes(),
    ];
    parts.push(b"-nostdlib -ffreestanding -fPIC -shared -O2".to_vec());
    let key = crate::build_cache::digest_owned(&parts);
    let built = crate::build_cache::ensure(
        &key,
        &["libacos_seccomp_guest.so", "seccomp.bpf"],
        |stage| {
            let src = stage.join("seccomp_guest.c");
            std::fs::write(&src, GUEST_SOURCE)?;
            std::fs::write(stage.join("seccomp.bpf"), &blob_bytes)?;
            let st = Command::new(&cc)
                .args([
                    "-nostdlib",
                    "-ffreestanding",
                    "-fno-builtin",
                    "-fPIC",
                    "-shared",
                    "-O2",
                    "-Wall",
                    "-o",
                ])
                .arg(stage.join("libacos_seccomp_guest.so"))
                .arg(&src)
                .status()
                .map_err(|e| Error::Tool(format!("running {cc}: {e}")))?;
            if !st.success() {
                return Err(Error::Tool(format!(
                    "{cc} failed to build the seccomp guest guard ({})",
                    src.display()
                )));
            }
            std::fs::remove_file(&src)?;
            Ok(())
        },
    )?;
    let installed = crate::build_cache::install(
        &built,
        &["libacos_seccomp_guest.so", "seccomp.bpf"],
        dest_dir,
    )?;
    Ok((installed[0].clone(), installed[1].clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compiled() -> Vec<Insn> {
        build_program_with(&DenyAction::Errno, false)
    }

    #[test]
    fn program_is_small_enough_for_u8_jumps_and_ends_in_return() {
        let p = compiled();
        assert!(!p.is_empty());
        assert!(p.len() < 255, "jt/jf are u8-limited, got {}", p.len());
        assert_eq!(p.last().map(|i| i.code), Some(RET_K));
    }

    #[test]
    fn audit_action_kills_and_never_returns_eacces() {
        let p = build_program_with(&DenyAction::Kill(None), false);
        // The kill terminal is emitted before the allow/deny terminals.
        assert!(p.iter().any(|i| i.k == RET_KILL_PROCESS));
        assert_eq!(build_blob_with(&DenyAction::Kill(None)).len(), p.len() * 8);
        // The production build carries the same (unreachable) kill terminal, so
        // its last instruction is still the EACCES deny.
        let q = build_program_with(&DenyAction::Errno, false);
        assert_eq!(q.last().map(|i| i.k), Some(RET_ERRNO | (libc::EACCES as u32 & 0xffff)));
        // A subset build jumps to the kill terminal only for the listed nrs.
        let sub = DenyAction::Kill(Some(vec![libc::SYS_ptrace as u32]));
        assert!(sub.kills(libc::SYS_ptrace as u32));
        assert!(!sub.kills(libc::SYS_mount as u32));
        assert!(!DenyAction::Errno.kills(libc::SYS_ptrace as u32));
    }

    #[test]
    fn deny_terminal_returns_eacces_and_default_allows() {
        let p = compiled();
        // The last instruction is the deny terminal; the one before it is the
        // allow terminal.
        assert_eq!(p[p.len() - 1].k, RET_ERRNO | (libc::EACCES as u32 & 0xffff));
        assert_eq!(p[p.len() - 2].k, RET_ALLOW);
    }

    #[test]
    fn every_escape_syscall_is_denied() {
        let list = deny_list();
        for (name, nr) in [
            ("ptrace", libc::SYS_ptrace),
            ("mount", libc::SYS_mount),
            ("umount2", libc::SYS_umount2),
            ("keyctl", libc::SYS_keyctl),
            ("bpf", libc::SYS_bpf),
            ("perf_event_open", libc::SYS_perf_event_open),
            ("kexec_load", libc::SYS_kexec_load),
            ("init_module", libc::SYS_init_module),
            ("unshare", libc::SYS_unshare),
            ("setns", libc::SYS_setns),
            ("io_uring_setup", libc::SYS_io_uring_setup),
            ("userfaultfd", libc::SYS_userfaultfd),
            ("process_vm_readv", libc::SYS_process_vm_readv),
            ("process_vm_writev", libc::SYS_process_vm_writev),
            ("open_by_handle_at", libc::SYS_open_by_handle_at),
        ] {
            assert!(list.contains(&(nr as u32)), "{name} ({nr}) not denied");
        }
    }

    #[test]
    fn art_needs_are_not_denied() {
        let list = deny_list();
        for (name, nr) in [
            ("mmap", libc::SYS_mmap),
            ("mprotect", libc::SYS_mprotect),
            ("madvise", libc::SYS_madvise),
            ("memfd_create", libc::SYS_memfd_create),
            ("futex", libc::SYS_futex),
            ("clone", libc::SYS_clone),
            ("prctl", libc::SYS_prctl),
            ("openat", libc::SYS_openat),
            ("read", libc::SYS_read),
            ("write", libc::SYS_write),
            ("epoll_create1", libc::SYS_epoll_create1),
            ("eventfd2", libc::SYS_eventfd2),
            ("socket", libc::SYS_socket),
            ("rt_sigaction", libc::SYS_rt_sigaction),
        ] {
            assert!(!list.contains(&(nr as u32)), "{name} ({nr}) must stay allowed");
        }
    }

    #[test]
    fn blob_has_the_sock_filter_layout() {
        let blob = build_blob();
        let n = compiled().len();
        assert_eq!(blob.len(), n * 8, "bwrap needs a bare sock_filter[] (8-byte multiple)");
        // First instruction: `ld [4]` (arch).
        assert_eq!(u16::from_le_bytes([blob[0], blob[1]]), LD_ABS_W);
        assert_eq!(&blob[2..4], &[0, 0]);
        assert_eq!(u32::from_le_bytes([blob[4], blob[5], blob[6], blob[7]]), OFF_ARCH);
    }
}
