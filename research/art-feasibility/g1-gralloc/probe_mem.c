/* probe_mem.c -- G1 item (4): what a 1080x2400 RGBA_8888, 3-slot buffer set
 * actually costs with the software (memfd) allocator.
 *
 * Three stock libui GraphicBuffers are held at the same time (the shape a
 * 3-slot BufferQueue keeps resident), every page of each is touched through the
 * mapper's lock, and /proc/self/smaps_rollup Rss is sampled before/after. No
 * BufferQueue and no binder are involved, so this measurement does not depend on
 * the rest of the G1 probe suite.
 *
 * The point of the number: a memfd costs *anonymous-class shmem*, i.e. it is
 * billed to the process that wrote it, and it is billed once per page even if
 * three processes map it (the pages are shared, not copied). A real gralloc
 * (ION/DMA-BUF) would bill it to the graphics allocator instead. That is the
 * memory trade-off of option A, measured rather than asserted.
 */
#define _GNU_SOURCE
#include <fcntl.h>
#include <sys/mman.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#define STORAGE 512
#define W 1080u
#define H 2400u
#define FMT_RGBA_8888 1
#define USAGE 0x300u
#define SLOTS 3

extern void gb_ctor(void *self, uint32_t w, uint32_t h, int32_t format, uint32_t layers,
                    uint64_t usage, void *name)
    __asm__("_ZN7android13GraphicBufferC1EjjijmNSt3__112basic_stringIcNS1_11char_traitsIcEENS1_9allocatorIcEEEE");
extern void gb_dtor(void *self) __asm__("_ZN7android13GraphicBufferD1Ev");
extern int32_t gb_lock(void *self, uint32_t usage, void **vaddr, int32_t *bpp, int32_t *stride)
    __asm__("_ZN7android13GraphicBuffer4lockEjPPvPiS3_");
extern int32_t gb_unlock(void *self) __asm__("_ZN7android13GraphicBuffer6unlockEv");

static void out(const char *s) { ssize_t r = write(1, s, strlen(s)); (void)r; }
static void outnum(const char *label, long v) {
    char b[192];
    int n = snprintf(b, sizeof(b), "%s%ld\n", label, v);
    ssize_t r = write(1, b, (size_t)n); (void)r;
}

/* Read one field out of /proc/self/smaps_rollup, in kB. */
static long smaps_field(const char *key) {
    FILE *f = fopen("/proc/self/smaps_rollup", "r");
    if (!f) return -1;
    char line[256];
    size_t klen = strlen(key);
    long v = -1;
    while (fgets(line, sizeof(line), f)) {
        if (strncmp(line, key, klen) == 0) { v = strtol(line + klen, NULL, 10); break; }
    }
    fclose(f);
    return v;
}

int main(void) {
    out("[g1-mem] 3 slots x 1080x2400 RGBA_8888 through stock libui + memfd shim\n");

    long rss0 = smaps_field("Rss:");
    long shd0 = smaps_field("Shared_Dirty:");
    outnum("[g1-mem] before: Rss kB = ", rss0);
    outnum("[g1-mem] before: Shared_Dirty kB = ", shd0);

    unsigned char gb[SLOTS][STORAGE];
    unsigned char emptystr[32];
    memset(emptystr, 0, sizeof(emptystr));
    void *maps[SLOTS];
    int32_t stride_bytes[SLOTS];

    /* Phase A: the 3 slots are live AND locked, which is the worst case for a
     * consumer (every page resident in this process's page table). */
    for (int i = 0; i < SLOTS; i++) {
        memset(gb[i], 0, STORAGE);
        gb_ctor(gb[i], W, H, FMT_RGBA_8888, 1, USAGE, emptystr);
        void *v = NULL;
        int32_t bpp = -1, stride = -1;
        int32_t rc = gb_lock(gb[i], USAGE, &v, &bpp, &stride);
        if (rc != 0 || !v) { outnum("[g1-mem] FAIL lock slot = ", i); return 2; }
        maps[i] = v;
        stride_bytes[i] = stride;
        memset(v, 0x5a, (size_t)stride * H);   /* touch every page */
        outnum("[g1-mem] slot locked+written, bytesPerStride = ", stride);
    }
    long rss_a = smaps_field("Rss:");
    long shd_a = smaps_field("Shared_Dirty:");
    outnum("[g1-mem] A: all 3 slots mapped+written, Rss kB = ", rss_a);
    outnum("[g1-mem] A: Rss delta kB total = ", rss_a - rss0);
    outnum("[g1-mem] A: Rss delta kB per slot = ", (rss_a - rss0) / SLOTS);
    outnum("[g1-mem] A: Shared_Dirty kB = ", shd_a);
    outnum("[g1-mem] A: payload kB per slot = ", (long)stride_bytes[0] * H / 1024);
    outnum("[g1-mem] A: payload kB total = ", (long)stride_bytes[0] * H * SLOTS / 1024);

    /* Phase B: unlocked but still alive. The pixels do not go away -- they are
     * the memfd's page cache (shmem), reclaimable but charged to this process's
     * cgroup; mincore() proves they are still resident. */
    for (int i = 0; i < SLOTS; i++) gb_unlock(gb[i]);
    long rss_b = smaps_field("Rss:");
    outnum("[g1-mem] B: unlocked, Rss kB = ", rss_b);
    outnum("[g1-mem] B: Rss delta kB vs A = ", rss_b - rss_a);

    for (int i = 0; i < SLOTS; i++) {
        void *v = NULL;
        int32_t bpp = -1, stride = -1;
        if (gb_lock(gb[i], USAGE, &v, &bpp, &stride) == 0 && v) {
            size_t pages = ((size_t)stride * H) / 4096;
            unsigned char *vec = (unsigned char *)malloc(pages + 1);
            if (vec) {
                if (mincore(v, (size_t)stride * H, vec) == 0 && pages > 0) {
                    size_t resident = 0;
                    for (size_t k = 0; k < pages; k++) resident += vec[k] & 1;
                    outnum("[g1-mem] B: mincore resident pages / total = ", (long)(resident * 100 / pages));
                }
                free(vec);
            }
            gb_unlock(gb[i]);
        }
    }

    for (int i = 0; i < SLOTS; i++) gb_dtor(gb[i]);
    out("[g1-mem] PASS (measured; see numbers above)\n");
    return 0;
}
