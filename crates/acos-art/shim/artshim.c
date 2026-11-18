/* artshim.c -- freestanding (no libc, no DT_NEEDED) LD_PRELOAD shim for real
 * ART running inside the ACOS bwrap mini-Android root.
 *
 *   clang -nostdlib -ffreestanding -fno-builtin -fPIC -shared -O2 \
 *         -o libartshim.so artshim.c
 *
 * Two jobs, both of which bionic cannot do for us on a plain Linux host:
 *
 * 1. liblog sink.  Android has no logd here, and liblog's "already have a
 *    log" fallbacks are compiled out, so every ART LOG()/CHECK() message is
 *    lost.  We format the message ourselves and send one datagram per entry to
 *    /dev/socket/logdw, where the Rust `acos-art` logd records structured
 *    entries ({prio, tag, message}).  If the socket is absent the entry goes to
 *    fd 2 instead, so a bare `artroot`-style launch still shows the message.
 *
 * 2. Properties.  bionic 34 reads properties from /dev/__properties__ through
 *    `property_info` (a serialised trie written by init, which we do not run).
 *    We interpose the `__system_property_*` API and serve a table parsed from
 *    /data/acos.properties (written by `acos-art`, see src/props.rs) in the
 *    exact in-memory `prop_info` layout bionic exposes, so callers that read
 *    the struct directly (`__system_property_serial`, `pi->value`) also work.
 *
 * Fatal paths are NOT suppressed.  `__android_log_call_aborter` and
 * `__android_log_assert` are interposed only to route the final message into
 * the logd sink first (liblog has no working writer here), then they do what
 * bionic's default aborter does: raise SIGABRT.  A `CHECK` failure therefore
 * kills the process, exactly as on device; the launcher reads the recorded
 * fatal entry plus the signal and reports `ART_FATAL_CHECK`.
 *
 * 3. Priority adapter (W21).  Stock `init.rc` sets `RLIMIT_NICE=40`, which is
 *    what lets an app thread ask for a negative nice (e.g.
 *    `Process.setThreadPriority(THREAD_PRIORITY_DISPLAY)` = -4).  An
 *    unprivileged ACOS host session has a hard limit of 0, and a user-namespace
 *    CAP_SYS_RESOURCE cannot raise it (the kernel checks the initial user
 *    namespace).  `setpriority` is therefore interposed so a negative nice is
 *    clamped to 0 and reported as success, inside this guest only: the app sees
 *    its priority-setting call succeed, exactly as on device, while nothing on
 *    the host is re-prioritised (the effective difference is that the thread
 *    runs at nice 0 instead of -4).  Non-negative priorities take the same
 *    syscall path as bionic's wrapper.  No capability is added and the seccomp
 *    filter is unaffected; this is not a general errno-forgery switch (see
 *    docs/IMAGE_ABI_NOTEBOOK.md, W21).
 */
typedef unsigned long usize;
typedef __builtin_va_list va_list;
#define va_start(v, l) __builtin_va_start(v, l)
#define va_arg(v, t) __builtin_va_arg(v, t)
#define va_end(v) __builtin_va_end(v)

/* ---- raw syscalls (x86_64 / aarch64 generic numbers via asm registers) --- */
#define SIGABRT 6

#if defined(__x86_64__)
#define SYS_read 0
#define SYS_write 1
#define SYS_open 2
#define SYS_close 3
#define SYS_getpid 39
#define SYS_socket 41
#define SYS_connect 42
#define SYS_gettid 186
#define SYS_tgkill 234
#define SYS_setpriority 141
#elif defined(__aarch64__)
#define SYS_read 63
#define SYS_write 64
#define SYS_getpid 172
#define SYS_close 57
#define SYS_tgkill 131
#define SYS_gettid 178
#define SYS_setpriority 140
#define SYS_socket 198
#define SYS_connect 203
#define SYS_open 1024
#else
#error "artshim: unsupported architecture"
#endif

static long sys0(long n) {
  long r;
  __asm__ volatile("syscall" : "=a"(r) : "a"(n) : "rcx", "r11", "memory");
  return r;
}
static long sys1(long n, long a) {
  long r;
  __asm__ volatile("syscall" : "=a"(r) : "a"(n), "D"(a) : "rcx", "r11", "memory");
  return r;
}
static long sys3(long n, long a, long b, long c) {
  long r;
  __asm__ volatile("syscall" : "=a"(r) : "a"(n), "D"(a), "S"(b), "d"(c)
                   : "rcx", "r11", "memory");
  return r;
}
static long sys6(long n, long a, long b, long c, long d, long e, long f) {
  register long r10 __asm__("r10") = d;
  register long r8 __asm__("r8") = e;
  register long r9 __asm__("r9") = f;
  long r;
  __asm__ volatile("syscall" : "=a"(r)
                   : "a"(n), "D"(a), "S"(b), "d"(c), "r"(r10), "r"(r8), "r"(r9)
                   : "rcx", "r11", "memory");
  return r;
}

/* ---- small string builder ------------------------------------------------ */
struct sbuf { char b[2048]; usize n; };

static void sb_putc(struct sbuf *s, char c) { if (s->n + 1 < sizeof(s->b)) s->b[s->n++] = c; }
static void sb_puts(struct sbuf *s, const char *p) { if (!p) { sb_puts(s, "(null)"); return; } while (*p) sb_putc(s, *p++); }
static void sb_putn(struct sbuf *s, const char *p, usize n) { while (n--) sb_putc(s, *p++); }
static void sb_dec(struct sbuf *s, long v) {
  char t[24]; int i = 0;
  unsigned long u = v < 0 ? (unsigned long)(-v) : (unsigned long)v;
  if (!u) { sb_putc(s, '0'); return; }
  while (u) { t[i++] = (char)('0' + (u % 10)); u /= 10; }
  if (v < 0) sb_putc(s, '-');
  while (i) sb_putc(s, t[--i]);
}
static void sb_hex(struct sbuf *s, unsigned long v) {
  char t[20]; int i = 0;
  if (!v) { sb_putc(s, '0'); return; }
  while (v) { unsigned d = (unsigned)(v & 0xf); t[i++] = (char)(d < 10 ? '0' + d : 'a' + d - 10); v >>= 4; }
  while (i) sb_putc(s, t[--i]);
}
static void sb_fmt(struct sbuf *s, const char *fmt, va_list ap) {
  for (const char *p = fmt; *p; p++) {
    if (*p != '%') { sb_putc(s, *p); continue; }
    p++;
    int lng = 0;
    if (*p == 'l') { lng = 1; p++; if (*p == 'l') p++; }
    else if (*p == 'z') { lng = 1; p++; }
    else if (*p == 'j') { lng = 1; p++; }
    switch (*p) {
      case 's': sb_puts(s, va_arg(ap, const char *)); break;
      case 'd': case 'i': sb_dec(s, lng ? (long)va_arg(ap, long) : (long)va_arg(ap, int)); break;
      case 'u': sb_dec(s, (long)(lng ? va_arg(ap, unsigned long) : va_arg(ap, unsigned int))); break;
      case 'x': case 'X': case 'p':
        sb_hex(s, (unsigned long)(lng ? va_arg(ap, unsigned long) : va_arg(ap, unsigned int)));
        break;
      case 'c': sb_putc(s, (char)va_arg(ap, int)); break;
      case '%': sb_putc(s, '%'); break;
      case 0: p--; break;
      default: sb_putc(s, '%'); sb_putc(s, *p); break;
    }
  }
}

/* ---- logd transport ------------------------------------------------------ */
#define AF_UNIX 1
#define SOCK_DGRAM 2

struct sockaddr_un {
  unsigned short sun_family;
  char sun_path[108];
};

/* ZPROD7: the sink path. `/dev/socket` is a *per-session bind mount* of a
 * zygote child (the keeper binds its own socket directory before it forks), so
 * the path is the only per-session fact the logger needs. The default is the
 * production one; a host test overrides it at build time (`-D`) to point the
 * shipped shim at throwaway sinks, which is the only way to exercise this code
 * outside a guest (no code path sets it at run time). */
#ifndef ACOSART_LOGD_SOCK
#define ACOSART_LOGD_SOCK "/dev/socket/logdw"
#endif

static int log_fd = -2; /* -2 = not tried, -1 = unavailable */
static long log_pid = -1; /* the process `log_fd` was resolved in (ZPROD7) */

/* ZDEFAULT1: forget the connection (and the process it belongs to) so the next
 * entry connects through *this* process's own `/dev/socket`. Exported for the
 * zygote child, which calls it right after it entered its own mount namespace
 * (`dev.acos.art.Nat.resetLogSink`, `ZygoteApp`). */
void acosart_log_reset(void) {
  if (log_fd >= 0) sys1(SYS_close, log_fd);
  log_fd = -2;
  log_pid = -1;
}

static int log_socket(void) {
  /* ZPROD7/ZDEFAULT1: a fork child inherits the connected *descriptor*, not the
   * path. The keeper connects to its own sink before it forks, so a forked
   * app's entries went to the keeper's buffer instead of its session's
   * (measured: the flag-on `form-validation`, `w54-dialog-pixels` and
   * `w42-back-frame` gates failed at `assert_log` with an empty session log).
   *
   * The pid comparison below catches an ordinary fork, but it is NOT enough for
   * a zygote child: that child lives in its own pid namespace, where it is pid 2
   * exactly like the keeper in its own (ZPROD7's check therefore never fired;
   * proven with a `getpid` trace: one resolve, pid=2, for keeper and child).
   * The authoritative signal is `acosart_log_reset()`, called by the child once
   * its mount namespace is its own. A failed connect is not sticky: the next
   * entry retries, so an entry written before the session launcher bound its
   * sink is lost, not the whole stream. */
  long pid = sys0(SYS_getpid);
  if (pid != log_pid) {
    if (log_fd >= 0) sys1(SYS_close, log_fd);
    log_fd = -2;
    log_pid = pid;
  }
  if (log_fd >= 0) return log_fd;
  long fd = sys3(SYS_socket, AF_UNIX, SOCK_DGRAM, 0);
  if (fd < 0) { log_fd = -1; return -1; }
  struct sockaddr_un sa;
  const char *path = ACOSART_LOGD_SOCK;
  usize i = 0;
  sa.sun_family = AF_UNIX;
  while (path[i] && i + 1 < sizeof(sa.sun_path)) { sa.sun_path[i] = path[i]; i++; }
  sa.sun_path[i] = 0;
  if (sys3(SYS_connect, fd, (long)&sa, (long)sizeof(sa)) < 0) {
    sys1(SYS_close, fd);
    log_fd = -1;
    return -1;
  }
  log_fd = (int)fd;
  return log_fd;
}

/* frame: [prio u8][tag_len u16le][msg_len u16le][tag][msg] */
static void log_frame(int prio, const char *tag, const char *msg, usize mlen) {
  unsigned char frame[2 + 2 + 2 + 512 + 2048];
  usize tlen = 0;
  while (tag && tag[tlen] && tlen < 512) tlen++;
  if (mlen > sizeof(frame) - 6 - tlen) mlen = sizeof(frame) - 6 - tlen;
  frame[0] = (unsigned char)prio;
  frame[1] = (unsigned char)(tlen & 0xff);
  frame[2] = (unsigned char)(tlen >> 8);
  frame[3] = (unsigned char)(mlen & 0xff);
  frame[4] = (unsigned char)(mlen >> 8);
  for (usize i = 0; i < tlen; i++) frame[6 + i] = tag[i];
  for (usize i = 0; i < mlen; i++) frame[6 + tlen + i] = msg[i];
  usize total = 6 + tlen + mlen;
  int fd = log_socket();
  if (fd >= 0) {
    if (sys3(SYS_write, fd, (long)frame, (long)total) > 0) return;
  }
  /* Fallback: plain line on stderr so an un-served launch is still visible. */
  static const char pre[] = "\n[art] ";
  sys3(SYS_write, 2, (long)pre, (long)sizeof(pre) - 1);
  if (tlen) { sys3(SYS_write, 2, (long)tag, (long)tlen); sys3(SYS_write, 2, (long)": ", 2); }
  sys3(SYS_write, 2, (long)msg, (long)mlen);
  sys3(SYS_write, 2, (long)"\n", 1);
}

static void log_text(int prio, const char *tag, const char *msg) {
  usize n = 0;
  while (msg && msg[n]) n++;
  log_frame(prio, tag, msg, n);
}

static void log_vfmt(int prio, const char *tag, const char *fmt, va_list ap) {
  struct sbuf s;
  s.n = 0;
  sb_fmt(&s, fmt, ap);
  log_frame(prio, tag, s.b, s.n);
}

int __android_log_write(int prio, const char *tag, const char *msg) {
  log_text(prio, tag, msg);
  return 1;
}
int __android_log_print(int prio, const char *tag, const char *fmt, ...) {
  va_list ap; va_start(ap, fmt);
  log_vfmt(prio, tag, fmt, ap);
  va_end(ap);
  return 1;
}
int __android_log_vprint(int prio, const char *tag, const char *fmt, va_list ap) {
  log_vfmt(prio, tag, fmt, ap);
  return 1;
}
int __android_log_buf_write(int buf, int prio, const char *tag, const char *msg) {
  (void)buf;
  log_text(prio, tag, msg);
  return 1;
}
/* Raise SIGABRT on the calling thread, like bionic's abort().  Never returns
 * when the signal is delivered; the spin keeps freestanding control flow
 * defined if a handler somehow resumes us. */
static void shim_abort(void) {
  long pid = sys0(SYS_getpid);
  long tid = sys0(SYS_gettid);
  sys3(SYS_tgkill, pid, tid, SIGABRT);
  for (;;) {
  }
}

int __android_log_assert(const char *cond, const char *tag, const char *fmt, ...) {
  struct sbuf s;
  s.n = 0;
  sb_puts(&s, "assertion failed: ");
  sb_puts(&s, cond ? cond : "(null)");
  if (fmt) { sb_puts(&s, " "); va_list ap; va_start(ap, fmt); sb_fmt(&s, fmt, ap); va_end(ap); }
  log_frame(1 /* ANDROID_LOG_FATAL */, tag, s.b, s.n);
  shim_abort();
  return 1;
}

struct logmsg {
  unsigned long struct_size;
  int buffer_id;
  int priority;
  const char *tag;
  const char *file;
  unsigned line;
  const char *message;
};
int __android_log_write_log_message(struct logmsg *m) {
  if (!m) return -1;
  log_text(m->priority, m->tag, m->message);
  return 1;
}
int __android_log_logd_logger(const struct logmsg *m) {
  if (!m) return -1;
  log_text(m->priority, m->tag, m->message);
  return 1;
}
/* ART's LOG(FATAL)/CHECK path funnels here.  Record the message into the logd
 * sink (liblog cannot write here), then do what liblog's default aborter does:
 * die of SIGABRT.  Suppressing this hid a real fatal CHECK in earlier runs. */
void __android_log_call_aborter(const char *msg) {
  log_text(1, "art_aborter", msg ? msg : "(null)");
  shim_abort();
}

/* ---- properties ---------------------------------------------------------- */
#define PROP_VALUE_MAX 92
#define MAX_PROPS 128
#define ARENA_BYTES (MAX_PROPS * 160)
#define PROPS_PATH "/data/acos.properties"

struct prop_info {
  unsigned int serial;
  char value[PROP_VALUE_MAX];
  char name[0];
};

static unsigned char arena[ARENA_BYTES];
static struct prop_info *props[MAX_PROPS];
static int nprops = -1; /* -1 = not loaded */

static int streq(const char *a, const char *b) {
  while (*a && *a == *b) { a++; b++; }
  return *a == *b;
}

static void props_load(void) {
  if (nprops >= 0) return;
  nprops = 0;
  static char file[1 << 15];
  long fd = sys3(SYS_open, (long)PROPS_PATH, 0, 0);
  if (fd < 0) return;
  long n = 0, r;
  while (n < (long)sizeof(file) - 1) {
    r = sys3(SYS_read, fd, (long)(file + n), (long)(sizeof(file) - 1 - n));
    if (r <= 0) break;
    n += r;
  }
  sys1(SYS_close, fd);
  file[n] = 0;

  usize cursor = 0;
  char *p = file;
  while (*p && nprops < MAX_PROPS) {
    /* skip blank lines and comments */
    while (*p == '\n' || *p == '\r') p++;
    if (*p == '#' || !*p) { while (*p && *p != '\n') p++; continue; }
    char *line = p;
    while (*p && *p != '\n' && *p != '\r') p++;
    char *end = p;
    int last_line = (*end == 0);
    *end = 0;
    char *eq = line;
    while (*eq && *eq != '=') eq++;
    if (!*eq) continue;
    *eq = 0;
    const char *name = line;
    const char *value = eq + 1;
    usize nlen = 0; while (name[nlen]) nlen++;
    usize vlen = 0; while (value[vlen]) vlen++;
    usize need = (sizeof(struct prop_info) + nlen + 1 + 3) & ~(usize)3;
    if (cursor + need > ARENA_BYTES) break;
    struct prop_info *pi = (struct prop_info *)(arena + cursor);
    cursor += need;
    pi->serial = 1u; /* non-long: bits 24.. must stay clear (long-property flag) */
    for (usize i = 0; i < PROP_VALUE_MAX; i++) pi->value[i] = 0;
    if (vlen >= PROP_VALUE_MAX) vlen = PROP_VALUE_MAX - 1;
    for (usize i = 0; i < vlen; i++) pi->value[i] = value[i];
    pi->value[vlen] = 0;
    for (usize i = 0; i <= nlen; i++) pi->name[i] = name[i];
    props[nprops++] = pi;
    /* Advance past the line terminator we just overwrote. */
    if (last_line) break;
    p = end + 1;
  }
}

static struct prop_info *find_prop(const char *name) {
  props_load();
  if (!name) return 0;
  for (int i = 0; i < nprops; i++)
    if (streq(props[i]->name, name)) return props[i];
  return 0;
}

int __system_property_get(const char *name, char *value) {
  struct prop_info *pi = find_prop(name);
  if (!pi) { if (value) value[0] = 0; return 0; }
  usize i = 0;
  while (i + 1 < PROP_VALUE_MAX && pi->value[i]) { value[i] = pi->value[i]; i++; }
  value[i] = 0;
  return (int)i;
}

const struct prop_info *__system_property_find(const char *name) {
  return find_prop(name);
}

unsigned int __system_property_serial(const struct prop_info *pi) {
  return pi ? pi->serial : 0;
}

void __system_property_read_callback(
    const struct prop_info *pi,
    void (*callback)(void *cookie, const char *name, const char *value,
                     unsigned int serial),
    void *cookie) {
  if (!pi || !callback) return;
  callback(cookie, pi->name, pi->value, pi->serial);
}

int __system_property_wait(const struct prop_info *pi, unsigned int old_serial,
                           unsigned int *new_serial, const void *timeout) {
  (void)timeout;
  if (pi) {
    if (new_serial) *new_serial = pi->serial;
    return pi->serial != old_serial;
  }
  return 0;
}

int __system_property_wait_any(unsigned int old_serial, unsigned int *new_serial,
                              const void *timeout) {
  (void)old_serial; (void)timeout;
  if (new_serial) *new_serial = 0;
  return 0;
}

int __system_property_foreach(
    int (*callback)(const struct prop_info *pi, void *cookie), void *cookie) {
  props_load();
  if (!callback) return -1;
  for (int i = 0; i < nprops; i++)
    if (callback(props[i], cookie)) return i;
  return 0;
}

/* ---- priority adapter (see the header) ---------------------------------- */

/* bionic's errno accessor; resolved from the guest's libc.so at load time.
 * Weak so the same shim can be loaded by a host libc (tests) where the symbol
 * does not exist: errno is then simply not recorded, which is the documented
 * host-side test behaviour, never a guest path. */
extern int *__errno(void) __attribute__((weak));

int setpriority(int which, int who, int prio) {
  static int clamped_once = 0;
  int requested = prio;
  if (prio < 0) {
    prio = 0;
    if (!clamped_once) {
      clamped_once = 1;
      __android_log_print(5, "AcosPriority",
                          "setpriority(%d) clamped to 0 (host RLIMIT_NICE is below "
                          "stock init.rc's 40; guest-only adapter, W21)",
                          requested);
    }
  }
  long r = sys3(SYS_setpriority, which, who, prio);
  if (r == 0) return 0;
  /* A clamped request is already a no-op: keep the app's call successful even
   * if the kernel refuses the nice-0 write for another reason. */
  if (requested < 0) return 0;
  if (__errno) *__errno() = (int) -r;
  return -1;
}
