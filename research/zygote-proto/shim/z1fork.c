/* z1fork.c -- the ONLY custom native code in experiment Z1 (61 lines of C).
 *
 * A JNI shim over the handful of process primitives a Zygote-style parent needs
 * (fork, pipe, wait, a monotonic clock).  It exists only because Android's Java
 * surface does not expose fork(): `android.system.Os` has no `fork`, and
 * `com.android.internal.os.Zygote`'s natives all go through
 * `ForkAndSpecializeCommon`, which needs SELinux/uid/namespace privileges this
 * prototype does not have.  Everything else -- the zygote-space freeze, the
 * no-thread window, the post-fork runtime re-init -- is Google's own code,
 * called through `dalvik.system.ZygoteHooks` (see java/dev/acos/z1/ZygoteMain).
 *
 * Build (the same recipe as crates/acos-art/shim/artshim.c):
 *   clang -nostdlib -ffreestanding -fno-builtin -fPIC -shared -O2 \
 *         -o libz1fork.so z1fork.c
 *
 * Raw `syscall` instructions, not libc wrappers: bionic exports fork/waitpid/
 * pipe only under symbol versions a `-nostdlib` .so cannot bind (measured:
 * "cannot locate symbol waitpid").  Raw fork(2) skips bionic's pthread_atfork
 * handlers, which is safe *here* because ART's `ZygoteHooks.preFork()` has
 * already stopped the world -- the parent is a one-thread process at the fork
 * point (the parent re-checks /proc/self/status Threads:1 and logs it), so no
 * other thread can hold a libc/allocator lock that a child would inherit.
 */
typedef int jint;
typedef long jlong;
typedef unsigned long usize;

struct ts {
  long sec;
  long nsec;
};

#define EXPORT __attribute__((visibility("default")))

/* x86_64 syscall numbers (arch of .cache/art/sysroot: abi=x86_64). */
#define SYS_getpid 39
#define SYS_rt_sigaction 13
#define SYS_getppid 110
#define SYS_fork 57
#define SYS_wait4 61
#define SYS_clock_gettime 228

static long z1_sys4(long n, long a, long b, long c, long d) {
  register long r10 __asm__("r10") = d;
  long ret;
  __asm__ volatile("syscall"
                   : "=a"(ret)
                   : "a"(n), "D"(a), "S"(b), "d"(c), "r"(r10)
                   : "rcx", "r11", "memory");
  return ret;
}

static long z1_sys3(long n, long a, long b, long c) { return z1_sys4(n, a, b, c, 0); }
static long z1_sys2(long n, long a, long b) { return z1_sys4(n, a, b, 0, 0); }

EXPORT jint Java_dev_acos_z1_Nat_fork(void) { return (jint)z1_sys2(SYS_fork, 0, 0); }

EXPORT jint Java_dev_acos_z1_Nat_getpid(void) { return (jint)z1_sys2(SYS_getpid, 0, 0); }

EXPORT jint Java_dev_acos_z1_Nat_getppid(void) { return (jint)z1_sys2(SYS_getppid, 0, 0); }

/* Blocking wait for one child. Encoded as (wait4_ret << 32) | (status & 0xffffffff)
 * so the caller can tell "reaped pid N with status S" from "wait4 failed" (ret<=0). */
EXPORT jlong Java_dev_acos_z1_Nat_waitPid(jint pid) {
  int st = 0;
  long r = z1_sys4(SYS_wait4, (long)pid, (long)&st, 0, 0);
  return (jlong)r * 4294967296L + (jlong)(unsigned int)st;
}

/* Restore SIGCHLD to SIG_DFL. bwrap is PID 1 of the sandbox's pid namespace and
 * installs a SIGCHLD disposition to reap orphans; that disposition survives its
 * exec into dalvikvm64, and with SIGCHLD ignored a child is auto-reaped and
 * wait4(2) fails with ECHILD (measured). One rt_sigaction(SIGCHLD, SIG_DFL). */
EXPORT jint Java_dev_acos_z1_Nat_resetSigchld(void) {
  struct {
    void *handler;
    unsigned long flags;
    void *restorer;
    unsigned long mask;
  } sa = {0, 0, 0, 0};
  return (jint)z1_sys4(SYS_rt_sigaction, 17 /* SIGCHLD */, (long)&sa, 0, 8 /* sizeof(sigset_t) */);
}

/* CLOCK_MONOTONIC ns: the one clock used for every fork->ready and cold-start
 * figure in this experiment (the cold control derives its own start time from
 * /proc/self/stat's starttime jiffies on the same clock base). */
EXPORT jlong Java_dev_acos_z1_Nat_nanos(void) {
  struct ts t = {0, 0};
  if (z1_sys2(SYS_clock_gettime, 1 /* CLOCK_MONOTONIC */, (long)&t) != 0) return 0;
  return (jlong)t.sec * 1000000000L + (jlong)t.nsec;
}
