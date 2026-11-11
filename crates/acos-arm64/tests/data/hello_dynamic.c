/* W192-A integration guest: a dynamically-linked aarch64 bionic binary.
 *
 * It runs under `qemu-aarch64 -L <bionic-sysroot>` inside the ACOS
 * confinement. Its observable lines let the integration test prove:
 *   - the real bionic `linker64` + `libc.so` resolved (the program ran);
 *   - Landlock denied a host path outside the granted roots (EACCES);
 *   - the post-execve seccomp guard denied `ptrace`/`unshare` (EACCES).
 */
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <sys/ptrace.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef SYS_unshare
#define SYS_unshare 97
#endif
#ifndef PTRACE_TRACEME
#define PTRACE_TRACEME 0
#endif
#ifndef CLONE_NEWUSER
#define CLONE_NEWUSER 0x10000000
#endif

int main(void) {
    errno = 0;
    int fd = open("/etc/hostname", O_RDONLY);
    int hostname_errno = errno;
    if (fd >= 0) {
        close(fd);
    }

    errno = 0;
    long ptrace_rc = ptrace(PTRACE_TRACEME, 0, 0, 0);
    int ptrace_errno = errno;

    errno = 0;
    long unshare_rc = syscall(SYS_unshare, CLONE_NEWUSER);
    int unshare_errno = errno;

    errno = 0;
    long setuid_rc = setuid(0);
    int setuid_errno = errno;

    printf(
        "guest pid=%d hostname_fd=%d hostname_errno=%d "
        "ptrace=%ld ptrace_errno=%d unshare=%ld unshare_errno=%d "
        "setuid=%ld setuid_errno=%d\n",
        (int)getpid(), fd, hostname_errno, ptrace_rc, ptrace_errno,
        unshare_rc, unshare_errno, setuid_rc, setuid_errno);
    fflush(stdout);
    return 0;
}
