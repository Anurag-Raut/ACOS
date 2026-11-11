/* probe_blast.c -- G1 item (2), corrected design: a stock libgui BufferQueue
 * (3 slots) driven by a *producer thread* and a *consumer thread*, which is the
 * assumption real BLAST satisfies (app thread produces, SurfaceFlinger/BLAST
 * consumes). G1 §4.2 measured the first design hanging in
 * `BufferQueueProducer::waitForFreeSlotThenRelock`'s futex because one thread
 * both produced and consumed: `mDequeueCondition` is only signalled by the
 * consumer side, so a single-threaded loop can never wake itself. G2 §2 shows
 * the stack that proved it and the second, independent probe bug it exposed
 * (RefBase refcount of a placement-constructed ConsumerBase).
 *
 * Producer: `Surface::lock(ANativeWindow_Buffer*)` -> write a per-frame pattern
 *   -> `Surface::unlockAndPost()`: stock dequeueBuffer -> requestBuffer ->
 *   GraphicBuffer::lockAsync -> unlockAsync -> queueBuffer, with every gralloc
 *   call answered by the LD_PRELOAD shim (libacoshw.so).
 * Consumer: `CpuConsumer::lockNextBuffer` -> checksum the pixels the consumer
 *   itself can see -> `unlockBuffer` (BufferQueueConsumer::releaseBuffer).
 *
 * C1 bridge (`G1_C1_SOCK`, optional): after each queue the producer hands the
 * frame to the Rust harness in the form a binder transaction carries it -- the
 * shim's 88-byte flattened GraphicBuffer (13 words + 9 handle ints) plus the
 * frame's memfd over SCM_RIGHTS. The harness runs C1's real `surface_client`
 * path (legacy SET_TRANSACTION_STATE -> latest-buffer store -> onReleaseBuffer
 * for the buffer it replaced), checksums the pixels from the *stored* fd, and
 * answers with that latch checksum plus the release callback it delivered.
 *
 * Because `Surface::lock` (the legacy CPU path) does not expose the
 * GraphicBuffer, the probe mirrors the frame into its own memfd for transport.
 * The wire form is byte-identical to the shim's flatten output; G1 item 1
 * proved the real flatten -> SCM_RIGHTS -> unflatten round-trip separately.
 */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <execinfo.h>
#include <fcntl.h>
#include <pthread.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/syscall.h>
#include <sys/uio.h>
#include <sys/un.h>
#include <time.h>
#include <unistd.h>

#define STORAGE       16384 /* opaque libgui object storage (see G1 §4.1) */
#define W             1080u
#define H             2400u
#define FMT_RGBA_8888 1
#define CYCLES        60
#define SLOTS         3
#define MAX_LOCKED    1 /* 3 slots: maxDequeued 1 + maxAcquired 1 + 1 <= 3 */
#define BUF_BYTES     ((size_t)W * H * 4u)
#define LB_SIZE       104
#define G1_HDR        48 /* acos_binder_peer::protocol::HEADER_SIZE */

/* the image's mangled entry points (nm -D on the sysroot libs) */
typedef void *sp_t;
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
extern int32_t surf_lock(void *self, void *outBuffer, void *inOutDirty)
    __asm__("_ZN7android7Surface4lockEP20ANativeWindow_BufferP5ARect");
extern int32_t surf_unlock_and_post(void *self) __asm__("_ZN7android7Surface13unlockAndPostEv");
/* Surface::lock() does not set the producer-side crop; without it queueBuffer
 * carries an empty Rect, and CpuConsumer's own lockAsync(item.mCrop) is then
 * BAD_VALUE (-22) — measured in G2 §2.3. setBuffersDimensions is what a real
 * producer calls (native_window_set_buffers_dimensions). */
extern int32_t surf_set_dims(void *self, uint32_t w, uint32_t h)
    __asm__("_ZN7android7Surface20setBuffersDimensionsEjj");
/* The producer-side crop is what queueBuffer puts in QueueBufferInput, and the
 * consumer's CpuConsumer locks `BufferItem::mCrop`. Surface::lock() does not set
 * it, so without setCrop the consumer sees Rect::EMPTY_RECT and its own
 * lockAsync returns BAD_VALUE (-22) — measured in G2 §2.3. A real producer sets
 * it (native_window_set_crop). */
extern int32_t surf_set_crop(void *self, const void *rect)
    __asm__("_ZN7android7Surface7setCropEPKNS_4RectE");
extern void cc_ctor(void *self, const void *consumer_sp, unsigned long maxLocked, int controlledByApp)
    __asm__("_ZN7android11CpuConsumerC1ERKNS_2spINS_22IGraphicBufferConsumerEEEmb");
extern int32_t cc_lock_next(void *self, void *outLocked)
    __asm__("_ZN7android11CpuConsumer14lockNextBufferEPNS0_12LockedBufferE");
extern int32_t cc_unlock(void *self, const void *locked)
    __asm__("_ZN7android11CpuConsumer12unlockBufferERKNS0_12LockedBufferE");
/* ConsumerBase inherits RefBase *virtually*. A placement-constructed object has
 * no owning sp<>, so the first sp<> the frame-available path hands out drops the
 * last reference and runs ~BufferQueueConsumer while dequeueBuffer still holds
 * the queue mutex -> self-deadlock (G2 §2.2). Taking one real strong reference
 * is what `new CpuConsumer(...)` would have done. RefBase is not at +0 because
 * of the virtual base; the Itanium ABI keeps its offset at vptr[-3]. */
extern void refbase_inc_strong(void *self, const void *id)
    __asm__("_ZNK7android7RefBase9incStrongEPKv");

/* ANativeWindow_Buffer / ARect: stable public ABI (nativewindow/ANativeWindow.h) */
typedef struct { int32_t width, height, stride, format; void *bits; uint32_t reserved[6]; } ANWB;
struct g1_rect { int32_t left, top, right, bottom; };

/* CpuConsumer::LockedBuffer field offsets (cross-checked against the shape) */
#define LB_DATA   0
#define LB_W      8
#define LB_H      12
#define LB_FORMAT 16
#define LB_STRIDE 20
#define LB_FRAME  64
typedef unsigned char lb_t[LB_SIZE];
static uint32_t lb_u32(const lb_t b, int off) { uint32_t v; memcpy(&v, b + off, 4); return v; }
static uint64_t lb_u64(const lb_t b, int off) { uint64_t v; memcpy(&v, b + off, 8); return v; }
static void *lb_ptr(const lb_t b, int off) { void *v; memcpy(&v, b + off, sizeof(v)); return v; }

/* stdio printf and raw write(2) must not interleave out of order in a captured
 * log: flush stdio before every direct write. */
static void out(const char *s) { fflush(stdout); ssize_t r = write(1, s, strlen(s)); (void)r; }
static void outnum(const char *label, long v) {
    fflush(stdout);
    char b[192];
    int n = snprintf(b, sizeof(b), "%s%ld\n", label, v);
    ssize_t r = write(1, b, (size_t)n); (void)r;
}
static uint64_t now_us(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return (uint64_t)t.tv_sec * 1000000u + (uint64_t)t.tv_nsec / 1000u;
}
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
static uint64_t fnv(const unsigned char *p, size_t n) {
    uint64_t h = 0xcbf29ce484222325ull;
    for (size_t i = 0; i < n; i++) { h ^= p[i]; h *= 0x100000001b3ull; }
    return h;
}
static uint32_t pattern_word(uint32_t cycle, uint32_t x, uint32_t y) {
    return cycle * 2654435761u + x * 40503u + y * 2246822519u;
}
static void fill_pattern(unsigned char *base, uint32_t stride_bytes, uint32_t cycle) {
    for (uint32_t y = 0; y < H; y++) {
        unsigned char *row = base + (size_t)y * stride_bytes;
        for (uint32_t x = 0; x < W; x++) {
            uint32_t v = pattern_word(cycle, x, y);
            memcpy(row + (size_t)x * 4, &v, 4);
        }
    }
}

/* ---- hang diagnosis: SIGALRM dumps the stack (no gdb in the sandbox) ---- */
static void on_fatal(int sig) {
    void *bt[40];
    int n = backtrace(bt, 40);
    dprintf(2, "[g1-blast] stuck/signal %d, %d frames:\n", sig, n);
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

/* ---- C1 bridge: one SOCK_SEQPACKET frame (acos_binder_peer wire form) ---- */
static int g1_sock = -1;
static int g_bridge_fd = -1;
static void *g_bridge_bits;

struct g1_hdr { uint32_t ty, seq, code, flags; uint64_t target, dsize, osize, rsv; };

static int g1_send(uint32_t seq, const void *data, size_t len, int fd) {
    struct g1_hdr h;
    memset(&h, 0, sizeof(h));
    h.ty = 1; /* F_REQ */
    h.seq = seq;
    h.dsize = len;
    struct iovec iov[2] = {
        { .iov_base = &h, .iov_len = sizeof(h) },
        { .iov_base = (void *)data, .iov_len = len },
    };
    char cbuf[CMSG_SPACE(sizeof(int))];
    struct msghdr msg;
    memset(&msg, 0, sizeof(msg));
    msg.msg_iov = iov;
    msg.msg_iovlen = 2;
    if (fd >= 0) {
        memset(cbuf, 0, sizeof(cbuf));
        msg.msg_control = cbuf;
        msg.msg_controllen = sizeof(cbuf);
        struct cmsghdr *c = CMSG_FIRSTHDR(&msg);
        c->cmsg_level = SOL_SOCKET;
        c->cmsg_type = SCM_RIGHTS;
        c->cmsg_len = CMSG_LEN(sizeof(int));
        memcpy(CMSG_DATA(c), &fd, sizeof(int));
    }
    for (;;) {
        ssize_t n = sendmsg(g1_sock, &msg, MSG_NOSIGNAL);
        if (n >= 0) return 0;
        if (errno != EINTR) return -1;
    }
}

struct g1_reply { int32_t status; uint32_t released_frame; uint32_t released_ok;
                  uint64_t release_calls; uint64_t latch_checksum; uint64_t stored_bytes; };

static int g1_recv(struct g1_reply *r) {
    unsigned char buf[G1_HDR + 4096];
    ssize_t n = recv(g1_sock, buf, sizeof(buf), 0);
    if (n < (ssize_t)(G1_HDR + sizeof(*r))) return -1;
    memcpy(r, buf + G1_HDR, sizeof(*r));
    return 0;
}

static int g1_connect(const char *path) {
    struct sockaddr_un sa;
    memset(&sa, 0, sizeof(sa));
    sa.sun_family = AF_UNIX;
    if (strlen(path) >= sizeof(sa.sun_path)) return -2;
    strcpy(sa.sun_path, path);
    int fd = socket(AF_UNIX, SOCK_SEQPACKET | SOCK_CLOEXEC, 0);
    if (fd < 0) return -1;
    if (connect(fd, (struct sockaddr *)&sa, sizeof(sa)) != 0) { close(fd); return -1; }
    return fd;
}

/* ---- shared state between the two threads ---- */
static unsigned char *cc;
static void *surf;
static uint64_t prod_sum[CYCLES];
static uint64_t cons_sum[CYCLES];
static uint64_t prod_ts[CYCLES];
static uint64_t cons_ts[CYCLES];
static int prod_ok[CYCLES];
static int cons_ok[CYCLES];
static volatile int cons_done;
static long cons_fail;
static int bridge_on;
static uint64_t latch_checked, releases_seen, releases_ok, latch_mismatch;

static void *consumer_thread(void *arg) {
    (void)arg;
    lb_t lb;
    for (int i = 0; i < CYCLES; i++) {
        uint64_t t0 = now_us();
        memset(lb, 0, sizeof(lb));
        /* In this image `BufferQueueConsumer::acquireBuffer` returns
         * NO_BUFFER_AVAILABLE instead of waiting when the queue is empty, and
         * CpuConsumer maps that to BAD_VALUE (-22). On a device the frame is
         * there because `onFrameAvailable` ran; here the consumer thread polls
         * (the Java side does the same through its looper callback). */
        int32_t rc;
        uint64_t deadline = now_us() + 5000000u;
        for (;;) {
            rc = cc_lock_next(cc, lb);
            if (rc != -22) break;
            if (now_us() > deadline) break;
            usleep(200);
        }
        if (rc != 0) {
            outnum("[g1-blast] FAIL CpuConsumer::lockNextBuffer rc = ", rc);
            cons_fail++;
            break;
        }
        uint32_t frame = (uint32_t)lb_u64(lb, LB_FRAME);
        void *data = lb_ptr(lb, LB_DATA);
        uint32_t stride = lb_u32(lb, LB_STRIDE);
        int shape_ok = data && lb_u32(lb, LB_W) == W && lb_u32(lb, LB_H) == H &&
                       stride >= W && lb_u32(lb, LB_FORMAT) == (uint32_t)FMT_RGBA_8888;
        uint32_t expect_first = pattern_word((uint32_t)i, 0, 0);
        uint32_t first = 0;
        if (data) memcpy(&first, data, 4);
        if (data) cons_sum[i] = fnv((const unsigned char *)data, (size_t)stride * H * 4u);
        cons_ok[i] = shape_ok && first == expect_first;
        if (!cons_ok[i]) {
            outnum("[g1-blast] FAIL consumer shape/pattern at frame = ", (long)frame);
            outnum("[g1-blast] FAIL   first word seen = ", (long)first);
            outnum("[g1-blast] FAIL   first word expected = ", (long)expect_first);
            outnum("[g1-blast] FAIL   shape ok = ", shape_ok);
            cons_fail++;
        }
        int32_t urc = cc_unlock(cc, lb);
        cons_ts[i] = now_us() - t0;
        if (urc != 0) {
            outnum("[g1-blast] FAIL CpuConsumer::unlockBuffer rc = ", urc);
            cons_fail++;
            break;
        }
    }
    cons_done = 1;
    return NULL;
}

int main(void) {
    struct sigaction sa;
    memset(&sa, 0, sizeof(sa));
    sa.sa_handler = on_fatal;
    sigaction(SIGABRT, &sa, NULL);
    sigaction(SIGSEGV, &sa, NULL);
    sigaction(SIGBUS, &sa, NULL);
    sigaction(SIGALRM, &sa, NULL);
    alarm(150); /* a hang must print a stack, not just time out */

    const char *sockp = getenv("G1_C1_SOCK");
    bridge_on = sockp && *sockp;
    if (bridge_on) {
        g1_sock = g1_connect(sockp);
        outnum("[g1-blast] C1 bridge connect rc = ", g1_sock);
        if (g1_sock < 0) return 2;
        g_bridge_fd = (int)syscall(319 /* memfd_create */, "g1-frame", 1 /* CLOEXEC */);
        if (g_bridge_fd < 0 || ftruncate(g_bridge_fd, (off_t)BUF_BYTES) != 0) {
            out("[g1-blast] FAIL bridge memfd\n");
            return 2;
        }
        g_bridge_bits = mmap(NULL, BUF_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, g_bridge_fd, 0);
        if (g_bridge_bits == MAP_FAILED) { out("[g1-blast] FAIL bridge mmap\n"); return 2; }
    }

    out("[g1-blast] stock BufferQueue 1080x2400 RGBA_8888, 3 slots, two threads\n");
    outnum("[g1-blast] per-buffer payload (host view) = ", (long)BUF_BYTES);

    sp_t producer = NULL, consumer = NULL;
    bq_create(&producer, &consumer, 0);
    if (!producer || !consumer) { out("[g1-blast] FAIL no queue\n"); return 2; }

    outnum("[g1-blast] consumer setDefaultBufferSize rc = ", bqc_set_size(consumer, W, H));
    outnum("[g1-blast] consumer setDefaultBufferFormat rc = ", bqc_set_format(consumer, FMT_RGBA_8888));
    outnum("[g1-blast] consumer setMaxBufferCount(3) rc = ", bqc_set_max_count(consumer, SLOTS));
    outnum("[g1-blast] consumer setMaxAcquiredBufferCount(1) rc = ", bqc_set_max_acquired(consumer, MAX_LOCKED));

    cc = (unsigned char *)calloc(1, STORAGE);
    cc_ctor(cc, &consumer, MAX_LOCKED, 0); /* ctor re-sets maxAcquired = 1 */
    {
        void **vt = *(void ***)cc;
        long vboff = ((long *)vt)[-3];
        void *rb = (void *)(cc + vboff);
        refbase_inc_strong(rb, rb);
    }

    surf = (unsigned char *)calloc(1, STORAGE);
    sp_t binder = NULL;
    surf_ctor(surf, &producer, 0, &binder);
    outnum("[g1-blast] producer setMaxDequeuedBufferCount(1) rc = ", bqp_set_max_dequeued(producer, 1));
    outnum("[g1-blast] Surface::connect(CPU) rc = ", surf_connect(surf, 2 /* API_CPU */));
    outnum("[g1-blast] Surface::setBuffersDimensions rc = ", surf_set_dims(surf, W, H));
    {
        struct g1_rect full = {0, 0, (int32_t)W, (int32_t)H};
        outnum("[g1-blast] Surface::setCrop rc = ", surf_set_crop(surf, &full));
    }
    long rss0 = rss_kb();
    outnum("[g1-blast] Rss before any buffer (kB) = ", rss0);

    pthread_t th;
    if (pthread_create(&th, NULL, consumer_thread, NULL) != 0) {
        out("[g1-blast] FAIL pthread_create\n");
        return 2;
    }

    for (int i = 0; i < CYCLES; i++) {
        uint64_t t0 = now_us();
        ANWB anwb;
        memset(&anwb, 0, sizeof(anwb));
        struct g1_rect dirty = {0, 0, (int32_t)W, (int32_t)H};
        int32_t rc = surf_lock(surf, &anwb, &dirty);
        if (rc != 0 || !anwb.bits || anwb.width != (int32_t)W || anwb.height != (int32_t)H ||
            anwb.stride < (int32_t)W) {
            outnum("[g1-blast] FAIL Surface::lock rc = ", rc);
            outnum("[g1-blast] FAIL   w = ", anwb.width);
            outnum("[g1-blast] FAIL   stride = ", anwb.stride);
            break;
        }
        uint32_t stride_bytes = (uint32_t)anwb.stride * 4u;
        fill_pattern((unsigned char *)anwb.bits, stride_bytes, (uint32_t)i);
        prod_sum[i] = fnv((const unsigned char *)anwb.bits, (size_t)stride_bytes * H);
        if (bridge_on)
            memcpy(g_bridge_bits, anwb.bits, (size_t)stride_bytes * H);
        rc = surf_unlock_and_post(surf);
        if (rc != 0) { outnum("[g1-blast] FAIL Surface::unlockAndPost rc = ", rc); break; }
        prod_ts[i] = now_us() - t0;
        prod_ok[i] = 1;

        if (bridge_on) {
            /* The shim's 88-byte GraphicBuffer wire form + this frame's memfd. */
            unsigned char payload[88 + 24];
            memset(payload, 0, sizeof(payload));
            int32_t *w32 = (int32_t *)payload;
            /* 13 header words */
            w32[0] = 0x31304247; /* 'GB01' */
            w32[1] = (int32_t)W;
            w32[2] = (int32_t)H;
            w32[3] = (int32_t)anwb.stride;
            w32[4] = FMT_RGBA_8888;
            w32[5] = 1;      /* layers */
            w32[6] = 0;      /* usage lo */
            w32[7] = 0;      /* id hi */
            w32[8] = i + 1;  /* id lo */
            w32[9] = 0;      /* generation */
            w32[10] = 1;     /* handle numFds */
            w32[11] = 9;     /* handle numInts */
            w32[12] = 0;     /* usage hi */
            /* 9 handle ints */
            w32[13] = (int32_t)W;
            w32[14] = (int32_t)H;
            w32[15] = (int32_t)anwb.stride;
            w32[16] = FMT_RGBA_8888;
            w32[17] = 0;
            w32[18] = 0;
            w32[19] = 1;     /* layers */
            w32[20] = 4;     /* bpp */
            w32[21] = 0;     /* dataspace */
            struct { uint32_t w, h, stride, format; uint64_t checksum; } meta = {
                W, H, (uint32_t)anwb.stride, FMT_RGBA_8888, prod_sum[i] };
            memcpy(payload + 88, &meta, sizeof(meta));
            if (g1_send((uint32_t)i, payload, sizeof(payload), g_bridge_fd) != 0) {
                outnum("[g1-blast] FAIL bridge send errno = ", errno);
                break;
            }
            struct g1_reply rep;
            if (g1_recv(&rep) != 0) { out("[g1-blast] FAIL bridge reply\n"); break; }
            if (rep.status != 0) { outnum("[g1-blast] FAIL C1 status = ", rep.status); break; }
            if (rep.latch_checksum != prod_sum[i]) {
                latch_mismatch++;
                outnum("[g1-blast] FAIL C1 latch checksum at frame = ", i);
            }
            latch_checked++;
            releases_seen = rep.release_calls;
            if (rep.released_ok) releases_ok++;
        }
    }

    pthread_join(th, NULL);
    alarm(0);

    long queued = 0, acquired = 0, pixel_ok = 0;
    uint64_t psum = 0, csum = 0;
    for (int i = 0; i < CYCLES; i++) {
        queued += prod_ok[i];
        if (cons_sum[i] || cons_ok[i]) acquired++;
        if (cons_ok[i]) pixel_ok++;
        psum += prod_ts[i];
        csum += cons_ts[i];
    }
    for (int i = 0; i < CYCLES; i++) {
        printf("[g1-blast] cycle %02d produce_us=%5llu consume_us=%5llu "
               "prod_check=%016llx cons_check=%016llx %s\n", i,
               (unsigned long long)prod_ts[i], (unsigned long long)cons_ts[i],
               (unsigned long long)prod_sum[i], (unsigned long long)cons_sum[i],
               (prod_ok[i] && cons_sum[i] == prod_sum[i]) ? "match" : "MISMATCH");
    }
    for (int side = 0; side < 2; side++) {
        uint64_t v[CYCLES];
        int n = 0;
        for (int i = 0; i < CYCLES; i++) {
            uint64_t x = side ? cons_ts[i] : prod_ts[i];
            if (x) v[n++] = x;
        }
        for (int i = 1; i < n; i++) {
            uint64_t k = v[i];
            int j = i - 1;
            while (j >= 0 && v[j] > k) { v[j + 1] = v[j]; j--; }
            v[j + 1] = k;
        }
        printf("[g1-blast] %s latency us: n=%d min=%llu median=%llu p95=%llu max=%llu\n",
               side ? "consume" : "produce", n, (unsigned long long)(n ? v[0] : 0),
               (unsigned long long)(n ? v[n / 2] : 0),
               (unsigned long long)(n ? v[(n * 95) / 100] : 0),
               (unsigned long long)(n ? v[n - 1] : 0));
    }
    outnum("[g1-blast] cycles attempted   = ", CYCLES);
    outnum("[g1-blast] queued             = ", queued);
    outnum("[g1-blast] acquired+released   = ", acquired);
    outnum("[g1-blast] consumer pixel ok   = ", pixel_ok);
    outnum("[g1-blast] consumer failures   = ", cons_fail);
    outnum("[g1-blast] mean produce us     = ", (long)(psum / (queued ? queued : 1)));
    outnum("[g1-blast] mean consume us     = ", (long)(csum / (queued ? queued : 1)));
    if (bridge_on) {
        outnum("[g1-blast] C1 latch checksums  = ", (long)latch_checked);
        outnum("[g1-blast] C1 latch mismatches = ", (long)latch_mismatch);
        outnum("[g1-blast] C1 release cb frames= ", (long)releases_ok);
        outnum("[g1-blast] C1 release calls    = ", (long)releases_seen);
    }
    long rss1 = rss_kb();
    outnum("[g1-blast] Rss after cycles (kB) = ", rss1);
    outnum("[g1-blast] Rss growth total (kB) = ", rss1 - rss0);

    int pass = queued == CYCLES && acquired == CYCLES && pixel_ok == CYCLES && cons_fail == 0;
    if (bridge_on)
        pass = pass && latch_checked == CYCLES && latch_mismatch == 0 && releases_ok + 1 >= CYCLES;
    out(pass ? "[g1-blast] PASS: 60 consecutive dequeue/write/queue/acquire/release cycles\n"
             : "[g1-blast] FAIL\n");
    return pass ? 0 : 1;
}
