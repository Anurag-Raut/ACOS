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
 * S6: the same constructor first applies the *Landlock* ruleset (policy in
 * `acos-sandbox`, shipped as /data/seccomp/landlock.plan) with the raw
 * landlock_* syscalls, and only then installs this filter. Order matters twice:
 * bionic has finished loading (so the ruleset does not break the loader), and
 * both layers are in effect before ART's `main`/JNI code. Landlock denials are
 * `EACCES` too; the applier logs `OK landlock=active abi=N rules=M`, or
 * `SKIP landlock=kernel-unsupported` on a kernel without Landlock, and exits
 * 126 (fail closed) on any other failure.
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
#define SYS_landlock_create_ruleset 444
#define SYS_landlock_add_rule 445
#define SYS_landlock_restrict_self 446
#define SYS_fstat 5


#define SECCOMP_SET_MODE_FILTER 1
#define SECCOMP_FILTER_FLAG_TSYNC 1
#define MAX_INSNS 1024
#define BLOB_PATH "/data/seccomp/seccomp.bpf"

#define O_RDONLY 0
#define O_WRONLY 1
#define O_CREAT 64
#define O_APPEND 1024
#define O_PATH 0x200000
#define O_CLOEXEC 0x80000
#define LOG_PATH "/data/out/seccomp.log"
#define LL_PLAN_PATH "/data/seccomp/landlock.plan"

#define LANDLOCK_CREATE_RULESET_VERSION 1
#define LANDLOCK_RULE_PATH_BENEATH 1
/* Rights that only apply to a directory: the kernel rejects them on a
 * non-directory path (EINVAL), so the applier masks them per path type. */
#define LL_DIRECTORY_ONLY 0x3ff8UL
#define AT_FDCWD -100
#define ENOENT 2
#define ENOSYS 38

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

struct ruleset_attr {
    u64 handled_access_fs;
    u64 handled_access_net;
    u64 scoped;
};

struct path_beneath_attr {
    u64 allowed_access;
    int parent_fd;
} __attribute__((packed));

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


/* ------------------------------------------------------------------ */
/* S6: deferred Landlock applier                                       */
/* ------------------------------------------------------------------ */

static char plan_buf[4096];

static int starts_with(const char *s, const char *p)
{
    while (*p) {
        if (*s != *p)
            return 0;
        s++;
        p++;
    }
    return 1;
}

static int hex_digit(int c)
{
    if (c >= '0' && c <= '9')
        return c - '0';
    if (c >= 'a' && c <= 'f')
        return c - 'a' + 10;
    if (c >= 'A' && c <= 'F')
        return c - 'A' + 10;
    return -1;
}

/* Parse a lowercase hex mask; advances *pp past it. Returns 0 on success. */
static int parse_hex(const char **pp, u64 *out)
{
    const char *p = *pp;
    u64 v = 0;
    int d, any = 0;
    while ((d = hex_digit(*p)) >= 0) {
        v = (v << 4) | (u64)d;
        p++;
        any = 1;
    }
    if (!any)
        return -1;
    *out = v;
    *pp = p;
    return 0;
}

/* Read at most `want` bytes; returns the byte count, or -1. */
static s64 read_file(const char *path, char *dst, u64 want)
{
    s64 fd, rc;
    u64 got = 0;
    fd = sc6(SYS_open, (s64)path, O_RDONLY, 0, 0, 0, 0);
    if (fd < 0)
        return -1;
    while (got < want) {
        rc = sc6(SYS_read, fd, (s64)(dst + got), (s64)(want - got), 0, 0, 0);
        if (rc < 0) {
            sc6(SYS_close, fd, 0, 0, 0, 0, 0);
            return -1;
        }
        if (rc == 0)
            break;
        got += (u64)rc;
    }
    sc6(SYS_close, fd, 0, 0, 0, 0, 0);
    return (s64)got;
}

/* Find `handled <hex>` in the plan (first pass). */
static int plan_handled(const char *buf, u64 len, u64 *handled)
{
    u64 i = 0;
    while (i < len) {
        const char *line = buf + i;
        u64 j = i;
        while (j < len && buf[j] != '\n')
            j++;
        if (starts_with(line, "handled ")) {
            const char *p = line + 8;
            if (parse_hex(&p, handled) == 0)
                return 0;
        }
        i = j + 1;
    }
    return -1;
}

/* Is the opened O_PATH fd a directory? `struct stat` st_mode is at offset 24
 * on x86_64 (st_dev, st_ino, st_nlink are 8 bytes each). */
static int fd_is_dir(int fd)
{
    unsigned char st[144];
    s64 rc;
    u32 mode;
    rc = sc6(SYS_fstat, fd, (s64)st, 0, 0, 0, 0);
    if (rc != 0)
        return 0;
    mode = *(u32 *)(void *)(st + 24);
    return (mode & 0170000) == 0040000;
}

static int ll_fail(const char *what, s64 err)
{
    emit_str((int)log_fd, "ERR landlock=");
    emit_str((int)log_fd, what);
    if (err != 0) {
        emit_str((int)log_fd, " errno=");
        emit_num((int)log_fd, err < 0 ? -err : err);
    }
    emit_str((int)log_fd, "\n");
    return -1;
}

/* Apply the plan. Returns 0 (applied or kernel-unsupported) or -1 (fatal). */
static int apply_landlock(void)
{
    s64 abi, n, rc;
    u64 handled = 0;
    u64 i;
    int rs;
    int rules = 0, skipped = 0;

    abi = sc6(SYS_landlock_create_ruleset, 0, 0, LANDLOCK_CREATE_RULESET_VERSION, 0, 0, 0);
    if (abi < 0) {
        if (abi == -ENOSYS || abi == -38 || abi == -95) {
            emit_str((int)log_fd, "SKIP landlock=kernel-unsupported\n");
            return 0;
        }
        return ll_fail("probe", abi);
    }

    n = read_file(LL_PLAN_PATH, plan_buf, sizeof(plan_buf) - 1);
    if (n <= 0) {
        /* Kernel supports Landlock but the host shipped no plan: refuse. */
        return ll_fail("plan-missing", 0);
    }
    plan_buf[n] = '\0';
    if (!starts_with(plan_buf, "acos-landlock 1\n"))
        return ll_fail("plan-format", 0);
    if (plan_handled(plan_buf, (u64)n, &handled) != 0)
        return ll_fail("plan-handled", 0);

    {
        struct ruleset_attr attr;
        attr.handled_access_fs = handled;
        attr.handled_access_net = 0;
        attr.scoped = 0;
        /* The kernel accepts the known prefix: 8 bytes (ABI 1-3), +8 for
         * handled_access_net (ABI 4-5), +8 for scoped (ABI 6+). */
        rs = (int)sc6(SYS_landlock_create_ruleset, (s64)&attr,
                      abi >= 6 ? 24 : (abi >= 4 ? 16 : 8), 0, 0, 0, 0);
    }
    if (rs < 0)
        return ll_fail("create-ruleset", rs);

    i = 0;
    while (i < (u64)n) {
        const char *line = plan_buf + i;
        u64 j = i;
        const char *p;
        u64 mask;
        int fd;
        while (j < (u64)n && plan_buf[j] != '\n')
            j++;
        /* Terminate the line so the path handed to open(2) is exact. */
        if (j < (u64)n)
            plan_buf[j] = '\0';
        if (starts_with(line, "ro ") || starts_with(line, "rw ")) {
            p = line + 3;
            if (parse_hex(&p, &mask) != 0 || *p != ' ') {
                sc6(SYS_close, rs, 0, 0, 0, 0, 0);
                return ll_fail("plan-rule", 0);
            }
            p++;
            /* Path runs to the end of the line. */
            fd = (int)sc6(SYS_open, (s64)p, O_PATH | O_CLOEXEC, 0, 0, 0, 0);
            if (fd < 0) {
                if (-fd == ENOENT) {
                    skipped++;
                } else {
                    s64 e = fd;
                    sc6(SYS_close, rs, 0, 0, 0, 0, 0);
                    return ll_fail("open-path", e);
                }
            } else {
                struct path_beneath_attr pb;
                if (!fd_is_dir(fd))
                    mask &= ~(u64)LL_DIRECTORY_ONLY;
                pb.allowed_access = mask;
                pb.parent_fd = fd;
                rc = sc6(SYS_landlock_add_rule, rs, LANDLOCK_RULE_PATH_BENEATH,
                         (s64)&pb, 0, 0, 0);
                sc6(SYS_close, fd, 0, 0, 0, 0, 0);
                if (rc != 0) {
                    s64 e = rc;
                    sc6(SYS_close, rs, 0, 0, 0, 0, 0);
                    return ll_fail("add-rule", e);
                }
                rules++;
            }
        }
        i = j + 1;
    }

    rc = sc6(SYS_landlock_restrict_self, rs, 0, 0, 0, 0, 0);
    sc6(SYS_close, rs, 0, 0, 0, 0, 0);
    if (rc != 0)
        return ll_fail("restrict-self", rc);

    emit_str((int)log_fd, "OK landlock=active abi=");
    emit_num((int)log_fd, abi);
    emit_str((int)log_fd, " rules=");
    emit_num((int)log_fd, rules);
    emit_str((int)log_fd, " skipped=");
    emit_num((int)log_fd, skipped);
    emit_str((int)log_fd, "\n");
    return 0;
}

__attribute__((constructor)) static void acos_seccomp_guest_init(void)
{
    struct sock_fprog prog;
    u16 n;
    s64 rc;

    log_fd = sc6(SYS_open, (s64)LOG_PATH, O_WRONLY | O_CREAT | O_APPEND, 0644, 0, 0, 0);
    if (log_fd < 0)
        return; /* no evidence channel: the launcher fails closed on the gap */

    /* S6 layer: Landlock filesystem ruleset first (it only needs reads of the
     * plan and O_PATH opens of the granted roots), then the syscall filter. */
    if (apply_landlock() != 0) {
        sc6(SYS_exit_group, 126, 0, 0, 0, 0, 0);
        return;
    }

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
