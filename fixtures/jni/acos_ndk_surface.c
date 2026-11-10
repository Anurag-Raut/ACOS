/*
 * ACOS NDK platform-surface fixture (`libandroid` / `libjnigraphics` / EGL).
 *
 * Compiled with the NDK and loaded through `acos-native-loader` exactly like a
 * third-party React Native / Flutter `.so`. Every call below goes through the
 * loader's bionic table into the NDK shims; the shims answer from the installed
 * host (the runtime's bridge, or the test host in `tests/ndk.rs`).
 *
 * Native handler arguments are `void*` instead of `jobject` because the
 * fixture is driven from a Rust integration test, not from Java: a `jobject`
 * is an opaque pointer to the NDK, so the host maps the sentinel address to
 * its `Bitmap`/`Surface` record. `AAssetManager_fromJava` is called with the
 * same sentinel.
 *
 * Each function returns 0 when every check passed and a non-zero bitmask of
 * the failing checks otherwise.
 */
#include <android/asset_manager.h>
#include <android/asset_manager_jni.h>
#include <android/bitmap.h>
#include <android/choreographer.h>
#include <android/configuration.h>
#include <android/looper.h>
#include <android/native_window.h>
#include <android/native_window_jni.h>
#include <jni.h>
#include <EGL/egl.h>
#include <GLES2/gl2.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

#define FAIL(bit) do { failures |= (1u << (bit)); } while (0)

/* ------------------------------------------------------------------ */
/* Assets                                                              */
/* ------------------------------------------------------------------ */

int acos_ndk_asset_test(void *java_manager, const char *path, const char *dir) {
    unsigned failures = 0;
    AAssetManager *mgr = AAssetManager_fromJava(NULL, java_manager);
    if (mgr == NULL) return 1u << 0;

    AAsset *a = AAssetManager_open(mgr, path, AASSET_MODE_BUFFER);
    if (a == NULL) return 1u << 1;

    if (AAsset_getLength(a) != 12) FAIL(2);
    if (AAsset_getLength64(a) != 12) FAIL(3);
    if (!AAsset_isAllocated(a)) FAIL(4);

    char buf[16];
    memset(buf, 0, sizeof(buf));
    int n = AAsset_read(a, buf, sizeof(buf));
    if (n != 12 || memcmp(buf, "hello-acos!!", 12) != 0) FAIL(5);

    /* getBuffer returns the whole entry, whatever the read position. */
    const void *whole = AAsset_getBuffer(a);
    if (whole == NULL || memcmp(whole, "hello-acos!!", 12) != 0) FAIL(6);

    /* seek(0, SEEK_SET) then a 5-byte prefix read. */
    if (AAsset_seek64(a, 0, SEEK_SET) != 0) FAIL(7);
    memset(buf, 0, sizeof(buf));
    if (AAsset_read(a, buf, 5) != 5 || memcmp(buf, "hello", 5) != 0) FAIL(8);
    if (AAsset_getRemainingLength64(a) != 7) FAIL(9);
    AAsset_close(a);

    AAssetDir *d = AAssetManager_openDir(mgr, dir);
    if (d == NULL) FAIL(10);
    int seen = 0;
    const char *name;
    while (d != NULL && (name = AAssetDir_getNextFileName(d)) != NULL) {
        if (strcmp(name, "hello.txt") == 0) seen = 1;
    }
    if (!seen) FAIL(11);
    if (d != NULL) {
        AAssetDir_rewind(d);
        if (AAssetDir_getNextFileName(d) == NULL) FAIL(12);
        AAssetDir_close(d);
    }
    return (int) failures;
}

/* ------------------------------------------------------------------ */
/* Bitmaps (libjnigraphics)                                            */
/* ------------------------------------------------------------------ */

int acos_ndk_bitmap_test(void *java_bitmap) {
    unsigned failures = 0;
    AndroidBitmapInfo info;
    memset(&info, 0, sizeof(info));
    if (AndroidBitmap_getInfo(NULL, java_bitmap, &info) != ANDROID_BITMAP_RESULT_SUCCESS)
        FAIL(0);
    if (info.width != 4 || info.height != 3 || info.stride != 16) FAIL(1);
    if (info.format != 1 /* ANDROID_BITMAP_FORMAT_RGBA_8888 */) FAIL(2);

    void *pixels = NULL;
    if (AndroidBitmap_lockPixels(NULL, java_bitmap, &pixels) != ANDROID_BITMAP_RESULT_SUCCESS)
        FAIL(3);
    if (pixels == NULL) {
        FAIL(4);
    } else {
        /* Pixel (0,0) = opaque ARGB 0xFF112233, RGBA byte order (premultiplied
         * storage: an opaque pixel round-trips unchanged through getPixel). */
        uint8_t *p = (uint8_t *) pixels;
        p[0] = 0x11;
        p[1] = 0x22;
        p[2] = 0x33;
        p[3] = 0xff;
        /* Pixel (2,1), row 1. */
        uint8_t *q = p + info.stride + 2 * 4;
        q[0] = 0xaa;
        q[1] = 0xbb;
        q[2] = 0xcc;
        q[3] = 0xff;
    }
    if (AndroidBitmap_unlockPixels(NULL, java_bitmap) != ANDROID_BITMAP_RESULT_SUCCESS)
        FAIL(5);
    return (int) failures;
}

/* ------------------------------------------------------------------ */
/* ANativeWindow                                                       */
/* ------------------------------------------------------------------ */

int acos_ndk_window_test(void *java_surface) {
    unsigned failures = 0;
    ANativeWindow *w = ANativeWindow_fromSurface(NULL, java_surface);
    if (w == NULL) return 1u << 0;

    if (ANativeWindow_getWidth(w) != 8) FAIL(1);
    if (ANativeWindow_getHeight(w) != 4) FAIL(2);
    if (ANativeWindow_getFormat(w) != WINDOW_FORMAT_RGBA_8888) FAIL(3);
    if (ANativeWindow_setBuffersGeometry(w, 8, 4, WINDOW_FORMAT_RGBA_8888) != 0) FAIL(4);

    ANativeWindow_acquire(w);
    ANativeWindow_Buffer b;
    memset(&b, 0, sizeof(b));
    if (ANativeWindow_lock(w, &b, NULL) != 0) {
        FAIL(5);
    } else {
        if (b.width != 8 || b.height != 4 || b.stride < 8 || b.format != 1) FAIL(6);
        if (b.bits == NULL) {
            FAIL(7);
        } else {
            uint8_t *p = (uint8_t *) b.bits;
            for (int y = 0; y < b.height; y++) {
                for (int x = 0; x < b.width; x++) {
                    uint8_t *q = p + (size_t) y * (size_t) b.stride * 4 + (size_t) x * 4;
                    q[0] = (uint8_t) (x * 16);
                    q[1] = (uint8_t) (y * 32);
                    q[2] = 0x7f;
                    q[3] = 0xff;
                }
            }
        }
        if (ANativeWindow_unlockAndPost(w) != 0) FAIL(8);
    }
    ANativeWindow_release(w);
    return (int) failures;
}

/* ------------------------------------------------------------------ */
/* ALooper                                                             */
/* ------------------------------------------------------------------ */

int acos_ndk_looper_test(void) {
    unsigned failures = 0;
    if (ALooper_forThread() != NULL) FAIL(0); /* nothing prepared yet */
    ALooper *looper = ALooper_prepare(ALOOPER_PREPARE_ALLOW_NON_CALLBACKS);
    if (looper == NULL) return 1u << 1;
    if (ALooper_forThread() != looper) FAIL(2);

    int fds[2];
    if (pipe(fds) != 0) return 1u << 3;
    if (ALooper_addFd(looper, fds[0], 42, ALOOPER_EVENT_INPUT, NULL, NULL) != 1) FAIL(4);

    int out_fd = -1, out_ev = 0;
    void *out_data = NULL;
    if (ALooper_pollOnce(0, &out_fd, &out_ev, &out_data) != ALOOPER_POLL_TIMEOUT) FAIL(5);

    if (write(fds[1], "x", 1) != 1) FAIL(6);
    int r = ALooper_pollOnce(1000, &out_fd, &out_ev, &out_data);
    if (r != 42) FAIL(7);
    if (out_fd != fds[0] || (out_ev & ALOOPER_EVENT_INPUT) == 0) FAIL(8);
    char c;
    (void) read(fds[0], &c, 1);

    if (ALooper_removeFd(looper, fds[0]) != 1) FAIL(9);
    ALooper_acquire(looper);
    ALooper_wake(looper);
    ALooper_release(looper);
    ALooper_release(looper);
    close(fds[0]);
    close(fds[1]);
    return (int) failures;
}

/* ------------------------------------------------------------------ */
/* AConfiguration                                                      */
/* ------------------------------------------------------------------ */

int acos_ndk_config_test(void) {
    unsigned failures = 0;
    AConfiguration *c = AConfiguration_new();
    if (c == NULL) return 1u << 0;
    if (AConfiguration_getDensity(c) != 420) FAIL(1);
    int32_t size = AConfiguration_getScreenSize(c);
    int w = size >> 16;
    int h = size & 0xffff;
    if (w != 1260 || h != 2100) FAIL(2);
    if (AConfiguration_getOrientation(c) != 1) FAIL(3);
    if (AConfiguration_getScreenWidthDp(c) != 480) FAIL(4);
    AConfiguration_delete(c);
    return (int) failures;
}

/* ------------------------------------------------------------------ */
/* AChoreographer                                                      */
/* ------------------------------------------------------------------ */

static int frames_seen = 0;

static void on_frame(long frame_time_nanos, void *data) {
    (void) frame_time_nanos;
    (void) data;
    frames_seen++;
}

int acos_ndk_choreographer_test(void) {
    unsigned failures = 0;
    AChoreographer *ch = AChoreographer_getInstance();
    if (ch == NULL) return 1u << 0;
    if (AChoreographer_getInstance() != ch) FAIL(1);
    frames_seen = 0;
    AChoreographer_postFrameCallback(ch, on_frame, NULL);
    AChoreographer_postFrameCallback64(ch, on_frame, NULL);
    if (frames_seen != 2) FAIL(2);
    return (int) failures;
}

/* ------------------------------------------------------------------ */
/* EGL / GLES: reported traps, never a fake success                    */
/* ------------------------------------------------------------------ */

int acos_ndk_egl_test(void) {
    unsigned failures = 0;
    if (eglGetDisplay(EGL_DEFAULT_DISPLAY) != EGL_NO_DISPLAY) FAIL(0);
    if (eglGetCurrentDisplay() != EGL_NO_DISPLAY) FAIL(1);
    if (eglInitialize(EGL_NO_DISPLAY, NULL, NULL) != EGL_FALSE) FAIL(2);
    if (eglGetError() != EGL_NOT_INITIALIZED) FAIL(3);
    if (eglGetProcAddress("eglCreateContext") != NULL) FAIL(4);
    if (eglQueryString(EGL_NO_DISPLAY, EGL_VENDOR) != NULL) FAIL(5);
    if (glGetError() != GL_INVALID_OPERATION) FAIL(6);
    if (glGetString(GL_VERSION) != NULL) FAIL(7);
    return (int) failures;
}
