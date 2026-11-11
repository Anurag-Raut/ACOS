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
