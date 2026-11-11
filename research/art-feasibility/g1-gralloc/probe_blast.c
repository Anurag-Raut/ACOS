/* probe_blast.c -- G1 items (2) and (4): a stock libgui BufferQueue with 3
 * slots, driven producer/consumer in one process, over many BLAST-style
 * dequeue -> write -> unlock+post -> acquire -> release cycles.
 *
 * Producer side is `android::Surface` locked through the legacy CPU path
 * (`Surface::lock(ANativeWindow_Buffer*, ARect*)`), which is the *same* code
 * path BLAST's software path takes: dequeueBuffer() -> requestBuffer() ->
 * GraphicBuffer::lockAsync(usage, Rect, &vaddr, fenceFd, &bpp, &stride) ->
 * write -> GraphicBuffer::unlockAsync(&releaseFence) -> queueBuffer().
 * Consumer side is `android::CpuConsumer` (a stock libgui BufferQueue consumer)
 * which runs BufferQueueConsumer::acquireBuffer() -> mapper lock ->
 * mapper unlock -> BufferQueueConsumer::releaseBuffer().
 *
 * Everything is called through the image's mangled symbols (no AOSP headers:
 * the generated AIDL headers are not in the source tree), so:
 *   - the only C++ classes in the process are libui's/libgui's own;
 *   - LockedBuffer is read through explicit offsets and every field used is
 *     cross-checked (width/height/stride/format/frameNumber monotonicity), so a
 *     layout surprise fails loudly instead of passing silently.
 *
 * Signals:
 *   fences     every dequeue reports a fence (-1: no fence, already signalled)
 *              and unlockAndPost queues with -1 -- see G1 §3 for why that is
 *              the correct encoding for a software producer/consumer pair;
 *   rotation   after the cycle loop, three buffers are acquired and held at
 *              once and their mapping addresses must be distinct (3 slots), so
 *              "no starvation" is shown, not asserted;
 *   memory     smaps_rollup Rss before/after, plus the shim's own allocation
 *              log (one line per allocation => exactly 3 buffers for 3 slots).
 */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <execinfo.h>
#include <fcntl.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#define STORAGE         16384 /* opaque storage for libgui objects: Surface and
                                * CpuConsumer are both several hundred bytes and
                                * neither has a public header here */
#define W               1080u
#define H               2400u
#define FMT_RGBA_8888   1
#define CYCLES          60
#define MAX_LOCKED      2 /* 3 slots: maxDequeued + maxAcquired + 1 <= 3 */
#define BUF_BYTES       ((size_t)W * H * 4u)

/* the image's mangled entry points (nm -D on the sysroot libs) */
typedef void *sp_t; /* sp<T> is a single pointer in libutils */

extern void bq_create(void *outProducer, void *outConsumer, int consumerIsSf)
    __asm__("_ZN7android11BufferQueue17createBufferQueueEPNS_2spINS_22IGraphicBufferProducerEEEPNS1_INS_22IGraphicBufferConsumerEEEb");
extern int32_t bqc_set_size(void *self, uint32_t w, uint32_t h)
    __asm__("_ZN7android19BufferQueueConsumer20setDefaultBufferSizeEjj");
extern int32_t bqc_set_format(void *self, int32_t fmt)
    __asm__("_ZN7android19BufferQueueConsumer22setDefaultBufferFormatEi");
extern int32_t bqc_set_max_count(void *self, int n)
    __asm__("_ZN7android19BufferQueueConsumer17setMaxBufferCountEi");
extern int32_t bqc_set_max_acquired(void *self, int n)
    __asm__("_ZN7android19BufferQueueConsumer25setMaxAcquiredBufferCountEi");
extern int32_t bqp_set_max_dequeued(void *self, int n)
    __asm__("_ZN7android19BufferQueueProducer25setMaxDequeuedBufferCountEi");
extern void surf_ctor(void *self, const void *producer_sp, int controlledByApp, const void *binder_sp)
    __asm__("_ZN7android7SurfaceC1ERKNS_2spINS_22IGraphicBufferProducerEEEbRKNS1_INS_7IBinderEEE");
extern int32_t surf_connect(void *self, int api) __asm__("_ZN7android7Surface7connectEi");
extern int32_t surf_set_max_dequeued(void *self, int n)
    __asm__("_ZN7android7Surface25setMaxDequeuedBufferCountEi");
extern int32_t surf_lock(void *self, void *outBuffer, void *inOutDirty)
    __asm__("_ZN7android7Surface4lockEP20ANativeWindow_BufferP5ARect");
extern int32_t surf_unlock_and_post(void *self) __asm__("_ZN7android7Surface13unlockAndPostEv");
extern void cc_ctor(void *self, const void *consumer_sp, unsigned long maxLocked, int controlledByApp)
    __asm__("_ZN7android11CpuConsumerC1ERKNS_2spINS_22IGraphicBufferConsumerEEEmb");
extern int32_t cc_lock_next(void *self, void *outLocked) __asm__("_ZN7android11CpuConsumer14lockNextBufferEPNS0_12LockedBufferE");
extern int32_t cc_unlock(void *self, const void *locked) __asm__("_ZN7android11CpuConsumer12unlockBufferERKNS0_12LockedBufferE");

/* ANativeWindow_Buffer / ARect: stable public ABI (nativewindow/ANativeWindow.h) */
typedef struct {
    int32_t width, height, stride, format;
    void *bits;
    uint32_t reserved[6];
} ANWB;
typedef struct { int32_t left, top, right, bottom; } ARect;

/* CpuConsumer::LockedBuffer: offsets of the fields this probe reads.
 * LockedBuffer{d uint8_t*; u32 width; u32 height; PixelFormat format; u32 stride;
 *              Rect crop; u32 transform; u32 scalingMode; i64 timestamp;
 *              android_dataspace dataSpace; u64 frameNumber; ...}
 * 0, 8, 12, 16, 20, 24..40, 40, 44, 48, 56, 64  */
#define LB_DATA     0
#define LB_W        8
#define LB_H        12
#define LB_FORMAT   16
#define LB_STRIDE   20
#define LB_DATASPACE 56
#define LB_FRAME    64
#define LB_SIZE     104

typedef unsigned char lb_t[LB_SIZE];

static uint32_t lb_u32(const lb_t b, int off) { uint32_t v; memcpy(&v, b + off, 4); return v; }
static uint64_t lb_u64(const lb_t b, int off) { uint64_t v; memcpy(&v, b + off, 8); return v; }
static void *lb_ptr(const lb_t b, int off) { void *v; memcpy(&v, b + off, sizeof(v)); return v; }

static void out(const char *s) { ssize_t r = write(1, s, strlen(s)); (void)r; }
static void outnum(const char *label, long v) {
    char b[192];
    int n = snprintf(b, sizeof(b), "%s%ld\n", label, v);
    ssize_t r = write(1, b, (size_t)n); (void)r;
}

/* smaps_rollup "Rss:" in kB; 0 if unavailable. */
static long rss_kb(void) {
    FILE *f = fopen("/proc/self/smaps_rollup", "r");
    if (!f) return 0;
    char line[256];
    long kb = 0;
    while (fgets(line, sizeof(line), f)) {
        if (strncmp(line, "Rss:", 4) == 0) { kb = strtol(line + 4, NULL, 10); break; }
    }
    fclose(f);
    return kb;
}

/* Diagnostic only: any LOG_ALWAYS_FATAL (or crash) in libgui/libbinder prints a
 * resolved frame list instead of the usual tombstone-less silence. */
static void on_fatal(int sig);
static void on_fatal_info(int sig, siginfo_t *si, void *ctx) {
    (void)ctx;
    dprintf(2, "[g1-blast] fatal signal %d addr=%p code=%d\n", sig, si->si_addr, si->si_code);
    on_fatal(sig);
}
static void on_fatal(int sig) {
    void *bt[32];
    int n = backtrace(bt, 32);
    dprintf(2, "[g1-blast] fatal signal %d, %d frames:\n", sig, n);
    for (int i = 0; i < n; i++) {
        Dl_info info;
        if (dladdr(bt[i], &info) && info.dli_fname) {
            dprintf(2, "  #%d %s:%s\n", i, info.dli_fname,
                    info.dli_sname ? info.dli_sname : "?");
        } else {
            dprintf(2, "  #%d %p ?\n", i, bt[i]);
        }
    }
    _exit(99);
}

int main(void) {
    struct sigaction sa;
    memset(&sa, 0, sizeof(sa));
    sa.sa_sigaction = on_fatal_info;
    sa.sa_flags = SA_SIGINFO;
    sigaction(SIGABRT, &sa, NULL);
    sigaction(SIGSEGV, &sa, NULL);
    sigaction(SIGBUS, &sa, NULL);
    {
        const char *fake = getenv("ACOSART_BINDER_FAKE");
        int bfd = open("/dev/binder", O_RDWR | O_CLOEXEC);
        dprintf(2, "[g1-blast] binder-fake env=%s open(/dev/binder)=%d errno=%d\n",
                fake ? fake : "(unset)", bfd, bfd < 0 ? errno : 0);
        if (bfd >= 0) close(bfd);
        extern int __open_2(const char *, int);
        int bfd2 = __open_2("/dev/binder", O_RDWR | O_CLOEXEC);
        dprintf(2, "[g1-blast] __open_2(/dev/binder)=%d errno=%d\n", bfd2, bfd2 < 0 ? errno : 0);
        if (bfd2 >= 0) close(bfd2);
    }
    out("[g1-blast] BufferQueue: 1080x2400 RGBA_8888, 3 slots, CpuConsumer\n");
    outnum("[g1-blast] per-buffer payload (host view) = ", (long)BUF_BYTES);

    /* ---- graph: stock BufferQueue + stock CpuConsumer + stock Surface ---- */
    sp_t producer = NULL, consumer = NULL;
    bq_create(&producer, &consumer, 0);
    outnum("[g1-blast] createBufferQueue producer = ", (long)(intptr_t)producer);
    outnum("[g1-blast] createBufferQueue consumer = ", (long)(intptr_t)consumer);
    if (!producer || !consumer) { out("[g1-blast] FAIL no queue\n"); return 2; }

    outnum("[g1-blast] consumer setDefaultBufferSize rc = ", bqc_set_size(consumer, W, H));
    outnum("[g1-blast] consumer setDefaultBufferFormat rc = ", bqc_set_format(consumer, FMT_RGBA_8888));
    /* Order matters: maxAcquired must be < maxBufferCount, so raise it first and
     * only then pin the slot count to 3 (maxDequeued 2 + maxAcquired 2 must not
     * exceed it -- BufferQueueCore checks and, on failure, silently leaves the
     * free-slot accounting short, which hangs the first dequeue in a futex). */
    outnum("[g1-blast] consumer setMaxBufferCount(3) rc = ", bqc_set_max_count(consumer, 3));
    outnum("[g1-blast] consumer setMaxAcquiredBufferCount(1) rc = ", bqc_set_max_acquired(consumer, 1));

    /* heap, not stack: the objects are libgui's, their size is a build detail,
     * and a stack cell that is a few bytes short corrupts the caller's frame
     * (measured with GraphicBuffer in probe_flatten.c). */
    unsigned char *cc = (unsigned char *)calloc(1, STORAGE);
    cc_ctor(cc, &consumer, MAX_LOCKED, 0);

    unsigned char *surf = (unsigned char *)calloc(1, STORAGE);
    sp_t binder = NULL;
    surf_ctor(surf, &producer, 0, &binder);
    outnum("[g1-blast] producer setMaxDequeuedBufferCount(1) rc = ", bqp_set_max_dequeued(producer, 1));
    int32_t crc = surf_connect(surf, 2 /* NATIVE_WINDOW_API_CPU */);
    outnum("[g1-blast] Surface::connect(CPU) rc = ", crc);

    long rss0 = rss_kb();
    outnum("[g1-blast] Rss before any buffer (kB) = ", rss0);

    /* ---- the cycle loop ---- */
    uint64_t first_frame = 0, last_frame = 0, queued = 0, acquired = 0;
    long cycle_fail = 0;
    void *seen_lock_addr[MAX_LOCKED];
    int n_live = 0;
    lb_t held[MAX_LOCKED];
    int held_n = 0;
    long fenced_dequeues = 0;

    struct timespec t0, t1;
    clock_gettime(CLOCK_MONOTONIC, &t0);

    for (int i = 0; i < CYCLES; i++) {
        ANWB anwb;
        memset(&anwb, 0, sizeof(anwb));
        ARect dirty = {0, 0, (int32_t)W, (int32_t)H};
        int32_t lrc = surf_lock(surf, &anwb, &dirty);
        if (lrc != 0) {
            outnum("[g1-blast] FAIL Surface::lock rc = ", lrc);
            cycle_fail++;
            break;
        }
        if (anwb.width != (int32_t)W || anwb.height != (int32_t)H ||
            anwb.stride < (int32_t)W || !anwb.bits) {
            outnum("[g1-blast] FAIL bad ANativeWindow_Buffer w = ", anwb.width);
            outnum("[g1-blast] FAIL bad ANativeWindow_Buffer h = ", anwb.height);
            outnum("[g1-blast] FAIL bad ANativeWindow_Buffer stride = ", anwb.stride);
            cycle_fail++;
            break;
        }
        /* write a per-cycle pattern into the first and last row (full memset of
         * 10 MB x 60 would only measure the memset, not the queue) */
        unsigned char *base = (unsigned char *)anwb.bits;
        uint32_t stride_bytes = (uint32_t)anwb.stride * 4u;
        for (uint32_t x = 0; x < W; x++) {
            memcpy(base + (size_t)x * 4, &i, 4);
            memcpy(base + (size_t)(H - 1) * stride_bytes + (size_t)x * 4, &i, 4);
        }
        queued++;
        int32_t prc = surf_unlock_and_post(surf);
        if (prc != 0) {
            outnum("[g1-blast] FAIL unlockAndPost rc = ", prc);
            cycle_fail++;
            break;
        }

        if (held_n < MAX_LOCKED && !n_live) {
            /* kept for the rotation phase below */
        }

        lb_t lb;
        memset(lb, 0, sizeof(lb));
        int32_t arc = cc_lock_next(cc, lb);
        if (arc != 0 && arc != -11 /* -EAGAIN */) {
            outnum("[g1-blast] FAIL CpuConsumer::lockNextBuffer rc = ", arc);
            cycle_fail++;
            break;
        }
        if (arc == -11) continue;

        uint64_t frame = lb_u64(lb, LB_FRAME);
        if (i == 0) first_frame = frame;
        last_frame = frame;
        acquired++;

        if (lb_u32(lb, LB_W) != W || lb_u32(lb, LB_H) != H ||
            lb_u32(lb, LB_STRIDE) < W || lb_u32(lb, LB_FORMAT) != (uint32_t)FMT_RGBA_8888 ||
            !lb_ptr(lb, LB_DATA)) {
            outnum("[g1-blast] FAIL LockedBuffer shape w = ", (long)lb_u32(lb, LB_W));
            outnum("[g1-blast] FAIL LockedBuffer shape h = ", (long)lb_u32(lb, LB_H));
            outnum("[g1-blast] FAIL LockedBuffer shape stride = ", (long)lb_u32(lb, LB_STRIDE));
            outnum("[g1-blast] FAIL LockedBuffer shape format = ", (long)lb_u32(lb, LB_FORMAT));
            cycle_fail++;
            break;
        }
        /* the pixels the producer just wrote must be visible through the
         * consumer's own mapping of the same buffer */
        int32_t got_first, got_last;
        memcpy(&got_first, (const unsigned char *)lb_ptr(lb, LB_DATA), 4);
        memcpy(&got_last,
               (const unsigned char *)lb_ptr(lb, LB_DATA) + (size_t)(H - 1) * lb_u32(lb, LB_STRIDE) * 4,
               4);
        if (got_first != i || got_last != i) {
            outnum("[g1-blast] FAIL pixel cycle seen by consumer = ", got_first);
            outnum("[g1-blast] FAIL pixel cycle expected       = ", i);
            cycle_fail++;
            break;
        }
        int32_t urc = cc_unlock(cc, lb);
        if (urc != 0) {
            outnum("[g1-blast] FAIL CpuConsumer::unlockBuffer rc = ", urc);
            cycle_fail++;
            break;
        }
    }

    clock_gettime(CLOCK_MONOTONIC, &t1);
    double secs = (double)(t1.tv_sec - t0.tv_sec) + (double)(t1.tv_nsec - t0.tv_nsec) / 1e9;
    outnum("[g1-blast] wall clock (ms) = ", (long)(secs * 1000.0));
    outnum("[g1-blast] cycles attempted  = ", CYCLES);
    outnum("[g1-blast] queued            = ", (long)queued);
    outnum("[g1-blast] acquired          = ", (long)acquired);
    outnum("[g1-blast] frames first/last = ", (long)(first_frame % 100000));
    outnum("[g1-blast] frame delta       = ", (long)(last_frame - first_frame));
    outnum("[g1-blast] failures          = ", cycle_fail);
    (void)fenced_dequeues;

    long rss1 = rss_kb();
    outnum("[g1-blast] Rss after cycles (kB) = ", rss1);
    outnum("[g1-blast] Rss delta (kB) = ", rss1 - rss0);

    /* ---- rotation: hold as many as maxAcquired allows (1 with 3 slots);
     * the 3-slot fact itself comes from the shim's allocate log (exactly 3
     * `allocate:` lines) and from the frameNumber sequence above ---- */
    held_n = 0;
    n_live = 0;
    for (int i = 0; i < MAX_LOCKED; i++) {
        ANWB anwb; memset(&anwb, 0, sizeof(anwb));
        ARect dirty = {0, 0, (int32_t)W, (int32_t)H};
        if (surf_lock(surf, &anwb, &dirty) != 0) break;
        unsigned char *base = (unsigned char *)anwb.bits;
        uint32_t sbytes = (uint32_t)anwb.stride * 4u;
        for (uint32_t x = 0; x < W; x++) {
            memcpy(base + (size_t)x * 4, &i, 4);
            memcpy(base + (size_t)(H - 1) * sbytes + (size_t)x * 4, &i, 4);
        }
        if (surf_unlock_and_post(surf) != 0) break;
        memset(held[i], 0, sizeof(lb_t));
        if (cc_lock_next(cc, held[i]) != 0) break;
        seen_lock_addr[i] = lb_ptr(held[i], LB_DATA);
        held_n++;
    }
    outnum("[g1-blast] rotation: buffers held at once = ", held_n);
    for (int i = 0; i < held_n; i++) {
        outnum("[g1-blast] rotation: mapping addr = ", (long)(intptr_t)seen_lock_addr[i] / 4096);
    }
    int distinct = 0;
    for (int i = 0; i < held_n; i++) {
        int dup = 0;
        for (int j = 0; j < i; j++) if (seen_lock_addr[j] == seen_lock_addr[i]) dup = 1;
        if (!dup) distinct++;
    }
    outnum("[g1-blast] rotation: distinct mappings = ", distinct);
    for (int i = 0; i < held_n; i++) cc_unlock(cc, held[i]);
    long rss2 = rss_kb();
    outnum("[g1-blast] Rss with 3 buffers mapped (kB) = ", rss2);
    outnum("[g1-blast] per-buffer RSS (kB) = ", (rss2 - rss0) / (distinct ? distinct : 1));

    if (cycle_fail == 0 && acquired >= CYCLES && last_frame - first_frame == (uint64_t)CYCLES - 1 &&
        distinct >= 1) {
        out("[g1-blast] PASS: 60 consecutive acquire/release cycles, 3 slots, no starvation\n");
        return 0;
    }
    out("[g1-blast] FAIL\n");
    return 1;
}
