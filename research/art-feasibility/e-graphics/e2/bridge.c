/* E2 probe: load the *real* libhwui.so inside real ART and wire its
 * RegisterNatives tables onto the *real* framework classes.
 *
 * Why this is needed: libhwui does not export Java_* symbols; its natives are
 * registered by name from the register functions inside libandroid_runtime /
 * libhwui (hidden visibility).  We reconstruct the tables statically from the
 * .so (name, signature, offset) and RegisterNatives them ourselves, with the
 * function pointer rebased onto the dlopen()ed image.  If this works, the real
 * Skia/CPU canvas in libhwui is live under ART with no Android boot.
 */
#include <jni.h>
#include <dlfcn.h>
#include <link.h>
#include <stdio.h>
#include <string.h>
#include <stdint.h>
#include "natives.h"

static uintptr_t g_base;
static void *g_hwui;
static char g_path[512];

static int find_hwui(struct dl_phdr_info *info, size_t sz, void *data) {
    (void)sz; (void)data;
    if (info->dlpi_name && strstr(info->dlpi_name, "libhwui.so")) {
        g_base = (uintptr_t)info->dlpi_addr;
        snprintf(g_path, sizeof g_path, "%s", info->dlpi_name);
    }
    return 0;
}

/* candidate classes: graphics-only list (a RegisterNatives for a method the
 * class does not declare fails and is skipped; ART dumps the whole class on
 * every failure, so the list is kept curated rather than all 621 dex types). */
#include "classes.h"

/* generate the candidate class list at build time (classes.h) */
static int load_hwui(JNIEnv *env) {
    static int done = 0;
    if (done) return done;
    g_hwui = dlopen("/system/lib64/libhwui.so", RTLD_NOW | RTLD_GLOBAL);
    if (!g_hwui) {
        fprintf(stderr, "dlopen failed: %s\n", dlerror());
        return 0;
    }
    dl_iterate_phdr(find_hwui, NULL);
    fprintf(stderr, "libhwui base=%p path=%s natives=%d\n", (void *)g_base, g_path, N_NATIVES);
    done = 1;
    return 1;
}

JNIEXPORT jint JNICALL Java_dev_acos_e2_Bridge_nativeCount(JNIEnv *env, jclass c) {
    (void)env; (void)c;
    load_hwui(env);
    return N_NATIVES;
}

JNIEXPORT jobjectArray JNICALL Java_dev_acos_e2_Bridge_classNames(JNIEnv *env, jclass c) {
    (void)c;
    jclass str = (*env)->FindClass(env, "java/lang/String");
    jobjectArray arr = (*env)->NewObjectArray(env, N_CLASSES, str, NULL);
    for (int i = 0; i < N_CLASSES; i++) {
        (*env)->SetObjectArrayElement(env, arr, i, (*env)->NewStringUTF(env, gClasses[i]));
    }
    return arr;
}

/* register(cls, name, sig) -> 0 on success, 1 if libhwui has no such native */
JNIEXPORT jint JNICALL Java_dev_acos_e2_Bridge_register(JNIEnv *env, jclass c,
                                                        jstring jc, jstring jn, jstring js) {
    (void)c;
    fprintf(stderr, "ENTER register\n");
    if (!load_hwui(env)) return 2;
    const char *cn = (*env)->GetStringUTFChars(env, jc, NULL);
    const char *nm = (*env)->GetStringUTFChars(env, jn, NULL);
    const char *sg = (*env)->GetStringUTFChars(env, js, NULL);
    int rc = 1;
    for (unsigned i = 0; i < N_NATIVES; i++) {
        if (strcmp(gNatives[i].name, nm) || strcmp(gNatives[i].sig, sg)) continue;
        fprintf(stderr, "REG %s %s%s\n", cn, nm, sg);
        jclass k = (*env)->FindClass(env, cn);
        if (!k) { (*env)->ExceptionClear(env); fprintf(stderr, "  findclass failed\n"); break; }
        JNINativeMethod m;
        m.name = (char *)gNatives[i].name;
        m.signature = (char *)gNatives[i].sig;
        m.fnPtr = (void *)(g_base + gNatives[i].off);
        if ((*env)->RegisterNatives(env, k, &m, 1) == 0) rc = 0;
        else (*env)->ExceptionClear(env);
        break;
    }
    (*env)->ReleaseStringUTFChars(env, jc, cn);
    (*env)->ReleaseStringUTFChars(env, jn, nm);
    (*env)->ReleaseStringUTFChars(env, js, sg);
    return rc;
}

/* Java_dev_acos_e2_Bridge_sym(name) -> long : dlsym offset (introspection) */
JNIEXPORT jlong JNICALL Java_dev_acos_e2_Bridge_sym(JNIEnv *env, jclass c, jstring n) {
    (void)c;
    const char *s = (*env)->GetStringUTFChars(env, n, NULL);
    void *p = dlsym(RTLD_DEFAULT, s);
    (*env)->ReleaseStringUTFChars(env, n, s);
    return (jlong)(uintptr_t)p;
}

JNIEXPORT jint JNICALL Java_dev_acos_e2_Bridge_installAll(JNIEnv *env, jclass c) {
    (void)c;
    if (!load_hwui(env)) return -1;
    int ok = 0;
    for (int ci = 0; ci < N_CLASSES; ci++) {
        jclass k = (*env)->FindClass(env, gClasses[ci]);
        if (!k) { (*env)->ExceptionClear(env); continue; }
        for (unsigned i = 0; i < N_NATIVES; i++) {
            JNINativeMethod m;
            m.name = (char *)gNatives[i].name;
            m.signature = (char *)gNatives[i].sig;
            m.fnPtr = (void *)(g_base + gNatives[i].off);
            if ((*env)->RegisterNatives(env, k, &m, 1) == 0) ok++;
            else (*env)->ExceptionClear(env);
        }
    }
    fprintf(stderr, "installAll registered=%d\n", ok);
    return ok;
}

/* The real entry point: libhwui exports register_android_graphics_classes(JNIEnv*),
 * which registers the whole android.graphics native surface AND caches the
 * class-global refs the natives rely on.  This is the seam we actually want. */
JNIEXPORT jint JNICALL Java_dev_acos_e2_Bridge_registerReal(JNIEnv *env, jclass c) {
    (void)c;
    if (!load_hwui(env)) return -1;
    void *sym = dlsym(g_hwui, "register_android_graphics_classes");
    if (!sym) { fprintf(stderr, "dlsym register_android_graphics_classes failed\n"); return -1; }
    fprintf(stderr, "calling register_android_graphics_classes @%p\n", sym);
    int rc = ((int (*)(JNIEnv *))sym)(env);
    fprintf(stderr, "register_android_graphics_classes returned %d\n", rc);

    /* the framework JNI surface is *also* exported from libandroid_runtime, as
     * C++-mangled register_android_* functions.  Call the ones the graphics
     * path needs (SystemProperties is the first thing DisplayMetrics touches). */
    void *hrt = dlopen("/system/lib64/libandroid_runtime.so", RTLD_NOW | RTLD_GLOBAL);
    fprintf(stderr, "dlopen libandroid_runtime -> %s\n", hrt ? "OK" : dlerror());
    static const char *extra[] = {
        "_ZN7android36register_android_os_SystemPropertiesEP7_JNIEnv",
        "_ZN7android25register_android_util_LogEP7_JNIEnv",
        "_ZN7android31register_android_os_SystemClockEP7_JNIEnv",
        "_ZN7android34register_android_util_CharsetUtilsEP7_JNIEnv",
    };
    for (unsigned i = 0; i < sizeof extra / sizeof extra[0]; i++) {
        void *p2 = hrt ? dlsym(hrt, extra[i]) : NULL;
        if (!p2) p2 = dlsym(RTLD_DEFAULT, extra[i]);
        if (!p2) p2 = dlsym(g_hwui, extra[i]);
        if (!p2) { fprintf(stderr, "  %s not resolvable\n", extra[i]); continue; }
        int r2 = ((int (*)(JNIEnv *))p2)(env);
        fprintf(stderr, "  %s -> %d\n", extra[i], r2);
    }
    return rc;
}
