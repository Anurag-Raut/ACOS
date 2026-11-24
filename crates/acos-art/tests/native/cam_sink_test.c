// Native test for the render_shim.c camera frame sink (section 4 of the shim).
//
// The Rust test `tests/cam_sink_native.rs` slices that section out of
// `shim/render_shim.c` (so this exercises the real code, not a copy), compiles it
// together with this file on the host, and runs it. Every Android window / buffer
// call the sink makes goes through `g_cam_api`, so a fake BufferQueue stands in
// for libgui here. The fake follows android::BufferQueueProducer: a second
// connect on a connected producer fails, disconnect wakes a blocked dequeue and
// a dequeue with no free buffer blocks (timeout < 0) or times out (timeout >= 0).
//
// Usage: cam_sink_test <case>     exit 0 = pass; a hang is killed by alarm().
#define _GNU_SOURCE
#include <jni.h>
#include <dlfcn.h>
#include <errno.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>
#include <sys/mman.h>
#include <sys/stat.h>

#define LOG(...)                         \
    do {                                 \
        dprintf(2, "[cam-sink] ");       \
        dprintf(2, __VA_ARGS__);         \
        dprintf(2, "\n");                \
    } while (0)

#include "cam_sink_slice.inc"

#define CHECK(cond, ...)                                        \
    do {                                                        \
        if (!(cond)) {                                          \
            dprintf(2, "CHECK FAILED %s:%d: %s: ", __FILE__, __LINE__, #cond); \
            dprintf(2, __VA_ARGS__);                            \
            dprintf(2, "\n");                                   \
            exit(1);                                            \
        }                                                       \
    } while (0)

// ------------------------------------------------------------- fake window

typedef struct FakeWin {
    char pad[ACOS_ANW_PERFORM_OFFSET];
    fn_ANW_perform perform;
    pthread_mutex_t m;
    pthread_cond_t c;
    int connected;
    int connect_calls;
    int connect_failed;
    int disconnect_calls;
    int free_buffers;
    int64_t timeout_ns;  // -1 = block forever, like BufferQueueProducer's default
    int timeout_calls;
    int queued;
    int cancelled;
    int dequeue_calls;
    int refs;
    int dequeue_waiting;
    int timeout_supported;  // perform(SET_DEQUEUE_TIMEOUT) works on this producer
} FakeWin;

static char g_pixels[64 * 48 * 4 + 4096];
static int g_ahb_dummy;
static int g_buf_dummy;

static int fake_perform(ANativeWindow* w, int op, ...) {
    FakeWin* f = (FakeWin*)w;
    pthread_mutex_lock(&f->m);
    int rc = 0;
    if (op == ACOS_NATIVE_WINDOW_API_CONNECT) {
        f->connect_calls++;
        if (f->connected) {
            f->connect_failed++;
            rc = -22;  // BufferQueueProducer::connect: already connected -> BAD_VALUE
        } else {
            f->connected = 1;
        }
    } else if (op == 37 /* NATIVE_WINDOW_SET_DEQUEUE_TIMEOUT */) {
        va_list ap;
        va_start(ap, op);
        int64_t ns = va_arg(ap, int64_t);
        va_end(ap);
        if (f->timeout_supported) {
            f->timeout_ns = ns;
            f->timeout_calls++;
        } else {
            rc = -22;
        }
    } else if (op == ACOS_NATIVE_WINDOW_API_DISCONNECT) {
        f->disconnect_calls++;
        if (!f->connected) {
            rc = -22;
        } else {
            f->connected = 0;
            pthread_cond_broadcast(&f->c);
        }
    }
    pthread_mutex_unlock(&f->m);
    return rc;
}

static void fake_init(FakeWin* f, int free_buffers) {
    memset(f, 0, sizeof(*f));
    f->perform = fake_perform;
    pthread_mutex_init(&f->m, NULL);
    pthread_cond_init(&f->c, NULL);
    f->free_buffers = free_buffers;
    f->timeout_ns = -1;
    f->timeout_supported = 1;
}

static ANativeWindow* fake_fromSurface(JNIEnv* env, jobject surface) {
    (void)env;
    FakeWin* f = (FakeWin*)surface;
    pthread_mutex_lock(&f->m);
    f->refs++;
    pthread_mutex_unlock(&f->m);
    return (ANativeWindow*)f;
}
static void fake_release(ANativeWindow* w) {
    FakeWin* f = (FakeWin*)w;
    pthread_mutex_lock(&f->m);
    f->refs--;
    pthread_mutex_unlock(&f->m);
}
static int32_t fake_dim(ANativeWindow* w) { (void)w; return 64; }
static int32_t fake_dim_h(ANativeWindow* w) { (void)w; return 48; }
static int32_t fake_fmt(ANativeWindow* w) { (void)w; return 1; }
static int32_t fake_usage(ANativeWindow* w, uint64_t u) { (void)w; (void)u; return 0; }
static int32_t fake_dims(ANativeWindow* w, int32_t a, int32_t b) { (void)w; (void)a; (void)b; return 0; }
static int32_t fake_setfmt(ANativeWindow* w, int32_t a) { (void)w; (void)a; return 0; }
static int32_t fake_ts(ANativeWindow* w, int64_t a) { (void)w; (void)a; return 0; }

static int32_t fake_dequeue(ANativeWindow* w, ANativeWindowBuffer** buf, int* fence) {
    FakeWin* f = (FakeWin*)w;
    *fence = -1;
    pthread_mutex_lock(&f->m);
    f->dequeue_calls++;
    int rc;
    for (;;) {
        if (!f->connected) { rc = -19; break; }  // NO_INIT
        if (f->free_buffers > 0) {
            f->free_buffers--;
            *buf = (ANativeWindowBuffer*)&g_buf_dummy;
            rc = 0;
            break;
        }
        if (f->timeout_ns >= 0) { rc = -ETIMEDOUT; break; }
        f->dequeue_waiting++;
        pthread_cond_wait(&f->c, &f->m);
        f->dequeue_waiting--;
    }
    pthread_mutex_unlock(&f->m);
    return rc;
}
static int32_t fake_queue(ANativeWindow* w, ANativeWindowBuffer* b, int fence) {
    (void)b; (void)fence;
    FakeWin* f = (FakeWin*)w;
    pthread_mutex_lock(&f->m);
    int rc = f->connected ? 0 : -19;
    if (rc == 0) f->queued++;
    pthread_mutex_unlock(&f->m);
    return rc;
}
static int32_t fake_cancel(ANativeWindow* w, ANativeWindowBuffer* b, int fence) {
    (void)b; (void)fence;
    FakeWin* f = (FakeWin*)w;
    pthread_mutex_lock(&f->m);
    f->cancelled++;
    f->free_buffers++;
    pthread_cond_broadcast(&f->c);
    pthread_mutex_unlock(&f->m);
    return 0;
}
static AHardwareBuffer* fake_getAHB(ANativeWindowBuffer* b) { (void)b; return (AHardwareBuffer*)&g_ahb_dummy; }
static void fake_describe(const AHardwareBuffer* a, acos_ahb_desc_t* d) {
    (void)a;
    d->width = 64; d->height = 48; d->layers = 1; d->format = 1; d->stride = 64;
}
static int fake_lockPlanes(AHardwareBuffer* a, uint64_t u, int32_t f, const acos_arect_t* r, acos_ahb_planes_t* p) {
    (void)a; (void)u; (void)f; (void)r; (void)p;
    return -ENOSYS;
}
static int fake_lock(AHardwareBuffer* a, uint64_t u, int32_t f, const acos_arect_t* r, void** v) {
    (void)a; (void)u; (void)f; (void)r;
    *v = g_pixels;
    return 0;
}
static int fake_unlock(AHardwareBuffer* a, int32_t* f) { (void)a; (void)f; return 0; }

static void install_fakes(int with_dequeue_timeout) {
    memset(&g_cam_api, 0, sizeof(g_cam_api));
    g_cam_api.fromSurface = fake_fromSurface;
    g_cam_api.release = fake_release;
    g_cam_api.getWidth = fake_dim;
    g_cam_api.getHeight = fake_dim_h;
    g_cam_api.getFormat = fake_fmt;
    g_cam_api.setUsage = fake_usage;
    g_cam_api.setDims = fake_dims;
    g_cam_api.setFormat = fake_setfmt;
    g_cam_api.setTimestamp = fake_ts;
    g_cam_api.dequeue = fake_dequeue;
    g_cam_api.queue = fake_queue;
    g_cam_api.cancel = fake_cancel;
    g_cam_api.getAHB = fake_getAHB;
    g_cam_api.describe = fake_describe;
    g_cam_api.lockPlanes = fake_lockPlanes;
    g_cam_api.lock = fake_lock;
    g_cam_api.unlock = fake_unlock;
    (void)with_dequeue_timeout;
    g_cam_api.loaded = 1;
}

static jint reg(int stream, FakeWin* f) {
    return Java_dev_acos_art_RenderNat_cameraSinkRegister(NULL, NULL, stream, (jobject)f, 64, 48);
}
static jint deliver(int stream, int frame) {
    return Java_dev_acos_art_RenderNat_cameraSinkDeliver(NULL, NULL, stream, 64, 48, frame,
                                                          (jlong)frame * 1000, 1);
}
static jint release_stream(int stream) {
    return Java_dev_acos_art_RenderNat_cameraSinkRelease(NULL, NULL, stream);
}

static int64_t now_ms(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return (int64_t)t.tv_sec * 1000 + t.tv_nsec / 1000000;
}

typedef struct { int stream; int frame; volatile int rc; volatile int done; } Job;
static void* deliver_thread(void* p) {
    Job* j = (Job*)p;
    j->rc = deliver(j->stream, j->frame);
    j->done = 1;
    return NULL;
}
static void wait_until_dequeue_blocked(FakeWin* f) {
    for (int i = 0; i < 500; i++) {
        pthread_mutex_lock(&f->m);
        int w = f->dequeue_waiting;
        pthread_mutex_unlock(&f->m);
        if (w > 0) return;
        usleep(10 * 1000);
    }
    CHECK(0, "a delivery never reached the blocking dequeue");
}

// ---------------------------------------------------------------- the cases

// #17: a stream whose consumer never frees a buffer parks a delivery inside
// dequeue (no dequeue timeout available). That must not stall delivery to another
// stream, nor `release`, and release must wake the parked delivery.
static void case_blocked_dequeue_does_not_wedge(void) {
    install_fakes(0);
    static FakeWin a, b;
    fake_init(&a, 1);
    a.timeout_supported = 0;  // a producer whose dequeue cannot be bounded
    fake_init(&b, 1000);
    CHECK(reg(10, &a) == 0, "register a");
    CHECK(reg(11, &b) == 0, "register b");
    CHECK(deliver(10, 1) == 0, "first frame fits the single free buffer");
    pthread_t t;
    Job job = {10, 2, 0, 0};
    pthread_create(&t, NULL, deliver_thread, &job);
    wait_until_dequeue_blocked(&a);

    int64_t t0 = now_ms();
    CHECK(deliver(11, 3) == 0, "delivery to the other stream while stream 10 is parked");
    CHECK(b.queued == 1, "other stream got its frame");
    CHECK(now_ms() - t0 < 2000, "other stream delivery was not delayed");

    t0 = now_ms();
    CHECK(release_stream(10) == 1, "release of the stalled stream");
    CHECK(now_ms() - t0 < 2000, "release was not delayed");
    pthread_join(t, NULL);
    CHECK(job.done, "the parked delivery was woken by the release");
    CHECK(job.rc != 0, "the woken delivery reports failure, not a delivered frame");
    CHECK(a.refs == 0, "window reference of the closed stream dropped (refs=%d)", a.refs);
    CHECK(a.connected == 0, "producer disconnected");
    CHECK(release_stream(-1) == 1, "release(-1) closes the remaining stream");
    CHECK(b.refs == 0, "second window released");
}

// #17: with a dequeue timeout installed at registration a full queue DROPS the
// frame: no delay, the delivery reports no error, nothing is queued.
static void case_full_queue_drops_frame(void) {
    install_fakes(1);
    static FakeWin a;
    fake_init(&a, 1);
    CHECK(reg(20, &a) == 0, "register");
#ifdef ACOS_CAM_DEQUEUE_TIMEOUT_NS
    CHECK(a.timeout_calls == 1 && a.timeout_ns == ACOS_CAM_DEQUEUE_TIMEOUT_NS,
          "dequeue timeout installed (calls=%d ns=%lld)", a.timeout_calls, (long long)a.timeout_ns);
#else
    CHECK(0, "this shim has no dequeue timeout");
#endif
    CHECK(deliver(20, 1) == 0, "frame 1 delivered");
    int64_t t0 = now_ms();
    CHECK(deliver(20, 2) == 0, "frame 2 dropped, not an error");
    CHECK(deliver(20, 3) == 0, "frame 3 dropped, not an error");
    CHECK(now_ms() - t0 < 1000, "drops are immediate");
    CHECK(a.queued == 1, "only the first frame was queued (queued=%d)", a.queued);
    CHECK(release_stream(20) == 1, "release");
}

// Release/replace of a stream with deliveries in flight waits for them and
// leaves no leaked window reference.
static void case_replace_waits_for_inflight(void) {
    install_fakes(0);
    static FakeWin a;
    fake_init(&a, 1000);
    CHECK(reg(30, &a) == 0, "register");
    for (int i = 0; i < 50; i++) CHECK(deliver(30, i) == 0, "deliver %d", i);
    CHECK(a.queued == 50, "all frames queued");
    CHECK(release_stream(30) == 1, "release");
    CHECK(a.refs == 0 && a.connected == 0, "released and disconnected");
    CHECK(deliver(30, 99) == -ENOENT, "deliver after release is ENOENT");
}

int main(int argc, char** argv) {
    alarm(10);  // a wedged sink is a failure, not a hung test run
    if (argc < 2) { dprintf(2, "usage: cam_sink_test <case>\n"); return 2; }
    const char* c = argv[1];
    if (!strcmp(c, "blocked_dequeue_does_not_wedge")) case_blocked_dequeue_does_not_wedge();
    else if (!strcmp(c, "full_queue_drops_frame")) case_full_queue_drops_frame();
    else if (!strcmp(c, "replace_waits_for_inflight")) case_replace_waits_for_inflight();
    else { dprintf(2, "unknown case %s\n", c); return 2; }
    printf("PASS %s\n", c);
    return 0;
}
