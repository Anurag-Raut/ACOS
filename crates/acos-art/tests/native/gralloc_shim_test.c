// Native regression tests for `crates/acos-art/shim/gralloc_shim.c`.
//
// Exercises the real shim source compiled directly on the host so both:
//   1. `GraphicBufferMapper` virtuals (`getDataspace` et al.) survive `self=NULL`
//      without dereferencing a null `mMapper` (the `RenderThread` / `libui.so`
//      PixelCopy crash during Gaten dark-mode transitions), and
//   2. Concurrent `lock` / `unlock` callers on the same `native_handle_t` share
//      a single refcounted `mmap`, so an early `unlock` (e.g. `Surface::unlock`)
//      never unmaps pages another active locker (e.g. SwiftShader `Thread<01>`)
//      is still reading/writing.
//
// Usage: gralloc_shim_test <case>   exit 0 = pass; alarm(15) guards against hangs.

#include "../../shim/gralloc_shim.c"

#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#define CHECK(cond, ...)                                                           \
    do {                                                                           \
        if (!(cond)) {                                                             \
            dprintf(2, "CHECK FAILED %s:%d: %s: ", __FILE__, __LINE__, #cond);     \
            dprintf(2, __VA_ARGS__);                                               \
            dprintf(2, "\n");                                                      \
            exit(1);                                                               \
        }                                                                          \
    } while (0)

static int count_live_handles(void) {
    pthread_mutex_lock(&g_lock);
    int n = 0;
    for (int i = 0; i < MAX_HANDLES; i++) {
        if (g_handles[i].h) n++;
    }
    pthread_mutex_unlock(&g_lock);
    return n;
}

static int count_live_maps(void) {
    pthread_mutex_lock(&g_lock);
    int n = 0;
    for (int i = 0; i < MAX_MAPS; i++) {
        if (g_maps[i].h) n++;
    }
    pthread_mutex_unlock(&g_lock);
    return n;
}

static void case_dataspace_and_metadata_without_hal(void) {
    const nh_t *nh = NULL;
    uint32_t stride = 0;
    CHECK(alloc9(NULL, 1080, 2400, 1 /* RGBA_8888 */, 1, 0x933ULL, &nh, &stride, 1ULL, NULL) == 0,
          "alloc9 should succeed with self=NULL");
    CHECK(nh != NULL, "handle allocated");
    CHECK(stride == 1080, "stride=%u expected 1080", stride);

    int32_t ds = -1;
    CHECK(mapper_get_dataspace(NULL, nh, &ds) == 0, "initial getDataspace");
    CHECK(ds == 0, "initial dataspace=%d expected 0", ds);

    const int32_t kSrgb = 0x08810000;
    CHECK(mapper_set_dataspace(NULL, nh, kSrgb) == 0, "setDataspace");
    CHECK(mapper_get_dataspace(NULL, nh, &ds) == 0, "getDataspace after set");
    CHECK(ds == kSrgb, "dataspace=0x%x expected 0x%x", (unsigned)ds, (unsigned)kSrgb);

    /* Imported handle must preserve dataspace and survive independent free. */
    const nh_t *imported = NULL;
    CHECK(mapper_import_novalidate(NULL, nh, &imported) == 0, "importBufferNoValidate");
    CHECK(imported != NULL && imported != nh, "imported handle distinct");
    ds = -1;
    CHECK(mapper_get_dataspace(NULL, imported, &ds) == 0, "getDataspace on imported");
    CHECK(ds == kSrgb, "imported dataspace=0x%x expected 0x%x", (unsigned)ds, (unsigned)kSrgb);
    CHECK(mapper_free_buffer(NULL, imported) == 0, "free imported buffer");

    uint64_t u64 = 0;
    int32_t fmt = 0;
    CHECK(mapper_get_width(NULL, nh, &u64) == 0 && u64 == 1080, "getWidth");
    CHECK(mapper_get_height(NULL, nh, &u64) == 0 && u64 == 2400, "getHeight");
    CHECK(mapper_get_layer_count(NULL, nh, &u64) == 0 && u64 == 1, "getLayerCount");
    CHECK(mapper_get_usage(NULL, nh, &u64) == 0 && u64 == 0x933ULL, "getUsage");
    CHECK(mapper_get_allocation_size(NULL, nh, &u64) == 0 && u64 == 1080ULL * 2400ULL * 4ULL,
          "getAllocationSize");
    CHECK(mapper_get_pixel_format_requested(NULL, nh, &fmt) == 0 && fmt == 1,
          "getPixelFormatRequested");
    u64 = 99;
    CHECK(mapper_get_protected_content(NULL, nh, &u64) == 0 && u64 == 0, "getProtectedContent");
    u64 = 99;
    CHECK(mapper_get_pixel_format_modifier(NULL, nh, &u64) == 0 && u64 == 0,
          "getPixelFormatModifier");

    uint32_t u32 = 0;
    int32_t i32 = 0;
    char opaque[64] = {0};
    CHECK(mapper_get_buffer_id(NULL, nh, &u64) == -38, "getBufferId");
    CHECK(mapper_get_name(NULL, nh, opaque) == -38, "getName");
    CHECK(mapper_get_pixel_format_fourcc(NULL, nh, &u32) == -38, "getPixelFormatFourCC");
    CHECK(mapper_get_compression(NULL, nh, opaque) == -38, "getCompression");
    CHECK(mapper_get_compression_ext(NULL, nh, opaque) == -38, "getCompressionExt");
    CHECK(mapper_get_interlaced(NULL, nh, opaque) == -38, "getInterlaced");
    CHECK(mapper_get_interlaced_ext(NULL, nh, opaque) == -38, "getInterlacedExt");
    CHECK(mapper_get_chroma_siting(NULL, nh, opaque) == -38, "getChromaSiting");
    CHECK(mapper_get_chroma_siting_ext(NULL, nh, opaque) == -38, "getChromaSitingExt");
    CHECK(mapper_get_plane_layouts(NULL, nh, opaque) == -38, "getPlaneLayouts");
    CHECK(mapper_get_blend_mode(NULL, nh, &i32) == -38, "getBlendMode");
    CHECK(mapper_get_smpte2086(NULL, nh, opaque) == -38, "getSmpte2086");
    CHECK(mapper_set_smpte2086(NULL, nh, opaque) == 0, "setSmpte2086");
    CHECK(mapper_get_cta861_3(NULL, nh, opaque) == -38, "getCta861_3");
    CHECK(mapper_set_cta861_3(NULL, nh, opaque) == 0, "setCta861_3");
    CHECK(mapper_get_smpte2094_40(NULL, nh, opaque) == -38, "getSmpte2094_40");
    CHECK(mapper_set_smpte2094_40(NULL, nh, opaque) == 0, "setSmpte2094_40");
    CHECK(mapper_get_smpte2094_10(NULL, nh, opaque) == -38, "getSmpte2094_10");
    CHECK(mapper_set_smpte2094_10(NULL, nh, opaque) == 0, "setSmpte2094_10");
    mapper_dump_buffer(NULL, nh, opaque, 1);

    CHECK(alloc_free(NULL, nh) == 0, "alloc_free");
    CHECK(mapper_free_buffer(NULL, nh) == 0, "double free is safe no-op");
    CHECK(count_live_handles() == 0, "no leaked handles");
    CHECK(count_live_maps() == 0, "no leaked maps");
}

static void case_concurrent_lock_unlock_preserves_mapping(void) {
    const nh_t *nh = NULL;
    uint32_t stride = 0;
    CHECK(alloc9(NULL, 1080, 2400, 1, 1, 0x933ULL, &nh, &stride, 2ULL, NULL) == 0, "alloc9");
    const size_t expected_size = 1080ULL * 2400ULL * 4ULL;

    /* Lock #1 (e.g. SwiftShader AHardwareBuffer_lockPlanes) */
    void *raw1 = NULL;
    int32_t bpp = 0;
    int32_t stride_bytes = 0;
    CHECK(mapper_lock_async64(NULL, nh, 0x33ULL, 0x100ULL, NULL, &raw1, -1, &bpp, &stride_bytes) == 0,
          "lock #1");
    CHECK(raw1 != NULL && bpp == 4 && stride_bytes == 4320,
          "lock #1 outputs addr=%p bpp=%d stride=%d", raw1, bpp, stride_bytes);

    volatile uint8_t *p1 = (volatile uint8_t *)raw1;
    p1[0] = 0xAA;
    p1[4 * 4320] = 0xBB;
    p1[10 * 4320] = 0xCC;
    p1[expected_size - 1] = 0xDD;

    /* Lock #2 (e.g. Surface::lock / Readback on the same handle) */
    void *raw2 = NULL;
    CHECK(mapper_lock(NULL, nh, 0x33ULL, NULL, &raw2, &bpp, &stride_bytes) == 0, "lock #2");
    CHECK(raw2 == raw1, "concurrent lock must reuse canonical mapping (%p vs %p)", raw2, raw1);
    CHECK(count_live_maps() == 1, "one shared mapping slot");

    /* Unlock #1 (e.g. Surface::unlockAndPost finishes first) */
    int fence = 0;
    CHECK(mapper_unlock_async(NULL, nh, &fence) == 0 && fence == -1, "unlock #1");
    CHECK(count_live_maps() == 1, "mapping stays alive while refcount > 0");

    /* Verify Lock #1's pages are STILL mapped and writable/readable! */
    CHECK(p1[0] == 0xAA, "scanline 0 intact");
    CHECK(p1[4 * 4320] == 0xBB, "scanline 4 intact");
    CHECK(p1[10 * 4320] == 0xCC, "scanline 10 intact");
    CHECK(p1[expected_size - 1] == 0xDD, "tail byte intact");
    p1[4 * 4320] = 0xEE;

    /* Unlock #2 (last holder drops refcount to 0 and unmaps) */
    CHECK(mapper_unlock(NULL, nh) == 0, "unlock #2");
    CHECK(count_live_maps() == 0, "mapping released after final unlock");

    /* Re-lock sees writes persisted in memfd */
    void *raw3 = NULL;
    CHECK(mapper_lock(NULL, nh, 0x33ULL, NULL, &raw3, NULL, NULL) == 0, "lock #3");
    CHECK(((volatile uint8_t *)raw3)[4 * 4320] == 0xEE, "memfd preserved write");
    CHECK(mapper_unlock(NULL, nh) == 0, "unlock #3");

    CHECK(alloc_free(NULL, nh) == 0, "alloc_free");
    CHECK(count_live_handles() == 0 && count_live_maps() == 0, "clean registry");
}

typedef struct {
    const nh_t *nh;
    size_t size;
    volatile int stop;
} StressCtx;

static void *worker_reader_thread(void *arg) {
    StressCtx *ctx = (StressCtx *)arg;
    void *addr = NULL;
    CHECK(mapper_lock_async64(NULL, ctx->nh, 0x33ULL, 0x100ULL, NULL, &addr, -1, NULL, NULL) == 0,
          "worker lock");
    volatile uint8_t *p = (volatile uint8_t *)addr;
    uint64_t acc = 0;
    while (!ctx->stop) {
        for (size_t off = 0; off < ctx->size; off += 4096) {
            acc += p[off];
        }
        acc += p[ctx->size - 1];
    }
    (void)acc;
    CHECK(mapper_unlock(NULL, ctx->nh) == 0, "worker unlock");
    return NULL;
}

static void case_multithreaded_lock_unlock_stress(void) {
    const nh_t *nh = NULL;
    uint32_t stride = 0;
    CHECK(alloc9(NULL, 64, 64, 1, 1, 0x933ULL, &nh, &stride, 3ULL, NULL) == 0, "alloc9");
    size_t size = (size_t)stride * 64 * 4;

    StressCtx ctx = {nh, size, 0};
    pthread_t workers[4];
    for (int i = 0; i < 4; i++) {
        CHECK(pthread_create(&workers[i], NULL, worker_reader_thread, &ctx) == 0, "pthread_create");
    }

    for (int iter = 0; iter < 500; iter++) {
        void *addr = NULL;
        CHECK(mapper_lock(NULL, nh, 0x33ULL, NULL, &addr, NULL, NULL) == 0, "churn lock");
        ((volatile uint8_t *)addr)[0] = (uint8_t)iter;
        int fence = 0;
        CHECK(mapper_unlock_async(NULL, nh, &fence) == 0, "churn unlock");
    }

    ctx.stop = 1;
    for (int i = 0; i < 4; i++) {
        pthread_join(workers[i], NULL);
    }

    CHECK(count_live_maps() == 0, "no leaked maps after stress");
    CHECK(alloc_free(NULL, nh) == 0, "alloc_free");
    CHECK(count_live_handles() == 0, "no leaked handles after stress");
}

int main(int argc, char **argv) {
    alarm(15);
    if (argc < 2) {
        dprintf(2, "usage: gralloc_shim_test <case>\n");
        return 2;
    }
    const char *c = argv[1];
    if (!strcmp(c, "dataspace_and_metadata_without_hal")) {
        case_dataspace_and_metadata_without_hal();
    } else if (!strcmp(c, "concurrent_lock_unlock_preserves_mapping")) {
        case_concurrent_lock_unlock_preserves_mapping();
    } else if (!strcmp(c, "multithreaded_lock_unlock_stress")) {
        case_multithreaded_lock_unlock_stress();
    } else {
        dprintf(2, "unknown case: %s\n", c);
        return 2;
    }
    printf("PASS %s\n", c);
    return 0;
}
