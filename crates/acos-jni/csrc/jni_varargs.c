/*
 * C-variadic JNI trampolines.
 *
 * The JNI `JNINativeInterface` declares variadic entry points
 * (`CallIntMethod(env, obj, mid, ...)`, `NewObject(env, cls, mid, ...)`). Stable
 * Rust cannot define C-variadic functions, so these shims read the promoted
 * arguments according to the method descriptor (fetched from Rust) and forward
 * them as an array of 64-bit words. Rust performs the actual Java call.
 *
 * Each shim has the exact C return type of the slot it implements: the guest
 * calls it through a function pointer typed by the JNI header, so the ABI must
 * match (integer/pointer in rax, float/double in xmm0).
 */
#include <stdarg.h>
#include <stdint.h>
#include <string.h>

typedef struct {
    uint64_t bits;
    uint32_t kind; /* 0 = integer/ref, 1 = float, 2 = double */
} JniCallResult;

extern const char *acos_jni_method_codes(uint64_t mid);
extern JniCallResult acos_jni_va_invoke(uint64_t env, uint64_t target,
                                        uint64_t mid, int mode,
                                        uint64_t *words, int n);

#define MAX_WORDS 32

/* Read one promoted C argument into a 64-bit word. */
static uint64_t read_one(va_list ap, char code) {
    switch (code) {
    case 'L':
        return (uint64_t)(uintptr_t)va_arg(ap, void *);
    case 'J':
        return (uint64_t)va_arg(ap, int64_t);
    case 'F': {
        double d = va_arg(ap, double);
        float f = (float)d;
        uint32_t u;
        memcpy(&u, &f, sizeof(u));
        return (uint64_t)u;
    }
    case 'D': {
        double d = va_arg(ap, double);
        uint64_t u;
        memcpy(&u, &d, sizeof(u));
        return u;
    }
    default:
        /* jboolean/jbyte/jchar/jshort/jint are promoted to int. */
        return (uint64_t)(int64_t)va_arg(ap, int);
    }
}

static JniCallResult invoke_words(uint64_t env, uint64_t target, uint64_t mid,
                                  int mode, const uint64_t *a) {
    uint64_t words[MAX_WORDS];
    int n = 0;
    const char *codes = acos_jni_method_codes(mid);
    if (codes != 0) {
        for (const char *p = codes; *p != 0 && n < MAX_WORDS; ++p) {
            words[n] = a ? a[n] : 0;
            ++n;
        }
    }
    return acos_jni_va_invoke(env, target, mid, mode, words, n);
}

/* ---- return conversions ------------------------------------------------ */

#define AS_OBJECT(r) ((void *)(uintptr_t)(r).bits)
#define AS_BOOLEAN(r) ((uint8_t)(r).bits)
#define AS_BYTE(r) ((int8_t)(r).bits)
#define AS_CHAR(r) ((uint16_t)(r).bits)
#define AS_SHORT(r) ((int16_t)(r).bits)
#define AS_INT(r) ((int32_t)(r).bits)
#define AS_LONG(r) ((int64_t)(r).bits)

static float as_float(JniCallResult r) {
    float f;
    uint32_t u = (uint32_t)r.bits;
    memcpy(&f, &u, sizeof(f));
    return f;
}

static double as_double(JniCallResult r) {
    double d;
    uint64_t u = r.bits;
    memcpy(&d, &u, sizeof(d));
    return d;
}

#define AS_VOID(r) ((void)(r))

/* ---- shims ------------------------------------------------------------- */

#define DEFINE_VARIADIC(name, TYPE, mode, cast)                                 \
    TYPE name(uint64_t env, uint64_t target, uint64_t mid, ...) {               \
        va_list ap;                                                            \
        uint64_t words[MAX_WORDS];                                             \
        int n = 0;                                                             \
        const char *codes = acos_jni_method_codes(mid);                        \
        va_start(ap, mid);                                                     \
        if (codes != 0) {                                                      \
            for (const char *p = codes; *p != 0 && n < MAX_WORDS; ++p)          \
                words[n++] = read_one(ap, *p);                                \
        }                                                                      \
        va_end(ap);                                                            \
        JniCallResult r = acos_jni_va_invoke(env, target, mid, mode, words, n); \
        return cast(r);                                                        \
    }

#define DEFINE_VALIST(name, TYPE, mode, cast)                                   \
    TYPE name(uint64_t env, uint64_t target, uint64_t mid, va_list ap) {        \
        uint64_t words[MAX_WORDS];                                             \
        int n = 0;                                                             \
        const char *codes = acos_jni_method_codes(mid);                        \
        if (codes != 0) {                                                      \
            for (const char *p = codes; *p != 0 && n < MAX_WORDS; ++p)          \
                words[n++] = read_one(ap, *p);                                \
        }                                                                      \
        JniCallResult r = acos_jni_va_invoke(env, target, mid, mode, words, n); \
        return cast(r);                                                        \
    }

#define DEFINE_ARRAY(name, TYPE, mode, cast)                                    \
    TYPE name(uint64_t env, uint64_t target, uint64_t mid,                      \
              const uint64_t *a) {                                              \
        JniCallResult r = invoke_words(env, target, mid, mode, a);              \
        return cast(r);                                                        \
    }

#define DEFINE_CALL(RET, NAME, mode, cast)                                      \
    DEFINE_VARIADIC(acosrjni_Call##NAME##Method, RET, mode, cast)               \
    DEFINE_VALIST(acosrjni_Call##NAME##MethodV, RET, mode, cast)                \
    DEFINE_ARRAY(acosrjni_Call##NAME##MethodA, RET, mode, cast)

#define DEFINE_STATIC(RET, NAME, mode, cast)                                    \
    DEFINE_VARIADIC(acosrjni_CallStatic##NAME##Method, RET, mode, cast)         \
    DEFINE_VALIST(acosrjni_CallStatic##NAME##MethodV, RET, mode, cast)          \
    DEFINE_ARRAY(acosrjni_CallStatic##NAME##MethodA, RET, mode, cast)

#define DEFINE_BOTH(RET, NAME, cast)                                            \
    DEFINE_CALL(RET, NAME, 0, cast)                                             \
    DEFINE_STATIC(RET, NAME, 1, cast)

DEFINE_BOTH(void *, Object, AS_OBJECT)
DEFINE_BOTH(uint8_t, Boolean, AS_BOOLEAN)
DEFINE_BOTH(int8_t, Byte, AS_BYTE)
DEFINE_BOTH(uint16_t, Char, AS_CHAR)
DEFINE_BOTH(int16_t, Short, AS_SHORT)
DEFINE_BOTH(int32_t, Int, AS_INT)
DEFINE_BOTH(int64_t, Long, AS_LONG)
DEFINE_BOTH(float, Float, as_float)
DEFINE_BOTH(double, Double, as_double)
DEFINE_BOTH(void, Void, AS_VOID)

/* Constructors. */
DEFINE_VARIADIC(acosrjni_NewObject, void *, 2, AS_OBJECT)
DEFINE_VALIST(acosrjni_NewObjectV, void *, 2, AS_OBJECT)
DEFINE_ARRAY(acosrjni_NewObjectA, void *, 2, AS_OBJECT)
