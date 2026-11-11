/*
 * seccomp_guest.c -- freestanding LD_PRELOAD constructor that installs the
 * ACOS seccomp-bpf policy inside the ART guest (S5).
 *
 * The launcher writes the program as a binary classic-BPF array (bare
 * `struct sock_filter` records, little-endian) to /data/seccomp/seccomp.bpf and
 * binds it read-only into the run. This stub is preloaded so the filter lands
 * *inside* the guest, after bionic has finished loading the process and before
 * ART's `main` / any JNI code runs. That ordering matters: the launcher cannot
 * install it (`pre_exec` runs before bwrap's `mount`/`unshare`), and bwrap's
 * own `--seccomp` would trap its `execve` of the guest (measured, see S5.md).
 *
 * Denials return `EACCES` (RET_ERRNO); the effect is observed by the guest and
 * recorded in the run result. A SIGSYS-based denial log was tried and rejected:
 * a handler installed from this constructor is not in effect when the denial
 * fires (a probe that installs its own handler sees the trap; without it the
 * process dies of SIGSEGV and nothing is logged). See S5.md.
 *
 * Fails closed: if the blob cannot be read or the kernel rejects the filter the
 * guest exits 126 before ART code runs (the launcher also fails closed when
 * `OK guard=active` never appears in /data/out/seccomp.log).
 *
 * Freestanding (`-nostdlib -ffreestanding`, raw x86_64 syscalls) because the
 * guest links against bionic, not the host glibc. Compiled at run time by
 * `acos_art::seccomp::build_guest`.
 */

#define SYS_read 0
#define SYS_write 1
#define SYS_open 2
#define SYS_close 3
#define SYS_exit_group 231
#define SYS_seccomp 317


#define SECCOMP_SET_MODE_FILTER 1
#define SECCOMP_FILTER_FLAG_TSYNC 1
#define MAX_INSNS 1024
#define BLOB_PATH "/data/seccomp/seccomp.bpf"

#define O_RDONLY 0
#define O_WRONLY 1
#define O_CREAT 64
#define O_APPEND 1024
#define LOG_PATH "/data/out/seccomp.log"

struct sock_filter {
    unsigned short code;
    unsigned char jt;
    unsigned char jf;
    unsigned int k;
};

struct sock_fprog {
    unsigned short len;
    struct sock_filter *filter;
};

typedef unsigned long u64;
typedef unsigned int u32;
typedef unsigned short u16;
typedef unsigned char u8;
typedef long s64;

static s64 sc6(s64 n, s64 a, s64 b, s64 c, s64 d, s64 e, s64 f)
{
    register s64 r10 __asm__("r10") = d;
    register s64 r8 __asm__("r8") = e;
    register s64 r9 __asm__("r9") = f;
    s64 ret;
    __asm__ volatile("syscall"
                     : "=a"(ret)
                     : "a"(n), "D"(a), "S"(b), "d"(c), "r"(r10), "r"(r8), "r"(r9)
                     : "rcx", "r11", "memory");
    return ret;
}

static s64 log_fd = -1;

static void emit_str(int fd, const char *s)
{
    u64 len = 0;
    while (s[len])
        len++;
    while (len > 0) {
        s64 w = sc6(SYS_write, fd, (s64)s, (s64)len, 0, 0, 0);
        if (w <= 0)
            return;
        s += w;
        len -= (u64)w;
    }
}

static void emit_num(int fd, s64 v)
{
    char buf[24];
    int i = 23;
    buf[i] = '\0';
    if (v < 0) {
        emit_str(fd, "-");
        v = -v;
    }
    if (v == 0) {
        emit_str(fd, "0");
        return;
    }
    while (v > 0 && i > 0) {
        buf[--i] = (char)('0' + (v % 10));
        v /= 10;
    }
    emit_str(fd, &buf[i]);
}

static struct sock_filter insns[MAX_INSNS];

/* Read the launcher-generated program: a bare array of 8-byte
 * `struct sock_filter` records (the count is the file size / 8). */
static u16 read_program(struct sock_filter *out)
{
    unsigned char *dst = (unsigned char *)out;
    const u64 want = (u64)MAX_INSNS * sizeof(struct sock_filter);
    s64 fd;
    s64 rc;
    u64 got = 0;

    fd = sc6(SYS_open, (s64)BLOB_PATH, O_RDONLY, 0, 0, 0, 0);
    if (fd < 0)
        return 0;
    while (got < want) {
        rc = sc6(SYS_read, fd, (s64)(dst + got), (s64)(want - got), 0, 0, 0);
        if (rc < 0) {
            sc6(SYS_close, fd, 0, 0, 0, 0, 0);
            return 0;
        }
        if (rc == 0)
            break;
        got += (u64)rc;
    }
    sc6(SYS_close, fd, 0, 0, 0, 0, 0);
    if (got == 0 || (got % sizeof(struct sock_filter)) != 0)
        return 0;
    if (got / sizeof(struct sock_filter) > MAX_INSNS)
        return 0;
    return (u16)(got / sizeof(struct sock_filter));
}

__attribute__((constructor)) static void acos_seccomp_guest_init(void)
{
    struct sock_fprog prog;
    u16 n;
    s64 rc;

    log_fd = sc6(SYS_open, (s64)LOG_PATH, O_WRONLY | O_CREAT | O_APPEND, 0644, 0, 0, 0);
    if (log_fd < 0)
        return; /* no evidence channel: the launcher fails closed on the gap */

    n = read_program(insns);
    if (n == 0) {
        emit_str((int)log_fd, "ERR read " BLOB_PATH "\n");
        sc6(SYS_exit_group, 126, 0, 0, 0, 0, 0);
        return;
    }
    prog.len = n;
    prog.filter = insns;
    rc = sc6(SYS_seccomp, SECCOMP_SET_MODE_FILTER, SECCOMP_FILTER_FLAG_TSYNC, (s64)&prog, 0, 0, 0);
    if (rc != 0) {
        emit_str((int)log_fd, "ERR seccomp(SET_MODE_FILTER, TSYNC) errno=");
        emit_num((int)log_fd, -rc);
        emit_str((int)log_fd, "\n");
        sc6(SYS_exit_group, 126, 0, 0, 0, 0, 0);
        return;
    }

    emit_str((int)log_fd, "OK guard=active insns=");
    emit_num((int)log_fd, (s64)n);
    emit_str((int)log_fd, "\n");
}
