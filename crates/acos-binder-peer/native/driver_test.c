// driver_test.c -- driver-level acceptance test for the userspace binder driver.
//
// No ART, no libbinder, no guest: this test drives `binder_driver.c` through the
// same entries libbinder uses (open("/dev/binder"), ioctl(BINDER_WRITE_READ)
// with BC_*/BR_* commands) against a scripted peer on a unix socket, then
// asserts the kernel's delivery rules on what the *driver* did.
//
// It is one translation unit with the driver (`#include "binder_driver.c"`) so
// the assertions can read the driver's own tables (`g.nodes`, `g.in_head`) and
// so no symbol of the driver is stubbed out.
//
// Run: `cc -O1 -pthread -o driver_test driver_test.c` (the Rust test
// `tests/driver.rs` compiles and runs it). Exit 0 = every check passed.
//
// What it covers, and why each check is a *behaviour* and not an implementation
// detail:
//
//   1. node export + root: writing a BINDER object gives the peer a wire handle
//      and makes the driver emit BR_INCREFS + BR_ACQUIRE (the two commands
//      libbinder answers with incWeak/incStrong; without them the JavaBBinder
//      behind a Java `Binder` dies the moment the parcel is written).
//   2. a host->guest transaction to that node is delivered (BR_TRANSACTION with
//      the node's weakref/cookie).
//   3. *the reported bug*: the node must survive the lifetime of the parcel that
//      first exposed it. After BC_FREE_BUFFER of the transaction buffer and a
//      full acquire/release churn on the guest side (BC_ACQUIRE/BC_RELEASE of the
//      peer handle), a host->guest transaction must still arrive. A node's
//      lifetime follows the references the remote side holds, never a
//      transaction buffer.
//   4. many callbacks in one turn: N host->guest transactions written back to
//      back must all arrive, in order, each addressed to the node it names.
//   5. a transaction to a node nobody ever exported must NOT be dropped
//      silently: the host gets a DEAD_OBJECT reply so a callback the framework
//      lost is reported, not swallowed.
//   6. several objects registered for one event, each must receive its callback
//      *at the object that is registered now*: the `JavaBBinder` behind a Java
//      `Binder` is re-created by `JavaBBinderHolder::get` (it only keeps a weak
//      pointer), and a re-export of the same cookie with a new weakref must
//      refresh the node and root the new object, keeping the wire handle.
//   7. the transaction-buffer heap is shared by every thread (the caller's
//      reply path and the looper's delivery path) and must never hand the same
//      memory to two live transactions.
//   8. a session with many registrations (more listeners than one page of the
//      node table) still reaches every one of them.
//
// Every check prints `ok N - name` or `not ok N - name: detail`; the last line
// is `RESULT failures=<n>`.
#define _GNU_SOURCE
#include "binder_driver.c"

#include <sys/socket.h>
#include <sys/un.h>

// ---------------------------------------------------------------- harness ----
static int g_failures;
static int g_check;

static void check(int cond, const char* name, const char* detail) {
    g_check++;
    if (cond) {
        printf("ok %d - %s\n", g_check, name);
    } else {
        printf("not ok %d - %s%s%s\n", g_check, name, detail && *detail ? ": " : "",
               detail ? detail : "");
        g_failures++;
    }
    fflush(stdout);
}

// ------------------------------------------------------------- fake peer ----
// The peer is what `acos-binder-peer` is to a real guest: it receives frames and
// answers requests. `peer_txn(wire, ...)` is the host-initiated callback.
struct peer {
    int listen_fd;
    int fd;
    pthread_t thread;
    pthread_mutex_t mu;
    volatile int stop;
    uint32_t requests;
    uint32_t txns_sent;
    uint32_t dead_replies;   // F_REP carrying TF_STATUS_CODE (a failed delivery)
    uint32_t bad_replies;    // F_REP carrying an exception body
};

static struct peer g_peer;

static int peer_read_full(int fd, void* buf, size_t n) {
    uint8_t* p = buf;
    while (n) {
        ssize_t r = read(fd, p, n);
        if (r == 0) return -1;
        if (r < 0) {
            if (errno == EINTR) continue;
            return -1;
        }
        p += r;
        n -= (size_t)r;
    }
    return 0;
}

static int peer_write_frame(uint32_t type, uint32_t seq, uint32_t code, uint32_t flags,
                            uint64_t target, const void* data, size_t dsize, const void* offs,
                            size_t osize) {
    struct fhdr h = {0};
    h.type = type;
    h.seq = seq;
    h.code = code;
    h.flags = flags;
    h.target = target;
    h.data_size = dsize;
    h.offsets_size = osize;
    struct iovec iov[3];
    int niov = 0;
    iov[niov].iov_base = &h;
    iov[niov].iov_len = sizeof(h);
    niov++;
    if (dsize) {
        iov[niov].iov_base = (void*)data;
        iov[niov].iov_len = dsize;
        niov++;
    }
    if (osize) {
        iov[niov].iov_base = (void*)offs;
        iov[niov].iov_len = osize;
        niov++;
    }
    struct msghdr msg = {0};
    msg.msg_iov = iov;
    msg.msg_iovlen = (size_t)niov;
    ssize_t n;
    do {
        n = sendmsg(g_peer.fd, &msg, MSG_NOSIGNAL);
    } while (n < 0 && errno == EINTR);
    return n > 0 ? 0 : -1;
}

static void* peer_main(void* arg) {
    (void)arg;
    int lfd = g_peer.listen_fd;
    int fd = accept(lfd, NULL, NULL);
    if (fd < 0) return NULL;
    g_peer.fd = fd;
    size_t cap = sizeof(struct fhdr) + E3G_MAX_FRAME + 64;
    uint8_t* buf = malloc(cap);
    for (;;) {
        struct fhdr h;
        ssize_t n = recv(fd, buf, cap, 0);
        if (n <= 0) {
            if (n < 0 && errno == EINTR) continue;
            return NULL;
        }
        if ((size_t)n < sizeof(h)) return NULL;
        memcpy(&h, buf, sizeof(h));
        g_peer.requests++;
        if (h.type == F_REQ) {
            // A real reply body: 4-byte "no exception" word + an int, so the
            // driver has something to copy into the caller's BR_REPLY.
            uint8_t body[8] = {0, 0, 0, 0, 7, 0, 0, 0};
            peer_write_frame(F_RSP, h.seq, 0, 0, h.target, body, sizeof(body), NULL, 0);
        }
        if (h.type == F_REP) {
            if (h.flags & TF_STATUS_CODE) g_peer.dead_replies++;
            else if (h.data_size > 0) g_peer.bad_replies++;
        }
    }
    return NULL;
}

// The host side of a callback: one F_TXN frame addressed at `wire`.
static int peer_txn(uint32_t wire, uint32_t code, uint32_t seq) {
    uint8_t data[16];
    memset(data, 0, sizeof(data));
    memcpy(data, &code, 4);
    int rc = peer_write_frame(F_TXN, seq, code, 0 /* sync */, wire, data, sizeof(data), NULL, 0);
    if (rc == 0) g_peer.txns_sent++;
    return rc;
}

// ------------------------------------------------------------ guest side ----
// The record of what libbinder would have done with the driver's output.
struct seen {
    uint32_t cmd;
    uint64_t ptr;
    uint64_t cookie;
    uint32_t code;
    uint8_t* buffer;    // transaction buffer handed out by the driver
};

static pthread_mutex_t g_seen_mu = PTHREAD_MUTEX_INITIALIZER;
static struct seen g_seen[1024];
static int g_nseen;
static volatile int g_looper_stop;
static int g_binder_fd;

static void seen_add(uint32_t cmd, uint64_t ptr, uint64_t cookie, uint32_t code, uint8_t* buffer) {
    pthread_mutex_lock(&g_seen_mu);
    if (g_nseen < (int)(sizeof(g_seen) / sizeof(g_seen[0]))) {
        g_seen[g_nseen].cmd = cmd;
        g_seen[g_nseen].ptr = ptr;
        g_seen[g_nseen].cookie = cookie;
        g_seen[g_nseen].code = code;
        g_seen[g_nseen].buffer = buffer;
        g_nseen++;
    }
    pthread_mutex_unlock(&g_seen_mu);
}

static int seen_count(uint32_t cmd) {
    int n = 0;
    pthread_mutex_lock(&g_seen_mu);
    for (int i = 0; i < g_nseen; i++)
        if (g_seen[i].cmd == cmd) n++;
    pthread_mutex_unlock(&g_seen_mu);
    return n;
}

/// Copy the recorded commands of one kind (oldest first) out of `g_seen`.
static int seen_copy(uint32_t cmd, struct seen* out, int max) {
    int n = 0;
    pthread_mutex_lock(&g_seen_mu);
    for (int i = 0; i < g_nseen && n < max; i++)
        if (g_seen[i].cmd == cmd) out[n++] = g_seen[i];
    pthread_mutex_unlock(&g_seen_mu);
    return n;
}

static int seen_flush(uint32_t cmd) {
    int n = 0;
    pthread_mutex_lock(&g_seen_mu);
    for (int i = 0; i < g_nseen; i++)
        if (g_seen[i].cmd == cmd) n++;
    g_nseen = 0;
    pthread_mutex_unlock(&g_seen_mu);
    return n;
}

// Free every transaction buffer the driver handed out (what libbinder's Parcel
// destructor does with BC_FREE_BUFFER), so a later delivery must not depend on
// an earlier buffer.
static void free_all_txn_buffers(void) {
    uint8_t* bufs[256];
    int n = 0;
    pthread_mutex_lock(&g_seen_mu);
    for (int i = 0; i < g_nseen; i++)
        if (g_seen[i].buffer != NULL && n < 256) bufs[n++] = g_seen[i].buffer;
    pthread_mutex_unlock(&g_seen_mu);
    for (int i = 0; i < n; i++) {
        uint8_t write_buf[16];
        uint32_t cmd = BC_FREE_BUFFER;
        uint64_t ptr = (uint64_t)(uintptr_t)bufs[i];
        memcpy(write_buf, &cmd, 4);
        memcpy(write_buf + 4, &ptr, 8);
        struct binder_write_read bwr;
        memset(&bwr, 0, sizeof(bwr));
        bwr.write_size = 12;
        bwr.write_buffer = (binder_size_t)(uintptr_t)write_buf;
        bwr.write_consumed = 0;
        ioctl(g_binder_fd, BINDER_WRITE_READ, &bwr);
    }
}

// One blocking read that dispatches the driver's BR_* commands the way
// IPCThreadState::executeCommand does, minus libbinder's own bookkeeping.
static int guest_read(int fd) {
    uint8_t read_buf[512];
    // libbinder's own write buffer here holds `BC_REPLY` (4 + 64 bytes) plus the
    // `BC_FREE_BUFFER` of the transaction it just handled (4 + 8).
    uint8_t write_buf[128];
    struct binder_write_read bwr;
    memset(&bwr, 0, sizeof(bwr));
    bwr.read_size = sizeof(read_buf);
    bwr.read_buffer = (binder_size_t)(uintptr_t)read_buf;
    bwr.write_size = 0;
    bwr.write_consumed = 0;
    if (ioctl(fd, BINDER_WRITE_READ, &bwr) != 0) return -1;
    size_t pos = 0;
    // Replies/acks libbinder would send while draining; collected here so the
    // next write can carry them.
    size_t wpos = 0;
    while (pos + 4 <= bwr.read_consumed) {
        uint32_t cmd;
        memcpy(&cmd, read_buf + pos, 4);
        pos += 4;
        switch (cmd) {
            case BR_NOOP:
            case BR_TRANSACTION_COMPLETE:
            case BR_DEAD_REPLY:
            case BR_FAILED_REPLY:
                seen_add(cmd, 0, 0, 0, NULL);
                break;
            case BR_INCREFS:
            case BR_ACQUIRE:
            case BR_RELEASE:
            case BR_DECREFS: {
                struct binder_ptr_cookie pc;
                if (pos + sizeof(pc) > bwr.read_consumed) return -1;
                memcpy(&pc, read_buf + pos, sizeof(pc));
                pos += sizeof(pc);
                seen_add(cmd, pc.ptr, pc.cookie, 0, NULL);
                // libbinder acks INCREFS/ACQUIRE and queues the derefs.
                uint32_t ack = cmd == BR_INCREFS   ? BC_INCREFS_DONE
                               : cmd == BR_ACQUIRE ? BC_ACQUIRE_DONE
                                                   : 0;
                if (ack != 0 && wpos + 20 <= sizeof(write_buf)) {
                    memcpy(write_buf + wpos, &ack, 4);
                    memcpy(write_buf + wpos + 4, &pc, sizeof(pc));
                    wpos += 20;
                }
                break;
            }
            case BR_TRANSACTION: {
                struct binder_transaction_data tr;
                if (pos + sizeof(tr) > bwr.read_consumed) return -1;
                memcpy(&tr, read_buf + pos, sizeof(tr));
                pos += sizeof(tr);
                seen_add(BR_TRANSACTION, (uint64_t)tr.target.ptr, (uint64_t)tr.cookie, tr.code,
                         (uint8_t*)(uintptr_t)tr.data.ptr.buffer);
                // A sync delivery must be answered, like BBinder::transact does,
                // and the received transaction buffer is freed right after (the
                // kernel ties it to the receiving thread, so this is the looper
                // thread's own buffer).
                if ((tr.flags & TF_ONE_WAY) == 0 && wpos + 4 + sizeof(tr) <= sizeof(write_buf)) {
                    uint32_t rcmd = BC_REPLY;
                    struct binder_transaction_data reply_tr;
                    memset(&reply_tr, 0, sizeof(reply_tr));
                    memcpy(write_buf + wpos, &rcmd, 4);
                    memcpy(write_buf + wpos + 4, &reply_tr, sizeof(reply_tr));
                    wpos += 4 + sizeof(reply_tr);
                    uint32_t fcmd = BC_FREE_BUFFER;
                    uint64_t fptr = (uint64_t)tr.data.ptr.buffer;
                    if (wpos + 12 <= sizeof(write_buf)) {
                        memcpy(write_buf + wpos, &fcmd, 4);
                        memcpy(write_buf + wpos + 4, &fptr, 8);
                        wpos += 12;
                    }
                }
                break;
            }
            case BR_REPLY: {
                struct binder_transaction_data tr;
                if (pos + sizeof(tr) > bwr.read_consumed) return -1;
                memcpy(&tr, read_buf + pos, sizeof(tr));
                pos += sizeof(tr);
                seen_add(BR_REPLY, (uint64_t)tr.target.ptr, (uint64_t)tr.cookie, tr.code,
                         (uint8_t*)(uintptr_t)tr.data.ptr.buffer);
                break;
            }
            default:
                printf("# unexpected BR_ cmd 0x%x\n", cmd);
                return -1;
        }
    }
    if (wpos != 0) {
        struct binder_write_read wbwr;
        memset(&wbwr, 0, sizeof(wbwr));
        wbwr.write_size = wpos;
        wbwr.write_buffer = (binder_size_t)(uintptr_t)write_buf;
        ioctl(fd, BINDER_WRITE_READ, &wbwr);
    }
    return (int)bwr.read_consumed;
}

static void* looper_main(void* arg) {
    int fd = (int)(intptr_t)arg;
    // BC_ENTER_LOOPER: from here on this thread receives peer-initiated
    // transactions (the driver only delivers to a looper).
    uint32_t cmd = BC_ENTER_LOOPER;
    struct binder_write_read bwr;
    memset(&bwr, 0, sizeof(bwr));
    bwr.write_size = 4;
    bwr.write_buffer = (binder_size_t)(uintptr_t)&cmd;
    ioctl(fd, BINDER_WRITE_READ, &bwr);
    while (!g_looper_stop) guest_read(fd);
    return NULL;
}

// Read until the reply to the transaction just written has arrived, which is
// what `IPCThreadState::waitForResponse` does: it keeps reading (with no writes
// of its own) until the driver hands it `BR_REPLY`.  Without this the caller's
// reply slot is never released, which is a *harness* artefact, not a driver one.
static void guest_wait_reply(int fd) {
    int before = seen_count(BR_REPLY) + seen_count(BR_FAILED_REPLY);
    for (int i = 0; i < 4; i++) {
        if (seen_count(BR_REPLY) + seen_count(BR_FAILED_REPLY) > before) return;
        guest_read(fd);
    }
}

// One outgoing transaction from the guest, carrying one BINDER object, exactly
// like `Parcel::writeStrongBinder` of a local Java `Binder`.
static int guest_transact_with_binder(int fd, uint64_t weakref, uint64_t cookie,
                                      uint32_t* out_wire) {
    // parcel: interface token-ish int, then the flat object at offset 8, then a
    // trailing int. Object offsets are relative to the data buffer.
    uint8_t data[64];
    memset(data, 0, sizeof(data));
    uint32_t word = 0x1234;
    memcpy(data, &word, 4);
    struct flat_binder_object obj;
    memset(&obj, 0, sizeof(obj));
    obj.hdr.type = BINDER_TYPE_BINDER;
    obj.flags = FLAT_BINDER_FLAG_ACCEPTS_FDS;
    obj.binder = (binder_uintptr_t)weakref;
    obj.cookie = (binder_uintptr_t)cookie;
    memcpy(data + 8, &obj, sizeof(obj));
    // The 4-byte stability record libbinder writes after every flattened binder.
    uint32_t stability = 0;
    memcpy(data + 8 + sizeof(obj), &stability, 4);
    uint64_t offsets[1] = {8};
    struct binder_transaction_data tr;
    memset(&tr, 0, sizeof(tr));
    tr.target.handle = 0;  // servicemanager: the peer script answers everything
    tr.code = 1;
    tr.flags = 0;
    tr.data_size = sizeof(data);
    tr.offsets_size = sizeof(offsets);
    tr.data.ptr.buffer = (binder_uintptr_t)(uintptr_t)data;
    tr.data.ptr.offsets = (binder_uintptr_t)(uintptr_t)offsets;
    uint8_t write_buf[4 + sizeof(tr)];
    uint32_t cmd = BC_TRANSACTION;
    memcpy(write_buf, &cmd, 4);
    memcpy(write_buf + 4, &tr, sizeof(tr));
    struct binder_write_read bwr;
    memset(&bwr, 0, sizeof(bwr));
    bwr.write_size = sizeof(write_buf);
    bwr.write_buffer = (binder_size_t)(uintptr_t)write_buf;
    if (ioctl(fd, BINDER_WRITE_READ, &bwr) != 0) return -1;
    // Read the reply/complete + the node's rooting commands.
    guest_read(fd);
    guest_wait_reply(fd);
    if (out_wire) {
        // The wire handle the peer saw: the driver's node wire for this cookie.
        struct node* n = NULL;
        for (int i = 0; i < g.n_nodes; i++)
            if (g.nodes[i].cookie == cookie) n = &g.nodes[i];
        *out_wire = n ? n->wire : 0;
    }
    return 0;
}

// A handle refcount command, e.g. BC_ACQUIRE/BC_RELEASE on the peer handle.
static void guest_ref_cmd(int fd, uint32_t cmd, uint32_t handle) {
    uint8_t write_buf[8];
    memcpy(write_buf, &cmd, 4);
    memcpy(write_buf + 4, &handle, 4);
    struct binder_write_read bwr;
    memset(&bwr, 0, sizeof(bwr));
    bwr.write_size = 8;
    bwr.write_buffer = (binder_size_t)(uintptr_t)write_buf;
    ioctl(fd, BINDER_WRITE_READ, &bwr);
}

static void msleep(int ms) {
    struct timespec ts = {ms / 1000, (long)(ms % 1000) * 1000000L};
    nanosleep(&ts, NULL);
}

// Wait until the guest recorded `want` transactions (or the deadline passes).
static int wait_for_txns(int want, int timeout_ms) {
    for (int waited = 0; waited < timeout_ms; waited += 10) {
        if (seen_count(BR_TRANSACTION) >= want) return seen_count(BR_TRANSACTION);
        msleep(10);
    }
    return seen_count(BR_TRANSACTION);
}

// ------------------------------------------------- concurrent heap pressure --
// The transaction-buffer heap (`mm_alloc`/`mm_free`) is shared by every thread:
// the caller's reply path (`build_reply`) allocates while the looper thread's
// delivery path (`deliver_incoming`) allocates. Two live transaction buffers
// must never be the same memory, because the second writer would corrupt the
// first parcel while libbinder is still decoding it.
#define ALLOC_THREADS 16
#define ALLOC_PER_THREAD 2000
#define ALLOC_SIZE 16

struct alloc_rec {
    size_t off;
};

static struct alloc_rec g_allocs[ALLOC_THREADS][ALLOC_PER_THREAD];

static void* alloc_main(void* arg) {
    struct alloc_rec* rec = arg;
    for (int i = 0; i < ALLOC_PER_THREAD; i++) {
        void* p = mm_alloc(ALLOC_SIZE);
        rec[i].off = p == NULL ? (size_t)-1 : (size_t)((uint8_t*)p - g.map);
    }
    return NULL;
}

static int alloc_off_cmp(const void* a, const void* b) {
    const struct alloc_rec* x = a;
    const struct alloc_rec* y = b;
    return (x->off > y->off) - (x->off < y->off);
}

int main(void) {
    // ---- peer socket -------------------------------------------------------
    char sock_path[108];
    snprintf(sock_path, sizeof(sock_path), "/tmp/acos-binder-driver-test-%d.sock", (int)getpid());
    unlink(sock_path);
    g_peer.listen_fd = socket(AF_UNIX, SOCK_SEQPACKET | SOCK_CLOEXEC, 0);
    if (g_peer.listen_fd < 0) {
        printf("Bail out! socket: %s\n", strerror(errno));
        return 2;
    }
    struct sockaddr_un sa;
    memset(&sa, 0, sizeof(sa));
    sa.sun_family = AF_UNIX;
    snprintf(sa.sun_path, sizeof(sa.sun_path), "%s", sock_path);
    if (bind(g_peer.listen_fd, (struct sockaddr*)&sa, sizeof(sa)) != 0 ||
        listen(g_peer.listen_fd, 1) != 0) {
        printf("Bail out! bind/listen: %s\n", strerror(errno));
        return 2;
    }
    setenv("E3G_SOCK", sock_path, 1);
    pthread_create(&g_peer.thread, NULL, peer_main, NULL);

    // ---- the driver --------------------------------------------------------
    int fd = open("/dev/binder", O_RDWR);
    if (fd < 0) {
        printf("Bail out! open(/dev/binder): %s\n", strerror(errno));
        return 2;
    }
    g_binder_fd = fd;
    for (int i = 0; i < 200 && g_peer.fd == 0; i++) msleep(10);
    check(g_peer.fd != 0, "the driver connected to the peer socket", "no accept");

    // A node owner: the weakref pointer and the BBinder pointer libbinder would
    // pass. They only have to be stable, distinct addresses here; the driver
    // never dereferences them.
    static int weakref_object;
    static int bbinder_object;
    uint64_t weakref = (uint64_t)(uintptr_t)&weakref_object;
    uint64_t cookie = (uint64_t)(uintptr_t)&bbinder_object;

    // The guest's looper: the one thread that receives host->guest transactions.
    pthread_t looper;
    pthread_create(&looper, NULL, looper_main, (void*)(intptr_t)fd);
    msleep(100);

    // 1. export + root
    uint32_t wire = 0;
    guest_transact_with_binder(fd, weakref, cookie, &wire);
    char detail[256];
    snprintf(detail, sizeof(detail), "wire=%u nodes=%d", wire, g.n_nodes);
    check(wire != 0, "exporting a BINDER object gives the peer a wire handle", detail);
    check(seen_flush(BR_INCREFS) == 1, "the driver roots a new node with BR_INCREFS",
          "libbinder answers BR_INCREFS with incWeak");
    check(seen_flush(BR_ACQUIRE) == 0 && g.n_nodes > 0,
          "BR_ACQUIRE follows BR_INCREFS in the same batch", "no node rooting");

    // 2. a host->guest callback arrives
    seen_flush(0);
    peer_txn(wire, 42, 1);
    int got = wait_for_txns(1, 3000);
    snprintf(detail, sizeof(detail), "delivered=%d", got);
    check(got == 1, "a host->guest transaction to the exported node is delivered", detail);
    free_all_txn_buffers();
    seen_flush(0);

    // 3. the node outlives the parcel that exposed it: buffer freed above, plus
    //    a full refcount churn on the guest side.
    guest_ref_cmd(fd, BC_ACQUIRE, wire);
    guest_ref_cmd(fd, BC_RELEASE, wire);
    guest_ref_cmd(fd, BC_INCREFS, wire);
    guest_ref_cmd(fd, BC_DECREFS, wire);
    msleep(50);
    peer_txn(wire, 43, 2);
    got = wait_for_txns(1, 3000);
    snprintf(detail, sizeof(detail), "delivered=%d", got);
    check(got == 1,
          "the node still receives callbacks after BC_FREE_BUFFER and refcount churn", detail);
    free_all_txn_buffers();
    seen_flush(0);

    // 4. many callbacks in one turn: all of them arrive, in order.
    for (int i = 0; i < 8; i++) peer_txn(wire, 100 + (uint32_t)i, 10 + (uint32_t)i);
    got = wait_for_txns(8, 5000);
    snprintf(detail, sizeof(detail), "delivered=%d of 8", got);
    check(got == 8, "eight back-to-back callbacks are all delivered", detail);
    free_all_txn_buffers();
    seen_flush(0);

    // 5. a transaction to a node nobody exported is reported, not dropped.
    peer_txn(0xdead, 44, 3);
    // The driver must answer the host with a DEAD_OBJECT reply frame.
    int dead_reply = 0;
    for (int waited = 0; waited < 3000; waited += 10) {
        if (g_peer.txns_sent != 0 && g_peer.dead_replies > 0) {
            dead_reply = 1;
            break;
        }
        msleep(10);
    }
    snprintf(detail, sizeof(detail), "dead_object replies=%u", g_peer.dead_replies);
    check(dead_reply, "a transaction to an unknown node answers a DEAD_OBJECT reply", detail);

    // 6. several distinct objects registered for one event, each receives its
    //    callback at the object that is registered *now*. A Java `Binder`'s
    //    `JavaBBinder` is re-created by `JavaBBinderHolder::get` (it keeps only a
    //    weak pointer), so a re-registration exports a new weakref under the
    //    same cookie: the node must keep its wire handle, refresh the object it
    //    delivers to, and root the new object.
    enum { REGS = 8 };
    static int wref_a[REGS], wref_b[REGS], bbinder[REGS];
    uint32_t wires[REGS], wires_again[REGS];
    for (int i = 0; i < REGS; i++)
        guest_transact_with_binder(fd, (uint64_t)(uintptr_t)&wref_a[i],
                                   (uint64_t)(uintptr_t)&bbinder[i], &wires[i]);
    seen_flush(0);
    int stable = 0, rooted = 0;
    for (int i = 0; i < REGS; i++) {
        guest_transact_with_binder(fd, (uint64_t)(uintptr_t)&wref_b[i],
                                   (uint64_t)(uintptr_t)&bbinder[i], &wires_again[i]);
        struct seen inc[8], acq[8];
        int ninc = seen_copy(BR_INCREFS, inc, 8);
        int nacq = seen_copy(BR_ACQUIRE, acq, 8);
        if (wires_again[i] != 0 && wires_again[i] == wires[i]) stable++;
        if (ninc == 1 && nacq == 1 &&
            inc[0].ptr == (uint64_t)(uintptr_t)&wref_b[i] &&
            acq[0].cookie == (uint64_t)(uintptr_t)&bbinder[i])
            rooted++;
        seen_flush(0);
    }
    snprintf(detail, sizeof(detail), "stable wires=%d of %d, re-rooted=%d of %d", stable,
             REGS, rooted, REGS);
    check(stable == REGS && rooted == REGS,
          "a re-exported node keeps its wire handle and roots the new object", detail);

    for (int i = 0; i < REGS; i++) peer_txn(wires[i], 200 + (uint32_t)i, 30 + (uint32_t)i);
    got = wait_for_txns(REGS, 5000);
    struct seen tx[64];
    int ntx = seen_copy(BR_TRANSACTION, tx, 64);
    int addressed = 0;
    for (int i = 0; i < REGS; i++) {
        for (int j = 0; j < ntx; j++) {
            if (tx[j].code != 200 + (uint32_t)i) continue;
            if (tx[j].ptr == (uint64_t)(uintptr_t)&wref_b[i] &&
                tx[j].cookie == (uint64_t)(uintptr_t)&bbinder[i])
                addressed++;
            break;
        }
    }
    snprintf(detail, sizeof(detail), "delivered=%d, addressed at the registered object=%d of %d",
             got, addressed, REGS);
    check(got == REGS && addressed == REGS,
          "every object registered for one event receives its callback", detail);
    free_all_txn_buffers();
    seen_flush(0);

    // 7. the shared transaction-buffer heap never hands one live transaction's
    //    memory to another, even when every thread allocates at once.
    {
        pthread_t th[ALLOC_THREADS];
        for (int i = 0; i < ALLOC_THREADS; i++)
            pthread_create(&th[i], NULL, alloc_main, g_allocs[i]);
        for (int i = 0; i < ALLOC_THREADS; i++) pthread_join(th[i], NULL);
        static struct alloc_rec all[ALLOC_THREADS * ALLOC_PER_THREAD];
        for (int i = 0; i < ALLOC_THREADS; i++)
            memcpy(&all[i * ALLOC_PER_THREAD], g_allocs[i], sizeof(all[0]) * ALLOC_PER_THREAD);
        qsort(all, ALLOC_THREADS * ALLOC_PER_THREAD, sizeof(all[0]), alloc_off_cmp);
        int failed = 0, overlaps = 0;
        for (int i = 0; i < ALLOC_THREADS * ALLOC_PER_THREAD; i++) {
            if (all[i].off == (size_t)-1) failed++;
            if (i > 0 && all[i].off != (size_t)-1 && all[i - 1].off != (size_t)-1 &&
                all[i].off < all[i - 1].off + ALLOC_SIZE)
                overlaps++;
        }
        snprintf(detail, sizeof(detail), "allocations=%d failed=%d overlapping=%d",
                 ALLOC_THREADS * ALLOC_PER_THREAD, failed, overlaps);
        check(failed == 0 && overlaps == 0,
              "two concurrent transaction buffers are never the same memory", detail);
    }

    // 8. a session with more registrations than one page of the node table still
    //    reaches every registered object (the acceptance runs register 100+).
    enum { MANY = 96 };
    static int many_wref[MANY], many_bb[MANY];
    uint32_t many_wire[MANY];
    int exported = 0;
    for (int i = 0; i < MANY; i++) {
        guest_transact_with_binder(fd, (uint64_t)(uintptr_t)&many_wref[i],
                                   (uint64_t)(uintptr_t)&many_bb[i], &many_wire[i]);
        if (many_wire[i] != 0) exported++;
    }
    seen_flush(0);
    for (int i = 0; i < MANY; i++)
        if (many_wire[i] != 0) peer_txn(many_wire[i], 300 + (uint32_t)i, 100 + (uint32_t)i);
    got = wait_for_txns(MANY, 10000);
    snprintf(detail, sizeof(detail), "exported=%d of %d, delivered=%d (queued %u)", exported, MANY,
             got, g_peer.txns_sent);
    check(exported == MANY && got == MANY,
          "every one of many registered objects still receives its callback", detail);
    free_all_txn_buffers();
    seen_flush(0);

    // 9. `BC_FREE_BUFFER` returns the buffer to the allocator: a freed block is
    //    handed to the next transaction that fits it (the kernel's
    //    `binder_alloc_free_buf` + free-list reuse), and it is never handed out
    //    while it is still live.
    {
        void* a = mm_alloc(4096);
        void* b = mm_alloc(4096);
        void* c = mm_alloc(4096);
        int distinct = a && b && c && a != b && b != c && a != c;
        mm_free(b);
        // First fit over the freed list: with `a` and `c` live, the only block
        // that can serve a 4096-byte request is `b`'s.
        void* again = mm_alloc(4096);
        int reused = again == b;
        mm_free(a);
        mm_free(c);
        mm_free(again);
        snprintf(detail, sizeof(detail), "a=%p b=%p c=%p again=%p", a, b, c, again);
        check(distinct && reused, "a freed transaction buffer is reused by the next transaction",
              detail);
    }

    // 10. a long session does not run out of transaction-buffer space: the 1 MB
    //     map is a *cache*, not a log. Before this change the bump pointer only
    //     moved forward, so a session that registered 100 callbacks exhausted
    //     the map after ~3000 transactions and the driver then dropped every
    //     later transaction *and* reply (measured: `transaction buffer exhausted`).
    {
        size_t high_water = 0;
        int failed = 0;
        pthread_mutex_lock(&g_slab_mu);
        high_water = g_bump;
        pthread_mutex_unlock(&g_slab_mu);
        size_t start = high_water;
        for (int i = 0; i < 4000; i++) {
            // One registration-shaped round: a delivered transaction buffer and
            // the reply buffer, both freed by their receiver.
            void* delivery = mm_alloc(1024);
            void* reply = mm_alloc(256);
            if (delivery == NULL || reply == NULL) {
                failed++;
                if (delivery) mm_free(delivery);
                if (reply) mm_free(reply);
                break;
            }
            mm_free(delivery);
            mm_free(reply);
        }
        pthread_mutex_lock(&g_slab_mu);
        high_water = g_bump;
        pthread_mutex_unlock(&g_slab_mu);
        snprintf(detail, sizeof(detail), "failed=%d high-water %zu -> %zu of %zu", failed, start,
                 high_water, g.map_size);
        check(failed == 0 && high_water <= start + 8192,
              "4000 transaction rounds reuse the 1 MB transaction-buffer map", detail);
    }

    // ---- teardown ----------------------------------------------------------
    g_looper_stop = 1;
    shutdown(g_peer.fd, SHUT_RDWR);
    unlink(sock_path);
    printf("RESULT failures=%d\n", g_failures);
    return g_failures == 0 ? 0 : 1;
}
