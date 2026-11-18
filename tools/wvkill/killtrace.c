// WVKILL diagnostic: LD_PRELOAD shim that logs every signal-sending libc call
// (kill, killpg, tgkill, pidfd_send_signal, syscall(SYS_kill|tgkill|pidfd_send_signal))
// as one line: <unix_ns> <sender pid> <sender comm> -> <call> target=<pid> sig=<n> (<target comm>)
#define _GNU_SOURCE
#include <dlfcn.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <fcntl.h>
#include <time.h>
#include <unistd.h>
#include <signal.h>
#include <sys/syscall.h>
#include <sys/types.h>

static void comm_of(pid_t p, char *out, size_t n) {
  char path[64]; snprintf(path, sizeof path, "/proc/%d/comm", (int)p);
  int fd = open(path, O_RDONLY); out[0] = 0;
  if (fd >= 0) { ssize_t r = read(fd, out, n - 1); if (r > 0) { out[r] = 0; char *nl = strchr(out, '\n'); if (nl) *nl = 0; } close(fd); }
  if (!out[0]) snprintf(out, n, "?");
}
static void note(const char *call, long target, int sig) {
  const char *path = getenv("WVKILL_LOG"); if (!path) return;
  int fd = open(path, O_WRONLY | O_APPEND | O_CREAT | O_CLOEXEC, 0644); if (fd < 0) return;
  struct timespec ts; clock_gettime(CLOCK_REALTIME, &ts);
  char me[32], tg[32], line[256];
  comm_of(getpid(), me, sizeof me);
  comm_of(target > 0 ? (pid_t)target : (pid_t)-target, tg, sizeof tg);
  int len = snprintf(line, sizeof line, "%ld.%09ld sender=%d(%s) tid=%ld ppid=%d %s target=%ld(%s) sig=%d\n",
                     (long)ts.tv_sec, ts.tv_nsec, (int)getpid(), me, (long)syscall(SYS_gettid), (int)getppid(), call, target, tg, sig);
  if (write(fd, line, len) < 0) {}
  close(fd);
}
int kill(pid_t pid, int sig) {
  static int (*real)(pid_t, int); if (!real) real = dlsym(RTLD_NEXT, "kill");
  if (sig != 0) note("kill", pid, sig);
  return real(pid, sig);
}
int killpg(int pg, int sig) {
  static int (*real)(int, int); if (!real) real = dlsym(RTLD_NEXT, "killpg");
  if (sig != 0) note("killpg", -pg, sig);
  return real(pg, sig);
}
int tgkill(pid_t tgid, pid_t tid, int sig) {
  static int (*real)(pid_t, pid_t, int); if (!real) real = dlsym(RTLD_NEXT, "tgkill");
  if (sig != 0) note("tgkill", tgid, sig);
  return real(tgid, tid, sig);
}
long syscall(long nr, ...) {
  static long (*real)(long, ...); if (!real) real = dlsym(RTLD_NEXT, "syscall");
  va_list ap; va_start(ap, nr);
  long a1 = va_arg(ap, long), a2 = va_arg(ap, long), a3 = va_arg(ap, long), a4 = va_arg(ap, long), a5 = va_arg(ap, long), a6 = va_arg(ap, long);
  va_end(ap);
  if (nr == SYS_kill && a2 != 0) note("syscall(kill)", a1, (int)a2);
  else if (nr == SYS_tgkill && a3 != 0) note("syscall(tgkill)", a1, (int)a3);
  else if (nr == SYS_pidfd_send_signal && a2 != 0) { note("syscall(pidfd_send_signal)", a1, (int)a2); }
  return real(nr, a1, a2, a3, a4, a5, a6);
}
