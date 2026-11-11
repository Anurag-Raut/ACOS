/* gbprobe.c -- T0 probe: drive the *image's* stock libui.so through
 * construct -> allocate -> lock -> write -> unlock -> (relock/verify) -> destroy,
 * with no Android runtime, no binder, no gralloc HAL.
 *
 * Build (bionic, NDK; no C++ stdlib in the probe on purpose -- every C++ symbol
 * is called by its mangled name, so the only libc++ in the process is the one
 * libui.so itself links):
 *   x86_64-linux-android34-clang -O2 -o gbprobe gbprobe.c \
 *       -L<sysroot>/system/lib64 -lui -Wl,-rpath-link,<sysroot>/system/lib64
 *
 * Layout facts used here (image libui.so, verified by disassembly):
 *   GraphicBuffer::GraphicBuffer(w,h,fmt,layers,usage,std::string)  C1
 *   GraphicBuffer::lock(uint32_t, void**, int32_t*, int32_t*)       PLT -> GraphicBufferMapper::lock
 *   GraphicBuffer::unlock()                                        tail-jmp -> GraphicBufferMapper::unlock
 *   ~GraphicBuffer() -> free_handle() -> GraphicBufferAllocator::free
 *   sizeof(GraphicBuffer) >= 0xc8 (handle at +0x70, mapper ref at +0xc0)
 */
#include <stdint.h>
#include <string.h>
#include <unistd.h>

#define STORAGE 256

static void out(const char *s) {
    size_t n = 0;
    while (s[n]) n++;
    ssize_t r = write(1, s, n);
    (void)r;
}
static void outnum(const char *label, long v) {
    char b[128];
    int n = 0;
    while (*label) b[n++] = *label++;
    char t[24];
    int i = 0;
    unsigned long u = (v < 0) ? (unsigned long)-v : (unsigned long)v;
    if (!u) t[i++] = '0';
    while (u) {
        t[i++] = (char)('0' + (u % 10));
        u /= 10;
    }
    if (v < 0) b[n++] = '-';
    while (i) b[n++] = t[--i];
    b[n++] = '\n';
    ssize_t r = write(1, b, (size_t)n);
    (void)r;
}

typedef struct {
    int32_t left, top, right, bottom;
} Rect;

extern void gb_ctor(void *self, uint32_t w, uint32_t h, int32_t format, uint32_t layers,
                    uint64_t usage, void *name)
    __asm__("_ZN7android13GraphicBufferC1EjjijmNSt3__112basic_stringIcNS1_11char_traitsIcEENS1_9allocatorIcEEEE");
extern void gb_dtor(void *self) __asm__("_ZN7android13GraphicBufferD1Ev");
extern int32_t gb_initcheck(const void *self) __asm__("_ZNK7android13GraphicBuffer9initCheckEv");
extern int32_t gb_lock(void *self, uint32_t usage, void **vaddr, int32_t *bpp, int32_t *stride)
    __asm__("_ZN7android13GraphicBuffer4lockEjPPvPiS3_");
extern int32_t gb_lock_rect(void *self, uint32_t usage, const Rect *rect, void **vaddr, int32_t *bpp,
                            int32_t *stride)
    __asm__("_ZN7android13GraphicBuffer4lockEjRKNS_4RectEPPvPiS6_");
extern int32_t gb_unlock(void *self) __asm__("_ZN7android13GraphicBuffer6unlockEv");

#define FMT_RGBA_8888 1
#define W 320u
#define H 240u
#define USAGE 0x300u /* SW_READ_OFTEN|SW_WRITE_OFTEN-ish; the shim does not validate */

int main(void) {
    unsigned char gb[STORAGE];
    memset(gb, 0, sizeof(gb));

    out("[gbprobe] step 1: GraphicBuffer(w=320,h=240,RGBA_8888,usage=0x300) -- stock libui ctor\n");
    /* The ctor takes std::string by value -> Itanium ABI passes an invisible
     * reference; an all-zero libc++ short-string is a valid empty std::string,
     * and libui only moves it (never frees it) into GraphicBufferAllocator. */
    unsigned char emptystr[32];
    memset(emptystr, 0, sizeof(emptystr));
    gb_ctor(gb, W, H, FMT_RGBA_8888, 1, USAGE, emptystr);
    out("[gbprobe] step 1 ok (no LOG_ALWAYS_FATAL)\n");

    int32_t ck = gb_initcheck(gb);
    outnum("[gbprobe] step 2: initCheck = ", ck);
    if (ck != 0) {
        out("[gbprobe] FAIL initCheck\n");
        return 2;
    }

    void *vaddr = 0;
    int32_t bpp = -1, stride = -1;
    int32_t rc = gb_lock(gb, USAGE, &vaddr, &bpp, &stride);
    outnum("[gbprobe] step 3: lock rc = ", rc);
    outnum("[gbprobe] step 3: vaddr = ", (long)(intptr_t)vaddr);
    outnum("[gbprobe] step 3: bytesPerPixel = ", bpp);
    outnum("[gbprobe] step 3: bytesPerStride = ", stride);
    if (rc != 0 || !vaddr) {
        out("[gbprobe] FAIL lock\n");
        return 3;
    }

    /* step 4: write a recognisable pattern (stride is in bytes) */
    unsigned char *p = (unsigned char *)vaddr;
    for (uint32_t y = 0; y < H; y++) {
        unsigned char *row = p + (size_t)y * (size_t)stride;
        for (uint32_t x = 0; x < W; x++) {
            row[x * 4 + 0] = (unsigned char)(x & 0xff);
            row[x * 4 + 1] = (unsigned char)(y & 0xff);
            row[x * 4 + 2] = 0x5a;
            row[x * 4 + 3] = 0xff;
        }
    }
    out("[gbprobe] step 4 ok: 320x240 RGBA written\n");

    int32_t urc = gb_unlock(gb);
    outnum("[gbprobe] step 5: unlock rc = ", urc);
    if (urc != 0) {
        out("[gbprobe] FAIL unlock\n");
        return 5;
    }

    /* step 6: re-lock and verify the pixels survived the unmap/remap cycle
     * (this is the memfd actually being the backing store, not a scratch page) */
    void *v2 = 0;
    int32_t bpp2 = -1, stride2 = -1;
    rc = gb_lock(gb, USAGE, &v2, &bpp2, &stride2);
    outnum("[gbprobe] step 6: relock rc = ", rc);
    if (rc != 0 || !v2) {
        out("[gbprobe] FAIL relock\n");
        return 6;
    }
    unsigned char *q = (unsigned char *)v2;
    int ok = 1;
    for (uint32_t y = 0; y < H && ok; y += 17) {
        for (uint32_t x = 0; x < W && ok; x += 13) {
            unsigned char *px = q + (size_t)y * (size_t)stride2 + (size_t)x * 4;
            if (px[0] != (unsigned char)(x & 0xff) || px[1] != (unsigned char)(y & 0xff) ||
                px[2] != 0x5a || px[3] != 0xff) {
                ok = 0;
            }
        }
    }
    out(ok ? "[gbprobe] step 6 ok: pixels round-tripped through the memfd\n"
           : "[gbprobe] FAIL pixel verification\n");
    gb_unlock(gb);
    if (!ok) return 7;

    /* step 7: Rect-shaped lock (the path Surface::lock takes) -- optional,
     * only to confirm the entry point exists; ignore failure to keep the
     * verdict about the primary sequence. */
    Rect r = {0, 0, (int32_t)W, (int32_t)H};
    void *v3 = 0;
    int32_t b3 = -1, s3 = -1;
    int32_t rrc = gb_lock_rect(gb, USAGE, &r, &v3, &b3, &s3);
    outnum("[gbprobe] step 7: lock(Rect) rc = ", rrc);
    if (rrc == 0) gb_unlock(gb);

    gb_dtor(gb);
    out("[gbprobe] step 8 ok: destroy (free_handle -> allocator free)\n");
    out("[gbprobe] ALL STEPS PASSED\n");
    return 0;
}
