// render_shim.c -- the `acos-art render-app` substrate shim (LD_PRELOAD).
//
// Ported from the E2D feasibility shim
// (research/art-feasibility/d-framework/native/framework_shim.c), with the
// property table dropped: `libartshim.so` (crates/acos-art/shim/artshim.c)
// already serves `__system_property_*` from /data/acos.properties, and both are
// LD_PRELOADed together. Three jobs remain:
//
//  1. LOAD libandroid_runtime.so + libhwui.so IN THE DEFAULT LINKER NAMESPACE.
//     `System.load` would go through libnativeloader and put the framework into
//     a fresh *classloader* namespace whose permitted paths are just
//     /data:/mnt/expand, so every system library is "not accessible". On device
//     the framework natives are registered from app_process in the default
//     namespace; a preload constructor does the same, RTLD_GLOBAL, before any
//     Java runs. This is what makes libhwui's exported
//     `register_android_graphics_classes(JNIEnv*)` reachable.
//
//  2. A memfd stand-in for /dev/binder (ACOSART_BINDER_FAKE=1). There is no
//     servicemanager and no binder driver, but ProcessState::open_driver()
//     wants open(O_RDWR|O_CLOEXEC), ioctl(BINDER_VERSION)==8, ioctl(SET_MAX_
//     THREADS)==0 and an mmap of BINDER_VM_SIZE. A memfd satisfies open+mmap;
//     the two ioctls are answered here, everything else gets ENOTTY (and
//     therefore a normal "service unavailable" on the Java side).
//
//  3. THE Java -> `android::register_*_(_JNIEnv*)` BRIDGE
//     (dev.acos.art.RenderNat). It must live in the *same* object as the
//     constructor: bionic symbol lookup is namespace-scoped, so a library in
//     the id/task classloader namespace cannot dlsym a library from the default
//     namespace even with RTLD_GLOBAL (measured in E2D).
#define _GNU_SOURCE
#include <jni.h>
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef __NR_memfd_create
#define __NR_memfd_create 319
#endif
#ifndef MFD_CLOEXEC
#define MFD_CLOEXEC 0x0001U
#endif

#define BINDER_VM_SIZE (1024 * 1024 - 2 * 4096)
#define LOG(...)                      \
    do {                              \
        dprintf(2, "ACOSART[render]: "); \
        dprintf(2, __VA_ARGS__);      \
        dprintf(2, "\n");             \
    } while (0)

// ------------------------------------------------------------------ (1) load

__attribute__((constructor)) static void acosart_load_framework(void) {
    const char* list = getenv("ACOSART_LOAD");
    if (list == NULL || *list == '\0') list = "libandroid_runtime.so:libhwui.so";
    char buf[4096];
    snprintf(buf, sizeof(buf), "%s", list);
    for (char* save = NULL, *tok = strtok_r(buf, ":", &save); tok;
         tok = strtok_r(NULL, ":", &save)) {
        void* h = dlopen(tok, RTLD_NOW | RTLD_GLOBAL);
        if (h == NULL && tok[0] != '/') {
            // A bare soname is what the device uses; if the default namespace
            // does not resolve it, retry at its on-device absolute path.
            char path[512];
            snprintf(path, sizeof(path), "/system/lib64/%s", tok);
            h = dlopen(path, RTLD_NOW | RTLD_GLOBAL);
            if (h == NULL) h = dlopen(strcat(path, ".so"), RTLD_NOW | RTLD_GLOBAL);
        }
        if (h == NULL) LOG("dlopen(%s) FAILED: %s", tok, dlerror());
        else LOG("dlopen(%s) ok @%p (default namespace, RTLD_GLOBAL)", tok, h);
    }
}

// -------------------------------------------------- (2) fake /dev/binder{,fs}

static const char* kBinderPaths[] = {
        "/dev/binder", "/dev/hwbinder", "/dev/vndbinder", "/dev/binderfs/binder", NULL};

static char g_fake[8][64];
static int g_fake_n;

static int is_binder_path(const char* p) {
    if (p == NULL) return 0;
    for (int i = 0; kBinderPaths[i]; i++)
        if (strcmp(p, kBinderPaths[i]) == 0) return 1;
    return 0;
}

static int is_our_fd(int fd) {
    for (int i = 0; i < g_fake_n; i++)
        if (atoi(g_fake[i] + strcspn(g_fake[i], "=") + 1) == fd) return 1;
    return 0;
}

static int fake_binder(const char* path) {
    const char* on = getenv("ACOSART_BINDER_FAKE");
    if (on == NULL || strcmp(on, "1") != 0) return -1;
    int fd = (int)syscall(__NR_memfd_create, "acosart-binder", MFD_CLOEXEC);
    if (fd < 0) {
        LOG("memfd_create failed: %s", strerror(errno));
        return -1;
    }
    if (ftruncate(fd, BINDER_VM_SIZE) != 0) LOG("ftruncate failed: %s", strerror(errno));
    if (g_fake_n < 8) {
        snprintf(g_fake[g_fake_n], sizeof(g_fake[0]), "%s=%d", path, fd);
        g_fake_n++;
    }
    return fd;
}

typedef int (*open_fn)(const char*, int, ...);
typedef int (*openat_fn)(int, const char*, int, ...);
typedef int (*ioctl_fn)(int, unsigned long, ...);

int open(const char* path, int flags, ...) {
    if (is_binder_path(path)) {
        int fd = fake_binder(path);
        if (fd >= 0) return fd;
    }
    static open_fn real;
    if (!real) real = (open_fn)dlsym(RTLD_NEXT, "open");
    va_list ap;
    va_start(ap, flags);
    mode_t mode = (mode_t)va_arg(ap, int);
    va_end(ap);
    return real(path, flags, mode);
}

int open64(const char* path, int flags, ...) {
    if (is_binder_path(path)) {
        int fd = fake_binder(path);
        if (fd >= 0) return fd;
    }
    static open_fn real;
    if (!real) real = (open_fn)dlsym(RTLD_NEXT, "open64");
    va_list ap;
    va_start(ap, flags);
    mode_t mode = (mode_t)va_arg(ap, int);
    va_end(ap);
    return real(path, flags, mode);
}

int __open_2(const char* path, int flags) {
    if (is_binder_path(path)) {
        int fd = fake_binder(path);
        if (fd >= 0) return fd;
    }
    static open_fn real;
    if (!real) real = (open_fn)dlsym(RTLD_NEXT, "__open_2");
    return real(path, flags);
}

int __openat_2(int dirfd, const char* path, int flags) {
    static openat_fn real;
    if (!real) real = (openat_fn)dlsym(RTLD_NEXT, "__openat_2");
    return real(dirfd, path, flags);
}

int ioctl(int fd, int request, ...) {
    va_list ap;
    va_start(ap, request);
    void* arg = va_arg(ap, void*);
    va_end(ap);

    static ioctl_fn real;
    if (!real) real = (ioctl_fn)dlsym(RTLD_NEXT, "ioctl");

    if (is_our_fd(fd)) {
        switch ((unsigned int)request) {
            case 0xc0046209u:  // BINDER_VERSION
                if (arg) *(int*)arg = 8;
                return 0;
            case 0x40046210u:  // BINDER_ENABLE_ONEWAY_SPAM_DETECTION
            case 0x40046208u:  // BINDER_SET_INHERIT_FIFO_PRIO
            case 0x40046205u:  // BINDER_SET_MAX_THREADS
                return 0;
            default:
                LOG("ioctl(fd=%d, req=0x%x) -> ENOTTY (no binder transport)", fd, (unsigned)request);
                errno = ENOTTY;
                return -1;
        }
    }
    return ((ioctl_fn)real)(fd, (unsigned long)request, arg);
}

// --------------------------------------------------------------------- (3) JNI

static void mangle_android_register(const char* plain, char* out, size_t n) {
    snprintf(out, n, "_ZN7android%zu%sEP7_JNIEnv", strlen(plain), plain);
}

static void mangle_global_register(const char* plain, char* out, size_t n) {
    snprintf(out, n, "_Z%zu%sP7_JNIEnv", strlen(plain), plain);
}

// Most entry points are in namespace android (_ZN7android...), a few
// (register_android_os_Binder/Process) are at global scope (_Z...).
static void* lookup_symbol(const char* plain, char* mangled, size_t n) {
    mangle_android_register(plain, mangled, n);
    void* p = dlsym(RTLD_DEFAULT, mangled);
    if (p) return p;
    char gmangled[256];
    mangle_global_register(plain, gmangled, sizeof(gmangled));
    p = dlsym(RTLD_DEFAULT, gmangled);
    if (p) {
        snprintf(mangled, n, "%s", gmangled);
        return p;
    }
    return dlsym(RTLD_DEFAULT, plain);
}

JNIEXPORT jlong JNICALL Java_dev_acos_art_RenderNat_sym(JNIEnv* env, jclass klass, jstring name) {
    (void)klass;
    const char* plain = (*env)->GetStringUTFChars(env, name, NULL);
    char mangled[256];
    void* p = lookup_symbol(plain, mangled, sizeof(mangled));
    (*env)->ReleaseStringUTFChars(env, name, plain);
    return (jlong)(intptr_t)p;
}

JNIEXPORT jint JNICALL Java_dev_acos_art_RenderNat_register(JNIEnv* env, jclass klass, jstring name) {
    (void)klass;
    const char* plain = (*env)->GetStringUTFChars(env, name, NULL);
    char mangled[256];
    void* p = lookup_symbol(plain, mangled, sizeof(mangled));
    if (p == NULL) {
        LOG("register: missing symbol %s (%s)", plain, mangled);
        (*env)->ReleaseStringUTFChars(env, name, plain);
        return 0;
    }
    typedef void (*register_fn)(JNIEnv*);
    ((register_fn)p)(env);
    jint rc = 1;
    if ((*env)->ExceptionCheck(env)) rc = -1;  // leave it pending: Java prints it
    (*env)->ReleaseStringUTFChars(env, name, plain);
    return rc;
}

// ------------------------------------------------ (4) Camera Option A frame sink
//
// CAMERA3 (ADR camera frames Option A): the host Session Controller (`hw.camera`
// in acos-services) serves ICameraService / ICameraDeviceUser and drives
// ICameraDeviceCallbacks. Pixels are filled in-process by this tiny JNI helper
// on the app's own local ANativeWindow (from the stream's Surface), exercising
// the real stock libgui BufferQueue, libnativewindow AHardwareBuffer_lockPlanes,
// gralloc_shim.c YUV_420_888/BLOB, and ImageReader.OnImageAvailableListener.

#include <pthread.h>

typedef struct ANativeWindow ANativeWindow;
typedef struct ANativeWindowBuffer ANativeWindowBuffer;
typedef struct AHardwareBuffer AHardwareBuffer;

typedef struct {
    int32_t left;
    int32_t top;
    int32_t right;
    int32_t bottom;
} acos_arect_t;

typedef struct {
    uint32_t width;
    uint32_t height;
    uint32_t layers;
    uint32_t format;
    uint64_t usage;
    uint32_t stride;
    uint32_t rfu0;
    uint64_t rfu1;
} acos_ahb_desc_t;

typedef struct {
    void* data;
    uint32_t pixelStride;
    uint32_t rowStride;
} acos_ahb_plane_t;

typedef struct {
    uint32_t planeCount;
    acos_ahb_plane_t planes[4];
} acos_ahb_planes_t;

typedef ANativeWindow* (*fn_ANW_fromSurface)(JNIEnv*, jobject);
typedef void (*fn_ANW_release)(ANativeWindow*);
typedef int32_t (*fn_ANW_getDim)(ANativeWindow*);
typedef int32_t (*fn_ANW_setUsage)(ANativeWindow*, uint64_t);
typedef int32_t (*fn_ANW_setDims)(ANativeWindow*, int32_t, int32_t);
typedef int32_t (*fn_ANW_setFormat)(ANativeWindow*, int32_t);
typedef int32_t (*fn_ANW_setTimestamp)(ANativeWindow*, int64_t);
typedef int32_t (*fn_ANW_dequeue)(ANativeWindow*, ANativeWindowBuffer**, int*);
typedef int32_t (*fn_ANW_queue)(ANativeWindow*, ANativeWindowBuffer*, int);
typedef int32_t (*fn_ANW_cancel)(ANativeWindow*, ANativeWindowBuffer*, int);
typedef AHardwareBuffer* (*fn_ANWB_getAHB)(ANativeWindowBuffer*);
typedef void (*fn_AHB_describe)(const AHardwareBuffer*, acos_ahb_desc_t*);
typedef int (*fn_AHB_lockPlanes)(AHardwareBuffer*, uint64_t, int32_t, const acos_arect_t*, acos_ahb_planes_t*);
typedef int (*fn_AHB_lock)(AHardwareBuffer*, uint64_t, int32_t, const acos_arect_t*, void**);
typedef int (*fn_AHB_unlock)(AHardwareBuffer*, int32_t*);
typedef int (*fn_ANW_perform)(ANativeWindow*, int, ...);

/* Verified against this image's libnativewindow.so and libgui.so (Surface::perform). */
#define ACOS_ANW_PERFORM_OFFSET 0x98
#define ACOS_NATIVE_WINDOW_API_CONNECT 13
#define ACOS_NATIVE_WINDOW_API_DISCONNECT 14
#define ACOS_NATIVE_WINDOW_API_CAMERA 4
#define ACOS_AHB_USAGE_CPU_WRITE_OFTEN 0x30ULL
#define ACOS_FMT_RGBA_8888 0x1u
#define ACOS_FMT_BLOB 0x21u
#define ACOS_FMT_YUV_420_888 0x23u
#define ACOS_CAMERA3_JPEG_BLOB_ID 0x000000FFu
#define ACOS_FRAME_BLOB_MAGIC 0x464D4341u
#define ACOS_FRAME_BLOB_VERSION 1u

typedef struct {
    uint32_t magic;
    uint32_t version;
    uint32_t width;
    uint32_t height;
    uint32_t flags;
    uint32_t rgba_offset;
    uint32_t rgba_size;
    uint32_t yuv_offset;
    uint32_t yuv_size;
    uint32_t jpeg_offset;
    uint32_t jpeg_size;
    uint32_t crc32_yuv;
} acos_cam_frame_blob_t;

static struct {
    int loaded;
    fn_ANW_fromSurface fromSurface;
    fn_ANW_release release;
    fn_ANW_getDim getWidth;
    fn_ANW_getDim getHeight;
    fn_ANW_getDim getFormat;
    fn_ANW_setUsage setUsage;
    fn_ANW_setDims setDims;
    fn_ANW_setFormat setFormat;
    fn_ANW_setTimestamp setTimestamp;
    fn_ANW_dequeue dequeue;
    fn_ANW_queue queue;
    fn_ANW_cancel cancel;
    fn_ANWB_getAHB getAHB;
    fn_AHB_describe describe;
    fn_AHB_lockPlanes lockPlanes;
    fn_AHB_lock lock;
    fn_AHB_unlock unlock;
} g_cam_api;

#define ACOS_MAX_CAM_STREAMS 16
static pthread_mutex_t g_cam_lock = PTHREAD_MUTEX_INITIALIZER;
static struct {
    int active;
    int stream_id;
    ANativeWindow* anw;
    int width;
    int height;
    int format;
} g_cam_streams[ACOS_MAX_CAM_STREAMS];

static int cam_load_api(void) {
    if (g_cam_api.loaded) return 0;
    void* h_android = dlopen("libandroid.so", RTLD_LAZY | RTLD_LOCAL);
    if (!h_android) h_android = dlopen("/system/lib64/libandroid.so", RTLD_LAZY | RTLD_LOCAL);
    void* h_nw = dlopen("libnativewindow.so", RTLD_LAZY | RTLD_LOCAL);
    if (!h_nw) h_nw = dlopen("/system/lib64/libnativewindow.so", RTLD_LAZY | RTLD_LOCAL);
    if (!h_android || !h_nw) {
        LOG("camera-sink: dlopen failed android=%p nw=%p err=%s", h_android, h_nw, dlerror());
        return -ENOSYS;
    }
    g_cam_api.fromSurface = (fn_ANW_fromSurface)dlsym(h_android, "ANativeWindow_fromSurface");
    g_cam_api.release = (fn_ANW_release)dlsym(h_nw, "ANativeWindow_release");
    g_cam_api.getWidth = (fn_ANW_getDim)dlsym(h_nw, "ANativeWindow_getWidth");
    g_cam_api.getHeight = (fn_ANW_getDim)dlsym(h_nw, "ANativeWindow_getHeight");
    g_cam_api.getFormat = (fn_ANW_getDim)dlsym(h_nw, "ANativeWindow_getFormat");
    g_cam_api.setUsage = (fn_ANW_setUsage)dlsym(h_nw, "ANativeWindow_setUsage");
    g_cam_api.setDims = (fn_ANW_setDims)dlsym(h_nw, "ANativeWindow_setBuffersDimensions");
    g_cam_api.setFormat = (fn_ANW_setFormat)dlsym(h_nw, "ANativeWindow_setBuffersFormat");
    g_cam_api.setTimestamp = (fn_ANW_setTimestamp)dlsym(h_nw, "ANativeWindow_setBuffersTimestamp");
    g_cam_api.dequeue = (fn_ANW_dequeue)dlsym(h_nw, "ANativeWindow_dequeueBuffer");
    g_cam_api.queue = (fn_ANW_queue)dlsym(h_nw, "ANativeWindow_queueBuffer");
    g_cam_api.cancel = (fn_ANW_cancel)dlsym(h_nw, "ANativeWindow_cancelBuffer");
    g_cam_api.getAHB = (fn_ANWB_getAHB)dlsym(h_nw, "ANativeWindowBuffer_getHardwareBuffer");
    g_cam_api.describe = (fn_AHB_describe)dlsym(h_nw, "AHardwareBuffer_describe");
    g_cam_api.lockPlanes = (fn_AHB_lockPlanes)dlsym(h_nw, "AHardwareBuffer_lockPlanes");
    g_cam_api.lock = (fn_AHB_lock)dlsym(h_nw, "AHardwareBuffer_lock");
    g_cam_api.unlock = (fn_AHB_unlock)dlsym(h_nw, "AHardwareBuffer_unlock");
    if (!g_cam_api.fromSurface || !g_cam_api.release || !g_cam_api.dequeue || !g_cam_api.queue ||
        !g_cam_api.getAHB || !g_cam_api.describe || !g_cam_api.lockPlanes || !g_cam_api.unlock) {
        LOG("camera-sink: missing ANativeWindow/AHardwareBuffer symbol(s)");
        return -ENOSYS;
    }
    g_cam_api.loaded = 1;
    return 0;
}

static int cam_anw_connect(ANativeWindow* anw) {
    fn_ANW_perform perform = *(fn_ANW_perform*)((char*)anw + ACOS_ANW_PERFORM_OFFSET);
    if (!perform) return -EINVAL;
    return perform(anw, ACOS_NATIVE_WINDOW_API_CONNECT, ACOS_NATIVE_WINDOW_API_CAMERA);
}

static void cam_anw_disconnect(ANativeWindow* anw) {
    fn_ANW_perform perform = *(fn_ANW_perform*)((char*)anw + ACOS_ANW_PERFORM_OFFSET);
    if (perform) {
        (void)perform(anw, ACOS_NATIVE_WINDOW_API_DISCONNECT, ACOS_NATIVE_WINDOW_API_CAMERA, 0);
    }
}

/* Deterministic ACOS_PATTERN_COLORBARS_V1 test pattern (YUV_420_888):
 *   - Rows 0 .. 3*H/4 - 1: 8 vertical BT.601 color bars
 *       0=White(235,128,128), 1=Yellow(210,16,146), 2=Cyan(170,166,16),
 *       3=Green(145,54,34),   4=Magenta(106,202,222), 5=Red(81,90,240),
 *       6=Blue(41,240,110),   7=Black(16,128,128)
 *   - Rows 3*H/4 .. H - 1: horizontal luma gradient Y = x*255/(W-1), U=128, V=128
 *   - Frame tag at row 0, x=0..3:
 *       Y(0,0)=0xA0|(fn&0xF), Y(1,0)=(fn>>4)&0xFF, Y(2,0)=(fn>>12)&0xFF, Y(3,0)=0x5A
 * Keep in exact sync with Rust reference in crates/acos-services/src/modules/camera.rs. */
static const uint8_t kBarY[8] = {235, 210, 170, 145, 106, 81, 41, 16};
static const uint8_t kBarU[8] = {128, 16, 166, 54, 202, 90, 240, 128};
static const uint8_t kBarV[8] = {128, 146, 16, 34, 222, 240, 110, 128};

static void fill_yuv420_colorbars(const acos_ahb_planes_t* p,
                                  uint32_t w, uint32_t h, uint64_t frame_number) {
    uint8_t* y_base = (uint8_t*)p->planes[0].data;
    uint32_t y_row = p->planes[0].rowStride;
    uint32_t y_px = p->planes[0].pixelStride ? p->planes[0].pixelStride : 1u;

    uint8_t* u_base = (uint8_t*)p->planes[1].data;
    uint32_t u_row = p->planes[1].rowStride;
    uint32_t u_px = p->planes[1].pixelStride ? p->planes[1].pixelStride : 1u;

    uint8_t* v_base = (uint8_t*)p->planes[2].data;
    uint32_t v_row = p->planes[2].rowStride;
    uint32_t v_px = p->planes[2].pixelStride ? p->planes[2].pixelStride : 1u;

    uint32_t ramp_y0 = (h * 3u) / 4u;
    for (uint32_t y = 0; y < h; y++) {
        uint8_t* row = y_base + (size_t)y * y_row;
        for (uint32_t x = 0; x < w; x++) {
            uint8_t val;
            if (y == 0 && x < 4u) {
                if (x == 0u) val = (uint8_t)(0xA0u | (frame_number & 0x0Fu));
                else if (x == 1u) val = (uint8_t)((frame_number >> 4) & 0xFFu);
                else if (x == 2u) val = (uint8_t)((frame_number >> 12) & 0xFFu);
                else val = 0x5Au;
            } else if (y >= ramp_y0) {
                val = (w > 1u) ? (uint8_t)((x * 255u) / (w - 1u)) : 128u;
            } else {
                uint32_t bar = (x * 8u) / w;
                if (bar > 7u) bar = 7u;
                val = kBarY[bar];
            }
            row[(size_t)x * y_px] = val;
        }
    }

    uint32_t cw = (w + 1u) / 2u;
    uint32_t ch = (h + 1u) / 2u;
    uint32_t ramp_cy0 = ramp_y0 / 2u;
    for (uint32_t cy = 0; cy < ch; cy++) {
        uint8_t* ur = u_base + (size_t)cy * u_row;
        uint8_t* vr = v_base + (size_t)cy * v_row;
        for (uint32_t cx = 0; cx < cw; cx++) {
            uint8_t uval, vval;
            if (cy >= ramp_cy0) {
                uval = 128u;
                vval = 128u;
            } else {
                uint32_t x = cx * 2u;
                uint32_t bar = (x * 8u) / w;
                if (bar > 7u) bar = 7u;
                uval = kBarU[bar];
                vval = kBarV[bar];
            }
            ur[(size_t)cx * u_px] = uval;
            vr[(size_t)cx * v_px] = vval;
        }
    }
}

JNIEXPORT jint JNICALL Java_dev_acos_art_RenderNat_cameraSinkRegister(
        JNIEnv* env, jclass klass, jint streamId, jobject surface, jint width, jint height) {
    (void)klass;
    if (!surface) return -EINVAL;
    pthread_mutex_lock(&g_cam_lock);
    int rc = cam_load_api();
    if (rc != 0) {
        pthread_mutex_unlock(&g_cam_lock);
        return rc;
    }
    ANativeWindow* anw = g_cam_api.fromSurface(env, surface);
    if (!anw) {
        pthread_mutex_unlock(&g_cam_lock);
        LOG("camera-sink: ANativeWindow_fromSurface returned NULL for stream %d", (int)streamId);
        return -EINVAL;
    }
    int conn_rc = cam_anw_connect(anw);
    if (conn_rc != 0) {
        LOG("camera-sink: stream %d connect(CAMERA) -> %d", (int)streamId, conn_rc);
    }
    if (g_cam_api.setUsage) {
        (void)g_cam_api.setUsage(anw, ACOS_AHB_USAGE_CPU_WRITE_OFTEN);
    }
    int w = g_cam_api.getWidth ? g_cam_api.getWidth(anw) : (int)width;
    int h = g_cam_api.getHeight ? g_cam_api.getHeight(anw) : (int)height;
    int fmt = g_cam_api.getFormat ? g_cam_api.getFormat(anw) : (int)ACOS_FMT_YUV_420_888;
    /* HAL_PIXEL_FORMAT_BLOB (0x21) uses (maxJpegSize, 1) on the BufferQueue;
     * never overwrite it with (width, height), or ImageReader.getLockedImageInfo asserts height == 1. */
    if (fmt != (int)ACOS_FMT_BLOB && width > 0 && height > 0 && (w != width || h != height) && g_cam_api.setDims) {
        (void)g_cam_api.setDims(anw, width, height);
        w = width;
        h = height;
    }
    /* SurfaceView / SurfaceTexture preview streams default to IMPLEMENTATION_DEFINED/YV12;
     * force RGBA_8888 so ACOS surface_client::capture_composite can composite the BLAST layer. */
    if (fmt != (int)ACOS_FMT_BLOB && fmt != (int)ACOS_FMT_YUV_420_888 && g_cam_api.setFormat) {
        (void)g_cam_api.setFormat(anw, (int32_t)ACOS_FMT_RGBA_8888);
        fmt = (int)ACOS_FMT_RGBA_8888;
    }
    int slot = -1;
    for (int i = 0; i < ACOS_MAX_CAM_STREAMS; i++) {
        if (g_cam_streams[i].active && g_cam_streams[i].stream_id == streamId) {
            cam_anw_disconnect(g_cam_streams[i].anw);
            g_cam_api.release(g_cam_streams[i].anw);
            g_cam_streams[i].active = 0;
            slot = i;
            break;
        }
    }
    if (slot < 0) {
        for (int i = 0; i < ACOS_MAX_CAM_STREAMS; i++) {
            if (!g_cam_streams[i].active) { slot = i; break; }
        }
    }
    if (slot < 0) {
        cam_anw_disconnect(anw);
        g_cam_api.release(anw);
        pthread_mutex_unlock(&g_cam_lock);
        return -ENOMEM;
    }
    g_cam_streams[slot].active = 1;
    g_cam_streams[slot].stream_id = (int)streamId;
    g_cam_streams[slot].anw = anw;
    g_cam_streams[slot].width = w;
    g_cam_streams[slot].height = h;
    g_cam_streams[slot].format = fmt;
    pthread_mutex_unlock(&g_cam_lock);
    LOG("camera-sink: registered stream %d (%dx%d fmt=0x%x)", (int)streamId, w, h, (unsigned)fmt);
    return 0;
}

JNIEXPORT jint JNICALL Java_dev_acos_art_RenderNat_cameraSinkDeliver(
        JNIEnv* env, jclass klass, jint streamId, jint width, jint height,
        jlong frameNumber, jlong timestampNs, jint patternId) {
    (void)env; (void)klass; (void)patternId;
    pthread_mutex_lock(&g_cam_lock);
    if (!g_cam_api.loaded) {
        pthread_mutex_unlock(&g_cam_lock);
        return -ENOSYS;
    }
    ANativeWindow* anw = NULL;
    int req_w = (int)width;
    int req_h = (int)height;
    for (int i = 0; i < ACOS_MAX_CAM_STREAMS; i++) {
        if (g_cam_streams[i].active && g_cam_streams[i].stream_id == streamId) {
            anw = g_cam_streams[i].anw;
            if (req_w <= 0) req_w = g_cam_streams[i].width;
            if (req_h <= 0) req_h = g_cam_streams[i].height;
            break;
        }
    }
    if (!anw) {
        pthread_mutex_unlock(&g_cam_lock);
        LOG("camera-sink: deliver unknown stream %d", (int)streamId);
        return -ENOENT;
    }

    if (g_cam_api.setTimestamp) {
        (void)g_cam_api.setTimestamp(anw, (int64_t)timestampNs);
    }
    ANativeWindowBuffer* buf = NULL;
    int fence_fd = -1;
    int rc = g_cam_api.dequeue(anw, &buf, &fence_fd);
    if (fence_fd >= 0) close(fence_fd);
    if (rc != 0 || !buf) {
        pthread_mutex_unlock(&g_cam_lock);
        LOG("camera-sink: dequeueBuffer(stream=%d) failed rc=%d", (int)streamId, rc);
        return rc ? rc : -EIO;
    }

    AHardwareBuffer* ahb = g_cam_api.getAHB(buf);
    if (!ahb) {
        if (g_cam_api.cancel) g_cam_api.cancel(anw, buf, -1);
        pthread_mutex_unlock(&g_cam_lock);
        return -EINVAL;
    }
    acos_ahb_desc_t desc;
    memset(&desc, 0, sizeof(desc));
    g_cam_api.describe(ahb, &desc);
    uint32_t w = desc.width ? desc.width : (uint32_t)req_w;
    uint32_t h = desc.height ? desc.height : (uint32_t)req_h;

    if (desc.format == ACOS_FMT_YUV_420_888) {
        acos_ahb_planes_t planes;
        memset(&planes, 0, sizeof(planes));
        rc = g_cam_api.lockPlanes(ahb, ACOS_AHB_USAGE_CPU_WRITE_OFTEN, -1, NULL, &planes);
        if (rc != 0 || planes.planeCount < 3u) {
            LOG("camera-sink: lockPlanes failed rc=%d count=%u", rc, planes.planeCount);
            if (rc == 0) g_cam_api.unlock(ahb, NULL);
            if (g_cam_api.cancel) g_cam_api.cancel(anw, buf, -1);
            pthread_mutex_unlock(&g_cam_lock);
            return rc ? rc : -EINVAL;
        }
        fill_yuv420_colorbars(&planes, w, h, (uint64_t)frameNumber);
        g_cam_api.unlock(ahb, NULL);
    } else if (g_cam_api.lock) {
        void* vaddr = NULL;
        rc = g_cam_api.lock(ahb, ACOS_AHB_USAGE_CPU_WRITE_OFTEN, -1, NULL, &vaddr);
        if (rc != 0 || !vaddr) {
            if (g_cam_api.cancel) g_cam_api.cancel(anw, buf, -1);
            pthread_mutex_unlock(&g_cam_lock);
            return rc ? rc : -EIO;
        }
        if (desc.format == ACOS_FMT_BLOB) {
            uint8_t* dst = (uint8_t*)vaddr;
            size_t cap = (size_t)w * (size_t)(h ? h : 1u);
            if (cap >= 24u) {
                memcpy(dst, "ACOSBLOB", 8);
                uint64_t fn = (uint64_t)frameNumber;
                uint64_t ts = (uint64_t)timestampNs;
                memcpy(dst + 8, &fn, 8);
                memcpy(dst + 16, &ts, 8);
            }
        } else {
            /* Little-endian RGBA_8888: byte0=R, byte1=G, byte2=B, byte3=A -> 0xAABBGGRR. */
            static const uint32_t kBarRgba[8] = {
                0xFFFFFFFFu, /* 0: White   (255,255,255) */
                0xFF00FFFFu, /* 1: Yellow  (255,255,0)   */
                0xFFFFFF00u, /* 2: Cyan    (0,255,255)   */
                0xFF00FF00u, /* 3: Green   (0,255,0)     */
                0xFFFF00FFu, /* 4: Magenta (255,0,255)   */
                0xFF0000FFu, /* 5: Red     (255,0,0)     */
                0xFFFF0000u, /* 6: Blue    (0,0,255)     */
                0xFF000000u, /* 7: Black   (0,0,0)       */
            };
            uint32_t stride = desc.stride ? desc.stride : w;
            uint32_t ramp_y0 = (h * 3u) / 4u;
            uint32_t* px = (uint32_t*)vaddr;
            for (uint32_t y = 0; y < h; y++) {
                for (uint32_t x = 0; x < w; x++) {
                    uint32_t color;
                    if (y == 0 && x < 4u) {
                        uint8_t tag = 0;
                        if (x == 0u) tag = (uint8_t)(0xA0u | (frameNumber & 0x0Full));
                        else if (x == 1u) tag = (uint8_t)((frameNumber >> 4) & 0xFFull);
                        else if (x == 2u) tag = (uint8_t)((frameNumber >> 12) & 0xFFull);
                        else tag = 0x5Au;
                        color = 0xFF000000u | ((uint32_t)tag << 16) | ((uint32_t)tag << 8) | tag;
                    } else if (y >= ramp_y0) {
                        uint8_t l = (w > 1u) ? (uint8_t)((x * 255u) / (w - 1u)) : 128u;
                        color = 0xFF000000u | ((uint32_t)l << 16) | ((uint32_t)l << 8) | l;
                    } else {
                        uint32_t bar = (x * 8u) / w;
                        if (bar > 7u) bar = 7u;
                        color = kBarRgba[bar];
                    }
                    px[(size_t)y * stride + x] = color;
                }
            }
        }
        g_cam_api.unlock(ahb, NULL);
    }

    rc = g_cam_api.queue(anw, buf, -1);
    pthread_mutex_unlock(&g_cam_lock);
    if (rc != 0) {
        LOG("camera-sink: queueBuffer(stream=%d) failed rc=%d", (int)streamId, rc);
    }
    return rc;
}

JNIEXPORT jint JNICALL Java_dev_acos_art_RenderNat_cameraSinkDeliverFd(
        JNIEnv* env, jclass klass, jint streamId, jint width, jint height,
        jlong frameNumber, jlong timestampNs, jint patternId, jint fd) {
    (void)env; (void)klass; (void)frameNumber; (void)patternId;
    if (fd < 0) return -EINVAL;
    struct stat st;
    if (fstat((int)fd, &st) != 0 || st.st_size < (off_t)sizeof(acos_cam_frame_blob_t)) {
        return -EINVAL;
    }
    size_t map_len = (size_t)st.st_size;
    void* mapped = mmap(NULL, map_len, PROT_READ, MAP_SHARED, (int)fd, 0);
    if (mapped == MAP_FAILED) {
        return -errno;
    }
    const acos_cam_frame_blob_t* hdr = (const acos_cam_frame_blob_t*)mapped;
    if (hdr->magic != ACOS_FRAME_BLOB_MAGIC || hdr->version != ACOS_FRAME_BLOB_VERSION ||
        hdr->width == 0u || hdr->height == 0u) {
        munmap(mapped, map_len);
        return -EINVAL;
    }
    if ((size_t)hdr->rgba_offset + (size_t)hdr->rgba_size > map_len ||
        (size_t)hdr->yuv_offset + (size_t)hdr->yuv_size > map_len ||
        (size_t)hdr->jpeg_offset + (size_t)hdr->jpeg_size > map_len) {
        munmap(mapped, map_len);
        return -EINVAL;
    }

    pthread_mutex_lock(&g_cam_lock);
    if (!g_cam_api.loaded) {
        pthread_mutex_unlock(&g_cam_lock);
        munmap(mapped, map_len);
        return -ENOSYS;
    }
    ANativeWindow* anw = NULL;
    int req_w = (int)width;
    int req_h = (int)height;
    for (int i = 0; i < ACOS_MAX_CAM_STREAMS; i++) {
        if (g_cam_streams[i].active && g_cam_streams[i].stream_id == streamId) {
            anw = g_cam_streams[i].anw;
            if (req_w <= 0) req_w = g_cam_streams[i].width;
            if (req_h <= 0) req_h = g_cam_streams[i].height;
            break;
        }
    }
    if (!anw) {
        pthread_mutex_unlock(&g_cam_lock);
        munmap(mapped, map_len);
        LOG("camera-sink: deliverFd unknown stream %d", (int)streamId);
        return -ENOENT;
    }
    if (g_cam_api.setTimestamp) {
        (void)g_cam_api.setTimestamp(anw, (int64_t)timestampNs);
    }
    ANativeWindowBuffer* buf = NULL;
    int fence_fd = -1;
    int rc = g_cam_api.dequeue(anw, &buf, &fence_fd);
    if (fence_fd >= 0) close(fence_fd);
    if (rc != 0 || !buf) {
        pthread_mutex_unlock(&g_cam_lock);
        munmap(mapped, map_len);
        LOG("camera-sink: deliverFd dequeueBuffer(stream=%d) failed rc=%d", (int)streamId, rc);
        return rc ? rc : -EIO;
    }

    AHardwareBuffer* ahb = g_cam_api.getAHB(buf);
    if (!ahb) {
        if (g_cam_api.cancel) g_cam_api.cancel(anw, buf, -1);
        pthread_mutex_unlock(&g_cam_lock);
        munmap(mapped, map_len);
        return -EINVAL;
    }
    acos_ahb_desc_t desc;
    memset(&desc, 0, sizeof(desc));
    g_cam_api.describe(ahb, &desc);
    uint32_t w = desc.width ? desc.width : (uint32_t)req_w;
    uint32_t h = desc.height ? desc.height : (uint32_t)req_h;
    const uint8_t* blob_bytes = (const uint8_t*)mapped;

    if (desc.format == ACOS_FMT_YUV_420_888) {
        uint32_t sw = hdr->width;
        uint32_t sh = hdr->height;
        uint32_t scw = (sw + 1u) / 2u;
        uint32_t sch = (sh + 1u) / 2u;
        size_t need_yuv = (size_t)sw * (size_t)sh + 2u * ((size_t)scw * (size_t)sch);
        if (hdr->yuv_size < need_yuv) {
            if (g_cam_api.cancel) g_cam_api.cancel(anw, buf, -1);
            pthread_mutex_unlock(&g_cam_lock);
            munmap(mapped, map_len);
            return -EINVAL;
        }
        acos_ahb_planes_t planes;
        memset(&planes, 0, sizeof(planes));
        rc = g_cam_api.lockPlanes(ahb, ACOS_AHB_USAGE_CPU_WRITE_OFTEN, -1, NULL, &planes);
        if (rc != 0 || planes.planeCount < 3u) {
            if (rc == 0) g_cam_api.unlock(ahb, NULL);
            if (g_cam_api.cancel) g_cam_api.cancel(anw, buf, -1);
            pthread_mutex_unlock(&g_cam_lock);
            munmap(mapped, map_len);
            return rc ? rc : -EINVAL;
        }
        uint32_t cw_dst = (w < sw) ? w : sw;
        uint32_t ch_dst = (h < sh) ? h : sh;
        const uint8_t* src_y = blob_bytes + hdr->yuv_offset;
        const uint8_t* src_u = src_y + (size_t)sw * (size_t)sh;
        const uint8_t* src_v = src_u + (size_t)scw * (size_t)sch;

        uint8_t* y_base = (uint8_t*)planes.planes[0].data;
        uint32_t y_px = planes.planes[0].pixelStride ? planes.planes[0].pixelStride : 1u;
        uint32_t y_row = planes.planes[0].rowStride ? planes.planes[0].rowStride : w;
        for (uint32_t y = 0; y < ch_dst; y++) {
            uint8_t* dst_r = y_base + (size_t)y * y_row;
            const uint8_t* src_r = src_y + (size_t)y * sw;
            if (y_px == 1u) {
                memcpy(dst_r, src_r, cw_dst);
            } else {
                for (uint32_t x = 0; x < cw_dst; x++) {
                    dst_r[(size_t)x * y_px] = src_r[x];
                }
            }
        }

        uint32_t ccw = ((w + 1u) / 2u < scw) ? (w + 1u) / 2u : scw;
        uint32_t cch = ((h + 1u) / 2u < sch) ? (h + 1u) / 2u : sch;
        uint8_t* u_base = (uint8_t*)planes.planes[1].data;
        uint32_t u_px = planes.planes[1].pixelStride ? planes.planes[1].pixelStride : 1u;
        uint32_t u_row = planes.planes[1].rowStride ? planes.planes[1].rowStride : ((w + 1u) / 2u);
        uint8_t* v_base = (uint8_t*)planes.planes[2].data;
        uint32_t v_px = planes.planes[2].pixelStride ? planes.planes[2].pixelStride : 1u;
        uint32_t v_row = planes.planes[2].rowStride ? planes.planes[2].rowStride : ((w + 1u) / 2u);
        for (uint32_t cy = 0; cy < cch; cy++) {
            uint8_t* du = u_base + (size_t)cy * u_row;
            uint8_t* dv = v_base + (size_t)cy * v_row;
            const uint8_t* su = src_u + (size_t)cy * scw;
            const uint8_t* sv = src_v + (size_t)cy * scw;
            if (u_px == 1u && v_px == 1u) {
                memcpy(du, su, ccw);
                memcpy(dv, sv, ccw);
            } else {
                for (uint32_t cx = 0; cx < ccw; cx++) {
                    du[(size_t)cx * u_px] = su[cx];
                    dv[(size_t)cx * v_px] = sv[cx];
                }
            }
        }
        g_cam_api.unlock(ahb, NULL);
    } else if (g_cam_api.lock) {
        void* vaddr = NULL;
        rc = g_cam_api.lock(ahb, ACOS_AHB_USAGE_CPU_WRITE_OFTEN, -1, NULL, &vaddr);
        if (rc != 0 || !vaddr) {
            if (g_cam_api.cancel) g_cam_api.cancel(anw, buf, -1);
            pthread_mutex_unlock(&g_cam_lock);
            munmap(mapped, map_len);
            return rc ? rc : -EIO;
        }
        if (desc.format == ACOS_FMT_BLOB) {
            uint8_t* dst = (uint8_t*)vaddr;
            size_t cap = (size_t)w * (size_t)(h ? h : 1u);
            if (hdr->jpeg_size == 0u || (size_t)hdr->jpeg_size + 8u > cap) {
                g_cam_api.unlock(ahb, NULL);
                if (g_cam_api.cancel) g_cam_api.cancel(anw, buf, -1);
                pthread_mutex_unlock(&g_cam_lock);
                munmap(mapped, map_len);
                LOG("camera-sink: BLOB capacity %zu too small for jpeg %u", cap, hdr->jpeg_size);
                return -ENOSPC;
            }
            memcpy(dst, blob_bytes + hdr->jpeg_offset, hdr->jpeg_size);
            /* camera3_jpeg_blob trailer at end of buffer: { u16 jpeg_blob_id=0x00FF, u16 pad=0, u32 jpeg_size }. */
            uint32_t trailer[2];
            trailer[0] = ACOS_CAMERA3_JPEG_BLOB_ID;
            trailer[1] = hdr->jpeg_size;
            memcpy(dst + cap - 8u, trailer, 8u);
        } else {
            uint32_t stride = desc.stride ? desc.stride : w;
            uint32_t cw_dst = (w < hdr->width) ? w : hdr->width;
            uint32_t ch_dst = (h < hdr->height) ? h : hdr->height;
            if (hdr->rgba_size >= (size_t)hdr->width * (size_t)hdr->height * 4u) {
                const uint8_t* src_rgba = blob_bytes + hdr->rgba_offset;
                uint8_t* dst = (uint8_t*)vaddr;
                for (uint32_t y = 0; y < ch_dst; y++) {
                    memcpy(dst + (size_t)y * stride * 4u,
                           src_rgba + (size_t)y * hdr->width * 4u,
                           (size_t)cw_dst * 4u);
                }
            }
        }
        g_cam_api.unlock(ahb, NULL);
    }

    rc = g_cam_api.queue(anw, buf, -1);
    pthread_mutex_unlock(&g_cam_lock);
    munmap(mapped, map_len);
    if (rc != 0) {
        LOG("camera-sink: deliverFd queueBuffer(stream=%d) failed rc=%d", (int)streamId, rc);
    }
    return rc;
}

JNIEXPORT jint JNICALL Java_dev_acos_art_RenderNat_cameraSinkRelease(
        JNIEnv* env, jclass klass, jint streamId) {
    (void)env; (void)klass;
    pthread_mutex_lock(&g_cam_lock);
    int released = 0;
    for (int i = 0; i < ACOS_MAX_CAM_STREAMS; i++) {
        if (g_cam_streams[i].active && (streamId < 0 || g_cam_streams[i].stream_id == streamId)) {
            if (g_cam_streams[i].anw) {
                cam_anw_disconnect(g_cam_streams[i].anw);
                if (g_cam_api.release) g_cam_api.release(g_cam_streams[i].anw);
            }
            g_cam_streams[i].active = 0;
            g_cam_streams[i].anw = NULL;
            released++;
        }
    }
    pthread_mutex_unlock(&g_cam_lock);
    LOG("camera-sink: released stream=%d count=%d", (int)streamId, released);
    return released;
}
