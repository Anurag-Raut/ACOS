#include <jni.h>
#include <stddef.h>

JNIEXPORT jstring JNICALL
Java_dev_acos_fixtures_hellojni_MainActivity_stringFromJNI(JNIEnv *env, jobject thiz) {
    (void) thiz;
    return (*env)->NewStringUTF(env, "Hello from native C");
}

JNIEXPORT jint JNICALL
Java_dev_acos_fixtures_hellojni_MainActivity_add(JNIEnv *env, jobject thiz, jint a, jint b) {
    (void) env;
    (void) thiz;
    return a + b;
}

JNIEXPORT jint JNICALL
Java_dev_acos_fixtures_hellojni_MainActivity_sumArray(JNIEnv *env, jobject thiz, jintArray values) {
    (void) thiz;
    jsize n = (*env)->GetArrayLength(env, values);
    jint *elems = (*env)->GetIntArrayElements(env, values, NULL);
    if (elems == NULL) {
        return -1;
    }
    jint sum = 0;
    for (jsize i = 0; i < n; i++) {
        sum += elems[i];
    }
    (*env)->ReleaseIntArrayElements(env, values, elems, JNI_ABORT);
    return sum;
}

/* Native -> Java: CallStaticIntMethod + CallVoidMethod. */
JNIEXPORT void JNICALL
Java_dev_acos_fixtures_hellojni_MainActivity_callBack(JNIEnv *env, jobject thiz) {
    jclass cls = (*env)->GetObjectClass(env, thiz);
    if (cls == NULL) {
        return;
    }
    jmethodID staticMid = (*env)->GetStaticMethodID(env, cls, "javaCallback", "(I)I");
    if (staticMid != NULL) {
        (void) (*env)->CallStaticIntMethod(env, cls, staticMid, (jint) 21);
    }
    jmethodID voidMid = (*env)->GetMethodID(env, cls, "voidCallback", "(Ljava/lang/String;)V");
    if (voidMid != NULL) {
        jstring s = (*env)->NewStringUTF(env, "from-native");
        if (s != NULL) {
            (*env)->CallVoidMethod(env, thiz, voidMid, s);
            (*env)->DeleteLocalRef(env, s);
        }
    }
    (*env)->DeleteLocalRef(env, cls);
}

/* Native -> Java: throw a Java exception with ThrowNew. */
JNIEXPORT void JNICALL
Java_dev_acos_fixtures_hellojni_MainActivity_throwFromJNI(JNIEnv *env, jobject thiz) {
    (void) thiz;
    jclass cls = (*env)->FindClass(env, "java/lang/IllegalStateException");
    if (cls != NULL) {
        (*env)->ThrowNew(env, cls, "thrown from native");
    }
}

/* Registered via RegisterNatives in JNI_OnLoad. */
static jstring JNICALL native_registeredEcho(JNIEnv *env, jobject thiz, jstring s) {
    (void) thiz;
    return s;
}

JNIEXPORT jint JNICALL
JNI_OnLoad(JavaVM *vm, void *reserved) {
    (void) reserved;
    JNIEnv *env;
    if ((*vm)->GetEnv(vm, (void **) &env, JNI_VERSION_1_6) != JNI_OK) {
        return JNI_ERR;
    }
    jclass cls = (*env)->FindClass(env, "dev/acos/fixtures/hellojni/MainActivity");
    if (cls == NULL) {
        return JNI_ERR;
    }
    static const JNINativeMethod methods[] = {
        {"registeredEcho", "(Ljava/lang/String;)Ljava/lang/String;", (void *) native_registeredEcho},
    };
    if ((*env)->RegisterNatives(env, cls, methods, 1) != JNI_OK) {
        return JNI_ERR;
    }
    return JNI_VERSION_1_6;
}
