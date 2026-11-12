/* z2fork.c -- Z2 shim: process primitives + in-process sandbox for the Zygote prototype.
 *
 * Z1 needed 91 lines to fork/wait.  Z2 keeps those (raw `syscall` instructions,
 * because bionic exports fork/waitpid only under symbol versions a -nostdlib
 * .so cannot bind) and adds the pieces the in-process sandbox needs:
 *
 *   - wait4 with options + the current SIGCHLD disposition (ECHILD diagnosis),
 *   - unshare(CLONE_NEWUSER | CLONE_NEWNS | CLONE_NEWPID),
 *   - private mounts (tmpfs /data/data, tmpfs /data/zygote-private, proc),
 *   - a seccomp-BPF blocklist installed while the process still has
 *     CAP_SYS_ADMIN in its own user namespace (AOSP order), then
 *   - an explicit capability drop (there is no uid change we can make
 *     unprivileged -- only one uid is mapped in the parent user namespace),
 *     and setresuid last, as AOSP does.
 *
 * Everything is raw syscalls: no bionic headers, no libc.
 *
 * Build (same recipe as crates/acos-art/shim/artshim.c):
 *   clang -nostdlib -ffreestanding -fno-builtin -fno-stack-protector \
 *         -fPIC -shared -O2 -o libz2fork.so z2fork.c
 */
typedef int jint;
typedef long jlong;
typedef unsigned long usize;
typedef unsigned int u32;
typedef unsigned short u16;
typedef unsigned char u8;

struct ts {
  long sec;
  long nsec;
};

#define EXPORT __attribute__((visibility("default")))
#define NULL ((void *)0)

/* x86_64 syscall numbers. */
#define SYS_read 0
#define SYS_write 1
#define SYS_close 3
#define SYS_mkdir 83
#define SYS_getpid 39
#define SYS_getppid 110
#define SYS_getuid 102
#define SYS_getgid 104
#define SYS_geteuid 107
#define SYS_setresuid 117
#define SYS_setresgid 119
#define SYS_setgroups 116
#define SYS_capset 126
#define SYS_prctl 157
#define SYS_ptrace 101
#define SYS_mount 165
#define SYS_unshare 272
#define SYS_openat 257
#define SYS_fork 57
#define SYS_wait4 61
#define SYS_pipe2 293
#define SYS_kill 62
#define SYS_rt_sigaction 13
#define SYS_clock_gettime 228
#define SYS_exit_group 231

#define AT_FDCWD (-100)
#define O_WRONLY 1
#define O_CREAT 0100
#define O_TRUNC 01000
#define O_CLOEXEC 02000000

#define CLONE_NEWNS 0x00020000
#define CLONE_NEWPID 0x20000000
#define CLONE_NEWUSER 0x10000000
#define MS_REC 16384
#define MS_PRIVATE (1 << 18)
#define MS_NOSUID 2
#define MS_NODEV 4
#define MS_NOEXEC 8

#define PR_SET_NAME 15
#define PR_SET_SECCOMP 22
#define PR_GET_SECCOMP 21
#define PR_SET_NO_NEW_PRIVS 38
#define PR_CAPBSET_DROP 24
#define SECCOMP_MODE_FILTER 2

#define SIGCHLD 17
#define SIG_DFL 0
#define WNOHANG 1
#define __WALL 0x40000000

static long z2_sys6(long n, long a, long b, long c, long d, long e, long f) {
  register long r10 __asm__("r10") = d;
  register long r8 __asm__("r8") = e;
  register long r9 __asm__("r9") = f;
  long ret;
  __asm__ volatile("syscall"
                   : "=a"(ret)
                   : "a"(n), "D"(a), "S"(b), "d"(c), "r"(r10), "r"(r8), "r"(r9)
                   : "rcx", "r11", "memory");
  return ret;
}
static long z2_sys5(long n, long a, long b, long c, long d, long e) {
  return z2_sys6(n, a, b, c, d, e, 0);
}
static long z2_sys4(long n, long a, long b, long c, long d) {
  return z2_sys6(n, a, b, c, d, 0, 0);
}
static long z2_sys3(long n, long a, long b, long c) { return z2_sys6(n, a, b, c, 0, 0, 0); }
static long z2_sys2(long n, long a, long b) { return z2_sys6(n, a, b, 0, 0, 0, 0); }
static long z2_sys1(long n, long a) { return z2_sys6(n, a, 0, 0, 0, 0, 0); }
static long z2_sys0(long n) { return z2_sys6(n, 0, 0, 0, 0, 0, 0); }

/* ------------------------------------------------------------------ process */

EXPORT jint Java_dev_acos_z1_Nat_fork(void) { return (jint)z2_sys0(SYS_fork); }
EXPORT jint Java_dev_acos_z1_Nat_getpid(void) { return (jint)z2_sys0(SYS_getpid); }
EXPORT jint Java_dev_acos_z1_Nat_getppid(void) { return (jint)z2_sys0(SYS_getppid); }
EXPORT jint Java_dev_acos_z1_Nat_getuid(void) { return (jint)z2_sys0(SYS_getuid); }
EXPORT jint Java_dev_acos_z1_Nat_getgid(void) { return (jint)z2_sys0(SYS_getgid); }
EXPORT jint Java_dev_acos_z1_Nat_geteuid(void) { return (jint)z2_sys0(SYS_geteuid); }

static long wait_any(int pid, int options) {
  int st = 0;
  long r = z2_sys4(SYS_wait4, (long)pid, (long)&st, (long)options, 0);
  return r * 4294967296L + (long)(unsigned int)st;
}

/* (wait4 return << 32) | status.  ret < 0 is -errno (raw syscall convention). */
EXPORT jlong Java_dev_acos_z1_Nat_waitPid(void *env, void *cls, jint pid) { return (jlong)wait_any(pid, 0); }
EXPORT jlong Java_dev_acos_z1_Nat_waitPidOpt(void *env, void *cls, jint pid, jint options) {
  return (jlong)wait_any(pid, options);
}

/* Overflow-safe enough for 10 threads: return the status word only. */
EXPORT jint Java_dev_acos_z1_Nat_waitStatus(void *env, void *cls, jint pid, jint options) {
  int st = 0;
  z2_sys4(SYS_wait4, (long)pid, (long)&st, (long)options, 0);
  return (jint)st;
}

struct ksigaction {
  void *handler;
  unsigned long flags;
  void *restorer;
  unsigned long mask;
};

static long sigaction_set(int sig, void *handler) {
  struct ksigaction sa;
  sa.handler = handler;
  sa.flags = 0;
  sa.restorer = 0;
  sa.mask = 0;
  return z2_sys4(SYS_rt_sigaction, (long)sig, (long)&sa, 0, 8);
}

EXPORT jint Java_dev_acos_z1_Nat_resetSigchld(void) {
  return (jint)sigaction_set(SIGCHLD, (void *)SIG_DFL);
}

/* (handler << 32) | flags; handler 0 = SIG_DFL, 1 = SIG_IGN. */
EXPORT jlong Java_dev_acos_z1_Nat_sigchldAction(void) {
  struct ksigaction old;
  old.handler = 0;
  old.flags = 0;
  old.restorer = 0;
  old.mask = 0;
  long r = z2_sys4(SYS_rt_sigaction, (long)SIGCHLD, 0, (long)&old, 8);
  if (r < 0) return (jlong)r;
  return (jlong)(unsigned long)old.handler * 4294967296L + (jlong)old.flags;
}

EXPORT jlong Java_dev_acos_z1_Nat_nanos(void) {
  struct ts t;
  t.sec = 0;
  t.nsec = 0;
  if (z2_sys2(SYS_clock_gettime, 1 /* CLOCK_MONOTONIC */, (long)&t) != 0) return 0;
  return (jlong)t.sec * 1000000000L + (jlong)t.nsec;
}

EXPORT jint Java_dev_acos_z1_Nat_kill(void *env, void *cls, jint pid, jint sig) {
  return (jint)z2_sys2(SYS_kill, (long)pid, (long)sig);
}

/* ----------------------------------------------------------------- file i/o */

static long z2_write_file(const char *path, const char *s, long len) {
  long fd = z2_sys4(SYS_openat, AT_FDCWD, (long)path, O_WRONLY | O_CLOEXEC, 0);
  if (fd < 0) return fd;
  long r = z2_sys3(SYS_write, fd, (long)s, len);
  z2_sys1(SYS_close, fd);
  return r == len ? 0 : (r < 0 ? r : -5);
}

static int z2_strlen(const char *s) {
  int n = 0;
  while (s[n]) n++;
  return n;
}

static char *z2_append(char *p, const char *s) {
  while (*s) *p++ = *s++;
  return p;
}

static char *z2_append_int(char *p, long v, int width) {
  char tmp[24];
  int n = 0;
  if (v < 0) {
    *p++ = '-';
    v = -v;
  }
  do {
    tmp[n++] = (char)('0' + (v % 10));
    v /= 10;
  } while (v);
  while (n < width--) tmp[n++] = '0';
  while (n) *p++ = tmp[--n];
  return p;
}

/* ------------------------------------------------------------- namespaces */

static long z2_prctl_name(const char *name) {
  return z2_sys5(SYS_prctl, PR_SET_NAME, (long)name, 0, 0, 0);
}

/* unshare(CLONE_NEWUSER) and map exactly the one uid/gid the parent user
 * namespace allows an unprivileged process to map: our own.  Returns 0 or
 * -errno; the mapping errors are folded into the return value (-1000-n). */
static long z2_setup_user_ns(int *uid_out, int *gid_out) {
  long uid = z2_sys0(SYS_getuid);
  long gid = z2_sys0(SYS_getgid);
  *uid_out = (int)uid;
  *gid_out = (int)gid;
  long r = z2_sys1(SYS_unshare, CLONE_NEWUSER);
  if (r < 0) return r;
  char buf[96];
  char *p;
  z2_write_file("/proc/self/setgroups", "deny", 4);
  p = z2_append(buf, "0 ");
  p = z2_append_int(p, uid, 0);
  p = z2_append(p, " 1\n");
  r = z2_write_file("/proc/self/uid_map", buf, p - buf);
  if (r < 0) return -1000 + r;
  p = z2_append(buf, "0 ");
  p = z2_append_int(p, gid, 0);
  p = z2_append(p, " 1\n");
  r = z2_write_file("/proc/self/gid_map", buf, p - buf);
  if (r < 0) return -1100 + r;
  return 0;
}

EXPORT jint Java_dev_acos_z1_Nat_setupUserNs(void) {
  int uid, gid;
  return (jint)z2_setup_user_ns(&uid, &gid);
}

/* Mount + PID namespace + mounts + seccomp, all in the forked child, then the
 * app process returns to Java.  Results are inherited by the app and read back
 * through Nat.sandboxRc(slot).
 *
 * Slots: 0 userns, 1 mntns, 2 pidns, 3 mount_private, 4 tmpfs_data,
 *        5 mkdir_private, 6 tmpfs_zygote, 7 proc, 8 seccomp, 9 capset,
 *        10 setresgid, 11 setresuid_other (EPERM/EINVAL proof), 12 setresuid,
 *        13 uid_before, 14 gid_before, 15 app_pid_in_ns, 16 reaper_pid.
 */
static long z2_rc[20];

EXPORT jint Java_dev_acos_z1_Nat_sandboxRc(void *env, void *cls, jint slot) {
  if (slot < 0 || slot >= 20) return 0;
  return (jint)z2_rc[slot];
}

/* --------------------------------------------------------------- seccomp */

struct sock_filter {
  u16 code;
  u8 jt;
  u8 jf;
  u32 k;
};
struct sock_fprog {
  u16 len;
  struct sock_filter *filter;
};

#define BPF_LD 0x00
#define BPF_W 0x00
#define BPF_ABS 0x20
#define BPF_JMP 0x05
#define BPF_JEQ 0x10
#define BPF_K 0x00
#define BPF_RET 0x06
#define SECCOMP_RET_KILL_PROCESS 0x80000000u
#define SECCOMP_RET_ERRNO 0x00050000u
#define SECCOMP_RET_ALLOW 0x7fff0000u
#define AUDIT_ARCH_X86_64 0xC000003Eu

static struct sock_filter z2_filter[96];
static int z2_nf;

static void z2_emit(u16 code, u8 jt, u8 jf, u32 k) {
  if (z2_nf >= 96) return;
  z2_filter[z2_nf].code = code;
  z2_filter[z2_nf].jt = jt;
  z2_filter[z2_nf].jf = jf;
  z2_filter[z2_nf].k = k;
  z2_nf++;
}

/* Blocklist, returning EPERM (SIGSYS-free, so the app sees an error instead of
 * dying).  Deliberately not AOSP's allowlist: this prototype wants to prove a
 * filter is installed and enforced; a product would use AOSP's list. */
static const int z2_blocked[] = {
    101,  /* ptrace           */
    165,  /* mount            */
    166,  /* umount2          */
    155,  /* pivot_root       */
    161,  /* chroot           */
    272,  /* unshare          */
    308,  /* setns            */
    57,   /* fork             */
    58,   /* vfork            */
    321,  /* bpf              */
    250,  /* keyctl           */
    248,  /* add_key          */
    249,  /* request_key      */
    310,  /* process_vm_readv */
    311,  /* process_vm_writev*/
    246,  /* kexec_load       */
    320,  /* kexec_file_load  */
    175,  /* init_module      */
    313,  /* finit_module     */
    176,  /* delete_module    */
    304,  /* open_by_handle_at*/
    303,  /* name_to_handle_at*/
    172,  /* iopl             */
    173,  /* ioperm           */
    323,  /* userfaultfd      */
    298,  /* perf_event_open  */
    167,  /* swapon           */
    168,  /* swapoff          */
    169,  /* reboot           */
    170,  /* sethostname      */
    171,  /* setdomainname    */
    163,  /* acct             */
    164,  /* settimeofday     */
    227,  /* clock_settime    */
    159,  /* adjtimex         */
    305,  /* clock_adjtime    */
    103,  /* syslog           */
    153,  /* vhangup          */
    133,  /* mknod            */
    259,  /* mknodat          */
};

static long z2_install_seccomp(void) {
  int i;
  z2_nf = 0;
  z2_emit(BPF_LD | BPF_W | BPF_ABS, 0, 0, 4);              /* seccomp_data.arch */
  z2_emit(BPF_JMP | BPF_JEQ | BPF_K, 1, 0, AUDIT_ARCH_X86_64);
  z2_emit(BPF_RET | BPF_K, 0, 0, SECCOMP_RET_KILL_PROCESS);
  z2_emit(BPF_LD | BPF_W | BPF_ABS, 0, 0, 0);              /* seccomp_data.nr */
  for (i = 0; i < (int)(sizeof(z2_blocked) / sizeof(z2_blocked[0])); i++) {
    z2_emit(BPF_JMP | BPF_JEQ | BPF_K, 0, 1, (u32)z2_blocked[i]);
    z2_emit(BPF_RET | BPF_K, 0, 0, SECCOMP_RET_ERRNO | 1 /* EPERM */);
  }
  z2_emit(BPF_RET | BPF_K, 0, 0, SECCOMP_RET_ALLOW);
  struct sock_fprog prog;
  prog.len = (u16)z2_nf;
  prog.filter = z2_filter;
  z2_sys2(SYS_prctl, PR_SET_NO_NEW_PRIVS, 1);
  return z2_sys3(SYS_prctl, PR_SET_SECCOMP, SECCOMP_MODE_FILTER, (long)&prog);
}

struct cap_hdr {
  u32 version;
  int pid;
};
struct cap_data {
  u32 effective;
  u32 permitted;
  u32 inheritable;
};

/* Drop every capability, including the bounding set.  There is no uid change
 * available (see Z2.md), so this is what actually removes privilege. */
static long z2_drop_caps(void) {
  int cap;
  for (cap = 0; cap <= 40; cap++) z2_sys5(SYS_prctl, PR_CAPBSET_DROP, cap, 0, 0, 0);
  struct cap_hdr hdr;
  struct cap_data data[2];
  hdr.version = 0x20080522; /* _LINUX_CAPABILITY_VERSION_3 */
  hdr.pid = 0;
  data[0].effective = data[0].permitted = data[0].inheritable = 0;
  data[1].effective = data[1].permitted = data[1].inheritable = 0;
  return z2_sys2(SYS_capset, (long)&hdr, (long)data);
}

/* ---------------------------------------------------------- sandbox spawn */

static char *z2_pkg_path(char *buf, int tag) {
  char *p = z2_append(buf, "/data/data/pkg-");
  p = z2_append_int(p, tag, 0);
  *p = 0;
  return buf;
}

/* Called in the zygote's forked child.  Topology:
 *
 *   zygote --fork--> A (this process, starts the sandbox; stays outside the new
 *                       PID namespace and forwards the app's real wait status)
 *                     A --unshare(NEWUSER|NEWNS|NEWPID), fork--> B (PID 1)
 *                       B mounts tmpfs/proc, forks--> C (PID 2, the ART app)
 *                       B waits C and writes the raw wait status to a pipe
 *                     A reads it, re-raises a signal death, exits with the code
 *
 * Returns 0 in C only; A and B _exit().  Returns <0 only if the pipe/fork
 * failed before a child existed. */
EXPORT jint Java_dev_acos_z1_Nat_spawnSandboxed(void *env, void *cls, jint tag) {
  int uid = 0, gid = 0;
  z2_rc[13] = uid;
  z2_rc[14] = gid;
  z2_prctl_name("z2ns");

  z2_rc[0] = z2_setup_user_ns(&uid, &gid);
  z2_rc[13] = uid;
  z2_rc[14] = gid;
  if (z2_rc[0] == 0) {
    z2_rc[1] = z2_sys1(SYS_unshare, CLONE_NEWNS);
    z2_rc[2] = z2_sys1(SYS_unshare, CLONE_NEWPID);
  }

  int pfd[2] = {-1, -1};
  if (z2_sys2(SYS_pipe2, (long)pfd, O_CLOEXEC) < 0) return -30;

  long b = z2_sys0(SYS_fork);
  if (b < 0) return (jint)b;
  if (b > 0) {
    /* ---- A: status forwarder (still in the bwrap PID namespace) ---- */
    int st = 0;
    int cst = 0;
    long n;
    z2_rc[16] = b;
    z2_sys1(SYS_close, pfd[1]);
    z2_sys4(SYS_wait4, b, (long)&st, 0, 0);
    n = z2_sys3(SYS_read, pfd[0], (long)&cst, 4);
    z2_sys1(SYS_close, pfd[0]);
    if (n != 4) cst = st;
    if ((cst & 0x7f) != 0) {
      int sig = cst & 0x7f;
      sigaction_set(sig, (void *)SIG_DFL);
      z2_sys2(SYS_kill, z2_sys0(SYS_getpid), sig);
    }
    z2_sys1(SYS_exit_group, (cst >> 8) & 0xff);
    for (;;) {
    }
  }

  /* ---- B: init of the private PID namespace ---- */
  z2_sys1(SYS_close, pfd[0]);
  z2_prctl_name("z2init");
  if (z2_rc[1] == 0) {
    z2_rc[3] = z2_sys4(SYS_mount, 0, (long)"/", 0, MS_REC | MS_PRIVATE);
    z2_rc[4] = z2_sys5(SYS_mount, (long)"tmpfs", (long)"/data/data", (long)"tmpfs",
                       MS_NOSUID | MS_NODEV, (long)"mode=0700");
    char pkg[64];
    z2_pkg_path(pkg, tag);
    z2_rc[5] = z2_sys2(SYS_mkdir, (long)pkg, 0700);
    z2_rc[6] = z2_sys5(SYS_mount, (long)"tmpfs", (long)"/data/zygote-private",
                       (long)"tmpfs", MS_NOSUID | MS_NODEV, (long)"mode=0700");
    z2_rc[7] = z2_sys5(SYS_mount, (long)"proc", (long)"/proc", (long)"proc",
                       MS_NOSUID | MS_NODEV | MS_NOEXEC, 0);
  }

  long c = z2_sys0(SYS_fork);
  if (c == 0) {
    /* ---- C: the ART app ---- */
    z2_sys1(SYS_close, pfd[1]);
    z2_rc[10] = z2_sys3(SYS_setresgid, 0, 0, 0);
    z2_rc[8] = z2_install_seccomp();
    z2_rc[9] = z2_drop_caps();
    /* Proof that a non-mapped uid cannot be assumed unprivileged. */
    z2_rc[11] = z2_sys3(SYS_setresuid, 10000, 10000, 10000);
    z2_rc[12] = z2_sys3(SYS_setresuid, 0, 0, 0);
    z2_rc[15] = z2_sys0(SYS_getpid);
    z2_prctl_name("z2app");
    return 0;
  }
  if (c < 0) {
    z2_sys1(SYS_close, pfd[1]);
    return (jint)c;
  }
  /* B waits for C and hands the raw wait status back to A. */
  int st = 0;
  z2_sys4(SYS_wait4, c, (long)&st, 0, 0);
  z2_sys3(SYS_write, pfd[1], (long)&st, 4);
  z2_sys1(SYS_close, pfd[1]);
  z2_sys1(SYS_exit_group, (st >> 8) & 0xff);
  for (;;) {
  }
}

/* ----------------------------------------------------------- test probes */

/* All of these are called by Java *after* the sandbox is installed. */
EXPORT jint Java_dev_acos_z1_Nat_ptraceSelf(void) {
  return (jint)z2_sys4(SYS_ptrace, 0 /* PTRACE_TRACEME */, 0, 0, 0);
}

EXPORT jint Java_dev_acos_z1_Nat_mountProbe(void) {
  return (jint)z2_sys5(SYS_mount, (long)"tmpfs", (long)"/data/probe", (long)"tmpfs", 0, 0);
}

EXPORT jint Java_dev_acos_z1_Nat_unshareProbe(void) {
  return (jint)z2_sys1(SYS_unshare, CLONE_NEWNS);
}

EXPORT jint Java_dev_acos_z1_Nat_seccompStatus(void) {
  return (jint)z2_sys2(SYS_prctl, PR_GET_SECCOMP, 0);
}
