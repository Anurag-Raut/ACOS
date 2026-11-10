/*
 * acos-native-loader host shims that MUST be written in C.
 *
 * Rationale: stable Rust cannot define C-variadic functions. Two families of
 * guest imports are variadic:
 *
 *   1. `__android_log_print` / `__android_log_vprint` / `__android_log_assert`
 *      (the `...` forms), which forward the formatted text to the Rust log
 *      sink `acos_log_write`.
 *   2. JNI `Call*Method(...)` varargs entry points, which the guest passes a
 *      C `va_list`. We read the arguments according to the method descriptor
 *      (obtained from the Rust test harness via `acos_tj_arg_types`) into a
 *      `uint64_t[]` and hand them to `acos_tj_call`.
 *
 * The host C compiler is used only to build this support object; it contains
 * no Android runtime code and never executes guest code.
 */

#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* ------------------------------------------------------------------ logging */

extern void acos_log_write(int prio, const char *tag, const char *msg);
extern void acos_log_assert(const char *cond, const char *tag, const char *msg);

static void acos_vlog(int prio, const char *tag, const char *fmt, va_list ap) {
    char buf[2048];
    vsnprintf(buf, sizeof(buf), fmt ? fmt : "", ap);
    acos_log_write(prio, tag ? tag : "", buf);
}

int __android_log_print(int prio, const char *tag, const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    acos_vlog(prio, tag, fmt, ap);
    va_end(ap);
    return 1;
}

int __android_log_vprint(int prio, const char *tag, const char *fmt, va_list ap) {
    acos_vlog(prio, tag, fmt, ap);
    return 1;
}

int __android_log_write(int prio, const char *tag, const char *text) {
    acos_log_write(prio, tag ? tag : "", text ? text : "");
    return 1;
}

void __android_log_assert(const char *cond, const char *tag, const char *fmt, ...) {
    char buf[2048];
    va_list ap;
    va_start(ap, fmt);
    vsnprintf(buf, sizeof(buf), fmt ? fmt : "", ap);
    va_end(ap);
    acos_log_assert(cond ? cond : "", tag ? tag : "", buf);
    /* bionic aborts after logging the assertion. */
    abort();
}

/* ---------------------------------------------------------------------- JNI */

extern const char *acos_tj_arg_types(uintptr_t mid);
extern uint64_t acos_tj_call(uintptr_t env, uintptr_t target, uintptr_t mid,
                             int is_static, const uint64_t *args, int nargs);

/* Read the arguments described by the method descriptor into `out`. */
static int acos_tj_collect(uintptr_t mid, va_list ap, uint64_t *out, int cap) {
    const char *s = acos_tj_arg_types(mid);
    int n = 0;
    if (s == NULL) {
        return 0;
    }
    for (; *s; ++s) {
        if (n >= cap) {
            break;
        }
        switch (*s) {
        case 'L':
        case '[':
            out[n++] = (uint64_t)(uintptr_t)va_arg(ap, void *);
            break;
        case 'J':
            out[n++] = (uint64_t)va_arg(ap, long long);
            break;
        case 'D':
            out[n++] = (uint64_t)va_arg(ap, double);
            break;
        case 'F':
            out[n++] = (uint64_t)va_arg(ap, double);
            break;
        case 'I':
        case 'Z':
        case 'B':
        case 'S':
        case 'C':
            /* Integer types narrower than int are promoted to int. */
            out[n++] = (uint64_t)(int64_t)va_arg(ap, int);
            break;
        default:
            /* Unknown descriptor char: skip. */
            break;
        }
    }
    return n;
}

#define ACOS_TJ_VARIADIC(NAME, IS_STATIC)                                      \
    uint64_t NAME(uintptr_t env, uintptr_t target, uintptr_t mid, ...) {       \
        uint64_t args[16];                                                     \
        va_list ap;                                                            \
        int n;                                                                 \
        va_start(ap, mid);                                                     \
        n = acos_tj_collect(mid, ap, args, 16);                                \
        va_end(ap);                                                            \
        return acos_tj_call(env, target, mid, IS_STATIC, args, n);             \
    }

#define ACOS_TJ_VA(NAME, IS_STATIC)                                            \
    uint64_t NAME(uintptr_t env, uintptr_t target, uintptr_t mid, va_list ap) {\
        uint64_t args[16];                                                     \
        int n = acos_tj_collect(mid, ap, args, 16);                            \
        return acos_tj_call(env, target, mid, IS_STATIC, args, n);             \
    }

/* Static calls (jclass target). */
ACOS_TJ_VARIADIC(acos_jni_CallStaticIntMethod, 1)
ACOS_TJ_VARIADIC(acos_jni_CallStaticVoidMethod, 1)
ACOS_TJ_VARIADIC(acos_jni_CallStaticObjectMethod, 1)
ACOS_TJ_VARIADIC(acos_jni_CallStaticBooleanMethod, 1)
ACOS_TJ_VA(acos_jni_CallStaticIntMethodV, 1)
ACOS_TJ_VA(acos_jni_CallStaticVoidMethodV, 1)
ACOS_TJ_VA(acos_jni_CallStaticObjectMethodV, 1)
ACOS_TJ_VA(acos_jni_CallStaticBooleanMethodV, 1)

/* Instance calls (jobject target). */
ACOS_TJ_VARIADIC(acos_jni_CallIntMethod, 0)
ACOS_TJ_VARIADIC(acos_jni_CallVoidMethod, 0)
ACOS_TJ_VARIADIC(acos_jni_CallObjectMethod, 0)
ACOS_TJ_VARIADIC(acos_jni_CallBooleanMethod, 0)
ACOS_TJ_VA(acos_jni_CallIntMethodV, 0)
ACOS_TJ_VA(acos_jni_CallVoidMethodV, 0)
ACOS_TJ_VA(acos_jni_CallObjectMethodV, 0)
ACOS_TJ_VA(acos_jni_CallBooleanMethodV, 0)

/* ------------------------------------------------------------ syslog stubs */

extern void acos_log_write(int prio, const char *tag, const char *msg);

void acos_openlog(const char *ident, int option, int facility) {
    (void)ident; (void)option; (void)facility;
}

void acos_closelog(void) {
}

void acos_syslog(int prio, const char *fmt, ...) {
    char buf[2048];
    va_list ap;
    va_start(ap, fmt);
    vsnprintf(buf, sizeof(buf), fmt ? fmt : "", ap);
    va_end(ap);
    acos_log_write(prio, "syslog", buf);
}
