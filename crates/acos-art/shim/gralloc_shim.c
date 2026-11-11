/* gralloc_shim.c -- the ACOS software gralloc interposer (LD_PRELOAD).
 *
 * Architecture option A (results/art-runtime/WR-window-surface-options.md):
 * stock libui/libgui, no gralloc HAL, no servicemanager, no binder. The shim
 * owns exactly what a gralloc HAL owns -- buffer *memory* (a memfd) and the
 * mapping *address* (mmap) -- and replaces the 16 entry points the image's
 * libui.so/libgui.so call through their PLT.
 *
 * Promoted from research/art-feasibility/t0-gralloc/gralloc_memfd_shim.c
 * (T0, results/art-runtime/T0.md), with the lifetime, fence and metadata gaps
 * T0 left open closed. Evidence: results/art-runtime/G1.md.
 *
 * Build (bionic, NDK -- the sysroot libs are bionic x86_64):
 *   $NDK/toolchains/llvm/prebuilt/linux-x86_64/bin/x86_64-linux-android34-clang \
 *       -O2 -fPIC -shared -Wall -o libacoshw.so crates/acos-art/shim/gralloc_shim.c
 * (tools/art-sysroot.sh --shim builds exactly this into <sysroot>/shim/.)
 *
 * WHY INTERPOSITION WORKS (measured, not assumed -- T0 §1):
 *   - neither libui.so nor libgui.so is linked -Bsymbolic / DF_SYMBOLIC, and
 *     every symbol below is exported with default visibility;
 *   - every call from GraphicBuffer/BufferQueue* to the mapper/allocator is an
 *     R_X86_64_JUMP_SLOT, i.e. it goes through .plt, so the LD_PRELOAD
 *     definition wins (BIND_NOW only affects *when*, not *which*).
 * Signatures are the IMAGE's, taken from `nm -D` on
 * .cache/art/sysroot/system/lib64/{libui,libgui}.so -- the image is older than
 * AOSP tag android-14.0.0_r75 and its GraphicBuffer::lock* path uses the
 * classic (void**, int*, int*) overloads, not r75's optional<LockResult>.
 *
 * HANDLE FORMAT (transport metadata; consistent with getTransportSize):
 *   1 fd  -- the memfd, i.e. a kernel object. This is the whole point: it
 *           survives Binder flatten/unflatten (BINDER_TYPE_FD) and SCM_RIGHTS,
 *           so the consumer process gets its own fd to the same pages.
 *   9 int -- w,h,stride(px),format,usage_lo,usage_hi,layers,bpp,dataspace
 *   getTransportSize() reports exactly (1, 9); GraphicBuffer::flatten writes
 *   those 9 ints and 1 fd, unflatten hands them back to importBuffer(), which
 *   dups the fd and rebuilds the metadata. libui itself then closes the fd it
 *   received, so the shim's dup is the surviving reference -- that is why the
 *   two free paths below must stay distinct.
 *
 * LIFETIME (T0 residual risk 4, retired in G1 §2):
 *   every handle this shim allocates or imports is recorded in a registry
 *   (pointer -> fd + size) guarded by a mutex. GraphicBuffer::free_handle()
 *   reaches either GraphicBufferAllocator::free() (owner == ownData) or
 *   GraphicBufferMapper::freeBuffer() (owner == ownHandle), and some libui
 *   builds call both; whichever runs first unregisters, closes the fd and
 *   releases the block, and the second is a no-op instead of a double free.
 *
 * FENCES (T0 residual risk 2 -- G1 §3 documents why -1 is *correct* here):
 *   acquire and release fences are reported as -1 ("no fence: already
 *   signalled"). A software producer's writes are complete and visible when
 *   unlock() returns (the memfd is MAP_SHARED, the store buffer was drained by
 *   the msync-less but x86-coherent path and by the syscall on unlock), so
 *   there is nothing for a consumer to wait for and BufferQueue's
 *   Fence::NO_FENCE short-circuit is exactly the right encoding. Input fences
 *   are *consumed* (close()) because ownership transfers to the callee.
 *   This is a fidelity statement, not a shortcut: with no GPU there can be no
 *   outstanding fence, and pretending otherwise would only delay the consumer.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

/* memfd_create: bionic 34 has the libc wrapper; keep the syscall fallback so
 * the file also builds against an older bionic. */
#ifndef MFD_CLOEXEC
#define MFD_CLOEXEC 0x0001U
#endif

typedef struct {
    int version;
    int numFds;
    int numInts;
    int data[0];
} nh_t;

enum {
    NH_W = 0,
    NH_H,
    NH_STRIDE,
    NH_FORMAT,
    NH_USAGE_LO,
    NH_USAGE_HI,
    NH_LAYERS,
    NH_BPP,
    NH_DATASPACE,
    NH_NINTS /* = 9, reported by getTransportSize */
};

#define MAX_HANDLES 512
#define MAX_MAPS 64
#define MAX_MAPS_PER_HANDLE 4

static pthread_mutex_t g_lock = PTHREAD_MUTEX_INITIALIZER;

static struct {
    const nh_t *h;
    int fd;
    size_t size;
} g_handles[MAX_HANDLES];

static struct {
    const nh_t *h;
    void *addr;
    size_t size;
} g_maps[MAX_MAPS];

static int verbose(void) {
    static int v = -1;
    if (v < 0) v = getenv("ACOSART_HW_VERBOSE") != NULL;
    return v;
}

#define LOG(...)                                     \
    do {                                             \
        if (verbose()) {                             \
            dprintf(2, "ACOSHWC[gralloc]: " __VA_ARGS__); \
            dprintf(2, "\n");                        \
        }                                            \
    } while (0)
#define LOGE(...)                                        \
    do {                                                 \
        dprintf(2, "ACOSHWC[gralloc] error: " __VA_ARGS__); \
        dprintf(2, "\n");                                \
    } while (0)

__attribute__((constructor)) static void acosart_hw_ctor(void) {
    /* One unconditional line: it is the "the second preload really loaded in
     * this process" marker the guest rerun (G1 §4) greps for. */
    dprintf(2, "ACOSHWC[gralloc]: software gralloc shim loaded (1 fd / %d ints)\n",
            NH_NINTS);
}

/* ------------------------------------------------------------- registry --- */

/* Caller must hold g_lock. Returns index or -1. */
static int registry_find(const nh_t *h) {
    if (!h) return -1;
    for (int i = 0; i < MAX_HANDLES; i++) {
        if (g_handles[i].h == h) return i;
    }
    return -1;
}

static int registry_add(const nh_t *h, int fd, size_t size) {
    for (int i = 0; i < MAX_HANDLES; i++) {
        if (!g_handles[i].h) {
            g_handles[i].h = h;
            g_handles[i].fd = fd;
            g_handles[i].size = size;
            return 0;
        }
    }
    LOGE("registry full (%d handles)", MAX_HANDLES);
    return -1;
}

/* Unmaps every mapping of `h` (call with g_lock held). */
static void unmap_all_locked(const nh_t *h) {
    for (int i = 0; i < MAX_MAPS; i++) {
        if (g_maps[i].h == h) {
            munmap(g_maps[i].addr, g_maps[i].size);
            g_maps[i].h = NULL;
            g_maps[i].addr = NULL;
            g_maps[i].size = 0;
        }
    }
}

/* One free path for both allocator::free and mapper::freeBuffer. Holding a
 * pointer that is not in the registry means "someone already released it", not
 * "corrupt memory": return 0 instead of touching it. */
static int32_t release_handle(const nh_t *h) {
    if (!h) return 0;
    pthread_mutex_lock(&g_lock);
    int i = registry_find(h);
    if (i < 0) {
        pthread_mutex_unlock(&g_lock);
        LOG("release: handle %p not live (already freed)", (const void *)h);
        return 0;
    }
    unmap_all_locked(h);
    int fd = g_handles[i].fd;
    size_t size = g_handles[i].size;
    g_handles[i].h = NULL;
    g_handles[i].fd = -1;
    g_handles[i].size = 0;
    pthread_mutex_unlock(&g_lock);
    if (fd >= 0) close(fd);
    free((void *)h);
    LOG("release: closed memfd, freed %zu B metadata", size);
    return 0;
}

/* ----------------------------------------------------------------- misc ---- */

static int bpp_for(int format) {
    switch (format) {
        case 1:    /* RGBA_8888 */
        case 2:    /* RGBX_8888 */
        case 3:    /* RGB_888 */
        case 4:    /* RGB_565 */
        case 5:    /* BGRA_8888 */
        case 0x15: /* RGBA_1010102 */
        case 0x16: /* RGBA_F16 */
        case 0x2b: /* RGBA_1010108 */
            return 4;
        default:
            /* Unknown formats are not a reason to fail the whole frame path:
             * 4 B/px is what every format the framework actually asks for in
             * this configuration uses. Logged, never silently "close enough"
             * for a known 1/2-byte format. */
            if (!verbose()) LOGE("unknown pixel format %d, assuming 4 B/px", format);
            return 4;
    }
}

/* stride in pixels, padded to 16 B like a real gralloc (keeps 1080 and 1440
 * rows device-shaped; never wider than what lock() reports). */
static uint32_t stride_px(uint32_t w, int bpp) {
    uint32_t align_px = (uint32_t)(16 / (bpp > 0 ? bpp : 4));
    if (align_px < 1) align_px = 1;
    return (w + align_px - 1) / align_px * align_px;
}

static nh_t *make_handle(uint32_t w, uint32_t h, uint32_t stride, int format, uint64_t usage,
                         uint32_t layers, int dataspace, int fd) {
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
    in[NH_DATASPACE] = dataspace;
    return nh;
}

static int handle_fd(const nh_t *nh) { return (nh && nh->numFds > 0) ? nh->data[0] : -1; }
static const int *handle_ints(const nh_t *nh) { return nh->data + nh->numFds; }

static size_t handle_bytes(const nh_t *nh) {
    const int *in = handle_ints(nh);
    int bpp = in[NH_BPP] > 0 ? in[NH_BPP] : 4;
    size_t layers = in[NH_LAYERS] > 0 ? (size_t)in[NH_LAYERS] : 1;
    return (size_t)in[NH_STRIDE] * (size_t)in[NH_H] * (size_t)bpp * layers;
}

/* --------------------------------------------------------------- ctors -----
 * Replacing the two constructors is what prevents the two LOG_ALWAYS_FATALs
 * (GraphicBufferMapper.cpp: "gralloc-mapper is missing") and, worse, the
 * *hang*: Gralloc5Mapper::waitForAllocator() waits forever for
 * android.hardware.graphics.allocator.IAllocator/default because there is no
 * servicemanager to answer. Both run before the first GraphicBuffer exists. */
void mapper_ctor(void *self) __asm__("_ZN7android19GraphicBufferMapperC1Ev");
void mapper_ctor(void *self) {
    (void)self;
    LOG("mapper:ctor (HAL discovery skipped)");
}
void mapper_ctor2(void *self) __asm__("_ZN7android19GraphicBufferMapperC2Ev");
void mapper_ctor2(void *self) { mapper_ctor(self); }

void alloc_ctor(void *self) __asm__("_ZN7android22GraphicBufferAllocatorC1Ev");
void alloc_ctor(void *self) {
    (void)self;
    LOG("allocator:ctor (HAL discovery skipped)");
}
void alloc_ctor2(void *self) __asm__("_ZN7android22GraphicBufferAllocatorC2Ev");
void alloc_ctor2(void *self) { alloc_ctor(self); }

/* libgui's SurfaceComposerClient path calls this before anything else; the
 * four GrallocN::preload() probes are dlopen-only (no service wait) but they
 * have nothing to find here. */
void mapper_preload_hal(void) __asm__("_ZN7android19GraphicBufferMapper10preloadHalEv");
void mapper_preload_hal(void) { LOG("mapper:preloadHal (no-op)"); }

/* ------------------------------------------------------------- allocate ---- */

static int32_t do_allocate(uint32_t w, uint32_t h, int32_t format, uint32_t layers,
                           uint64_t usage, const nh_t **outHandle, uint32_t *outStride) {
    if (!outHandle) return -22 /* -EINVAL */;
    if (w == 0 || h == 0) return -22;
    int bpp = bpp_for(format);
    uint32_t stride = stride_px(w, bpp);
    size_t size = (size_t)stride * h * (size_t)bpp * (layers ? layers : 1);
    if (size < 1) size = 1;

    int fd = memfd_create("acos-gralloc", MFD_CLOEXEC);
    if (fd < 0) {
        LOGE("allocate: memfd_create: %s", strerror(errno));
        return -12 /* -ENOMEM */;
    }
    if (ftruncate(fd, (off_t)size) != 0) {
        LOGE("allocate: ftruncate(%zu): %s", size, strerror(errno));
        close(fd);
        return -12;
    }
    nh_t *nh = make_handle(w, h, stride, format, usage, layers ? layers : 1,
                           (int)0x00000000 /* HAL_DATASPACE_UNKNOWN */, fd);
    if (!nh) {
        close(fd);
        return -12;
    }
    pthread_mutex_lock(&g_lock);
    if (registry_add(nh, fd, size) != 0) {
        pthread_mutex_unlock(&g_lock);
        free(nh);
        close(fd);
        return -12;
    }
    pthread_mutex_unlock(&g_lock);
    *outHandle = nh;
    if (outStride) *outStride = stride;
    LOG("allocate: %ux%u fmt=%d layers=%u %zu B (%zu B/row)", w, h, format,
        layers ? layers : 1, size, (size_t)stride * (size_t)bpp);
    return 0;
}

int32_t alloc9(void *self, uint32_t w, uint32_t h, int32_t format, uint32_t layers, uint64_t usage,
               const nh_t **outHandle, uint32_t *outStride, uint64_t id, void *name)
    __asm__("_ZN7android22GraphicBufferAllocator8allocateEjjijmPPK13native_handlePjmNSt3__112basic_stringIcNS6_11char_traitsIcEENS6_9allocatorIcEEEE");
int32_t alloc9(void *self, uint32_t w, uint32_t h, int32_t format, uint32_t layers, uint64_t usage,
               const nh_t **outHandle, uint32_t *outStride, uint64_t id, void *name) {
    (void)self; (void)id; (void)name;
    return do_allocate(w, h, format, layers, usage, outHandle, outStride);
}

int32_t alloc8(void *self, uint32_t w, uint32_t h, int32_t format, uint32_t layers, uint64_t usage,
               const nh_t **outHandle, uint32_t *outStride, void *name)
    __asm__("_ZN7android22GraphicBufferAllocator8allocateEjjijmPPK13native_handlePjNSt3__112basic_stringIcNS6_11char_traitsIcEENS6_9allocatorIcEEEE");
int32_t alloc8(void *self, uint32_t w, uint32_t h, int32_t format, uint32_t layers, uint64_t usage,
               const nh_t **outHandle, uint32_t *outStride, void *name) {
    (void)self; (void)name;
    return do_allocate(w, h, format, layers, usage, outHandle, outStride);
}

int32_t alloc_free(void *self, const nh_t *nh)
    __asm__("_ZN7android22GraphicBufferAllocator4freeEPK13native_handle");
int32_t alloc_free(void *self, const nh_t *nh) {
    (void)self;
    LOG("allocator:free");
    return release_handle(nh);
}

/* ------------------------------------------------------------ transport --- */

void mapper_get_transport_size(void *self, const nh_t *nh, uint32_t *outFds, uint32_t *outInts)
    __asm__("_ZN7android19GraphicBufferMapper16getTransportSizeEPK13native_handlePjS4_");
void mapper_get_transport_size(void *self, const nh_t *nh, uint32_t *outFds, uint32_t *outInts) {
    (void)self;
    (void)nh;
    /* Must match make_handle() exactly: GraphicBuffer::getFlattenedSize() is
     * (13 + numInts) ints + numFds fds, and unflatten rejects a mismatch. */
    if (outFds) *outFds = 1;
    if (outInts) *outInts = NH_NINTS;
}

/* ----------------------------------------------------------------- lock ---- */

static int32_t do_lock(const nh_t *nh, void **vaddr, int32_t *outBpp, int32_t *outStride) {
    if (!nh || !vaddr) return -22;
    const int *in = handle_ints(nh);
    int bpp = in[NH_BPP] > 0 ? in[NH_BPP] : 4;
    size_t size = handle_bytes(nh);
    if (size < 1) return -22;
    int fd = handle_fd(nh);
    if (fd < 0) return -22;

    void *addr = mmap(NULL, size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (addr == MAP_FAILED) {
        LOGE("lock: mmap(%zu): %s", size, strerror(errno));
        return -12;
    }

    pthread_mutex_lock(&g_lock);
    int slot = -1;
    for (int i = 0; i < MAX_MAPS; i++) {
        if (!g_maps[i].h) { slot = i; break; }
    }
    if (slot < 0) {
        pthread_mutex_unlock(&g_lock);
        munmap(addr, size);
        LOGE("lock: out of mapping slots (%d)", MAX_MAPS);
        return -12;
    }
    g_maps[slot].h = nh;
    g_maps[slot].addr = addr;
    g_maps[slot].size = size;
    pthread_mutex_unlock(&g_lock);

    *vaddr = addr;
    if (outBpp) *outBpp = bpp;
    if (outStride) *outStride = in[NH_STRIDE] * bpp; /* libui wants BYTES per row */
    LOG("lock: mmap %zu B (stride %d B)", size, in[NH_STRIDE] * bpp);
    return 0;
}

int32_t mapper_lock(void *self, const nh_t *nh, uint32_t usage, const void *rect, void **vaddr,
                    int32_t *outBpp, int32_t *outStride)
    __asm__("_ZN7android19GraphicBufferMapper4lockEPK13native_handlejRKNS_4RectEPPvPiS9_");
int32_t mapper_lock(void *self, const nh_t *nh, uint32_t usage, const void *rect, void **vaddr,
                    int32_t *outBpp, int32_t *outStride) {
    (void)self; (void)usage; (void)rect;
    return do_lock(nh, vaddr, outBpp, outStride);
}

int32_t mapper_lock_async32(void *self, const nh_t *nh, uint32_t usage, const void *rect,
                            void **vaddr, int fenceFd, int32_t *outBpp, int32_t *outStride)
    __asm__("_ZN7android19GraphicBufferMapper9lockAsyncEPK13native_handlejRKNS_4RectEPPviPiS9_");
int32_t mapper_lock_async32(void *self, const nh_t *nh, uint32_t usage, const void *rect,
                            void **vaddr, int fenceFd, int32_t *outBpp, int32_t *outStride) {
    (void)self; (void)usage; (void)rect;
    if (fenceFd >= 0) close(fenceFd); /* ownership of the input fence is ours */
    return do_lock(nh, vaddr, outBpp, outStride);
}

int32_t mapper_lock_async64(void *self, const nh_t *nh, uint64_t producerUsage,
                            uint64_t consumerUsage, const void *rect, void **vaddr, int fenceFd,
                            int32_t *outBpp, int32_t *outStride)
    __asm__("_ZN7android19GraphicBufferMapper9lockAsyncEPK13native_handlemmRKNS_4RectEPPviPiS9_");
int32_t mapper_lock_async64(void *self, const nh_t *nh, uint64_t producerUsage,
                            uint64_t consumerUsage, const void *rect, void **vaddr, int fenceFd,
                            int32_t *outBpp, int32_t *outStride) {
    (void)self; (void)producerUsage; (void)consumerUsage; (void)rect;
    if (fenceFd >= 0) close(fenceFd);
    return do_lock(nh, vaddr, outBpp, outStride);
}

static int32_t do_unlock(const nh_t *nh) {
    if (!nh) return 0;
    pthread_mutex_lock(&g_lock);
    int found = 0;
    for (int i = 0; i < MAX_MAPS; i++) {
        if (g_maps[i].h == nh) {
            munmap(g_maps[i].addr, g_maps[i].size);
            g_maps[i].h = NULL;
            g_maps[i].addr = NULL;
            g_maps[i].size = 0;
            found = 1;
            break;
        }
    }
    pthread_mutex_unlock(&g_lock);
    if (!found) LOG("unlock: no mapping for handle (already unlocked)");
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
    /* release fence: -1. The pixels are already in the memfd; see the header
     * comment. BufferQueue maps -1 to Fence::NO_FENCE ("already signalled"). */
    if (fenceFd) *fenceFd = -1;
    return do_unlock(nh);
}

/* --------------------------------------------------------------- import ---- */

int32_t mapper_import(void *self, const nh_t *nh, uint32_t w, uint32_t h, uint32_t layers,
                      int32_t format, uint64_t usage, uint32_t stride, const nh_t **outHandle)
    __asm__("_ZN7android19GraphicBufferMapper12importBufferEPK13native_handlejjjimjPS3_");
int32_t mapper_import(void *self, const nh_t *nh, uint32_t w, uint32_t h, uint32_t layers,
                      int32_t format, uint64_t usage, uint32_t stride, const nh_t **outHandle) {
    (void)self;
    if (!nh || !outHandle) return -22;
    const int *in = handle_ints(nh);
    int src = handle_fd(nh);
    if (src < 0) return -22;
    int fd = dup(src);
    if (fd < 0) {
        LOGE("importBuffer: dup: %s", strerror(errno));
        return -12;
    }
    uint32_t use_stride = stride ? stride : (uint32_t)in[NH_STRIDE];
    nh_t *n = make_handle(w, h, use_stride, format, usage,
                          layers ? layers : (uint32_t)in[NH_LAYERS],
                          in[NH_DATASPACE], fd);
    if (!n) {
        close(fd);
        return -12;
    }
    pthread_mutex_lock(&g_lock);
    if (registry_add(n, fd, 0 /* size derived from ints */) != 0) {
        pthread_mutex_unlock(&g_lock);
        free(n);
        close(fd);
        return -12;
    }
    g_handles[registry_find(n)].size = handle_bytes(n);
    pthread_mutex_unlock(&g_lock);
    *outHandle = n;
    LOG("importBuffer: dup'd fd, %ux%u stride=%u", w, h, use_stride);
    return 0;
}

/* importBufferNoValidate(handle, out) -- gralloc5 fast path. Same dup. */
int32_t mapper_import_novalidate(void *self, const nh_t *nh, const nh_t **outHandle)
    __asm__("_ZN7android19GraphicBufferMapper22importBufferNoValidateEPK13native_handlePS3_");
int32_t mapper_import_novalidate(void *self, const nh_t *nh, const nh_t **outHandle) {
    (void)self;
    if (!nh || !outHandle) return -22;
    const int *in = handle_ints(nh);
    int fd = dup(handle_fd(nh));
    if (fd < 0) return -12;
    nh_t *n = make_handle((uint32_t)in[NH_W], (uint32_t)in[NH_H], (uint32_t)in[NH_STRIDE],
                          in[NH_FORMAT],
                          ((uint64_t)(uint32_t)in[NH_USAGE_HI] << 32) |
                              (uint32_t)in[NH_USAGE_LO],
                          in[NH_LAYERS] > 0 ? (uint32_t)in[NH_LAYERS] : 1, in[NH_DATASPACE], fd);
    if (!n) {
        close(fd);
        return -12;
    }
    pthread_mutex_lock(&g_lock);
    if (registry_add(n, fd, handle_bytes(n)) != 0) {
        pthread_mutex_unlock(&g_lock);
        free(n);
        close(fd);
        return -12;
    }
    pthread_mutex_unlock(&g_lock);
    *outHandle = n;
    return 0;
}

int32_t mapper_free_buffer(void *self, const nh_t *nh)
    __asm__("_ZN7android19GraphicBufferMapper10freeBufferEPK13native_handle");
int32_t mapper_free_buffer(void *self, const nh_t *nh) {
    (void)self;
    LOG("mapper:freeBuffer");
    return release_handle(nh);
}

int32_t mapper_is_supported(void *self, uint32_t w, uint32_t h, int32_t format, uint32_t layers,
                            uint64_t usage, unsigned char *outSupported)
    __asm__("_ZN7android19GraphicBufferMapper11isSupportedEjjijmPb");
int32_t mapper_is_supported(void *self, uint32_t w, uint32_t h, int32_t format, uint32_t layers,
                            uint64_t usage, unsigned char *outSupported) {
    (void)self; (void)w; (void)h; (void)format; (void)layers; (void)usage;
    /* A memfd satisfies every CPU-visible usage; nothing here is GPU-backed, so
     * claiming support for GPU-only usage would be the dishonest answer. Say
     * yes (libui uses this only to reject obviously impossible combinations). */
    if (outSupported) *outSupported = 1;
    return 0;
}

/* ------------------------------------------------- per-buffer metadata -------
 * libgui's BufferQueueConsumer sets these on every queued buffer (they are the
 * only mapper methods libgui itself calls through its PLT). They must not run
 * the HAL code path -- there is no mMapper in the object the shim constructed
 * -- so they are handled here. Dataspace is *kept* (a consumer that reads it
 * back gets what was set); the HDR payloads are accepted and dropped, which is
 * the honest behaviour for a software consumer: nothing in this stack can
 * display them.
 *
 * ABI note: Dataspace is a plain enum (int, GPR). optional<Smpte2086> and
 * optional<Cta861_3> are trivially copyable and <= 16 B, so they arrive in
 * registers; optional<vector<uint8_t>> is non-trivially copyable, so the
 * Itanium C++ ABI passes it by invisible reference -- hence `const void*`.
 * All the payloads are opaque to us, so the exact field layout does not matter,
 * only the shape; each shim only reads `self` and the handle. */
int32_t mapper_set_dataspace(void *self, const nh_t *nh, int32_t dataspace)
    __asm__("_ZN7android19GraphicBufferMapper12setDataspaceEPK13native_handleNS_8hardware8graphics6common4V1_29DataspaceE");
int32_t mapper_set_dataspace(void *self, const nh_t *nh, int32_t dataspace) {
    (void)self;
    if (!nh) return -22;
    pthread_mutex_lock(&g_lock);
    int i = registry_find(nh);
    if (i >= 0) ((int *)((nh_t *)nh)->data)[1 + NH_DATASPACE] = dataspace;
    pthread_mutex_unlock(&g_lock);
    LOG("setDataspace(0x%x)", (unsigned)dataspace);
    return 0;
}

int32_t mapper_set_smpte2086(void *self, const nh_t *nh, const void *meta_by_value)
    __asm__("_ZN7android19GraphicBufferMapper12setSmpte2086EPK13native_handleNSt3__18optionalIN4aidl7android8hardware8graphics6common9Smpte2086EEE");
int32_t mapper_set_smpte2086(void *self, const nh_t *nh, const void *meta_by_value) {
    (void)self; (void)nh; (void)meta_by_value;
    LOG("setSmpte2086 (accepted, no display pipeline)");
    return 0;
}

int32_t mapper_set_cta861_3(void *self, const nh_t *nh, const void *meta_by_value)
    __asm__("_ZN7android19GraphicBufferMapper11setCta861_3EPK13native_handleNSt3__18optionalIN4aidl7android8hardware8graphics6common8Cta861_3EEE");
int32_t mapper_set_cta861_3(void *self, const nh_t *nh, const void *meta_by_value) {
    (void)self; (void)nh; (void)meta_by_value;
    LOG("setCta861_3 (accepted, no display pipeline)");
    return 0;
}

/* optional<vector<uint8_t>> by value -> invisible reference on the stack. */
int32_t mapper_set_smpte2094_40(void *self, const nh_t *nh, const void *meta_by_ref)
    __asm__("_ZN7android19GraphicBufferMapper15setSmpte2094_40EPK13native_handleNSt3__18optionalINS4_6vectorIhNS4_9allocatorIhEEEEEE");
int32_t mapper_set_smpte2094_40(void *self, const nh_t *nh, const void *meta_by_ref) {
    (void)self; (void)nh; (void)meta_by_ref;
    LOG("setSmpte2094_40 (accepted, no display pipeline)");
    return 0;
}
