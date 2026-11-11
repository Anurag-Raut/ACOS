/*
 * shim.c -- the JNI boilerplate half of libacosjniprobe.so.
 *
 * The experiment asks "can real ART use our Rust natives?"; the JNI entry
 * points must therefore look exactly like a normal NDK library
 * (Java_<pkg>_<Class>_<method>), which is 20 lines of jni.h glue.  All logic
 * lives in Rust (../src/lib.rs).  Compiled by the NDK clang, linked into the
 * Rust cdylib.
 */
#include <jni.h>
#include <pthread.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

/* Rust side */
extern int probe_add(int, int);
extern char *probe_echo(const char *);
extern void probe_free_string(char *);
extern int probe_array_inout(int *, int);
extern char *probe_service_call(const char *);
extern long long probe_service_bench(int, const char *);
extern int probe_fork(int, int, int *);

#define EXPORT __attribute__((visibility("default")))

static JavaVM *g_vm;
static jclass g_cb_class; /* global ref, needed to call back off-thread */

EXPORT jint JNICALL
Java_dev_acos_artprobe_NativeProbe_nativeAdd(JNIEnv *env, jclass cls, jint a, jint b) {
    (void)env; (void)cls;
    return probe_add((int)a, (int)b);
}

/* Java String -> C string -> Rust -> C string -> Java String. */
EXPORT jstring JNICALL
Java_dev_acos_artprobe_NativeProbe_nativeEcho(JNIEnv *env, jclass cls, jstring in) {
    (void)cls;
    if (!in) return NULL;
    const char *utf = (*env)->GetStringUTFChars(env, in, NULL);
    if (!utf) return NULL;
    char *out = probe_echo(utf);
    (*env)->ReleaseStringUTFChars(env, in, utf);
    if (!out) return NULL;
    jstring js = (*env)->NewStringUTF(env, out);
    probe_free_string(out);
    return js;
}

/* Native -> Java callback on the calling (Java) thread. */
EXPORT void JNICALL
Java_dev_acos_artprobe_NativeProbe_nativeCallback(JNIEnv *env, jclass cls, jstring msg) {
    if (!g_cb_class) {
        jclass local = (*env)->FindClass(env, "dev/acos/artprobe/Callback");
        if (!local) return;
        g_cb_class = (jclass)(*env)->NewGlobalRef(env, local);
        (*env)->DeleteLocalRef(env, local);
        if (!g_cb_class) return;
    }
    jmethodID m = (*env)->GetStaticMethodID(env, g_cb_class, "onNativeEvent",
                                            "(Ljava/lang/String;)V");
    if (!m) return;
    (*env)->CallStaticVoidMethod(env, g_cb_class, m, msg);
    (void)cls;
}

/* Same callback, but from a thread Rust/C created (attach + detach). */
static void *cb_thread(void *arg) {
    const char *msg = (const char *)arg;
    usleep(20 * 1000);
    JNIEnv *env = NULL;
    if ((*g_vm)->AttachCurrentThread(g_vm, &env, NULL) != JNI_OK) return NULL;
    jmethodID m = (*env)->GetStaticMethodID(env, g_cb_class, "onNativeEvent",
                                            "(Ljava/lang/String;)V");
    if (m) {
        jstring js = (*env)->NewStringUTF(env, msg);
        (*env)->CallStaticVoidMethod(env, g_cb_class, m, js);
        if (js) (*env)->DeleteLocalRef(env, js);
    }
    (*g_vm)->DetachCurrentThread(g_vm);
    return NULL;
}

EXPORT jint JNICALL
Java_dev_acos_artprobe_NativeProbe_nativeCallbackFromThread(JNIEnv *env, jclass cls) {
    (void)cls;
    if (!g_vm && (*env)->GetJavaVM(env, &g_vm) != JNI_OK) return -1;
    pthread_t t;
    if (pthread_create(&t, NULL, cb_thread, (void *)"from-native-thread") != 0) return -2;
    pthread_detach(t);
    return 0;
}

/* Primitive array in + out (GetIntArrayRegion / NewIntArray / SetIntArrayRegion). */
EXPORT jintArray JNICALL
Java_dev_acos_artprobe_NativeProbe_nativeCopyIntArray(JNIEnv *env, jclass cls, jintArray in) {
    (void)cls;
    if (!in) return NULL;
    jsize n = (*env)->GetArrayLength(env, in);
    jint *buf = (jint *)malloc(sizeof(jint) * (n ? (size_t)n : 1));
    if (!buf) return NULL;
    (*env)->GetIntArrayRegion(env, in, 0, n, buf);
    probe_array_inout((int *)buf, (int)n);
    jintArray out = (*env)->NewIntArray(env, n);
    if (out) (*env)->SetIntArrayRegion(env, out, 0, n, buf);
    free(buf);
    return out;
}

EXPORT jstring JNICALL
Java_dev_acos_artprobe_NativeProbe_nativeServiceCall(JNIEnv *env, jclass cls, jstring req) {
    (void)cls;
    const char *utf = req ? (*env)->GetStringUTFChars(env, req, NULL) : "";
    char *out = probe_service_call(utf);
    if (req && utf) (*env)->ReleaseStringUTFChars(env, req, utf);
    if (!out) return NULL;
    jstring js = (*env)->NewStringUTF(env, out);
    probe_free_string(out);
    return js;
}

EXPORT jlong JNICALL
Java_dev_acos_artprobe_NativeProbe_nativeServiceBench(JNIEnv *env, jclass cls, jint n,
                                                      jstring req) {
    (void)cls;
    const char *utf = req ? (*env)->GetStringUTFChars(env, req, NULL) : "";
    long long ns = probe_service_bench((int)n, utf);
    if (req && utf) (*env)->ReleaseStringUTFChars(env, req, utf);
    return (jlong)ns;
}

/* Fork N children off the warm parent; writes their pids into `pids`. */
EXPORT jint JNICALL
Java_dev_acos_artprobe_NativeProbe_nativeFork(JNIEnv *env, jclass cls, jint n, jint hold_ms,
                                              jintArray pids) {
    (void)cls;
    if (!pids) return -1;
    jsize cap = (*env)->GetArrayLength(env, pids);
    if (n > cap) n = cap;
    jint *buf = (jint *)calloc((size_t)(n ? n : 1), sizeof(jint));
    if (!buf) return -1;
    int made = probe_fork((int)n, (int)hold_ms, (int *)buf);
    if (made > 0) (*env)->SetIntArrayRegion(env, pids, 0, made, buf);
    free(buf);
    return (jint)made;
}
