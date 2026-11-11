/* r2shim.c -- the *only* framework shim R2 needs, and the smallest one that
 * works: make libandroid_runtime.so's exported JNI registration entry points
 * reachable from Java, then let the guest call them.
 *
 * Why it has to be one object (measured, E2D §2.2): bionic symbol lookup is
 * namespace-scoped. A library loaded into the app classloader namespace
 * ("clns-N") cannot see a library that was loaded RTLD_GLOBAL into the default
 * namespace -- dlsym(RTLD_DEFAULT) returns NULL -- so the LD_PRELOAD
 * constructor and the JNI bridge must live in the *same* .so.
 *
 * Built with the NDK for x86_64-linux-android; DT_NEEDED = libc/libdl/libm only,
 * so System.load("/data/lib/libr2shim.so") from the app namespace is legal.
 */
#include <dlfcn.h>
#include <jni.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* Same sink as the E1A logshim: plain stderr writes, so this works regardless
 * of whether liblog has been interposed. */
static long raw_write(int fd, const char* b, size_t n) {
    long r;
    __asm__ volatile("syscall" : "=a"(r) : "a"(1L), "D"((long)fd), "S"(b), "d"(n) : "rcx", "r11", "memory");
    return r;
}
static void say(const char* a, const char* b, void* p) {
    char buf[512];
    int n = snprintf(buf, sizeof(buf), "R2SHIM %s %s %p\n", a, b, p);
    if (n > 0) raw_write(2, buf, (size_t)n);
}

__attribute__((constructor)) static void r2_load_runtime(void) {
    /* Comma-separated; default is the one library the framework JNI lives in. */
    const char* list = getenv("R2_LOAD");
    if (list == NULL) list = "libandroid_runtime.so";
    char buf[4096];
    snprintf(buf, sizeof(buf), "%s", list);
    for (char* save = NULL, *tok = strtok_r(buf, ":", &save); tok; tok = strtok_r(NULL, ":", &save)) {
        void* h = dlopen(tok, RTLD_NOW | RTLD_GLOBAL);
        if (h == NULL) {
            say("dlopen ", tok, NULL);
            say("  FAILED: ", dlerror(), NULL);
        } else {
            say("dlopen ok ", tok, h);
        }
    }
}

/* android::register_android_util_Log(_JNIEnv*)
 *   -> _ZN7android25register_android_util_LogEP7_JNIEnv
 * A few entry points are at global scope, so try both manglings. */
static void* lookup(const char* plain) {
    char m[256];
    snprintf(m, sizeof(m), "_ZN7android%zu%sEP7_JNIEnv", strlen(plain), plain);
    void* p = dlsym(RTLD_DEFAULT, m);
    if (p) return p;
    snprintf(m, sizeof(m), "_Z%zu%sP7_JNIEnv", strlen(plain), plain);
    p = dlsym(RTLD_DEFAULT, m);
    if (p) return p;
    return dlsym(RTLD_DEFAULT, plain);
}

JNIEXPORT jint JNICALL Java_dev_acos_r2_Nat_register(JNIEnv* env, jclass klass, jstring name) {
    (void)klass;
    const char* plain = (*env)->GetStringUTFChars(env, name, NULL);
    void* p = lookup(plain);
    say("register ", plain, p);
    if (p == NULL) {
        (*env)->ReleaseStringUTFChars(env, name, plain);
        return 0;
    }
    ((void (*)(JNIEnv*))p)(env);
    jint rc = 1;
    if ((*env)->ExceptionCheck(env)) {
        jthrowable t = (*env)->ExceptionOccurred(env);
        (*env)->ExceptionClear(env);
        (*env)->Throw(env, t);
        rc = -1;
    }
    (*env)->ReleaseStringUTFChars(env, name, plain);
    return rc;
}
