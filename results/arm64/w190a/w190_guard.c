// W190-A spike: seccomp guard injected into the qemu-aarch64 host process.
//
// Loaded with LD_PRELOAD by the launcher *after* the launcher installed
// no_new_privs + Landlock but before `main`; the constructor installs the ACOS
// seccomp deny filter inside the qemu process.  This is the mechanism the
// design proposes, because ACOS's own `confine()` installs seccomp *before*
// `execve`, and seccomp denies `execve` outright -- confine-then-exec qemu is
// only possible if the filter is deferred past the exec.
//
// The policy mirrors `crates/acos-sandbox/src/linux/seccomp.rs` (default
// ALLOW, extended deny list, socket -> AF_UNIX only, clone namespace flags ->
// EACCES, clone3 -> ENOSYS).  Built with libseccomp for the spike; the product
// would reuse the crate's audited BPF assembler instead of a second copy.
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <errno.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <sys/socket.h>
#include <seccomp.h>

#define CLONE_NEW_MASK (0x00020000u | 0x02000000u | 0x04000000u | \
                        0x08000000u | 0x10000000u | 0x20000000u | 0x40000000u)

static const int deny_list[] = {
    __NR_execve, __NR_execveat,
    __NR_ptrace, __NR_process_vm_readv, __NR_process_vm_writev,
    __NR_process_madvise, __NR_kcmp,
    __NR_mount, __NR_umount2, __NR_pivot_root, __NR_chroot,
    __NR_unshare, __NR_setns, __NR_mount_setattr,
    __NR_open_by_handle_at, __NR_name_to_handle_at,
    __NR_io_uring_setup, __NR_io_uring_enter, __NR_io_uring_register,
    __NR_userfaultfd, __NR_bpf, __NR_perf_event_open,
    __NR_kexec_load, __NR_kexec_file_load,
    __NR_init_module, __NR_finit_module, __NR_delete_module,
    __NR_reboot, __NR_keyctl, __NR_add_key, __NR_request_key,
    __NR_setuid, __NR_setgid, __NR_setreuid, __NR_setregid,
    __NR_setresuid, __NR_setresgid, __NR_setfsuid, __NR_setfsgid,
    __NR_setgroups, __NR_capset,
    __NR_pidfd_open, __NR_pidfd_getfd, __NR_pidfd_send_signal,
    __NR_kill, __NR_tkill,
    __NR_lookup_dcookie, __NR_quotactl, __NR_swapon, __NR_swapoff,
    __NR_syslog, __NR_acct, __NR_ioperm, __NR_iopl,
    __NR_modify_ldt,
};
#define ARRAY_LEN(a) ((int)(sizeof(a) / sizeof((a)[0])))

static void msg(const char *m) { (void)!write(2, m, strlen(m)); }

static void install_seccomp(void) {
    if (prctl(PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) != 1)
        prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);

    scmp_filter_ctx ctx = seccomp_init(SCMP_ACT_ALLOW);
    if (!ctx) { msg("[guard] seccomp_init failed\n"); _exit(31); }
    seccomp_attr_set(ctx, SCMP_FLTATR_CTL_NNP, 1);

    int bad = 0;
    for (int i = 0; i < ARRAY_LEN(deny_list); i++)
        bad |= seccomp_rule_add(ctx, SCMP_ACT_ERRNO(EACCES), deny_list[i], 0);

    // socket/socketpair: allow AF_UNIX, deny every other family.
    bad |= seccomp_rule_add(ctx, SCMP_ACT_ERRNO(EACCES), SCMP_SYS(socket), 1,
                            SCMP_A0(SCMP_CMP_NE, AF_UNIX));
    bad |= seccomp_rule_add(ctx, SCMP_ACT_ERRNO(EACCES), SCMP_SYS(socketpair), 1,
                            SCMP_A0(SCMP_CMP_NE, AF_UNIX));
    // clone(flags,...): deny namespace-bearing flag words.
    bad |= seccomp_rule_add(ctx, SCMP_ACT_ERRNO(EACCES), SCMP_SYS(clone), 1,
                            SCMP_A0(SCMP_CMP_MASKED_EQ, CLONE_NEW_MASK, CLONE_NEW_MASK));
    // clone3 cannot be inspected in classic BPF; ENOSYS makes libc fall back to clone.
    bad |= seccomp_rule_add(ctx, SCMP_ACT_ERRNO(ENOSYS), SCMP_SYS(clone3), 0);
    if (bad) { msg("[guard] seccomp_rule_add failed\n"); _exit(32); }

    if (seccomp_load(ctx) != 0) {
        int e = errno;
        char m[96];
        int L = snprintf(m, sizeof m, "[guard] seccomp_load failed: %s\n", strerror(e));
        (void)!write(2, m, L);
        _exit(31);
    }
    { char m[64]; int L = snprintf(m, sizeof m, "[guard] seccomp installed (%d deny rules)\n", ARRAY_LEN(deny_list)); (void)!write(2, m, L); }
    seccomp_release(ctx);
}

__attribute__((constructor)) static void w190_guard_init(void) {
    msg("[guard] constructor entered\n");
    install_seccomp();
    errno = 0;
    long rc = syscall(SYS_ptrace, 0L, 0L, 0L, 0L);
    { char m[96]; int L = snprintf(m, sizeof m,
        "[guard] host ptrace() -> %ld errno=%d (%s)\n", rc, errno,
        errno == EACCES ? "denied" : "NOT denied"); (void)!write(2, m, L); }
    errno = 0;
    rc = syscall(SYS_unshare, 0x10000000u);
    { char m[112]; int L = snprintf(m, sizeof m,
        "[guard] host unshare(NEWUSER) -> %ld errno=%d (%s)\n", rc, errno,
        errno == EACCES ? "denied" : "NOT denied"); (void)!write(2, m, L); }
}
