// W190-A spike: Landlock + no_new_privs launcher that execs qemu-aarch64.
//
// The launcher performs, in order:
//   1. prctl(PR_SET_NO_NEW_PRIVS)         (persists across execve)
//   2. a Landlock ruleset: RO on granted roots, RW on the guest data root
//   3. execve(qemu-aarch64, guest)
// and sets LD_PRELOAD=libw190_guard.so so the seccomp filter is installed
// *inside* the qemu process after exec (seccomp denies execve, so it cannot be
// installed before the exec).
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <sys/prctl.h>
#include <sys/syscall.h>

#ifndef SYS_landlock_create_ruleset
#define SYS_landlock_create_ruleset 444
#endif
#ifndef SYS_landlock_add_rule
#define SYS_landlock_add_rule 445
#endif
#ifndef SYS_landlock_restrict_self
#define SYS_landlock_restrict_self 446
#endif
#define LANDLOCK_CREATE_RULESET_VERSION 1
#define LANDLOCK_RULE_PATH_BENEATH 1

#define FS_EXECUTE      (1ULL << 0)
#define FS_WRITE_FILE   (1ULL << 1)
#define FS_READ_FILE    (1ULL << 2)
#define FS_READ_DIR     (1ULL << 3)
#define FS_REMOVE_DIR   (1ULL << 4)
#define FS_REMOVE_FILE  (1ULL << 5)
#define FS_MAKE_CHAR    (1ULL << 6)
#define FS_MAKE_DIR     (1ULL << 7)
#define FS_MAKE_REG     (1ULL << 8)
#define FS_MAKE_SOCK    (1ULL << 9)
#define FS_MAKE_FIFO    (1ULL << 10)
#define FS_MAKE_BLOCK   (1ULL << 11)
#define FS_MAKE_SYM     (1ULL << 12)
#define FS_REFER        (1ULL << 13)
#define FS_TRUNCATE     (1ULL << 14)
#define FS_IOCTL_DEV    (1ULL << 15)

struct ruleset_attr { unsigned long long handled_access_fs; };
struct path_beneath_attr {
    unsigned long long allowed_access;
    int parent_fd;
} __attribute__((packed));

static unsigned long long handled_mask(void) {
    return FS_EXECUTE | FS_WRITE_FILE | FS_READ_FILE | FS_READ_DIR |
           FS_REMOVE_DIR | FS_REMOVE_FILE | FS_MAKE_CHAR | FS_MAKE_DIR |
           FS_MAKE_REG | FS_MAKE_SOCK | FS_MAKE_FIFO | FS_MAKE_BLOCK |
           FS_MAKE_SYM | FS_REFER | FS_TRUNCATE | FS_IOCTL_DEV;
}
#define RO_RIGHTS (FS_EXECUTE | FS_READ_FILE | FS_READ_DIR)

static int add_rule(int ruleset_fd, const char *path, unsigned long long access) {
    int fd = open(path, O_PATH | O_CLOEXEC);
    if (fd < 0) { fprintf(stderr, "[launch] open(%s) failed: %s\n", path, strerror(errno)); return -1; }
    struct path_beneath_attr a = { .allowed_access = access, .parent_fd = fd };
    if (syscall(SYS_landlock_add_rule, ruleset_fd, LANDLOCK_RULE_PATH_BENEATH, &a, 0) != 0) {
        fprintf(stderr, "[launch] landlock_add_rule(%s) failed: %s\n", path, strerror(errno));
        close(fd);
        return -1;
    }
    close(fd);
    return 0;
}

static int apply_landlock(const char **ro, int nro, const char *rw) {
    long abi = syscall(SYS_landlock_create_ruleset, NULL, 0, LANDLOCK_CREATE_RULESET_VERSION);
    if (abi < 0) { fprintf(stderr, "[launch] landlock unavailable: %s\n", strerror(errno)); return -1; }
    struct ruleset_attr attr = { .handled_access_fs = handled_mask() };
    // Trim to the ABI's supported bits by re-querying: the kernel rejects
    // unknown bits, so drop TRUNCATE/IOCTL_DEV on older ABIs.
    if (abi < 3) attr.handled_access_fs &= ~FS_TRUNCATE;
    if (abi < 5) attr.handled_access_fs &= ~FS_IOCTL_DEV;
    int fd = syscall(SYS_landlock_create_ruleset, &attr, sizeof(attr), 0);
    if (fd < 0) { fprintf(stderr, "[launch] create_ruleset failed: %s\n", strerror(errno)); return -1; }
    for (int i = 0; i < nro; i++) if (add_rule(fd, ro[i], RO_RIGHTS) != 0) return -1;
    if (rw && add_rule(fd, rw, attr.handled_access_fs) != 0) return -1;
    if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0) {
        fprintf(stderr, "[launch] no_new_privs failed: %s\n", strerror(errno)); return -1;
    }
    if (syscall(SYS_landlock_restrict_self, fd, 0) != 0) {
        fprintf(stderr, "[launch] restrict_self failed: %s\n", strerror(errno)); return -1;
    }
    fprintf(stderr, "[launch] landlock ABI=%ld active (ro=%d, rw=%s)\n", abi, nro, rw ? rw : "-");
    return 0;
}

int main(int argc, char **argv) {
    const char *ro[32]; int nro = 0; const char *rw = NULL; int i = 1;
    const char *guard = getenv("W190_GUARD");
    for (; i < argc; i++) {
        if (!strcmp(argv[i], "--ro") && i + 1 < argc) ro[nro++] = argv[++i];
        else if (!strcmp(argv[i], "--rw") && i + 1 < argc) rw = argv[++i];
        else break;
    }
    if (i < argc && !strcmp(argv[i], "--")) i++;
    if (i >= argc) { fprintf(stderr, "usage: w190_launch [--ro D]... [--rw D] -- CMD...\n"); return 2; }
    if (apply_landlock(ro, nro, rw) != 0) return 20;

    // Prove Landlock from the launcher, before exec.
    int probe = open("/etc/hostname", O_RDONLY);
    fprintf(stderr, "[launch] pre-exec open(/etc/hostname) -> %d (%s)\n",
            probe, probe < 0 ? strerror(errno) : "ALLOWED");

    if (guard) { setenv("LD_PRELOAD", guard, 1); fprintf(stderr, "[launch] LD_PRELOAD=%s\n", getenv("LD_PRELOAD")); }
    fprintf(stderr, "[launch] exec %s\n", argv[i]);
    execv(argv[i], &argv[i]);
    fprintf(stderr, "[launch] execv(%s) failed: %s\n", argv[i], strerror(errno));
    return 21;
}
