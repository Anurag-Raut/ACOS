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
 */
#include <jni.h>
#include <stddef.h>

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

    if (buf != NULL) {
        (*env)->DeleteLocalRef(env, buf);
    }
    return pass;
}
