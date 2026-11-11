/* w3-leash scratch preload (NOT part of the product): allow a host gdb to
 * attach under yama ptrace_scope=1, so the guest's parcel positions can be
 * observed.  Documented in results/art-runtime/W3.md; lives only in .cache. */
typedef unsigned long u64;
#define PR_SET_PTRACER 0x59616d61
#if defined(__x86_64__)
#define SYS_prctl 157
static long w3_sys5(long n, long a, long b, long c, long d, long e) {
    long r;
    register long r10 __asm__("r10") = d;
    register long r8 __asm__("r8") = e;
    __asm__ volatile("syscall"
                     : "=a"(r)
                     : "a"(n), "D"(a), "S"(b), "d"(c), "r"(r10), "r"(r8)
                     : "rcx", "r11", "memory");
    return r;
}
#elif defined(__aarch64__)
#define SYS_prctl 167
static long w3_sys5(long n, long a, long b, long c, long d, long e) {
    register long x8 __asm__("x8") = n;
    register long x0 __asm__("x0") = a;
    register long x1 __asm__("x1") = b;
    register long x2 __asm__("x2") = c;
    register long x3 __asm__("x3") = d;
    register long x4 __asm__("x4") = e;
    __asm__ volatile("svc 0" : "+r"(x0) : "r"(x8), "r"(x1), "r"(x2), "r"(x3), "r"(x4) : "memory");
    return x0;
}
#endif
__attribute__((constructor)) static void w3_ptracer_any(void) {
    (void)w3_sys5(SYS_prctl, PR_SET_PTRACER, -1, 0, 0, 0);
}
