// W190-A spike guest: aarch64 Android (NDK bionic, static) probe binary.
// Exercises: thread create/join, getpid, an allowed-path read, an outside-path
// read, and three syscalls the ACOS seccomp deny-list refuses (unshare with a
// namespace flag, ptrace, setuid).  It never needs a bionic linker64 because
// it is statically linked against the NDK's bionic libc.
#include <stdio.h>
#include <unistd.h>
#include <pthread.h>
#include <fcntl.h>
#include <string.h>
#include <errno.h>
#include <sys/syscall.h>

#ifndef CLONE_NEWUSER
#define CLONE_NEWUSER 0x10000000
#endif
#ifndef PTRACE_TRACEME
#define PTRACE_TRACEME 0
#endif

static void *worker(void *arg) {
    long id = (long)arg;
    return (void *)(id * 7);
}

static ssize_t try_read(const char *path) {
    if (!path) return -2;
    char buf[128] = {0};
    int fd = open(path, O_RDONLY);
    if (fd < 0) return -1;
    ssize_t n = read(fd, buf, sizeof buf - 1);
    close(fd);
    if (n > 0) {
        for (ssize_t i = 0; i < n; i++) if (buf[i] == '\n') buf[i] = ' ';
        fprintf(stderr, "  guest read %s -> %zd bytes: %s", path, n, buf);
    }
    return n;
}

int main(int argc, char **argv) {
    pthread_t t;
    void *ret = 0;
    if (pthread_create(&t, NULL, worker, (void *)6L) == 0) pthread_join(t, &ret);

    ssize_t allowed = try_read(argc > 1 ? argv[1] : NULL);
    ssize_t outside = try_read(argc > 2 ? argv[2] : NULL);
    if (allowed == -1 || outside == -1) fprintf(stderr, "  (a read was denied)\n");

    errno = 0;
    long unshare_rc = syscall(SYS_unshare, CLONE_NEWUSER);
    int unshare_err = errno;
    errno = 0;
    long ptrace_rc = syscall(SYS_ptrace, PTRACE_TRACEME, 0, 0, 0);
    int ptrace_err = errno;
    errno = 0;
    long setuid_rc = syscall(SYS_setuid, 0);
    int setuid_err = errno;

    printf("hello aarch64 pid=%d thread_ret=%ld allowed_read=%zd outside_read=%zd\n",
           (int)getpid(), (long)ret, allowed, outside);
    printf("  deny checks: unshare=%ld/errno=%d ptrace=%ld/errno=%d setuid=%ld/errno=%d\n",
           unshare_rc, unshare_err, ptrace_rc, ptrace_err, setuid_rc, setuid_err);
    fflush(stdout);
    return 0;
}
