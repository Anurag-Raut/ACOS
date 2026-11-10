/*
 * ACOS native-hello fixture library (owned test code).
 *
 * Loaded by `acos launch --native` through `acos-native-loader` under the
 * ADR-0022 confinement. It exercises three product seams:
 *   - a plain JNI String return;
 *   - an APK asset read through `AAssetManager_*`;
 *   - an `android.graphics.Bitmap` filled through `AndroidBitmap_lockPixels`.
 *
 * No third-party code is involved; this file is compiled by
 * `tools/build-apps.sh` with the NDK clang.
 */
#include <jni.h>
#include <android/asset_manager.h>
#include <android/asset_manager_jni.h>
#include <android/bitmap.h>
#include <stdint.h>
#include <string.h>

JNIEXPORT jstring JNICALL
Java_dev_acos_fixtures_nativehello_NativeHelloApp_nativeHello(JNIEnv *env, jobject thiz) {
    (void) thiz;
    return (*env)->NewStringUTF(env, "hello-from-native");
}

JNIEXPORT jstring JNICALL
Java_dev_acos_fixtures_nativehello_NativeHelloApp_readAsset(JNIEnv *env, jobject thiz, jobject manager) {
    (void) thiz;
    if (manager == NULL) {
        return NULL;
    }
    AAssetManager *mgr = AAssetManager_fromJava(env, manager);
    if (mgr == NULL) {
        return (*env)->NewStringUTF(env, "no-manager");
    }
    AAsset *asset = AAssetManager_open(mgr, "assets/hello.txt", AASSET_MODE_BUFFER);
    if (asset == NULL) {
        return (*env)->NewStringUTF(env, "no-asset");
    }
    char buf[64];
    memset(buf, 0, sizeof(buf));
    int n = AAsset_read(asset, buf, sizeof(buf) - 1);
    AAsset_close(asset);
    if (n < 0) {
        return NULL;
    }
    return (*env)->NewStringUTF(env, buf);
}

JNIEXPORT jint JNICALL
Java_dev_acos_fixtures_nativehello_NativeHelloApp_fillBitmap(JNIEnv *env, jobject thiz, jobject bitmap) {
    (void) thiz;
    AndroidBitmapInfo info;
    memset(&info, 0, sizeof(info));
    if (AndroidBitmap_getInfo(env, bitmap, &info) != ANDROID_BITMAP_RESULT_SUCCESS) {
        return -1;
    }
    void *pixels = NULL;
    if (AndroidBitmap_lockPixels(env, bitmap, &pixels) != ANDROID_BITMAP_RESULT_SUCCESS) {
        return -2;
    }
    uint8_t *base = (uint8_t *) pixels;
    for (uint32_t y = 0; y < info.height; y++) {
        for (uint32_t x = 0; x < info.width; x++) {
            uint8_t *p = base + (size_t) y * info.stride + (size_t) x * 4;
            p[0] = 0x00;
            p[1] = 0x00;
            p[2] = 0x00;
            p[3] = 0xff;
        }
    }
    /* Pixel (1,1): opaque ARGB 0xFF112233 (premultiplied storage: unchanged). */
    uint8_t *q = base + (size_t) info.stride + 4;
    q[0] = 0x11;
    q[1] = 0x22;
    q[2] = 0x33;
    q[3] = 0xff;
    if (AndroidBitmap_unlockPixels(env, bitmap) != ANDROID_BITMAP_RESULT_SUCCESS) {
        return -3;
    }
    return (jint) (info.width * info.height);
}
