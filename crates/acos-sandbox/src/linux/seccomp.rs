//! seccomp-bpf syscall confinement.
//!
//! The filter's default action is `SECCOMP_RET_ALLOW` with an *extended* deny
//! list; it denies the syscalls a confined guest must never reach, and for
//! `socket`/`socketpair` allows only the `AF_UNIX` domain. Denials return
//! `EACCES` so a confined process can handle them and an agent can observe a
//! clean failure rather than a signal.
//!
//! The extended list covers every host-escape surface an APK `.so` could reach
//! (W67A findings #1/#2): the io_uring `setup/enter/register` triplet,
//! `userfaultfd`, `open_by_handle_at`/`name_to_handle_at` (both bypass the
//! path-based Landlock ruleset), the new mount API
//! (`fsopen`/`fsconfig`/`fsmount`/`fspick`/`move_mount`/`open_tree`/
//! `mount_setattr`), the pidfd family, `kcmp`, `process_madvise`, `ptrace`,
//! `perf_event_open`, `lookup_dcookie`, `quotactl`, `swapon`/`swapoff`,
//! `syslog`, `acct`, `ioperm`/`iopl`, `modify_ldt`, the `setuid` family,
//! `capset`, and the remaining process-control surfaces.
//!
//! Argument filtering (classic BPF cannot dereference pointers, so this is the
//! strongest static posture available):
//!
//! * `clone` is denied only when `flags & CLONE_NEW*` is non-zero, so thread
//!   creation (`CLONE_THREAD`) still works while a new user/mount/net/pid/ipc/
//!   uts/cgroup namespace is refused (W67A #2);
//! * `clone3` cannot be inspected (its flags live in a pointed-to
//!   `struct clone_args`), so it is refused with `ENOSYS`; glibc and bionic
//!   fall back to the (filtered) `clone`, which keeps threads working;
//! * `kill`/`tkill` are always denied; `tgkill` is allowed only when its `tgid`
//!   argument equals the pid captured at filter-build time, and
//!   `rt_sigqueueinfo`/`rt_tgsigqueueinfo` only for the own pid (or pid 0), so a
//!   guest cannot signal any process outside its own thread group;
//! * `personality` is allowed only for `PER_LINUX` (`0`) and the read-only
//!   query form (`0xffff_ffff`).
//!
//! Everything else (the native-engine set) stays available on purpose:
//! `futex`, `mmap`/`mprotect` (including `PROT_EXEC`), `rt_sigaction`,
//! `clock_*`, `eventfd`/`epoll_*`/`poll`/`pipe`/`socketpair`, `read`/`write`,
//! `open*`/`stat*`/`getdents*` (Landlock-bounded), `madvise`, `prctl`, `rseq`,
//! `sched_*`, `getrandom`, ...
//!
//! The residual risk (default-allow) is documented in `DECISIONS.md`
//! ADR-0022: the allow-list is validated against the native-library inventory
//! (`acos native-inventory`) but not yet the boot path.
//!
//! The classic-BPF program is assembled with a tiny forward-only label
//! assembler so the control flow is auditable.

/// `AUDIT_ARCH_*` from `linux/audit.h`; libc does not export them.
///
/// The filter's arch check must name the architecture whose syscall numbers
/// the deny list uses. On the x86_64 host this is unchanged; on an aarch64
/// build (W194-A runtime-under-qemu) the numbers are the asm-generic arm64
/// ones, so the audit arch must follow the target.
#[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
const AUDIT_ARCH: u32 = 0xC000_003E; // AUDIT_ARCH_X86_64 (also 32-bit x86: 0x4000_0003)
#[cfg(target_arch = "aarch64")]
const AUDIT_ARCH: u32 = 0xC000_00B7; // AUDIT_ARCH_AARCH64
#[cfg(target_arch = "riscv64")]
const AUDIT_ARCH: u32 = 0xC000_00F3; // AUDIT_ARCH_RISCV64
#[cfg(not(any(
    target_arch = "x86_64",
    target_arch = "x86",
    target_arch = "aarch64",
    target_arch = "riscv64"
)))]
compile_error!("acos-sandbox: no AUDIT_ARCH for this target_arch");

const SECCOMP_SET_MODE_FILTER: u64 = 1;
const SECCOMP_FILTER_FLAG_TSYNC: u64 = 1;

const RET_KILL_PROCESS: u32 = 0x8000_0000;
const RET_ERRNO: u32 = 0x0005_0000;
const RET_ALLOW: u32 = 0x7fff_0000;
const ERRNO_DATA_MASK: u32 = 0x0000_ffff;

// `struct seccomp_data` offsets.
const OFF_NR: u32 = 0;
const OFF_ARCH: u32 = 4;
const OFF_ARG0: u32 = 16;

// Classic-BPF opcodes (`linux/bpf_common.h`).
const LD_ABS_W: u16 = 0x20; // BPF_LD | BPF_W | BPF_ABS
const JEQ_K: u16 = 0x15; // BPF_JMP | BPF_JEQ | BPF_K
const JA: u16 = 0x05; // BPF_JMP | BPF_JA
const RET_K: u16 = 0x06; // BPF_RET | BPF_K
const AND_K: u16 = 0x54; // BPF_ALU | BPF_AND | BPF_K

/// `CLONE_NEWNS | CLONE_NEWCGROUP | CLONE_NEWUTS | CLONE_NEWIPC |
/// CLONE_NEWUSER | CLONE_NEWPID | CLONE_NEWNET`: every namespace-creating
/// `clone` flag (W67A #2).
const CLONE_NEW_MASK: u32 = 0x0002_0000
    | 0x0200_0000
    | 0x0400_0000
    | 0x0800_0000
    | 0x1000_0000
    | 0x2000_0000
    | 0x4000_0000;

/// `kexec_file_load` on targets whose libc does not export the constant.
///
/// arm64 uses the asm-generic numbering (294) and libc's Android aarch64
/// module omits the name, so define it explicitly there; x86_64 keeps
/// `libc::SYS_kexec_file_load`.
#[cfg(target_arch = "aarch64")]
const SYS_KEXEC_FILE_LOAD: u32 = 294;
#[cfg(not(target_arch = "aarch64"))]
const SYS_KEXEC_FILE_LOAD: u32 = libc::SYS_kexec_file_load as u32;

/// `PER_LINUX`, the only personality a guest may install.
const PER_LINUX: u32 = 0;
/// `0xffff_ffff` is the documented read-only `personality` query argument.
const PERSONALITY_QUERY: u32 = 0xffff_ffff;

#[repr(C)]
#[derive(Clone, Copy)]
struct SockFilter {
    code: u16,
    jt: u8,
    jf: u8,
    k: u32,
}

#[repr(C)]
struct SockFprog {
    len: u16,
    filter: *const SockFilter,
}

type Label = usize;

enum Op {
    Ld(u32),
    And(u32),
    Jeq {
        k: u32,
        t: Label,
        f: Label,
    },
    Ja(Label),
    Ret(u32),
}

struct Builder {
    ops: Vec<Op>,
    labels: Vec<Option<usize>>,
}

impl Builder {
    fn new() -> Self {
        Builder {
            ops: Vec::new(),
            labels: Vec::new(),
        }
    }

    fn label(&mut self) -> Label {
        self.labels.push(None);
        self.labels.len() - 1
    }

    fn mark(&mut self, label: Label) {
        debug_assert!(self.labels[label].is_none(), "label marked twice");
        self.labels[label] = Some(self.ops.len());
    }

    fn ld_abs(&mut self, offset: u32) {
        self.ops.push(Op::Ld(offset));
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

    fn finish(self) -> Vec<SockFilter> {
        let target = |label: Label| self.labels[label].expect("label was never marked");
        let mut out = Vec::with_capacity(self.ops.len());
        for (pc, op) in self.ops.iter().enumerate() {
            let insn = match *op {
                Op::Ld(offset) => SockFilter {
                    code: LD_ABS_W,
                    jt: 0,
                    jf: 0,
                    k: offset,
                },
                Op::And(k) => SockFilter {
                    code: AND_K,
                    jt: 0,
                    jf: 0,
                    k,
                },
                Op::Jeq { k, t, f } => {
                    let jt = target(t) - pc - 1;
                    let jf = target(f) - pc - 1;
                    debug_assert!(target(t) > pc && target(f) > pc, "backward jump");
                    SockFilter {
                        code: JEQ_K,
                        jt: jt as u8,
                        jf: jf as u8,
                        k,
                    }
                }
                Op::Ja(t) => {
                    debug_assert!(target(t) > pc, "backward jump");
                    SockFilter {
                        code: JA,
                        jt: 0,
                        jf: 0,
                        k: (target(t) - pc - 1) as u32,
                    }
                }
                Op::Ret(k) => SockFilter {
                    code: RET_K,
                    jt: 0,
                    jf: 0,
                    k,
                },
            };
            out.push(insn);
        }
        out
    }
}

/// Every syscall that is denied unconditionally (no argument inspection
/// needed). Kept sorted+unique so the assembled lookup is a deterministic
/// binary-comparable program.
fn deny_list() -> Vec<u32> {
    let mut list = vec![
        // Execution / process replacement.
        libc::SYS_execve as u32,
        libc::SYS_execveat as u32,
        // Process introspection.
        libc::SYS_ptrace as u32,
        libc::SYS_process_vm_readv as u32,
        libc::SYS_process_vm_writev as u32,
        libc::SYS_process_madvise as u32,
        libc::SYS_kcmp as u32,
        // Namespaces and mounts: path-based Landlock cannot mediate these.
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
        // Handle-based lookups bypass the path ruleset.
        libc::SYS_open_by_handle_at as u32,
        libc::SYS_name_to_handle_at as u32,
        // Kernel attack surface.
        libc::SYS_io_uring_setup as u32,
        libc::SYS_io_uring_enter as u32,
        libc::SYS_io_uring_register as u32,
        libc::SYS_userfaultfd as u32,
        libc::SYS_bpf as u32,
        libc::SYS_perf_event_open as u32,
        // Modules / kexec / reboot.
        libc::SYS_kexec_load as u32,
        SYS_KEXEC_FILE_LOAD,
        libc::SYS_init_module as u32,
        libc::SYS_finit_module as u32,
        libc::SYS_delete_module as u32,
        libc::SYS_reboot as u32,
        // Kernel keyring.
        libc::SYS_keyctl as u32,
        libc::SYS_add_key as u32,
        libc::SYS_request_key as u32,
        // Credentials: a confined guest must never change them.
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
        // pidfd family and process signalling. `kill`/`tkill` are denied
        // outright; `tgkill`/`rt_*sigqueueinfo` are argument-filtered below.
        libc::SYS_pidfd_open as u32,
        libc::SYS_pidfd_getfd as u32,
        libc::SYS_pidfd_send_signal as u32,
        libc::SYS_kill as u32,
        libc::SYS_tkill as u32,
        // Legacy / privileged surfaces.
        libc::SYS_lookup_dcookie as u32,
        libc::SYS_quotactl as u32,
        libc::SYS_swapon as u32,
        libc::SYS_swapoff as u32,
        libc::SYS_syslog as u32,
        libc::SYS_acct as u32,
    ];
    // x86-only privileged surfaces. They have no arm64/riscv64 syscall
    // number at all, so the deny entry is x86-only by construction.
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

fn build() -> Vec<SockFilter> {
    let mut b = Builder::new();
    // Labels: plain deny/allow terminals, then the per-syscall argument checks.
    let deny = b.label();
    let kill_arch = b.label();
    let x86_ok = b.label();
    let list = b.label();
    let socket_check = b.label();
    let not_socket1 = b.label();
    let not_socket2 = b.label();
    let allow = b.label();
    let clone_check = b.label();
    let clone3_deny = b.label();
    let tgkill_check = b.label();
    let sigq_check = b.label();
    let sigq_zero = b.label();
    let personality_check = b.label();
    let personality_query = b.label();

    // The pid of this process, captured while the filter is built. `kill` to
    // the own thread group and `tgkill` to the own tgid compare against it.
    // SAFETY: `getpid` has no preconditions and cannot fail.
    let own_pid = unsafe { libc::getpid() } as u32;

    // Reject a mismatched architecture outright: syscall numbers are per-arch.
    b.ld_abs(OFF_ARCH);
    b.jeq(AUDIT_ARCH, x86_ok, kill_arch);
    b.mark(kill_arch);
    b.ret(RET_KILL_PROCESS);
    b.mark(x86_ok);

    // Route socket/socketpair to a domain check; everything else to the list.
    b.ld_abs(OFF_NR);
    b.jeq(libc::SYS_socket as u32, socket_check, not_socket1);
    b.mark(not_socket1);
    b.jeq(libc::SYS_socketpair as u32, socket_check, not_socket2);
    b.mark(not_socket2);
    b.ja(list);

    b.mark(socket_check);
    b.ld_abs(OFF_ARG0);
    b.jeq(libc::AF_UNIX as u32, list, deny);

    // Linear deny list. `list` reloads the syscall number because the socket
    // check may have clobbered the accumulator with args[0].
    b.mark(list);
    b.ld_abs(OFF_NR);
    for syscall in deny_list() {
        let next = b.label();
        b.jeq(syscall, deny, next);
        b.mark(next);
    }
    // Argument-filtered syscalls (`after_*` is marked after the test, so it is
    // the next instruction; a "continue scanning" fall-through).
    let after_clone = b.label();
    b.jeq(libc::SYS_clone as u32, clone_check, after_clone);
    b.mark(after_clone);
    let after_clone3 = b.label();
    b.jeq(libc::SYS_clone3 as u32, clone3_deny, after_clone3);
    b.mark(after_clone3);
    let after_tgkill = b.label();
    b.jeq(libc::SYS_tgkill as u32, tgkill_check, after_tgkill);
    b.mark(after_tgkill);
    let after_sigq = b.label();
    b.jeq(libc::SYS_rt_sigqueueinfo as u32, sigq_check, after_sigq);
    b.mark(after_sigq);
    let after_tgsigq = b.label();
    b.jeq(libc::SYS_rt_tgsigqueueinfo as u32, sigq_check, after_tgsigq);
    b.mark(after_tgsigq);
    let after_personality = b.label();
    b.jeq(libc::SYS_personality as u32, personality_check, after_personality);
    b.mark(after_personality);
    b.ja(allow);

    // `clone3` cannot be inspected in classic BPF (flags live in a pointed-to
    // struct). Refuse it with `ENOSYS` so glibc/bionic fall back to the
    // filtered `clone` path and thread creation keeps working.
    b.mark(clone3_deny);
    b.ret(RET_ERRNO | (libc::ENOSYS as u32 & ERRNO_DATA_MASK));

    // `tgkill(tgid, tid, sig)`: allow only the own thread group (arg0 == pid).
    b.mark(tgkill_check);
    b.ld_abs(OFF_ARG0);
    b.jeq(own_pid, allow, deny);

    // `rt_sigqueueinfo(pid, ...)` / `rt_tgsigqueueinfo(tgid, ...)`: own pid or 0.
    b.mark(sigq_check);
    b.ld_abs(OFF_ARG0);
    b.jeq(own_pid, allow, sigq_zero);
    b.mark(sigq_zero);
    b.jeq(0, allow, deny);

    // `clone(flags, ...)`: deny any namespace-bearing flag word.
    b.mark(clone_check);
    b.ld_abs(OFF_ARG0);
    b.and(CLONE_NEW_MASK);
    b.jeq(0, allow, deny);

    // `personality(persona)`: only PER_LINUX, plus the read-only query form.
    b.mark(personality_check);
    b.ld_abs(OFF_ARG0);
    b.jeq(PER_LINUX, allow, personality_query);
    b.mark(personality_query);
    b.jeq(PERSONALITY_QUERY, allow, deny);

    // Terminals, marked last so every jump is forward. Default is allow.
    b.mark(allow);
    b.ret(RET_ALLOW);
    // Deny terminal: `EACCES`, so a confined process observes a clean failure.
    b.mark(deny);
    b.ret(RET_ERRNO | (libc::EACCES as u32 & ERRNO_DATA_MASK));

    b.finish()
}

/// Install the filter on all threads via `TSYNC`.
///
/// `TSYNC` requires every thread to already have `PR_SET_NO_NEW_PRIVS`; if any
/// thread cannot be synchronised the filter is not installed and this fails
/// closed, so the caller must not treat the process as confined.
pub fn install() -> Result<String, String> {
    let filter = build();
    let prog = SockFprog {
        len: filter.len() as u16,
        filter: filter.as_ptr(),
    };
    // SAFETY: `prog` points at `filter`, a live Vec of `SockFilter` that
    // outlives the call, and `len` matches its length.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_seccomp,
            SECCOMP_SET_MODE_FILTER,
            SECCOMP_FILTER_FLAG_TSYNC,
            &prog as *const SockFprog,
        )
    };
    if rc != 0 {
        return Err(format!(
            "seccomp(SET_MODE_FILTER, TSYNC): {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(format!(
        "{} bpf insns, {} denied syscalls, AF_UNIX-only sockets",
        filter.len(),
        deny_list().len()
    ))
}

/// Exposed for unit tests: compile the filter without installing it.
#[cfg(test)]
fn compiled_filter() -> Vec<SockFilter> {
    build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_jump_is_forward_and_in_range() {
        let filter = compiled_filter();
        assert!(!filter.is_empty());
        assert!(filter.len() < 255, "jt/jf are u8-limited");
        // The program must end in an unconditional return.
        assert_eq!(filter.last().map(|i| i.code), Some(RET_K));
    }

    #[test]
    fn last_instruction_denies_with_eacces() {
        let filter = compiled_filter();
        let last = filter.last().expect("non-empty");
        assert_eq!(last.k, RET_ERRNO | (libc::EACCES as u32 & ERRNO_DATA_MASK));
    }

    #[test]
    fn deny_list_is_sorted_and_unique() {
        let list = deny_list();
        let mut sorted = list.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(list, sorted);
        assert!(list.contains(&(libc::SYS_execve as u32)));
        assert!(list.contains(&(libc::SYS_bpf as u32)));
    }

    #[test]
    fn deny_list_covers_sandbox_escape_syscalls() {
        // Finding W67A-seccomp: these are guest-reachable via a loaded APK .so
        // and each one either bypasses the path-based Landlock ruleset
        // (open_by_handle_at, the mount API family), creates a namespace the
        // `unshare` denial was meant to stop (clone CLONE_NEWUSER), or is a
        // known kernel-attack surface (io_uring, userfaultfd, pidfd_getfd).
        // They must be in the deny list; today they are not.
        let list = deny_list();
        for (name, nr) in [
            ("io_uring_setup", libc::SYS_io_uring_setup),
            ("io_uring_enter", libc::SYS_io_uring_enter),
            ("io_uring_register", libc::SYS_io_uring_register),
            ("userfaultfd", libc::SYS_userfaultfd),
            ("open_by_handle_at", libc::SYS_open_by_handle_at),
            ("name_to_handle_at", libc::SYS_name_to_handle_at),
            ("pidfd_getfd", libc::SYS_pidfd_getfd),
            ("kcmp", libc::SYS_kcmp),
            ("fsopen", libc::SYS_fsopen),
            ("fsmount", libc::SYS_fsmount),
            ("move_mount", libc::SYS_move_mount),
            ("open_tree", libc::SYS_open_tree),
        ] {
            assert!(
                list.contains(&(nr as u32)),
                "{name} ({nr}) is not denied by the seccomp filter"
            );
        }
    }

    #[test]
    fn allows_become_default_and_index_types_compile() {
        // A smoke check that the assembler produced 8-byte instructions too.
        let filter = compiled_filter();
        assert_eq!(std::mem::size_of::<SockFilter>(), 8);
        assert!(filter.iter().all(|i| i.code != 0 || i.k != 0));
    }
}

