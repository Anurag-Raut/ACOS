// interpose.c -- E2D substrate shim, LD_PRELOAD-ed into the bionic process.
//
// Two independent jobs, both of which are "what the real Android process does,
// minus the parts of Android we do not have":
//
//  1. LOAD libandroid_runtime.so IN THE DEFAULT NAMESPACE.
//     System.load("/system/lib64/libandroid_runtime.so") goes through
//     libnativeloader, which builds a fresh *classloader namespace* ("clns-1")
//     whose permitted paths are just /data:/mnt/expand.  Every system library
//     is then "not accessible for the namespace clns-1" (measured: first
//     libdl_android.so from /apex/com.android.runtime/lib64/bionic, then the
//     rest).  On device the framework natives are registered from
//     app_process/zygote in the *default* namespace, so we do the same: a
//     preload constructor dlopens it RTLD_GLOBAL before any Java runs.
//
//  3. BRIDGE Java -> the framework's exported JNI registration entry points
//     (android::register_android_os_Parcel etc.).
//
//  2. FAKE /dev/binder WITH A MEMFD  (E2D_BINDER_FAKE=1).
//     ProcessState::open_driver() wants: open(path, O_RDWR|O_CLOEXEC),
//     ioctl(BINDER_VERSION) == 8, ioctl(BINDER_SET_MAX_THREADS) == 0, and
//     mmap(fd) of BINDER_VM_SIZE.  A memfd satisfies open+mmap; we answer the
//     two ioctls and log every other one (BINDER_WRITE_READ = where a real
//     transport would have to take over).
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
#define LOG(...)                       \
    do {                               \
        dprintf(2, "E2D[preload]: ");  \
        dprintf(2, __VA_ARGS__);       \
        dprintf(2, "\n");              \
    } while (0)

static const char* kBinderPaths[] = {
        "/dev/binder", "/dev/hwbinder", "/dev/vndbinder", "/dev/binderfs/binder", NULL};

static char g_fake[8][64];
static int g_fake_n;

// ------------------------------------------------------ (0) property service
// There is no init/property service, so every __system_property_get() fails and
// returns "".  android.os.Build.<clinit> then does SUPPORTED_ABIS[0] on an empty
// array -> ArrayIndexOutOfBoundsException -> ExceptionInInitializerError, which
// kills *any* class whose clinit touches Build.VERSION (measured:
// ResourcesImpl.updateConfiguration, ActivityThread.<clinit>).  Serve the ~10
// properties 14 needs from a table; everything else falls through to bionic.
static const struct { const char* key; const char* value; } kProps[] = {
        {"ro.product.cpu.abilist", "x86_64,x86"},
        {"ro.product.cpu.abilist64", "x86_64"},
        {"ro.product.cpu.abilist32", "x86"},
        {"ro.product.cpu.abi", "x86_64"},
        {"ro.build.version.sdk", "34"},
        {"ro.build.version.release", "14"},
        {"ro.build.version.release_or_codename", "14"},
        {"ro.build.version.codename", "REL"},
        {"ro.build.version.all_codenames", "REL"},
        {"ro.build.version.preview_sdk", "0"},
        {"ro.build.version.min_supported_target_sdk", "23"},
        {"ro.build.version.security_patch", "2024-07-05"},
        {"ro.build.type", "userdebug"},
        {"ro.build.tags", "release-keys"},
        {"ro.build.characteristics", "nosdcard"},
        {"ro.debuggable", "1"},
        {"ro.product.device", "acos"},
        {"ro.product.model", "ACOS"},
        {"ro.product.brand", "acos"},
        {"ro.product.name", "acos"},
        {"ro.product.manufacturer", "acos"},
        {"ro.zygote", "zygote64"},
        {"ro.hardware", "ranchu"},
        {"ro.opengles.version", "196610"},
        {"debug.trace.atrace", "0"},
        {"persist.sys.locale", "en-US"},
        {"ro.kernel.qemu", "0"},
        {"dalvik.vm.dex2oat-filter", "verify"},
        {NULL, NULL},
};

int __system_property_get(const char* key, char* value) {
    for (int i = 0; kProps[i].key; i++) {
        if (strcmp(key, kProps[i].key) == 0) {
            size_t n = strlen(kProps[i].value);
            memcpy(value, kProps[i].value, n + 1);
            return (int)n;
        }
    }
    if (getenv("E2D_PROP_TRACE")) LOG("prop %s -> (unset, fall through)", key);
    static int (*real)(const char*, char*);
    if (!real) real = dlsym(RTLD_NEXT, "__system_property_get");
    if (real) return real(key, value);
    value[0] = 0;
    return 0;
}

// API 34 libbase reads properties through __system_property_find() +
// __system_property_read_callback(), not __system_property_get(), so the table
// above has to be reachable that way too.  Our own prop_info pointers are
// opaque to callers (they only hand them back to us / bionic).
static struct { char name[128]; char value[128]; } g_fake_prop[64];
static int g_fake_prop_n;

static void* fake_prop_info(const char* key, const char* value) {
    if (g_fake_prop_n >= 64) return NULL;
    snprintf(g_fake_prop[g_fake_prop_n].name, sizeof(g_fake_prop[0].name), "%s", key);
    snprintf(g_fake_prop[g_fake_prop_n].value, sizeof(g_fake_prop[0].value), "%s", value);
    return &g_fake_prop[g_fake_prop_n++];
}

static int is_fake_prop(const void* pi) {
    return (const char*)pi >= (const char*)g_fake_prop &&
           (const char*)pi < (const char*)(g_fake_prop + 64);
}

const void* __system_property_find(const char* key) {
    for (int i = 0; kProps[i].key; i++)
        if (strcmp(key, kProps[i].key) == 0) return fake_prop_info(kProps[i].key, kProps[i].value);
    static const void* (*real)(const char*);
    if (!real) real = dlsym(RTLD_NEXT, "__system_property_find");
    return real ? real(key) : NULL;
}

void __system_property_read_callback(const void* pi,
                                     void (*callback)(void*, const char*, const char*, unsigned),
                                     void* cookie) {
    if (is_fake_prop(pi)) {
        const char* base = (const char*)pi;
        callback(cookie, base, base + 128, 1);
        return;
    }
    static void (*real)(const void*, void (*)(void*, const char*, const char*, unsigned), void*);
    if (!real) real = dlsym(RTLD_NEXT, "__system_property_read_callback");
    if (real) real(pi, callback, cookie);
}

// ------------------------------------------------------------------ (1) load

__attribute__((constructor)) static void e2d_load_runtime(void) {
    const char* list = getenv("E2D_LOAD");
    if (list == NULL) list = "libandroid_runtime.so";
    char buf[4096];
    snprintf(buf, sizeof(buf), "%s", list);
    for (char* save = NULL, *tok = strtok_r(buf, ":", &save); tok;
         tok = strtok_r(NULL, ":", &save)) {
        void* h = dlopen(tok, RTLD_NOW | RTLD_GLOBAL);
        if (h == NULL) {
            LOG("dlopen(%s) FAILED: %s", tok, dlerror());
        } else {
            LOG("dlopen(%s) ok @%p (default namespace, RTLD_GLOBAL)", tok, h);
        }
    }
}

// -------------------------------------------------- (2) fake /dev/binder{,fs}

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
    const char* on = getenv("E2D_BINDER_FAKE");
    if (on == NULL || strcmp(on, "1") != 0) return -1;
    int fd = (int)syscall(__NR_memfd_create, "e2d-binder", MFD_CLOEXEC);
    if (fd < 0) {
        LOG("memfd_create failed: %s", strerror(errno));
        return -1;
    }
    if (ftruncate(fd, BINDER_VM_SIZE) != 0) LOG("ftruncate failed: %s", strerror(errno));
    if (g_fake_n < 8) {
        snprintf(g_fake[g_fake_n], sizeof(g_fake[0]), "%s=%d", path, fd);
        g_fake_n++;
    }
    LOG("open(%s) -> memfd %d (%d bytes, stands in for the binder driver)", path, fd,
        BINDER_VM_SIZE);
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

int openat(int dirfd, const char* path, int flags, ...) {
    static openat_fn real;
    if (!real) real = (openat_fn)dlsym(RTLD_NEXT, "openat");
    va_list ap;
    va_start(ap, flags);
    mode_t mode = (mode_t)va_arg(ap, int);
    va_end(ap);
    return real(dirfd, path, flags, mode);
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
                if (arg) *(int*)arg = 8;  // BINDER_CURRENT_PROTOCOL_VERSION
                LOG("ioctl(fd=%d, BINDER_VERSION) -> 8", fd);
                return 0;
            case 0x40046210u:  // BINDER_ENABLE_ONEWAY_SPAM_DETECTION
                LOG("ioctl(fd=%d, BINDER_ENABLE_ONEWAY_SPAM_DETECTION=%u) -> 0", fd,
                    arg ? *(unsigned int*)arg : 0u);
                return 0;
            case 0x40046208u:  // BINDER_SET_INHERIT_FIFO_PRIO
                LOG("ioctl(fd=%d, BINDER_SET_INHERIT_FIFO_PRIO) -> 0", fd);
                return 0;
            case 0x40046205u:  // BINDER_SET_MAX_THREADS
                LOG("ioctl(fd=%d, BINDER_SET_MAX_THREADS=%u) -> 0", fd,
                    arg ? *(unsigned int*)arg : 0u);
                return 0;
            default:
                LOG("ioctl(fd=%d, req=0x%x) -> ENOTTY (no transport yet)", fd, (unsigned)request);
                errno = ENOTTY;
                return -1;
        }
    }
    return ((ioctl_fn)real)(fd, (unsigned long)request, arg);
}

// --------------------------------------------------------------------- (3) JNI
// Reaching libandroid_runtime.so's exported register_* entry points.
// This code has to live in the *same* object as the LD_PRELOAD constructor:
// bionic symbol lookup is namespace-scoped, and a library in the app
// classloader namespace (clns-1) cannot see a library loaded in the default
// namespace even with RTLD_GLOBAL (measured: dlsym(RTLD_DEFAULT) -> NULL).

// android::register_android_os_Parcel(_JNIEnv*)
//   -> _ZN7android26register_android_os_ParcelEP7_JNIEnv
// Most are in namespace android (_ZN7android...E); a few (register_android_os_Binder,
// register_android_os_Process) are at global scope (_Z...).  Try both.
static void mangle_android_register(const char* plain, char* out, size_t n) {
    snprintf(out, n, "_ZN7android%zu%sEP7_JNIEnv", strlen(plain), plain);
}

static void mangle_global_register(const char* plain, char* out, size_t n) {
    snprintf(out, n, "_Z%zu%sP7_JNIEnv", strlen(plain), plain);
}

static void* lookup_symbol(const char* plain, char* mangled, size_t n, int* which) {
    mangle_android_register(plain, mangled, n);
    void* p = dlsym(RTLD_DEFAULT, mangled);
    if (p) { *which = 1; return p; }
    char gmangled[256];
    mangle_global_register(plain, gmangled, sizeof(gmangled));
    p = dlsym(RTLD_DEFAULT, gmangled);
    if (p) { snprintf(mangled, n, "%s", gmangled); *which = 2; return p; }
    p = dlsym(RTLD_DEFAULT, plain);
    *which = 3;
    return p;
}

JNIEXPORT jlong JNICALL Java_dev_acos_e2d_Nat_sym(JNIEnv* env, jclass klass, jstring name) {
    (void)klass;
    const char* plain = (*env)->GetStringUTFChars(env, name, NULL);
    char mangled[256];
    int which = 0;
    void* p = lookup_symbol(plain, mangled, sizeof(mangled), &which);
    LOG("sym(%s) -> %s = %p", plain, mangled, p);
    (*env)->ReleaseStringUTFChars(env, name, plain);
    return (jlong)(intptr_t)p;
}

JNIEXPORT jint JNICALL Java_dev_acos_e2d_Nat_register(JNIEnv* env, jclass klass, jstring name) {
    (void)klass;
    const char* plain = (*env)->GetStringUTFChars(env, name, NULL);
    char mangled[256];
    int which = 0;
    void* p = lookup_symbol(plain, mangled, sizeof(mangled), &which);
    if (p == NULL) {
        LOG("register: missing symbol %s (%s)", plain, mangled);
        (*env)->ReleaseStringUTFChars(env, name, plain);
        return 0;
    }
    typedef void (*register_fn)(JNIEnv*);
    register_fn fn = (register_fn)p;
    LOG("register: calling %s @ %p", plain, p);
    fn(env);
    jint rc = 1;
    if ((*env)->ExceptionCheck(env)) {
        jthrowable t = (*env)->ExceptionOccurred(env);
        (*env)->ExceptionClear(env);
        (*env)->Throw(env, t);  // let Java print it
        rc = -1;
    }
    (*env)->ReleaseStringUTFChars(env, name, plain);
    return rc;
}

// One line per device: exists? open()? ioctl(BINDER_VERSION)? mmap()?
JNIEXPORT jstring JNICALL Java_dev_acos_e2d_Nat_binderProbe(JNIEnv* env, jclass klass) {
    (void)klass;
    char buf[4096];
    size_t off = 0;
    const int BINDER_VERSION = (int)0xc0046209u;          // _IOWR('b', 9, int)
    const int BINDER_SET_MAX_THREADS = (int)0x40046205u;  // _IOW('b', 5, __u32)

    for (int i = 0; kBinderPaths[i]; i++) {
        const char* path = kBinderPaths[i];
        struct stat st;
        int r = stat(path, &st);
        off += snprintf(buf + off, sizeof(buf) - off, "%s: stat=%d", path, r);
        if (r == 0) {
            off += snprintf(buf + off, sizeof(buf) - off, "(mode=%o %s)", st.st_mode & 07777,
                            S_ISCHR(st.st_mode)   ? "char"
                            : S_ISREG(st.st_mode) ? "reg"
                            : S_ISSOCK(st.st_mode)
                                  ? "sock"
                                  : "other");
        }
        errno = 0;
        int fd = open(path, O_RDWR | O_CLOEXEC);
        off += snprintf(buf + off, sizeof(buf) - off, " open=%d(%s)", fd,
                        fd < 0 ? strerror(errno) : "ok");
        if (fd >= 0) {
            int vers = 0;
            errno = 0;
            int ir = ioctl(fd, BINDER_VERSION, &vers);
            char vbuf[24] = "";
            if (ir == 0) snprintf(vbuf, sizeof(vbuf), " vers=%d", vers);
            off += snprintf(buf + off, sizeof(buf) - off, " ioctl(VERSION)=%d(%s%s)", ir,
                            ir < 0 ? strerror(errno) : "ok", vbuf);
            unsigned int maxThreads = 0;
            errno = 0;
            int ir2 = ioctl(fd, BINDER_SET_MAX_THREADS, &maxThreads);
            off += snprintf(buf + off, sizeof(buf) - off, " ioctl(MAX_THREADS)=%d(%s)", ir2,
                            ir2 < 0 ? strerror(errno) : "ok");
            errno = 0;
            void* m = mmap(NULL, BINDER_VM_SIZE, PROT_READ, MAP_PRIVATE | MAP_NORESERVE, fd, 0);
            off += snprintf(buf + off, sizeof(buf) - off, " mmap=%s(%s)",
                            m == MAP_FAILED ? "FAILED" : "ok",
                            m == MAP_FAILED ? strerror(errno) : "ok");
            if (m != MAP_FAILED) munmap(m, BINDER_VM_SIZE);
            close(fd);
        }
        off += snprintf(buf + off, sizeof(buf) - off, "\n");
    }
    return (*env)->NewStringUTF(env, buf);
}
