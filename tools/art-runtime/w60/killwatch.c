#define _GNU_SOURCE
#include <dlfcn.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <time.h>
#include <stdlib.h>
#include <errno.h>
#include <stdarg.h>
#include <unistd.h>

static int logfd = -1;
static void initlog(void) {
    if (logfd >= 0) return;
    const char *p = getenv("ACOS_KILLWATCH_LOG");
    if (!p) { logfd = -2; return; }
    logfd = open(p, O_WRONLY | O_CREAT | O_APPEND, 0644);
    if (logfd < 0) logfd = -2;
}
static void logline(const char *fmt, ...) __attribute__((format(printf,1,2)));
static void logline(const char *fmt, ...) {
    initlog();
    if (logfd < 0) return;
    char buf[512];
    struct timespec ts; clock_gettime(CLOCK_REALTIME, &ts);
    int n = snprintf(buf, sizeof buf, "%ld.%06ld pid=%d ppid=%d comm=%s ",
                     (long)ts.tv_sec, ts.tv_nsec/1000, getpid(), getppid(), program_invocation_short_name);
    va_list ap; va_start(ap, fmt);
    n += vsnprintf(buf+n, sizeof buf - n, fmt, ap);
    va_end(ap);
    if (n > (int)sizeof buf - 2) n = sizeof buf - 2;
    buf[n++] = '\n';
    ssize_t r = write(logfd, buf, n); (void)r;
}

int kill(pid_t pid, int sig) {
    static int (*real)(pid_t,int);
    if (!real) real = dlsym(RTLD_NEXT, "kill");
    if (sig == SIGKILL) logline("KILL kill(pid=%d, SIGKILL)", pid);
    return real(pid, sig);
}
int killpg(pid_t pgid, int sig) {
    static int (*real)(pid_t,int);
    if (!real) real = dlsym(RTLD_NEXT, "killpg");
    if (sig == SIGKILL) logline("KILL killpg(pgid=%d, SIGKILL)", pgid);
    return real(pgid, sig);
}
int tgkill(pid_t tgid, pid_t tid, int sig) {
    static int (*real)(pid_t,pid_t,int);
    if (!real) real = dlsym(RTLD_NEXT, "tgkill");
    if (sig == SIGKILL) logline("KILL tgkill(tgid=%d, tid=%d, SIGKILL)", tgid, tid);
    return real(tgid, tid, sig);
}
int tkill(pid_t tid, int sig) {
    static int (*real)(pid_t,int);
    if (!real) real = dlsym(RTLD_NEXT, "tkill");
    if (sig == SIGKILL) logline("KILL tkill(tid=%d, SIGKILL)", tid);
    return real(tid, sig);
}
long syscall(long number, ...) {
    /* catch raw syscall(SYS_kill/tgkill/tkill) uses from libc internals is not needed;
       keep a marker for pidfd_send_signal via libc wrapper below. */
    va_list ap; va_start(ap, number);
    long a1=va_arg(ap,long), a2=va_arg(ap,long), a3=va_arg(ap,long), a4=va_arg(ap,long),
         a5=va_arg(ap,long), a6=va_arg(ap,long);
    va_end(ap);
    static long (*real)(long,...);
    if (!real) real = dlsym(RTLD_NEXT, "syscall");
    if (number == SYS_kill && a2 == SIGKILL) logline("KILL syscall(kill, pid=%ld, SIGKILL)", a1);
    if (number == SYS_tgkill && a3 == SIGKILL) logline("KILL syscall(tgkill, tgid=%ld, tid=%ld, SIGKILL)", a1, a2);
    return real(number,a1,a2,a3,a4,a5,a6);
}
ssize_t write(int fd, const void *buf, size_t count) {
    static ssize_t (*real)(int,const void*,size_t);
    if (!real) real = dlsym(RTLD_NEXT, "write");
    if (count == 2 && ((const char*)buf)[0] == '1' && ((const char*)buf)[1] == '\n') {
        char path[256], link[300]; int m = snprintf(path,sizeof path,"/proc/self/fd/%d",fd);
        if (m > 0) { ssize_t l = readlink(path, link, sizeof link - 1);
            if (l > 0) { link[l]=0;
                if (strstr(link, "cgroup.kill")) {
                    logline("KILL write(%s)", link);
                } } }
    }
    return real(fd, buf, count);
}
