/*
 * Native JNI slot fixture for ACOS.
 *
 * Compiled against the NDK `jni.h` and loaded through the same
 * `acos-native-loader` path as `fixtures/apps/hello-jni/jni/hello-jni.c`: the
 * library is mapped, relocated and executed in-process, but every JNI call it
 * makes goes back through the real `JNINativeInterface` table installed by
 * `acos-jni`. The fixture therefore exercises the newly implemented slots
 * through the C ABI exactly as a third-party `.so` would.
 *
 * `acos_jni_test_slots` returns a bitmask; every bit must be set.
 *
 *   bit 0  NewDirectByteBuffer + GetDirectBufferAddress/Capacity
 *   bit 1  NewWeakGlobalRef + GetObjectRefType
 *   bit 2  IsSameObject(weak, object)
 *   bit 3  GetStringCritical / ReleaseStringCritical
 *   bit 4  GetPrimitiveArrayCritical
 *   bit 5  Set/GetIntArrayRegion round trip
 *   bit 6  PushLocalFrame / PopLocalFrame
 *   bit 7  MonitorEnter / MonitorExit
 *   bit 8  CallNonvirtualIntMethod (varargs C shim)
 *   bit 9  CallNonvirtualIntMethodA (jvalue[] C shim)
 *   bit 10 native pthread attach / GetEnv / detach
 */
#include <jni.h>
#include <pthread.h>
#include <stddef.h>
#include <stdint.h>

/*
 * A native worker thread with no thread-local runtime: it must observe
 * JNI_EDETACHED from `GetEnv`, attach, observe a valid environment, and detach.
 */
typedef struct {
    JavaVM *vm;
    int bits;
} worker_arg;

static void *worker_main(void *raw) {
    worker_arg *w = (worker_arg *) raw;
    JNIEnv *env = NULL;
    int bits = 0;
    if ((*w->vm)->GetEnv(w->vm, (void **) &env, JNI_VERSION_1_6) == JNI_EDETACHED) {
        bits |= 1;
    }
    env = NULL;
    if ((*w->vm)->AttachCurrentThread(w->vm, &env, NULL) == JNI_OK && env != NULL) {
        bits |= 2;
    }
    JNIEnv *again = NULL;
    if ((*w->vm)->GetEnv(w->vm, (void **) &again, JNI_VERSION_1_6) == JNI_OK
        && again == env) {
        bits |= 4;
    }
    if ((*w->vm)->DetachCurrentThread(w->vm) == JNI_OK) {
        bits |= 8;
    }
    /* After detach GetEnv must report the thread is no longer attached. */
    if ((*w->vm)->GetEnv(w->vm, (void **) &env, JNI_VERSION_1_6) == JNI_EDETACHED) {
        bits |= 16;
    }
    w->bits = bits;
    return NULL;
}

#ifndef JNIWeakGlobalRefType
#define JNIWeakGlobalRefType 3
#endif

JNIEXPORT jint JNICALL
acos_jni_test_slots(JNIEnv *env, jclass cls) {
    (void) cls;
    jint pass = 0;

    /* Direct buffer with a freshly allocated backing region (address NULL). */
    jobject buf = (*env)->NewDirectByteBuffer(env, NULL, 16);
    if (buf != NULL) {
        void *addr = (*env)->GetDirectBufferAddress(env, buf);
        jlong cap = (*env)->GetDirectBufferCapacity(env, buf);
        if (addr != NULL && cap == 16) {
            pass |= 1;
        }

        /* Weak global reference: registered and typed as weak. */
        jweak w = (*env)->NewWeakGlobalRef(env, buf);
        if (w != NULL) {
            if ((*env)->GetObjectRefType(env, w) == JNIWeakGlobalRefType) {
                pass |= 2;
            }
            if ((*env)->IsSameObject(env, w, buf)) {
                pass |= 4;
            }
            (*env)->DeleteWeakGlobalRef(env, w);
        }

        /* Monitor enter/exit on the same object. */
        if ((*env)->MonitorEnter(env, buf) == JNI_OK) {
            if ((*env)->MonitorExit(env, buf) == JNI_OK) {
                pass |= 128;
            }
        }
    }

    /* String critical section. */
    jstring s = (*env)->NewStringUTF(env, "critical");
    if (s != NULL) {
        const jchar *chars = (*env)->GetStringCritical(env, s, NULL);
        if (chars != NULL && chars[0] == (jchar) 'c') {
            pass |= 8;
        }
        (*env)->ReleaseStringCritical(env, s, chars);
        (*env)->DeleteLocalRef(env, s);
    }

    /* Primitive array critical section and region round trip. */
    jintArray arr = (*env)->NewIntArray(env, 3);
    if (arr != NULL) {
        const jint values[3] = {1, 2, 3};
        (*env)->SetIntArrayRegion(env, arr, 0, 3, values);
        jint *critical = (*env)->GetPrimitiveArrayCritical(env, arr, NULL);
        if (critical != NULL && critical[2] == 3) {
            pass |= 16;
        }
        (*env)->ReleasePrimitiveArrayCritical(env, arr, critical, JNI_ABORT);

        jint back[3] = {0, 0, 0};
        (*env)->GetIntArrayRegion(env, arr, 0, 3, back);
        if (back[1] == 2) {
            pass |= 32;
        }
        (*env)->DeleteLocalRef(env, arr);
    }

    /* Local frame push/pop with a resolved class inside the frame. */
    if ((*env)->PushLocalFrame(env, 4) == JNI_OK) {
        jclass c = (*env)->FindClass(env, "java/lang/Object");
        if (c != NULL) {
            pass |= 64;
        }
        (*env)->PopLocalFrame(env, NULL);
    }

    /* Non-virtual call: the same String.length method through both the
     * varargs and the jvalue[] C shims. */
    {
        jclass strClass = (*env)->FindClass(env, "java/lang/String");
        jstring str = (*env)->NewStringUTF(env, "nonvirtual");
        if (strClass != NULL && str != NULL) {
            jmethodID lenMid = (*env)->GetMethodID(env, strClass, "length", "()I");
            if (lenMid != NULL) {
                jint v = (*env)->CallNonvirtualIntMethod(env, str, strClass, lenMid);
                if (v == 10) {
                    pass |= 256;
                }
                jint va = (*env)->CallNonvirtualIntMethodA(env, str, strClass, lenMid, NULL);
                if (va == 10) {
                    pass |= 512;
                }
            }
        }
        if (str != NULL) {
            (*env)->DeleteLocalRef(env, str);
        }
        if (strClass != NULL) {
            (*env)->DeleteLocalRef(env, strClass);
        }
    }

    /* Native OS-thread attach / GetEnv / detach. */
    {
        JavaVM *javaVm = NULL;
        if ((*env)->GetJavaVM(env, &javaVm) == JNI_OK && javaVm != NULL) {
            worker_arg w;
            w.vm = javaVm;
            w.bits = 0;
            pthread_t tid;
            if (pthread_create(&tid, NULL, worker_main, &w) == 0) {
                pthread_join(tid, NULL);
                if (w.bits == 0x1F) {
                    pass |= 1024;
                }
            }
        }
    }

    if (buf != NULL) {
        (*env)->DeleteLocalRef(env, buf);
    }
    return pass;
}
