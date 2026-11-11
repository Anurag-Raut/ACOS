/*
 * crashtrace.c -- LD_PRELOAD SIGSEGV reporter for the Q3 ripple investigation.
 *
 * A silent SIGSEGV with no tombstone is useless. This shim installs a
 * SIGSEGV/SIGBUS/SIGABRT handler that, from inside the guest process (bionic),
 * prints to fd 2:
 *   - siginfo (signal, si_code, si_addr) and the ucontext registers,
 *   - the /proc/self/maps lines of the interesting modules with their base,
 *   - a frame-pointer chain (rbp), and
 *   - a stack scan for return addresses that fall inside libhwui /
 *     libandroid_runtime (reliable even when the callee omitted the frame
 *     pointer), each annotated with module+offset and the nearest dynamic
 *     symbol via dladdr().
 * Then it restores SIG_DFL and re-raises, so the kernel core dump still happens.
 *
 * Address -> function name is done offline with the module + offset (the image
 * libs are stripped but carry .gnu_debugdata); dladdr only sees .dynsym and is
 * a hint.
 *
 * Build (NDK, bionic, x86_64 API 34) -- see scripts/build.sh.
 */
#include <fcntl.h>
#include <signal.h>
#include <stdint.h>
#include <string.h>
#include <ucontext.h>
#include <sys/prctl.h>
#include <unistd.h>

static char altstack[64 * 1024];

static void w(const char *s) { (void)!write(2, s, strlen(s)); }

static void whex(uint64_t v) {
    char b[19];
    int i = 18;
    b[i] = 0;
    if (!v) b[--i] = '0';
    while (v && i > 0) {
        int d = (int)(v & 15);
        b[--i] = (char)(d < 10 ? '0' + d : 'a' + d - 10);
        v >>= 4;
    }
    w("0x"); w(&b[i]);
}

static void wdec(long v) {
    char b[24];
    int i = 23;
    b[i] = 0;
    if (v == 0) b[--i] = '0';
    int neg = v < 0;
    unsigned long u = neg ? (unsigned long)(-v) : (unsigned long)v;
    while (u && i > 0) { b[--i] = (char)('0' + (u % 10)); u /= 10; }
    if (neg) b[--i] = '-';
    w(&b[i]);
}

/* --- module table from /proc/self/maps ------------------------------- */
typedef struct { uint64_t lo, hi, base; char name[64]; int interesting; } mod_t;
static mod_t mods[400];
static int nmods;

static int is_interesting(const char *name) {
    return strstr(name, "libhwui") || strstr(name, "libandroid_runtime") ||
           strstr(name, "libart.so") || strstr(name, "dalvikvm") ||
           strstr(name, "libc.so") || strstr(name, "liblog") ||
           strstr(name, "framework_shim") || strstr(name, "crashtrace") ||
           strstr(name, "libminikin") || strstr(name, "libskia") ||
           strstr(name, "libgui") || strstr(name, "libbinder") ||
           strstr(name, "libbase") || strstr(name, "libc++") ||
           strstr(name, "libutils") || strstr(name, "libui") ||
           strstr(name, "libnativewindow") || strstr(name, "libEGL");
}

static void load_maps(void) {
    int fd = open("/proc/self/maps", O_RDONLY);
    if (fd < 0) return;
    static char buf[1 << 20];
    ssize_t n = 0, r;
    while (n < (ssize_t)sizeof(buf) - 1 && (r = read(fd, buf + n, sizeof(buf) - 1 - n)) > 0) n += r;
    close(fd);
    if (n <= 0) return;
    buf[n] = 0;
    char *p = buf;
    while (*p && nmods < 400) {
        char *eol = strchr(p, '\n');
        if (!eol) break;
        *eol = 0;
        uint64_t lo = 0, hi = 0;
        char *q = p;
        while (*q && *q != '-') { char c = *q++; if (c >= '0' && c <= '9') lo = lo * 16 + (uint64_t)(c - '0'); else if (c >= 'a' && c <= 'f') lo = lo * 16 + (uint64_t)(c - 'a' + 10); }
        if (*q == '-') q++;
        while (*q && *q != ' ') { char c = *q++; if (c >= '0' && c <= '9') hi = hi * 16 + (uint64_t)(c - '0'); else if (c >= 'a' && c <= 'f') hi = hi * 16 + (uint64_t)(c - 'a' + 10); }
        char *perms = (*q == ' ') ? q + 1 : q;
        char *path = strchr(p, '/');
        if (path && perms[0] == 'r' && perms[2] == 'x') {
            int interesting = is_interesting(path);
            int found = -1;
            for (int i = 0; i < nmods; i++)
                if (!strcmp(mods[i].name, path)) { found = i; break; }
            if (found < 0) {
                found = nmods++;
                strncpy(mods[found].name, path, sizeof(mods[found].name) - 1);
                mods[found].lo = lo; mods[found].hi = hi;
                mods[found].base = lo; /* first exec mapping is file offset 0 for these libs */
                mods[found].interesting = interesting;
            } else {
                if (lo < mods[found].lo) { mods[found].lo = lo; mods[found].base = lo; }
                if (hi > mods[found].hi) mods[found].hi = hi;
            }
        }
        p = eol + 1;
    }
}

static mod_t *find_mod(uint64_t pc) {
    for (int i = 0; i < nmods; i++)
        if (pc >= mods[i].lo && pc < mods[i].hi) return &mods[i];
    return 0;
}

#include <dlfcn.h>
static void describe(uint64_t pc, const char *tag) {
    mod_t *m = find_mod(pc);
    w(tag); w(" "); whex(pc);
    if (!m) { w("  <not in an interesting exec map>\n"); return; }
    w("  ");
    const char *base = strrchr(m->name, '/');
    w(base ? base + 1 : m->name);
    w("+"); whex(pc - m->base);
    w("  ");
    Dl_info di;
    if (dladdr((void *)(uintptr_t)pc, &di) && di.dli_sname) { w(di.dli_sname); }
    w("\n");
}

/* Every executable mapping, verbatim: the fault PC may live in a mapping that
 * has no path (ART JIT code cache, [vdso]) and therefore no module entry. */
static void dump_exec_lines(void) {
    int fd = open("/proc/self/maps", O_RDONLY);
    if (fd < 0) return;
    static char b[1 << 20];
    ssize_t n = 0, r;
    while (n < (ssize_t)sizeof(b) - 1 && (r = read(fd, b + n, sizeof(b) - 1 - n)) > 0) n += r;
    close(fd);
    if (n <= 0) return;
    b[n] = 0;
    w("-- executable mappings (verbatim) --\n");
    for (char *p = b, *eol; *p; p = eol + 1) {
        eol = strchr(p, '\n');
        if (!eol) break;
        *eol = 0;
        /* perms are the 2nd field, chars [2..4] of it: r-x / rwx */
        char *sp = strchr(p, ' ');
        if (sp && sp[3] == 'x') { w("   "); w(p); w("\n"); }
    }
}

static void dump_maps(void) {
    w("-- interesting exec maps (name base..end) --\n");
    for (int i = 0; i < nmods; i++) {
        if (!mods[i].interesting) continue;
        w("   "); w(mods[i].name);
        w("  ["); whex(mods[i].lo); w(","); whex(mods[i].hi); w(")\n");
    }
}

static void report(int sig, siginfo_t *si, void *ucv) {
    ucontext_t *uc = (ucontext_t *)ucv;
    load_maps();
    w("\n=== Q3-CRASHTRACE sig="); wdec(sig);
    w(" code="); wdec(si->si_code);
    w(" addr="); whex((uint64_t)(uintptr_t)si->si_addr);
    w(" ===\n");
#if defined(__x86_64__)
    greg_t *g = uc->uc_mcontext.gregs;
    w("regs rip="); whex((uint64_t)g[REG_RIP]);
    w(" rsp="); whex((uint64_t)g[REG_RSP]);
    w(" rbp="); whex((uint64_t)g[REG_RBP]);
    w(" rax="); whex((uint64_t)g[REG_RAX]);
    w(" rdi="); whex((uint64_t)g[REG_RDI]);
    w(" rsi="); whex((uint64_t)g[REG_RSI]);
    w(" rdx="); whex((uint64_t)g[REG_RDX]);
    w("\n");
    describe((uint64_t)g[REG_RIP], "FAULT ");
#endif
    dump_maps();
    dump_exec_lines();
    w("-- frame-pointer chain (rbp) --\n");
#if defined(__x86_64__)
    {
        uint64_t rbp = (uint64_t)g[REG_RBP];
        for (int i = 0; i < 32 && rbp > 0x1000 && (rbp & 7) == 0; i++) {
            uint64_t *fp = (uint64_t *)(uintptr_t)rbp;
            uint64_t ret = fp[1];
            describe(ret, i == 0 ? "fp[" : "fp[");
            /* wdec(i) inside tag would be nicer; keep it simple */
            uint64_t next = fp[0];
            if (next <= rbp) break;
            rbp = next;
        }
    }
    w("-- stack scan for return addresses into the interesting modules --\n");
    {
        uint64_t rsp = (uint64_t)g[REG_RSP];
        uint64_t top = rsp + 4096 * 8;
        for (uint64_t a = rsp; a < top; a += 8) {
            uint64_t v = *(uint64_t *)(uintptr_t)a;
            mod_t *m = find_mod(v);
            if (m && m->interesting && !strstr(m->name, "crashtrace")) {
                w("  stack["); whex(a); w("] = "); whex(v); w("  ");
                const char *base = strrchr(m->name, '/');
                w(base ? base + 1 : m->name);
                w("+"); whex(v - m->base);
                w("\n");
            }
        }
    }
#endif
    w("=== end Q3-CRASHTRACE ===\n");
    signal(sig, SIG_DFL);
    raise(sig);
}

__attribute__((constructor)) static void install(void) {
    /* allow a host gdb to attach even though ptrace_scope=1 */
    prctl(PR_SET_PTRACER, -1, 0, 0, 0);
    stack_t ss;
    ss.ss_sp = altstack;
    ss.ss_size = sizeof(altstack);
    ss.ss_flags = 0;
    sigaltstack(&ss, 0);
    struct sigaction sa;
    memset(&sa, 0, sizeof(sa));
    sa.sa_sigaction = report;
    sa.sa_flags = SA_SIGINFO | SA_ONSTACK;
    sigaction(SIGSEGV, &sa, 0);
    sigaction(SIGBUS, &sa, 0);
    sigaction(SIGABRT, &sa, 0);
}
