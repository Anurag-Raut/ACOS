// driver_test.c -- host test for the bionic binder driver's death paths (MPROC5b).
//
// The driver is a guest-side `LD_PRELOAD`; its command paths are static
// functions, so this test includes the whole translation unit and drives
// `handle_commands` / `e3g_write_read` directly against a socketpair that
// stands in for the Rust peer. No ART, no guest, no NDK: just the node/death
// bookkeeping the peer's `F_DEATH` / dead `F_RSP` frames must produce.
//
// What it pins (the shapes libbinder itself reads back):
//   1. `BC_REQUEST_DEATH_NOTIFICATION(handle, cookie)` registers; an `F_DEATH`
//      for that handle's peer node delivers one `BR_DEAD_BINDER` carrying
//      **that exact cookie** (`BpBinder*`), which
//      `IPCThreadState::executeCommand(BR_DEAD_BINDER)` passes to
//      `proxy->sendObituary()`;
//   2. the registration is one-shot: a second `F_DEATH` queues nothing, and
//      `BC_DEAD_BINDER_DONE` is consumed (12/8-byte command bodies, not
//      silently mis-parsed);
//   3. a synchronous `BC_TRANSACTION` answered with `F_RSP` + `E3G_RSP_DEAD`
//      reaches the caller as `BR_DEAD_REPLY` (DeadObjectException), not
//      `BR_FAILED_REPLY`.
//
// Build/run (see results/handoff/MPROC5b/evidence-driver-test.txt):
//   cc -O0 -g -I"$JAVA_HOME/include" -I"$JAVA_HOME/include/linux" \
//      crates/acos-binder-peer/native/driver_test.c -o driver_test -lpthread -ldl
//   ./driver_test

#define E3G_HOST_TEST 1
#include "binder_driver.c"

static int g_failures;

#define CHECK(cond, ...)                                    \
    do {                                                    \
        if (!(cond)) {                                      \
            dprintf(2, "FAIL %s:%d: ", __FILE__, __LINE__); \
            dprintf(2, __VA_ARGS__);                        \
            dprintf(2, "\n");                               \
            g_failures++;                                   \
        } else {                                            \
            dprintf(2, "ok   ");                            \
            dprintf(2, __VA_ARGS__);                        \
            dprintf(2, "\n");                               \
        }                                                   \
    } while (0)

static int new_memfd(void) {
    int fd = (int)syscall(__NR_memfd_create, "acos-driver-test", 0);
    if (fd < 0) {
        dprintf(2, "memfd_create: %s\n", strerror(errno));
        exit(2);
    }
    return fd;
}

// The peer side of the transport: an explicit-fd copy of `send_frame`, so the
// test never swaps `g.sock` under the reader thread.
static void peer_send(int fd, uint32_t type, uint32_t seq, uint32_t code, uint32_t flags,
                      uint64_t target, const void* data, size_t dsize) {
    struct fhdr h = {0};
    h.type = type;
    h.seq = seq;
    h.code = code;
    h.flags = flags;
    h.target = target;
    h.data_size = dsize;
    struct iovec iov[2];
    int niov = 0;
    iov[niov].iov_base = &h;
    iov[niov].iov_len = sizeof(h);
    niov++;
    if (dsize) {
        iov[niov].iov_base = (void*)data;
        iov[niov].iov_len = dsize;
        niov++;
    }
    struct msghdr msg = {0};
    msg.msg_iov = iov;
    msg.msg_iovlen = (size_t)niov;
    ssize_t n;
    do {
        n = sendmsg(fd, &msg, MSG_NOSIGNAL);
    } while (n < 0 && errno == EINTR);
    if (n < 0) {
        dprintf(2, "peer sendmsg: %s\n", strerror(errno));
        exit(2);
    }
}

// The peer side receives one frame header (+ payload) from the driver.
static int peer_recv(int fd, struct fhdr* h, uint8_t* payload, size_t cap) {
    uint8_t buf[sizeof(struct fhdr) + E3G_MAX_FRAME];
    ssize_t n;
    do {
        n = recv(fd, buf, sizeof(buf), MSG_WAITALL);
    } while (n < 0 && errno == EINTR);
    if (n < (ssize_t)sizeof(*h)) return -1;
    memcpy(h, buf, sizeof(*h));
    if (h->data_size + h->offsets_size > cap) return -1;
    memcpy(payload, buf + sizeof(*h), h->data_size + h->offsets_size);
    return (int)n;
}

// Write one BC_* command stream exactly as libbinder's IPCThreadState would.
static void bc_write(const void* buf, size_t n) {
    struct binder_write_read bwr;
    memset(&bwr, 0, sizeof(bwr));
    bwr.write_size = n;
    bwr.write_buffer = (binder_uintptr_t)buf;
    CHECK(e3g_write_read(&bwr) == 0, "e3g_write_read(write %zu) returns 0", n);
    CHECK((size_t)bwr.write_consumed == n, "the whole %zu-byte command was consumed (%llu)", n,
          (unsigned long long)bwr.write_consumed);
}

// One read of at most `cap` bytes of BR_* commands; returns the byte count.
static size_t br_read(uint8_t* out, size_t cap) {
    struct binder_write_read bwr;
    memset(&bwr, 0, sizeof(bwr));
    bwr.read_size = cap;
    bwr.read_buffer = (binder_uintptr_t)out;
    CHECK(e3g_write_read(&bwr) == 0, "e3g_write_read(read) returns 0");
    return (size_t)bwr.read_consumed;
}

static void request_death_cmd(uint32_t handle, uint64_t cookie) {
    uint8_t w[4 + 12];
    uint32_t cmd = BC_REQUEST_DEATH_NOTIFICATION;
    memcpy(w, &cmd, 4);
    memcpy(w + 4, &handle, 4);
    memcpy(w + 8, &cookie, 8);
    bc_write(w, sizeof(w));
}

static void dead_binder_done_cmd(uint64_t cookie) {
    uint8_t w[4 + 8];
    uint32_t cmd = BC_DEAD_BINDER_DONE;
    memcpy(w, &cmd, 4);
    memcpy(w + 4, &cookie, 8);
    bc_write(w, sizeof(w));
}

int main(void) {
    // Boot the driver without a peer socket; the test supplies the transport.
    unsetenv("E3G_SOCK");
    driver_init(new_memfd());
    int sv[2];
    if (socketpair(AF_UNIX, SOCK_SEQPACKET | SOCK_CLOEXEC, 0, sv) != 0) {
        dprintf(2, "socketpair: %s\n", strerror(errno));
        return 2;
    }
    g.sock = sv[0];
    if (pthread_create(&g.reader, NULL, reader_main, NULL) != 0) {
        dprintf(2, "reader thread did not start\n");
        return 2;
    }
    g.reader_started = 1;
    // This thread dispatches callbacks, exactly like libbinder's looper.
    T.is_looper = 1;

    // ---- 1. BC_REQUEST_DEATH_NOTIFICATION -> F_DEATH -> BR_DEAD_BINDER -----
    const uint64_t node = 0x2f00000000ull | 1;
    const uint64_t cookie = 0x0000deadbeefcafeull;
    uint32_t handle = handle_for_peer_node(node);
    CHECK(handle != 0, "handle_for_peer_node(%llx) -> handle %u", (unsigned long long)node, handle);
    request_death_cmd(handle, cookie);
    peer_send(sv[1], F_DEATH, 0, 0, 0, node, NULL, 0);

    uint8_t rbuf[256];
    size_t got = br_read(rbuf, sizeof(rbuf));
    uint32_t cmd;
    memcpy(&cmd, rbuf, 4);
    CHECK(got == 12 && cmd == BR_DEAD_BINDER,
          "F_DEATH produced one BR_DEAD_BINDER (%zu bytes, cmd 0x%x)", got, cmd);
    uint64_t delivered = 0;
    if (got >= 12) memcpy(&delivered, rbuf + 4, 8);
    CHECK(delivered == cookie, "BR_DEAD_BINDER carries the registered cookie %llx (got %llx)",
          (unsigned long long)cookie, (unsigned long long)delivered);

    // ---- 2. one-shot: no second notification, DONE is consumed ------------
    peer_send(sv[1], F_DEATH, 0, 0, 0, node, NULL, 0);
    // Give the reader thread time to process the frame, then look at the queue.
    struct timespec nap = {0, 50 * 1000 * 1000};
    nanosleep(&nap, NULL);
    pthread_mutex_lock(&g.mu);
    int still_queued = g.death_count;
    pthread_mutex_unlock(&g.mu);
    CHECK(still_queued == 0, "a second F_DEATH for the same node queues nothing (%d)", still_queued);
    dead_binder_done_cmd(cookie);

    // ---- 3. F_RSP + E3G_RSP_DEAD -> BR_DEAD_REPLY -------------------------
    struct binder_transaction_data tr;
    memset(&tr, 0, sizeof(tr));
    tr.target.handle = handle;
    tr.code = 7;
    tr.flags = 0;
    uint8_t w[4 + sizeof(tr)];
    cmd = BC_TRANSACTION;
    memcpy(w, &cmd, 4);
    memcpy(w + 4, &tr, sizeof(tr));
    bc_write(w, sizeof(w));

    struct fhdr req;
    uint8_t payload[E3G_MAX_FRAME];
    CHECK(peer_recv(sv[1], &req, payload, sizeof(payload)) > 0,
          "the peer received the outgoing transaction frame");
    CHECK(req.type == F_REQ && req.target == node,
          "BC_TRANSACTION reached the peer with the peer node id (%u/%llx)", req.type,
          (unsigned long long)req.target);
    peer_send(sv[1], F_RSP, req.seq, 0, E3G_RSP_DEAD, 0, NULL, 0);

    int saw_dead_reply = 0, saw_failed_reply = 0;
    for (int i = 0; i < 4 && !saw_dead_reply; i++) {
        size_t n = br_read(rbuf, sizeof(rbuf));
        size_t at = 0;
        while (at + 4 <= n) {
            uint32_t c;
            memcpy(&c, rbuf + at, 4);
            at += 4;
            if (c == BR_DEAD_REPLY) saw_dead_reply = 1;
            if (c == BR_FAILED_REPLY) saw_failed_reply = 1;
            size_t len = br_cmd_len(rbuf + at - 4, n - (at - 4));
            if (len < 4) break;
            at += len - 4;
        }
    }
    CHECK(saw_dead_reply, "F_RSP + E3G_RSP_DEAD reached the caller as BR_DEAD_REPLY");
    CHECK(!saw_failed_reply, "the dead reply is never the generic BR_FAILED_REPLY");

    dprintf(2, "driver_test: %s (%d failure(s))\n", g_failures ? "FAIL" : "PASS", g_failures);
    return g_failures ? 1 : 0;
}
