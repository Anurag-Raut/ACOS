/* gralloc_memfd_shim.c -- T0 feasibility probe: a memfd-backed software gralloc
 * interposer for the *image's* stock libui.so (API 34, x86_64).
 *
 * Build (bionic, NDK):
 *   x86_64-linux-android34-clang -O2 -fPIC -shared -o libacet0.so gralloc_memfd_shim.c
 *
 * Loaded with LD_PRELOAD, it defines the *mangled* android:: entry points that
 * libui.so calls through its PLT (verified with `readelf -rW libui.so`:
 * every symbol below has an R_X86_64_JUMP_SLOT, and libui.so is linked without
 * -Bsymbolic / DF_SYMBOLIC, so the preload wins):
 *
 *   GraphicBufferMapper::GraphicBufferMapper()        (fatal HAL probe -> no-op)
 *   GraphicBufferAllocator::GraphicBufferAllocator()  (fatal HAL probe -> no-op)
 *   GraphicBufferAllocator::allocate(...)              -> memfd handle
 *   GraphicBufferAllocator::free(...)                  -> close+free
 *   GraphicBufferMapper::getTransportSize(...)         -> 1 fd / N ints
 *   GraphicBufferMapper::lock / lockAsync(32) / lockAsync(64) -> mmap
 *   GraphicBufferMapper::unlock / unlockAsync          -> munmap
 *   GraphicBufferMapper::importBuffer / freeBuffer     -> dup fd, register
 *   GraphicBufferMapper::isSupported                   -> true
 *
 * Deliberately *not* implemented (they would read the never-initialised
 * mMapper/mAllocator fields): getPlaneLayouts, get* metadata getters,
 * dumpBuffer, validateBufferSize. A caller that needs them crashes -- that is
 * part of the T0 finding, not an accident.
 */
#define _GNU_SOURCE
#include <stdint.h>
#include <errno.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/mman.h>
#include <linux/memfd.h>

typedef struct {
    int version;
    int numFds;
    int numInts;
    int data[0];
} nh_t;

/* ints carried in every handle (after the single fd) */
enum {
    NH_W = 0,
    NH_H,
    NH_STRIDE,
    NH_FORMAT,
    NH_USAGE_LO,
    NH_USAGE_HI,
    NH_LAYERS,
    NH_BPP,
    NH_NINTS
};

#define MAX_MAP 64
static struct {
    const nh_t *h;
    void *addr;
    size_t size;
} g_map[MAX_MAP];

static void logf_(const char *what, long a, long b) {
    char buf[160];
    int n = 0;
    const char *p = "[acet0] ";
    while (*p) buf[n++] = *p++;
    while (*what) buf[n++] = *what++;
    for (int k = 0; k < 2; k++) {
        long v = k ? b : a;
        buf[n++] = ' ';
        char t[24];
        int i = 0;
        unsigned long u = v < 0 ? (unsigned long)-v : (unsigned long)v;
        if (!u) t[i++] = '0';
        while (u) {
            t[i++] = (char)('0' + u % 10);
            u /= 10;
        }
        if (v < 0) buf[n++] = '-';
        while (i) buf[n++] = t[--i];
    }
    buf[n++] = '\n';
    ssize_t r = write(2, buf, (size_t)n);
    (void)r;
}

static int bpp_for(int format) {
    switch (format) {
        case 1:  /* RGBA_8888 */
        case 2:  /* RGBX_8888 */
        case 3:  /* RGB_888 */
        case 0x16: /* RGBA_F16 */
            return 4;
        case 4:  /* RGB_565 */
        case 0x15: /* RGBA_1010102 */
            return 4;
        case 5:  /* BGRA_8888 */
            return 4;
        default:
            return 4;
    }
}

static nh_t *make_handle(uint32_t w, uint32_t h, uint32_t stride, int format, uint64_t usage,
                         uint32_t layers, int fd) {
    nh_t *nh = (nh_t *)malloc(sizeof(nh_t) + (size_t)(1 + NH_NINTS) * sizeof(int));
    if (!nh) return NULL;
    nh->version = (int)sizeof(nh_t);
    nh->numFds = 1;
    nh->numInts = NH_NINTS;
    nh->data[0] = fd;
    int *in = nh->data + 1;
    in[NH_W] = (int)w;
    in[NH_H] = (int)h;
    in[NH_STRIDE] = (int)stride;
    in[NH_FORMAT] = format;
    in[NH_USAGE_LO] = (int)(uint32_t)usage;
    in[NH_USAGE_HI] = (int)(uint32_t)(usage >> 32);
    in[NH_LAYERS] = (int)layers;
    in[NH_BPP] = bpp_for(format);
    return nh;
}

static int handle_fd(const nh_t *nh) { return nh->numFds > 0 ? nh->data[0] : -1; }
static const int *handle_ints(const nh_t *nh) { return nh->data + nh->numFds; }

/* ---------------------------------------------------------------- ctors ---
 * Replacing the constructors is what prevents the two LOG_ALWAYS_FATALs:
 * GraphicBufferMapper::GraphicBufferMapper() ("gralloc-mapper is missing",
 * GraphicBufferMapper.cpp:82) and GraphicBufferAllocator::() ("Failed to load
 * matching allocator for mapper version %d", GraphicBufferAllocator.cpp:70).
 * Both already ran by the time the first GraphicBuffer exists.
 */
void mapper_ctor(void *self) __asm__("_ZN7android19GraphicBufferMapperC1Ev");
void mapper_ctor(void *self) {
    (void)self;
    logf_("mapper:ctor(no HAL probe)", 0, 0);
}
void mapper_ctor2(void *self) __asm__("_ZN7android19GraphicBufferMapperC2Ev");
void mapper_ctor2(void *self) { mapper_ctor(self); }

void alloc_ctor(void *self) __asm__("_ZN7android22GraphicBufferAllocatorC1Ev");
void alloc_ctor(void *self) {
    (void)self;
    logf_("allocator:ctor(no HAL probe)", 0, 0);
}
void alloc_ctor2(void *self) __asm__("_ZN7android22GraphicBufferAllocatorC2Ev");
void alloc_ctor2(void *self) { alloc_ctor(self); }

/* ------------------------------------------------------------- allocate --- */

static int32_t do_allocate(uint32_t w, uint32_t h, int32_t format, uint32_t layers, uint64_t usage,
                           const nh_t **outHandle, uint32_t *outStride) {
    uint32_t stride = w;                       /* pixels */
    size_t size = (size_t)stride * h * (size_t)bpp_for(format) * (layers ? layers : 1);
    int fd = memfd_create("acos-gralloc", MFD_CLOEXEC);
    if (fd < 0) {
        logf_("allocate:memfd_create failed errno", errno, 0);
        return -12 /* -ENOMEM */;
    }
    if (ftruncate(fd, (off_t)size) != 0) {
        logf_("allocate:ftruncate failed errno", errno, 0);
        close(fd);
        return -12;
    }
    nh_t *nh = make_handle(w, h, stride, format, usage, layers ? layers : 1, fd);
    if (!nh) {
        close(fd);
        return -12;
    }
    *outHandle = nh;
    if (outStride) *outStride = stride;
    logf_("allocate:memfd bytes=", (long)size, 0);
    return 0;
}

int32_t alloc9(void *self, uint32_t w, uint32_t h, int32_t format, uint32_t layers, uint64_t usage,
               const nh_t **outHandle, uint32_t *outStride, uint64_t id, void *name)
    __asm__("_ZN7android22GraphicBufferAllocator8allocateEjjijmPPK13native_handlePjmNSt3__112basic_stringIcNS6_11char_traitsIcEENS6_9allocatorIcEEEE");
int32_t alloc9(void *self, uint32_t w, uint32_t h, int32_t format, uint32_t layers, uint64_t usage,
               const nh_t **outHandle, uint32_t *outStride, uint64_t id, void *name) {
    (void)self;
    (void)id;
    (void)name;
    return do_allocate(w, h, format, layers, usage, outHandle, outStride);
}

int32_t alloc8(void *self, uint32_t w, uint32_t h, int32_t format, uint32_t layers, uint64_t usage,
               const nh_t **outHandle, uint32_t *outStride, void *name)
    __asm__("_ZN7android22GraphicBufferAllocator8allocateEjjijmPPK13native_handlePjNSt3__112basic_stringIcNS6_11char_traitsIcEENS6_9allocatorIcEEEE");
int32_t alloc8(void *self, uint32_t w, uint32_t h, int32_t format, uint32_t layers, uint64_t usage,
               const nh_t **outHandle, uint32_t *outStride, void *name) {
    (void)self;
    (void)name;
    return do_allocate(w, h, format, layers, usage, outHandle, outStride);
}

int32_t alloc_free(void *self, const nh_t *nh) __asm__("_ZN7android22GraphicBufferAllocator4freeEPK13native_handle");
int32_t alloc_free(void *self, const nh_t *nh) {
    (void)self;
    if (!nh) return 0;
    logf_("allocator:free(-)", 0, 0);
    int fd = handle_fd(nh);
    if (fd >= 0) close(fd);
    free((void *)nh);
    return 0;
}

/* ----------------------------------------------------------- transport ---- */

void mapper_get_transport_size(void *self, const nh_t *nh, uint32_t *outFds, uint32_t *outInts)
    __asm__("_ZN7android19GraphicBufferMapper16getTransportSizeEPK13native_handlePjS4_");
void mapper_get_transport_size(void *self, const nh_t *nh, uint32_t *outFds, uint32_t *outInts) {
    (void)self;
    (void)nh;
    if (outFds) *outFds = 1;
    if (outInts) *outInts = NH_NINTS;
}

/* ---------------------------------------------------------------- lock ---- */

static int32_t do_lock(const nh_t *nh, void **vaddr, int32_t *outBpp, int32_t *outStride) {
    if (!nh) return -22; /* -EINVAL */
    const int *in = handle_ints(nh);
    int bpp = in[NH_BPP] > 0 ? in[NH_BPP] : 4;
    size_t size = (size_t)in[NH_STRIDE] * (size_t)in[NH_H] * (size_t)bpp *
                  (size_t)(in[NH_LAYERS] > 0 ? in[NH_LAYERS] : 1);
    int fd = handle_fd(nh);
    void *addr = mmap(NULL, size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (addr == MAP_FAILED) {
        logf_("lock:mmap failed errno", errno, 0);
        return -12;
    }
    for (int i = 0; i < MAX_MAP; i++) {
        if (!g_map[i].h) {
            g_map[i].h = nh;
            g_map[i].addr = addr;
            g_map[i].size = size;
            break;
        }
    }
    *vaddr = addr;
    if (outBpp) *outBpp = bpp;
    if (outStride) *outStride = in[NH_STRIDE] * bpp;   /* bytes per stride, as libui expects */
    logf_("lock:mmap bytes=", (long)size, 0);
    return 0;
}

int32_t mapper_lock(void *self, const nh_t *nh, uint32_t usage, const void *rect, void **vaddr,
                    int32_t *outBpp, int32_t *outStride)
    __asm__("_ZN7android19GraphicBufferMapper4lockEPK13native_handlejRKNS_4RectEPPvPiS9_");
int32_t mapper_lock(void *self, const nh_t *nh, uint32_t usage, const void *rect, void **vaddr,
                    int32_t *outBpp, int32_t *outStride) {
    (void)self;
    (void)usage;
    (void)rect;
    return do_lock(nh, vaddr, outBpp, outStride);
}

int32_t mapper_lock_async32(void *self, const nh_t *nh, uint32_t usage, const void *rect,
                            void **vaddr, int fenceFd, int32_t *outBpp, int32_t *outStride)
    __asm__("_ZN7android19GraphicBufferMapper9lockAsyncEPK13native_handlejRKNS_4RectEPPviPiS9_");
int32_t mapper_lock_async32(void *self, const nh_t *nh, uint32_t usage, const void *rect,
                            void **vaddr, int fenceFd, int32_t *outBpp, int32_t *outStride) {
    (void)self;
    (void)usage;
    (void)rect;
    if (fenceFd >= 0) close(fenceFd);   /* no fences in a software allocator */
    return do_lock(nh, vaddr, outBpp, outStride);
}

int32_t mapper_lock_async64(void *self, const nh_t *nh, uint64_t producerUsage,
                            uint64_t consumerUsage, const void *rect, void **vaddr, int fenceFd,
                            int32_t *outBpp, int32_t *outStride)
    __asm__("_ZN7android19GraphicBufferMapper9lockAsyncEPK13native_handlemmRKNS_4RectEPPviPiS9_");
int32_t mapper_lock_async64(void *self, const nh_t *nh, uint64_t producerUsage,
                            uint64_t consumerUsage, const void *rect, void **vaddr, int fenceFd,
                            int32_t *outBpp, int32_t *outStride) {
    (void)self;
    (void)producerUsage;
    (void)consumerUsage;
    (void)rect;
    if (fenceFd >= 0) close(fenceFd);
    return do_lock(nh, vaddr, outBpp, outStride);
}

static int32_t do_unlock(const nh_t *nh) {
    for (int i = 0; i < MAX_MAP; i++) {
        if (g_map[i].h == nh) {
            munmap(g_map[i].addr, g_map[i].size);
            g_map[i].h = NULL;
            logf_("unlock:munmap ok", 0, 0);
            return 0;
        }
    }
    logf_("unlock:no mapping", 0, 0);
    return 0;
}

int32_t mapper_unlock(void *self, const nh_t *nh)
    __asm__("_ZN7android19GraphicBufferMapper6unlockEPK13native_handle");
int32_t mapper_unlock(void *self, const nh_t *nh) {
    (void)self;
    return do_unlock(nh);
}

int32_t mapper_unlock_async(void *self, const nh_t *nh, int *fenceFd)
    __asm__("_ZN7android19GraphicBufferMapper11unlockAsyncEPK13native_handlePi");
int32_t mapper_unlock_async(void *self, const nh_t *nh, int *fenceFd) {
    (void)self;
    if (fenceFd) *fenceFd = -1;
    return do_unlock(nh);
}

/* -------------------------------------------------------------- import ---- */

int32_t mapper_import(void *self, const nh_t *nh, uint32_t w, uint32_t h, uint32_t layers,
                      int32_t format, uint64_t usage, uint32_t stride, const nh_t **outHandle)
    __asm__("_ZN7android19GraphicBufferMapper12importBufferEPK13native_handlejjjimjPS3_");
int32_t mapper_import(void *self, const nh_t *nh, uint32_t w, uint32_t h, uint32_t layers,
                      int32_t format, uint64_t usage, uint32_t stride, const nh_t **outHandle) {
    (void)self;
    if (!nh) return -22;
    const int *in = handle_ints(nh);
    int fd = dup(handle_fd(nh));
    if (fd < 0) return -12;
    nh_t *n = make_handle(w, h, stride ? stride : (uint32_t)in[NH_STRIDE], format, usage,
                          layers ? layers : (uint32_t)in[NH_LAYERS], fd);
    if (!n) {
        close(fd);
        return -12;
    }
    *outHandle = n;
    logf_("mapper:importBuffer dup ok", 0, 0);
    return 0;
}

int32_t mapper_free_buffer(void *self, const nh_t *nh)
    __asm__("_ZN7android19GraphicBufferMapper10freeBufferEPK13native_handle");
int32_t mapper_free_buffer(void *self, const nh_t *nh) {
    (void)self;
    if (!nh) return 0;
    int fd = handle_fd(nh);
    if (fd >= 0) close(fd);
    free((void *)nh);
    logf_("mapper:freeBuffer ok", 0, 0);
    return 0;
}

int32_t mapper_is_supported(void *self, uint32_t w, uint32_t h, int32_t format, uint32_t layers,
                            uint64_t usage, unsigned char *outSupported)
    __asm__("_ZN7android19GraphicBufferMapper11isSupportedEjjijmPb");
int32_t mapper_is_supported(void *self, uint32_t w, uint32_t h, int32_t format, uint32_t layers,
                            uint64_t usage, unsigned char *outSupported) {
    (void)self; (void)w; (void)h; (void)format; (void)layers; (void)usage;
    if (outSupported) *outSupported = 1;
    return 0;
}
