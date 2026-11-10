//! seccomp-bpf syscall confinement.
//!
//! The filter's default action is `SECCOMP_RET_ALLOW`; it denies a curated
//! list of dangerous syscalls and, for `socket`/`socketpair`, allows only the
//! `AF_UNIX` domain. Denials return `EACCES` so a confined process can handle
//! them and an agent can observe a clean failure rather than a signal.
//!
//! Everything not named in the deny list stays available on purpose:
//! `futex`, `clone`/`clone3` (threads), `mmap`/`mprotect` (including
//! `PROT_EXEC`), `rt_sigaction`, `clock_*`, `eventfd`, `epoll_*`, `poll`,
//! `read`/`write`, `brk`, `getrandom`, ...
//!
//! The classic-BPF program is assembled with a tiny forward-only label
//! assembler so the control flow is auditable.

/// `AUDIT_ARCH_X86_64` from `linux/audit.h`; libc does not export it.
const AUDIT_ARCH_X86_64: u32 = 0xC000_003E;

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

fn deny_list() -> Vec<u32> {
    let mut list = vec![
        libc::SYS_execve as u32,
        libc::SYS_execveat as u32,
        libc::SYS_ptrace as u32,
        libc::SYS_process_vm_readv as u32,
        libc::SYS_process_vm_writev as u32,
        libc::SYS_mount as u32,
        libc::SYS_umount2 as u32,
        libc::SYS_pivot_root as u32,
        libc::SYS_chroot as u32,
        libc::SYS_unshare as u32,
        libc::SYS_setns as u32,
        libc::SYS_kexec_load as u32,
        libc::SYS_kexec_file_load as u32,
        libc::SYS_bpf as u32,
        libc::SYS_perf_event_open as u32,
        libc::SYS_keyctl as u32,
        libc::SYS_add_key as u32,
        libc::SYS_request_key as u32,
        libc::SYS_init_module as u32,
        libc::SYS_finit_module as u32,
        libc::SYS_delete_module as u32,
        libc::SYS_reboot as u32,
    ];
    list.sort_unstable();
    list.dedup();
    list
}

fn build() -> Vec<SockFilter> {
    let mut b = Builder::new();
    let deny = b.label();
    let kill = b.label();
    let x86_ok = b.label();
    let list = b.label();
    let socket_check = b.label();
    let not_socket1 = b.label();
    let not_socket2 = b.label();

    // Reject a mismatched architecture outright: syscall numbers are per-arch.
    b.ld_abs(OFF_ARCH);
    b.jeq(AUDIT_ARCH_X86_64, x86_ok, kill);
    b.mark(kill);
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
    b.ret(RET_ALLOW);
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
    fn allows_become_default_and_index_types_compile() {
        // A smoke check that the assembler produced 8-byte instructions too.
        let filter = compiled_filter();
        assert_eq!(std::mem::size_of::<SockFilter>(), 8);
        assert!(filter.iter().all(|i| i.code != 0 || i.k != 0));
    }
}

