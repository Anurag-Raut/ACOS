// binder_driver.c -- E3G: userspace binder DRIVER emulation for STOCK libbinder.
//
// Same shape as E2D's framework_shim.c (property table, default-namespace
// dlopen of libandroid_runtime.so, Java -> register_* bridge), except that the
// memfd that stands in for /dev/binder is no longer inert: BINDER_WRITE_READ is
// fully emulated, so unmodified libbinder (and therefore android.os.Binder,
// BinderProxy, ServiceManager, Parcel, Bundle) talks to a Rust peer over a unix
// socket instead of a kernel driver.
//
// Layers, bottom up:
//
//   1. transport  : SOCK_SEQPACKET-ish length-prefixed frames on a unix socket,
//                   plus SCM_RIGHTS for BINDER_TYPE_FD.  One reader thread
//                   demultiplexes replies (to the transacting thread) and
//                   incoming transactions (to a looper thread).
//   2. object table: flat_binder_object translation.  Incoming peer nodes become
//                   handles in our handle space; our local nodes (Java
//                   BBinder/JavaBBinder pointers) get a wire handle.  This is
//                   what makes readStrongBinder() return a BinderProxy and
//                   writeStrongBinder() a working reference.
//   3. driver     : BC_* parsing / BR_* synthesis, a bump+free-list allocator
//                   over the mmap'ed transaction buffer (the driver's half of
//                   the binder buffer), per-thread trees (looper state, pending
//                   BR_* output, in-flight reply slot).
//   4. substrate  : property table + framework natives (copied from E2D because
//                   bionic symbol lookup is namespace-scoped: one .so).
#define _GNU_SOURCE
#include <jni.h>
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/un.h>
#include <time.h>
#include <unistd.h>

#include <linux/android/binder.h>

#ifndef __NR_memfd_create
#define __NR_memfd_create 319
#endif
#ifndef MFD_CLOEXEC
#define MFD_CLOEXEC 0x0001U
#endif

#define BINDER_VM_SIZE (1024 * 1024 - 2 * 4096)
#define E3G_MAX_FRAME (256 * 1024)
#define E3G_MAX_FD 16
#define E3G_MAX_SLOT 32
#define E3G_MAX_NODE 64
#define E3G_MAX_HANDLE 64

static int g_dbg = -1;
#define DBG(...)                                     \
    do {                                             \
        if (g_dbg < 0) g_dbg = getenv("E3G_DEBUG") != NULL; \
        if (g_dbg) {                                 \
            dprintf(2, "E3G: ");                     \
            dprintf(2, __VA_ARGS__);                 \
            dprintf(2, "\n");                        \
        }                                            \
    } while (0)
#define WARN(...)                        \
    do {                                 \
        dprintf(2, "E3G[warn]: ");       \
        dprintf(2, __VA_ARGS__);         \
        dprintf(2, "\n");                \
    } while (0)

// --------------------------------------------------------- (4) property table
// (see E2D §1: Build.VERSION.<clinit> indexes SUPPORTED_ABIS[0]; a failure is
// sticky and poisons Resources/ActivityThread/anything touching Build).
static const struct { const char* key; const char* value; } kProps[] = {
        {"ro.product.cpu.abilist", "x86_64,x86"},
        {"ro.product.cpu.abilist64", "x86_64"},
        {"ro.product.cpu.abilist32", "x86"},
        {"ro.product.cpu.abi", "x86_64"},
        {"ro.build.version.sdk", "34"},
        {"ro.build.version.release", "14"},
        {"ro.build.version.release_or_codename", "14"},
        {"ro.build.version.codename", "REL"},
        {"ro.build.version.all_codenames", "REL"},
        {"ro.build.version.preview_sdk", "0"},
        {"ro.build.version.min_supported_target_sdk", "23"},
        {"ro.build.version.security_patch", "2024-07-05"},
        {"ro.build.type", "userdebug"},
        {"ro.build.tags", "release-keys"},
        {"ro.build.characteristics", "nosdcard"},
        {"ro.debuggable", "1"},
        {"ro.product.device", "acos"},
        {"ro.product.model", "ACOS"},
        {"ro.product.brand", "acos"},
        {"ro.product.name", "acos"},
        {"ro.product.manufacturer", "acos"},
        {"ro.zygote", "zygote64"},
        {"ro.hardware", "ranchu"},
        {"ro.opengles.version", "196610"},
        {"debug.trace.atrace", "0"},
        {"persist.sys.locale", "en-US"},
        {"ro.kernel.qemu", "0"},
        {"dalvik.vm.dex2oat-filter", "verify"},
        {NULL, NULL},
};

int __system_property_get(const char* key, char* value) {
    for (int i = 0; kProps[i].key; i++) {
        if (strcmp(key, kProps[i].key) == 0) {
            size_t n = strlen(kProps[i].value);
            memcpy(value, kProps[i].value, n + 1);
            return (int)n;
        }
    }
    static int (*real)(const char*, char*);
    if (!real) real = dlsym(RTLD_NEXT, "__system_property_get");
    if (real) return real(key, value);
    value[0] = 0;
    return 0;
}

static struct { char name[128]; char value[128]; } g_fake_prop[64];
static int g_fake_prop_n;

static void* fake_prop_info(const char* key, const char* value) {
    if (g_fake_prop_n >= 64) return NULL;
    snprintf(g_fake_prop[g_fake_prop_n].name, sizeof(g_fake_prop[0].name), "%s", key);
    snprintf(g_fake_prop[g_fake_prop_n].value, sizeof(g_fake_prop[0].value), "%s", value);
    return &g_fake_prop[g_fake_prop_n++];
}

static int is_fake_prop(const void* pi) {
    return (const char*)pi >= (const char*)g_fake_prop &&
           (const char*)pi < (const char*)(g_fake_prop + 64);
}

const void* __system_property_find(const char* key) {
    for (int i = 0; kProps[i].key; i++)
        if (strcmp(key, kProps[i].key) == 0) return fake_prop_info(kProps[i].key, kProps[i].value);
    static const void* (*real)(const char*);
    if (!real) real = dlsym(RTLD_NEXT, "__system_property_find");
    return real ? real(key) : NULL;
}

void __system_property_read_callback(const void* pi,
                                    void (*callback)(void*, const char*, const char*, unsigned),
                                    void* cookie) {
    if (is_fake_prop(pi)) {
        const char* base = (const char*)pi;
        callback(cookie, base, base + 128, 1);
        return;
    }
    static void (*real)(const void*, void (*)(void*, const char*, const char*, unsigned), void*);
    if (!real) real = dlsym(RTLD_NEXT, "__system_property_read_callback");
    if (real) real(pi, callback, cookie);
}

__attribute__((constructor)) static void e3g_load_runtime(void) {
    const char* list = getenv("E3G_LOAD");
    if (list == NULL) list = "libandroid_runtime.so";
    char buf[4096];
    snprintf(buf, sizeof(buf), "%s", list);
    for (char* save = NULL, *tok = strtok_r(buf, ":", &save); tok;
         tok = strtok_r(NULL, ":", &save)) {
        void* h = dlopen(tok, RTLD_NOW | RTLD_GLOBAL);
        if (h == NULL) WARN("dlopen(%s) FAILED: %s", tok, dlerror());
        else DBG("dlopen(%s) ok @%p", tok, h);
    }
}

// ------------------------------------------------------------- (3) transport
enum { F_REQ = 1, F_RSP = 2, F_TXN = 3, F_REP = 4 };

struct fhdr {
    uint32_t type;
    uint32_t seq;
    uint32_t code;
    uint32_t flags;
    uint64_t target;       // REQ: target handle (u32); TXN: node id
    uint64_t data_size;
    uint64_t offsets_size;
    uint64_t reserved;
};

struct slot {
    int used;
    uint32_t seq;
    int done;
    uint8_t* data;
    size_t data_size;
    uint8_t* offs;
    size_t offs_size;
    uint32_t status;
};

struct inq {
    struct inq* next;
    int fds[E3G_MAX_FD];
    int nfds;
    uint64_t target;
    uint32_t seq;
    uint32_t code;
    uint32_t flags;
    uint64_t data_size, offs_size;
    uint8_t* data;
};

struct node {          // a local binder node we exported
    uint64_t binder;   // obj.binder == weakref ptr
    uint64_t cookie;   // obj.cookie == BBinder*
    uint32_t wire;     // handle number used on the wire
};

struct handle {        // a handle to a peer node
    uint32_t h;
    uint64_t peer_node;
};

struct drv {
    int fd;            // memfd
    int sock;          // unix socket to the peer
    uint8_t* map;      // read-write alias of libbinder's read-only mapping
    size_t map_size;
    pthread_t reader;
    int reader_started;
    pthread_mutex_t mu;
    pthread_cond_t cv_txn;   // incoming transaction queued
    pthread_cond_t cv_rsp;   // any reply slot filled
    struct inq *in_head, *in_tail;
    struct slot slots[E3G_MAX_SLOT];
    struct node nodes[E3G_MAX_NODE];
    int n_nodes;
    struct handle handles[E3G_MAX_HANDLE];
    int n_handles;
    uint32_t next_wire;
    uint32_t next_handle;
};

static struct drv g;

struct tstate {
    int is_looper;
    int reply_seq;         // seq we are waiting a reply for (-1 none)
    uint32_t reply_to_seq; // seq of the incoming TXN we must reply to
    int have_reply_to;
    uint8_t* pend;         // BR_* bytes not yet copied into read_buffer
    size_t pend_len, pend_cap;
};
static __thread struct tstate T;

static void pend_append(const void* p, size_t n) {
    if (T.pend_len + n > T.pend_cap) {
        size_t cap = T.pend_cap ? T.pend_cap * 2 : 256;
        while (cap < T.pend_len + n) cap *= 2;
        T.pend = realloc(T.pend, cap);
        T.pend_cap = cap;
    }
    memcpy(T.pend + T.pend_len, p, n);
    T.pend_len += n;
}

static void pend_cmd(uint32_t cmd) { pend_append(&cmd, 4); }

// ----------------------------------------------------------- (2) object table
static uint32_t node_wire_for(uint64_t binder, uint64_t cookie) {
    for (int i = 0; i < g.n_nodes; i++)
        if (g.nodes[i].binder == binder && g.nodes[i].cookie == cookie) return g.nodes[i].wire;
    if (g.n_nodes >= E3G_MAX_NODE) {
        WARN("node table full");
        return 0;
    }
    struct node* n = &g.nodes[g.n_nodes++];
    n->binder = binder;
    n->cookie = cookie;
    n->wire = g.next_wire++;
    DBG("node: local binder=%llx cookie=%llx -> wire=%u", (unsigned long long)binder,
        (unsigned long long)cookie, n->wire);
    return n->wire;
}

static struct node* node_by_wire(uint32_t wire) {
    for (int i = 0; i < g.n_nodes; i++)
        if (g.nodes[i].wire == wire) return &g.nodes[i];
    return NULL;
}

static uint32_t handle_for_peer_node(uint64_t peer_node) {
    for (int i = 0; i < g.n_handles; i++)
        if (g.handles[i].peer_node == peer_node) return g.handles[i].h;
    if (g.n_handles >= E3G_MAX_HANDLE) {
        WARN("handle table full");
        return 0;
    }
    struct handle* h = &g.handles[g.n_handles++];
    h->h = g.next_handle++;
    h->peer_node = peer_node;
    DBG("handle: peer node=%llx -> handle=%u", (unsigned long long)peer_node, h->h);
    return h->h;
}

static uint64_t peer_node_for_handle(uint32_t h) {
    for (int i = 0; i < g.n_handles; i++)
        if (g.handles[i].h == h) return g.handles[i].peer_node;
    return 0;
}

// Rewrite the flat objects of a parcel in place.
//   dir 0: ours -> peer   (BINDER->HANDLE(wire), HANDLE->BINDER(peer node))
//   dir 1: peer -> ours   (BINDER(peer node)->HANDLE(ours), HANDLE(wire)->BINDER)
// fds[] collects BINDER_TYPE_FD payloads on the way out (dir 0); on the way in
// the received fds are substituted into the objects, in order.
static int xform_objects(uint8_t* data, size_t dsize, uint8_t* offs, size_t osize, int dir,
                         int* fds, int* nfds) {
    if (osize == 0) return 0;
    if (osize % 8) {
        WARN("offsets_size %zu not a multiple of 8", osize);
        return -1;
    }
    for (size_t i = 0; i + 8 <= osize; i += 8) {
        uint64_t off;
        memcpy(&off, offs + i, 8);
        if (off + sizeof(struct flat_binder_object) > dsize) {
            WARN("object offset %llu out of range (data_size %zu)", (unsigned long long)off,
                 dsize);
            return -1;
        }
        struct flat_binder_object* o = (struct flat_binder_object*)(data + off);
        switch (o->hdr.type) {
            case BINDER_TYPE_BINDER:
            case BINDER_TYPE_WEAK_BINDER: {
                uint32_t wire;
                if (o->binder == 0 && o->cookie == 0) {
                    // a null binder object: pass through unchanged
                    break;
                }
                if (dir == 0) {
                    wire = node_wire_for(o->binder, o->cookie);
                    o->hdr.type = (o->hdr.type == BINDER_TYPE_BINDER) ? BINDER_TYPE_HANDLE
                                                                     : BINDER_TYPE_WEAK_HANDLE;
                    o->binder = 0;
                    o->handle = wire;
                    o->cookie = 0;
                } else {
                    uint32_t h = handle_for_peer_node(o->binder);
                    o->hdr.type = (o->hdr.type == BINDER_TYPE_BINDER) ? BINDER_TYPE_HANDLE
                                                                     : BINDER_TYPE_WEAK_HANDLE;
                    o->binder = 0;
                    o->handle = h;
                    o->cookie = 0;
                }
                break;
            }
            case BINDER_TYPE_HANDLE:
            case BINDER_TYPE_WEAK_HANDLE: {
                if (dir == 0) {
                    uint64_t peer = peer_node_for_handle(o->handle);
                    if (peer == 0) {
                        // Unknown handle: our own node cannot appear as HANDLE in our
                        // own parcel, so this is a bug or a stale handle.
                        WARN("outgoing handle %u has no peer node", o->handle);
                        break;
                    }
                    o->hdr.type = (o->hdr.type == BINDER_TYPE_HANDLE) ? BINDER_TYPE_BINDER
                                                                     : BINDER_TYPE_WEAK_BINDER;
                    o->binder = peer;
                    o->cookie = peer;
                } else {
                    struct node* n = node_by_wire(o->handle);
                    if (n == NULL) {
                        WARN("incoming handle %u is not a local node", o->handle);
                        break;
                    }
                    o->hdr.type = (o->hdr.type == BINDER_TYPE_HANDLE) ? BINDER_TYPE_BINDER
                                                                     : BINDER_TYPE_WEAK_BINDER;
                    o->binder = n->binder;
                    o->cookie = n->cookie;
                }
                break;
            }
            case BINDER_TYPE_FD: {
                if (dir == 0) {
                    if (*nfds < E3G_MAX_FD) fds[(*nfds)++] = (int)o->handle;
                    else WARN("too many fds in one transaction");
                } else if (*nfds > 0) {
                    // received fds are consumed in order (see reader thread)
                    o->handle = (uint32_t)fds[0];
                    memmove(fds, fds + 1, sizeof(int) * (size_t)(*nfds - 1));
                    (*nfds)--;
                } else {
                    WARN("BINDER_TYPE_FD in incoming parcel but no fd was received");
                }
                break;
            }
            default:
                DBG("object type 0x%x not translated", o->hdr.type);
                break;
        }
    }
    return 0;
}

// ------------------------------------------------- (3) transaction-buffer heap
struct blk { size_t off, size; int used; };
static struct blk g_blk[512];
static int g_nblk;
static size_t g_bump;

static int blk_cmp(const void* a, const void* b) {
    const struct blk* x = a;
    const struct blk* y = b;
    return (x->off > y->off) - (x->off < y->off);
}

static void* mm_alloc(size_t n) {
    n = (n + 7) & ~(size_t)7;
    for (int i = 0; i < g_nblk; i++) {
        if (!g_blk[i].used && g_blk[i].size >= n) {
            g_blk[i].used = 1;
            if (g_blk[i].size > n) {  // split
                if (g_nblk < 512) {
                    memmove(&g_blk[i + 2], &g_blk[i + 1], sizeof(g_blk[0]) * (size_t)(g_nblk - i - 1));
                    g_blk[i + 1].off = g_blk[i].off + n;
                    g_blk[i + 1].size = g_blk[i].size - n;
                    g_blk[i + 1].used = 0;
                    g_nblk++;
                }
                g_blk[i].size = n;
            }
            return g.map + g_blk[i].off;
        }
    }
    if (g_bump + n > g.map_size) {
        WARN("transaction buffer exhausted (%zu/%zu used)", g_bump, g.map_size);
        return NULL;
    }
    size_t off = g_bump;
    g_bump += n;
    if (g_nblk < 512) {
        g_blk[g_nblk].off = off;
        g_blk[g_nblk].size = n;
        g_blk[g_nblk].used = 1;
        g_nblk++;
    }
    return g.map + off;
}

static void mm_free(void* p) {
    if (p == NULL) return;
    size_t off = (size_t)((uint8_t*)p - g.map);
    for (int i = 0; i < g_nblk; i++) {
        if (g_blk[i].off == off && g_blk[i].used) {
            g_blk[i].used = 0;
            // coalesce with neighbours
            qsort(g_blk, (size_t)g_nblk, sizeof(g_blk[0]), blk_cmp);
            for (int j = 0; j + 1 < g_nblk;) {
                if (!g_blk[j].used && !g_blk[j + 1].used &&
                    g_blk[j].off + g_blk[j].size == g_blk[j + 1].off) {
                    g_blk[j].size += g_blk[j + 1].size;
                    memmove(&g_blk[j + 1], &g_blk[j + 2],
                            sizeof(g_blk[0]) * (size_t)(g_nblk - j - 2));
                    g_nblk--;
                } else {
                    j++;
                }
            }
            return;
        }
    }
    DBG("free of unknown buffer %p", p);
}

// -------------------------------------------------------------- socket frames
static int send_frame(uint32_t type, uint32_t seq, uint32_t code, uint32_t flags, uint64_t target,
                      const void* data, size_t dsize, const void* offs, size_t osize, int* fds,
                      int nfds) {
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
    char cbuf[CMSG_SPACE(sizeof(int) * E3G_MAX_FD)];
    memset(cbuf, 0, sizeof(cbuf));
    if (nfds > 0) {
        msg.msg_control = cbuf;
        msg.msg_controllen = CMSG_SPACE(sizeof(int) * (size_t)nfds);
        struct cmsghdr* c = CMSG_FIRSTHDR(&msg);
        c->cmsg_level = SOL_SOCKET;
        c->cmsg_type = SCM_RIGHTS;
        c->cmsg_len = CMSG_LEN(sizeof(int) * (size_t)nfds);
        memcpy(CMSG_DATA(c), fds, sizeof(int) * (size_t)nfds);
    }
    ssize_t n;
    do {
        n = sendmsg(g.sock, &msg, MSG_NOSIGNAL);
    } while (n < 0 && errno == EINTR);
    if (n < 0) {
        WARN("sendmsg failed: %s", strerror(errno));
        return -1;
    }
    return 0;
}

static int read_full(int fd, void* buf, size_t n) {
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

static void dispatch_frame(uint32_t type, struct fhdr* h, uint8_t* payload, int* fds, int nfds);

static void* reader_main(void* arg) {
    (void)arg;
    size_t cap = 4096;
    uint8_t* buf = malloc(cap);
    for (;;) {
        struct fhdr h;
        char cbuf[CMSG_SPACE(sizeof(int) * E3G_MAX_FD)];
        struct iovec iov = {buf, cap};
        struct msghdr msg = {0};
        msg.msg_iov = &iov;
        msg.msg_iovlen = 1;
        msg.msg_control = cbuf;
        msg.msg_controllen = sizeof(cbuf);
        ssize_t n;
        do {
            n = recvmsg(g.sock, &msg, MSG_WAITALL);
        } while (n < 0 && errno == EINTR);
        if ((size_t)n < sizeof(h)) {
            if (n <= 0) WARN("peer socket closed (%zd)", n);
            else WARN("short frame %zd", n);
            return NULL;
        }
        memcpy(&h, buf, sizeof(h));
        if (h.data_size + h.offsets_size + sizeof(h) > (uint64_t)n) {
            WARN("truncated frame: need %llu got %zd",
                 (unsigned long long)(h.data_size + h.offsets_size + sizeof(h)), n);
            return NULL;
        }
        int fds[E3G_MAX_FD];
        int nfds = 0;
        for (struct cmsghdr* c = CMSG_FIRSTHDR(&msg); c; c = CMSG_NXTHDR(&msg, c)) {
            if (c->cmsg_level == SOL_SOCKET && c->cmsg_type == SCM_RIGHTS) {
                int cnt = (int)((c->cmsg_len - CMSG_LEN(0)) / sizeof(int));
                if (cnt > E3G_MAX_FD) cnt = E3G_MAX_FD;
                memcpy(fds, CMSG_DATA(c), sizeof(int) * (size_t)cnt);
                nfds = cnt;
            }
        }
        dispatch_frame(h.type, &h, buf + sizeof(h), fds, nfds);
    }
    return NULL;
}

static struct slot* slot_for(uint32_t seq) {
    for (int i = 0; i < E3G_MAX_SLOT; i++)
        if (g.slots[i].used && g.slots[i].seq == seq) return &g.slots[i];
    return NULL;
}

static void dispatch_frame(uint32_t type, struct fhdr* h, uint8_t* payload, int* fds, int nfds) {
    pthread_mutex_lock(&g.mu);
    switch (type) {
        case F_RSP: {
            struct slot* s = slot_for(h->seq);
            if (!s) {
                pthread_mutex_unlock(&g.mu);
                WARN("reply for unknown seq %u", h->seq);
                return;
            }
            s->data = malloc((size_t)h->data_size + 8);
            s->offs = malloc((size_t)h->offsets_size + 8);
            memcpy(s->data, payload, (size_t)h->data_size);
            memcpy(s->offs, payload + h->data_size, (size_t)h->offsets_size);
            s->data_size = (size_t)h->data_size;
            s->offs_size = (size_t)h->offsets_size;
            s->status = h->code;
            s->done = 1;
            pthread_cond_broadcast(&g.cv_rsp);
            break;
        }
        case F_TXN: {
            struct inq* q = calloc(1, sizeof(*q));
            q->target = h->target;
            q->seq = h->seq;
            q->code = h->code;
            q->flags = h->flags;
            q->data_size = h->data_size;
            q->offs_size = h->offsets_size;
            q->data = malloc((size_t)h->data_size + 8);
            memcpy(q->data, payload, (size_t)h->data_size);
            // keep offsets right after data in the same allocation
            q->data = realloc(q->data, (size_t)h->data_size + (size_t)h->offsets_size + 16);
            memcpy(q->data + h->data_size, payload + h->data_size, (size_t)h->offsets_size);
            // incoming fds are consumed when the transaction is delivered
            q->nfds = nfds;
            memcpy(q->fds, fds, sizeof(int) * (size_t)nfds);
            if (g.in_tail) g.in_tail->next = q;
            else g.in_head = q;
            g.in_tail = q;
            pthread_cond_broadcast(&g.cv_txn);
            break;
        }
        default:
            WARN("unexpected frame type %u", type);
            break;
    }
    pthread_mutex_unlock(&g.mu);
}

static int connect_peer(void) {
    const char* path = getenv("E3G_SOCK");
    if (path == NULL) {
        WARN("E3G_SOCK not set");
        return -1;
    }
    int s = socket(AF_UNIX, SOCK_SEQPACKET | SOCK_CLOEXEC, 0);
    if (s < 0) {
        WARN("socket: %s", strerror(errno));
        return -1;
    }
    struct sockaddr_un sa;
    memset(&sa, 0, sizeof(sa));
    sa.sun_family = AF_UNIX;
    snprintf(sa.sun_path, sizeof(sa.sun_path), "%s", path);
    if (connect(s, (struct sockaddr*)&sa, sizeof(sa)) != 0) {
        WARN("connect(%s): %s", path, strerror(errno));
        close(s);
        return -1;
    }
    g.sock = s;
    pthread_create(&g.reader, NULL, reader_main, NULL);
    g.reader_started = 1;
    DBG("connected to peer at %s", path);
    return 0;
}

// ---------------------------------------------------------------- (3) driver
static pthread_mutex_t g_start_mu = PTHREAD_MUTEX_INITIALIZER;
static uint32_t g_seq;

static void driver_init(int fd) {
    pthread_mutex_lock(&g_start_mu);
    if (g.fd == 0) {
        memset(&g, 0, sizeof(g));
        g.fd = fd;
        g.next_wire = 1;
        g.next_handle = 1;
        g_bump = 4096;
        pthread_mutex_init(&g.mu, NULL);
        pthread_cond_init(&g.cv_txn, NULL);
        pthread_cond_init(&g.cv_rsp, NULL);
        g.map = mmap(NULL, BINDER_VM_SIZE, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
        if (g.map == MAP_FAILED) {
            WARN("mmap rw alias failed: %s", strerror(errno));
            g.map = NULL;
            g.map_size = 0;
        } else {
            g.map_size = BINDER_VM_SIZE;
        }
        connect_peer();
    }
    pthread_mutex_unlock(&g_start_mu);
}

// Turn a pending BC_TRANSACTION/BC_REPLY into a wire frame.  Returns 0 on ok.
static int emit_transaction(const struct binder_transaction_data* tr, int is_reply, uint32_t seq) {
    size_t dsize = (size_t)tr->data_size;
    size_t osize = (size_t)tr->offsets_size;
    if (dsize + 16 > E3G_MAX_FRAME || osize > E3G_MAX_FRAME) {
        WARN("transaction too large (%zu/%zu)", dsize, osize);
        return -1;
    }
    uint8_t* data = malloc(dsize + 16);
    uint8_t* offs = malloc(osize + 16);
    if (dsize) memcpy(data, (const void*)(uintptr_t)tr->data.ptr.buffer, dsize);
    if (osize) memcpy(offs, (const void*)(uintptr_t)tr->data.ptr.offsets, osize);
    int fds[E3G_MAX_FD];
    int nfds = 0;
    if (xform_objects(data, dsize, offs, osize, 0, fds, &nfds) != 0) {
        free(data);
        free(offs);
        return -1;
    }
    // Dup the fds we hand to the peer: the peer owns the copies it receives.
    int dupfds[E3G_MAX_FD];
    int ndup = 0;
    for (int i = 0; i < nfds; i++) {
        int d = fcntl(fds[i], F_DUPFD_CLOEXEC, 0);
        if (d >= 0) dupfds[ndup++] = d;
        else WARN("dup fd %d failed: %s", fds[i], strerror(errno));
    }
    uint64_t target = is_reply ? 0 : (tr->target.ptr ? (uint64_t)tr->target.ptr : (uint64_t)tr->target.handle);
    uint32_t type = is_reply ? F_REP : F_REQ;
    int rc = send_frame(type, seq, tr->code, tr->flags, target, data, dsize, offs, osize, dupfds,
                        ndup);
    for (int i = 0; i < ndup; i++) close(dupfds[i]);
    free(data);
    free(offs);
    DBG("%s seq=%u code=%u flags=0x%x target=%llu size=%zu/%zu fds=%d",
        is_reply ? "REPLY" : "REQ", seq, tr->code, tr->flags, (unsigned long long)target, dsize,
        osize, nfds);
    return rc;
}

// Deliver one queued incoming transaction as a BR_TRANSACTION into T.pend.
static int deliver_incoming(void) {
    struct inq* q = g.in_head;
    if (!q) return -1;
    g.in_head = q->next;
    if (!g.in_head) g.in_tail = NULL;
    if (!g.in_head) pthread_cond_broadcast(&g.cv_txn);
    struct node* n = node_by_wire((uint32_t)q->target);
    if (n == NULL) {
        pthread_mutex_unlock(&g.mu);
        WARN("incoming transaction for unknown node %llu",
             (unsigned long long)q->target);
        pthread_mutex_lock(&g.mu);
        free(q->data);
        free(q);
        return -1;
    }
    size_t dsize = (size_t)q->data_size, osize = (size_t)q->offs_size;
    int fds[E3G_MAX_FD];
    memcpy(fds, q->fds, sizeof(int) * (size_t)q->nfds);
    int nfds = q->nfds;
    uint8_t* offs = q->data + dsize;
    pthread_mutex_unlock(&g.mu);
    xform_objects(q->data, dsize, offs, osize, 1, fds, &nfds);
    void* buf = mm_alloc(dsize + 8 + osize + 8);
    if (buf == NULL) {
        free(q->data);
        free(q);
        pthread_mutex_lock(&g.mu);
        return -1;
    }
    memcpy(buf, q->data, dsize);
    void* obuf = (uint8_t*)buf + ((dsize + 7) & ~(size_t)7);
    if (osize) memcpy(obuf, offs, osize);
    struct binder_transaction_data tr;
    memset(&tr, 0, sizeof(tr));
    tr.target.ptr = n->binder;
    tr.cookie = n->cookie;
    tr.code = q->code;
    tr.flags = q->flags;
    tr.sender_pid = 0;
    tr.sender_euid = 0;
    tr.data_size = dsize;
    tr.offsets_size = osize;
    tr.data.ptr.buffer = (uintptr_t)buf;
    tr.data.ptr.offsets = (uintptr_t)obuf;
    uint32_t cmd = BR_TRANSACTION;
    pend_append(&cmd, 4);
    pend_append(&tr, sizeof(tr));
    T.reply_to_seq = q->seq;
    T.have_reply_to = 1;
    DBG("BR_TRANSACTION seq=%u code=%u flags=0x%x node=%llx/%llx size=%zu/%zu", q->seq, q->code,
        q->flags, (unsigned long long)n->binder, (unsigned long long)n->cookie, dsize, osize);
    free(q->data);
    free(q);
    pthread_mutex_lock(&g.mu);
    return 0;
}

static int build_reply(struct slot* s) {
    size_t dsize = s->data_size, osize = s->offs_size;
    int fds[E3G_MAX_FD];
    int nfds = 0;
    xform_objects(s->data, dsize, s->offs, osize, 1, fds, &nfds);
    void* buf = mm_alloc(dsize + 8 + osize + 8);
    if (buf == NULL) return -1;
    memcpy(buf, s->data, dsize);
    void* obuf = (uint8_t*)buf + ((dsize + 7) & ~(size_t)7);
    if (osize) memcpy(obuf, s->offs, osize);
    struct binder_transaction_data tr;
    memset(&tr, 0, sizeof(tr));
    tr.target.ptr = 0;
    tr.cookie = 0;
    tr.code = 0;
    tr.flags = 0;
    tr.sender_pid = 0;
    tr.sender_euid = 0;
    tr.data_size = dsize;
    tr.offsets_size = osize;
    tr.data.ptr.buffer = (uintptr_t)buf;
    tr.data.ptr.offsets = (uintptr_t)obuf;
    uint32_t cmd = BR_REPLY;
    pend_append(&cmd, 4);
    pend_append(&tr, sizeof(tr));
    return 0;
}

static void handle_commands(const uint8_t* wbuf, size_t wsize) {
    size_t pos = 0;
    if (g_dbg < 0) g_dbg = getenv("E3G_DEBUG") != NULL;
    if (g_dbg) {
        char hex[3 * 64 + 1];
        int hn = 0;
        for (size_t i = 0; i < wsize && i < 64; i++)
            hn += snprintf(hex + hn, sizeof(hex) - (size_t)hn, "%02x", wbuf[i]);
        dprintf(2, "E3G: write_size=%zu bytes=%s\n", wsize, hex);
    }
    while (pos + 4 <= wsize) {
        uint32_t cmd;
        memcpy(&cmd, wbuf + pos, 4);
        pos += 4;
        switch (cmd) {
            case BC_TRANSACTION:
            case BC_REPLY: {
                if (pos + sizeof(struct binder_transaction_data) > wsize) {
                    WARN("truncated transaction command");
                    return;
                }
                struct binder_transaction_data tr;
                memcpy(&tr, wbuf + pos, sizeof(tr));
                pos += sizeof(tr);
                int is_reply = cmd == BC_REPLY;
                uint32_t seq;
                if (is_reply) {
                    if (!T.have_reply_to) {
                        WARN("BC_REPLY without an incoming transaction");
                        break;
                    }
                    seq = T.reply_to_seq;
                    T.have_reply_to = 0;
                    emit_transaction(&tr, 1, seq);
                } else {
                    seq = __sync_add_and_fetch(&g_seq, 1);
                    if (tr.target.ptr == 0 && tr.target.handle == 0) {
                        // servicemanager: synchronous, need a reply slot
                    }
                    if (!(tr.flags & TF_ONE_WAY)) {
                        pthread_mutex_lock(&g.mu);
                        struct slot* s = NULL;
                        for (int i = 0; i < E3G_MAX_SLOT; i++)
                            if (!g.slots[i].used) {
                                s = &g.slots[i];
                                break;
                            }
                        if (s == NULL) {
                            pthread_mutex_unlock(&g.mu);
                            WARN("no free reply slot");
                            break;
                        }
                        memset(s, 0, sizeof(*s));
                        s->used = 1;
                        s->seq = seq;
                        pthread_mutex_unlock(&g.mu);
                        T.reply_seq = (int)seq;
                    }
                    emit_transaction(&tr, 0, seq);
                }
                pend_cmd(BR_TRANSACTION_COMPLETE);
                break;
            }
            case BC_TRANSACTION_SG:
            case BC_REPLY_SG: {
                // The SG variant carries the same binder_transaction_data plus a
                // separate buffer for BINDER_TYPE_PTR objects; parcels below the
                // SG threshold never use it.  Handle the header half.
                if (pos + sizeof(struct binder_transaction_data_sg) > wsize) {
                    WARN("truncated SG command");
                    return;
                }
                struct binder_transaction_data_sg sg;
                memcpy(&sg, wbuf + pos, sizeof(sg));
                pos += sizeof(sg);
                DBG("SG transaction (buffers_size=%llu), treated as plain",
                    (unsigned long long)sg.buffers_size);
                struct binder_transaction_data tr = sg.transaction_data;
                int is_reply = cmd == BC_REPLY_SG;
                uint32_t seq = is_reply ? T.reply_to_seq : __sync_add_and_fetch(&g_seq, 1);
                if (is_reply) T.have_reply_to = 0;
                else if (!(tr.flags & TF_ONE_WAY)) {
                    pthread_mutex_lock(&g.mu);
                    for (int i = 0; i < E3G_MAX_SLOT; i++)
                        if (!g.slots[i].used) {
                            memset(&g.slots[i], 0, sizeof(g.slots[0]));
                            g.slots[i].used = 1;
                            g.slots[i].seq = seq;
                            break;
                        }
                    pthread_mutex_unlock(&g.mu);
                    T.reply_seq = (int)seq;
                }
                emit_transaction(&tr, is_reply, seq);
                pend_cmd(BR_TRANSACTION_COMPLETE);
                break;
            }
            case BC_FREE_BUFFER: {
                uint64_t p;
                if (pos + 8 > wsize) return;
                memcpy(&p, wbuf + pos, 8);
                pos += 8;
                mm_free((void*)(uintptr_t)p);
                break;
            }
            case BC_INCREFS:
            case BC_ACQUIRE:
            case BC_RELEASE:
            case BC_DECREFS: {
                if (pos + 4 > wsize) return;
                uint32_t handle;
                memcpy(&handle, wbuf + pos, 4);
                pos += 4;
                DBG("BC_%s handle=%u", cmd == BC_ACQUIRE ? "ACQUIRE" : cmd == BC_RELEASE ? "RELEASE"
                                       : cmd == BC_INCREFS ? "INCREFS" : "DECREFS",
                    handle);
                break;
            }
            case BC_INCREFS_DONE:
            case BC_ACQUIRE_DONE: {
                if (pos + 16 > wsize) return;
                pos += 16;
                break;
            }
            case BC_ATTEMPT_ACQUIRE: {
                if (pos + 12 > wsize) return;
                pos += 12;
                break;
            }
            case BC_ENTER_LOOPER:
            case BC_REGISTER_LOOPER:
                T.is_looper = 1;
                DBG("%s", cmd == BC_ENTER_LOOPER ? "BC_ENTER_LOOPER" : "BC_REGISTER_LOOPER");
                break;
            case BC_EXIT_LOOPER:
                T.is_looper = 0;
                break;
            case BC_REQUEST_DEATH_NOTIFICATION:
            case BC_CLEAR_DEATH_NOTIFICATION: {
                if (pos + 12 > wsize) return;
                pos += 12;  // struct binder_handle_cookie is packed
                DBG("%s (recorded, peer liveness not modelled)",
                    cmd == BC_REQUEST_DEATH_NOTIFICATION ? "BC_REQUEST_DEATH_NOTIFICATION"
                                                         : "BC_CLEAR_DEATH_NOTIFICATION");
                break;
            }
            case BC_DEAD_BINDER_DONE: {
                if (pos + 8 > wsize) return;
                pos += 8;
                break;
            }
            default:
                WARN("unknown BC cmd 0x%x at %zu/%zu", cmd, pos - 4, wsize);
                return;
        }
    }
}

static void flush_pend(struct binder_write_read* bwr) {
    size_t room = (size_t)bwr->read_size;
    size_t n = T.pend_len < room ? T.pend_len : room;
    if (n) {
        memcpy((void*)(uintptr_t)bwr->read_buffer, T.pend, n);
        memmove(T.pend, T.pend + n, T.pend_len - n);
        T.pend_len -= n;
    }
    bwr->read_consumed = n;
}

static int e3g_write_read(struct binder_write_read* bwr) {
    bwr->write_consumed = 0;
    bwr->read_consumed = 0;
    if (bwr->write_size) {
        handle_commands((const uint8_t*)(uintptr_t)bwr->write_buffer, (size_t)bwr->write_size);
        bwr->write_consumed = bwr->write_size;
    }
    if (bwr->read_size == 0) return 0;

    if (T.pend_len) {
        flush_pend(bwr);
        return 0;
    }
    if (T.reply_seq > 0) {
        uint32_t seq = (uint32_t)T.reply_seq;
        pthread_mutex_lock(&g.mu);
        struct slot* s = slot_for(seq);
        while (s && !s->done) pthread_cond_wait(&g.cv_rsp, &g.mu);
        if (!s) {
            pthread_mutex_unlock(&g.mu);
            WARN("reply slot for seq %u vanished", seq);
            T.reply_seq = -1;
            return 0;
        }
        struct slot done = *s;
        s->used = 0;
        s->data = NULL;
        s->offs = NULL;
        pthread_mutex_unlock(&g.mu);
        T.reply_seq = -1;
        // BR_TRANSACTION_COMPLETE was already handed out in the previous ioctl
        // (or is still pending); make sure it precedes BR_REPLY.
        pend_cmd(BR_TRANSACTION_COMPLETE);
        if (done.status != 0 || done.data == NULL) {
            uint32_t cmd = BR_FAILED_REPLY;
            pend_append(&cmd, 4);
        } else {
            build_reply(&done);
            free(done.data);
            free(done.offs);
        }
        flush_pend(bwr);
        return 0;
    }
    // Nothing pending: wait for an incoming transaction.  A non-looper thread
    // must not park here, or a oneway transact would hang; the timeout keeps a
    // misclassification from turning into a deadlock.
    pthread_mutex_lock(&g.mu);
    int waited = 0;
    while (g.in_head == NULL && waited < 1000) {
        struct timespec ts;
        clock_gettime(CLOCK_REALTIME, &ts);
        ts.tv_nsec += 20 * 1000 * 1000;
        if (ts.tv_nsec >= 1000000000) {
            ts.tv_sec++;
            ts.tv_nsec -= 1000000000;
        }
        if (!T.is_looper && waited >= 50) break;
        pthread_cond_timedwait(&g.cv_txn, &g.mu, &ts);
        waited += 20;
    }
    int have = g.in_head != NULL;
    int delivered = -1;
    if (have) delivered = deliver_incoming();
    pthread_mutex_unlock(&g.mu);
    if (delivered == 0) flush_pend(bwr);
    else bwr->read_consumed = 0;
    return 0;
}

// --------------------------------------------------- (4) fd / ioctl interception
static const char* kBinderPaths[] = {"/dev/binder", "/dev/hwbinder", "/dev/vndbinder",
                                     "/dev/binderfs/binder", NULL};
static int g_fake_fds[8];
static int g_fake_n;

static int is_binder_path(const char* p) {
    if (p == NULL) return 0;
    for (int i = 0; kBinderPaths[i]; i++)
        if (strcmp(p, kBinderPaths[i]) == 0) return 1;
    return 0;
}

static int is_our_fd(int fd) {
    for (int i = 0; i < g_fake_n; i++)
        if (g_fake_fds[i] == fd) return 1;
    return 0;
}

static int fake_binder(const char* path) {
    int fd = (int)syscall(__NR_memfd_create, "e3g-binder", MFD_CLOEXEC);
    if (fd < 0) {
        WARN("memfd_create failed: %s", strerror(errno));
        return -1;
    }
    if (ftruncate(fd, BINDER_VM_SIZE) != 0) WARN("ftruncate failed: %s", strerror(errno));
    if (g_fake_n < 8) g_fake_fds[g_fake_n++] = fd;
    driver_init(fd);
    DBG("open(%s) -> memfd %d (driver emulated in-process)", path, fd);
    return fd;
}

typedef int (*open_fn)(const char*, int, ...);
typedef int (*openat_fn)(int, const char*, int, ...);
typedef int (*ioctl_fn)(int, unsigned long, ...);

int open(const char* path, int flags, ...) {
    if (is_binder_path(path)) {
        int fd = fake_binder(path);
        if (fd >= 0) return fd;
    }
    static open_fn real;
    if (!real) real = (open_fn)dlsym(RTLD_NEXT, "open");
    va_list ap;
    va_start(ap, flags);
    mode_t mode = (mode_t)va_arg(ap, int);
    va_end(ap);
    return real(path, flags, mode);
}

int open64(const char* path, int flags, ...) {
    if (is_binder_path(path)) {
        int fd = fake_binder(path);
        if (fd >= 0) return fd;
    }
    static open_fn real;
    if (!real) real = (open_fn)dlsym(RTLD_NEXT, "open64");
    va_list ap;
    va_start(ap, flags);
    mode_t mode = (mode_t)va_arg(ap, int);
    va_end(ap);
    return real(path, flags, mode);
}

int __open_2(const char* path, int flags) {
    if (is_binder_path(path)) {
        int fd = fake_binder(path);
        if (fd >= 0) return fd;
    }
    static open_fn real;
    if (!real) real = (open_fn)dlsym(RTLD_NEXT, "__open_2");
    return real(path, flags);
}

int ioctl(int fd, int request, ...) {
    va_list ap;
    va_start(ap, request);
    void* arg = va_arg(ap, void*);
    va_end(ap);

    static ioctl_fn real;
    if (!real) real = (ioctl_fn)dlsym(RTLD_NEXT, "ioctl");

    if (is_our_fd(fd)) {
        switch ((unsigned int)request) {
            case BINDER_VERSION:
                if (arg) ((struct binder_version*)arg)->protocol_version =
                        BINDER_CURRENT_PROTOCOL_VERSION;
                return 0;
            case BINDER_SET_MAX_THREADS:
            case BINDER_ENABLE_ONEWAY_SPAM_DETECTION:
            case BINDER_THREAD_EXIT:
            case BINDER_SET_CONTEXT_MGR:
            case BINDER_SET_CONTEXT_MGR_EXT:
            case BINDER_SET_IDLE_TIMEOUT:
            case BINDER_SET_IDLE_PRIORITY:
                DBG("ioctl(0x%x) -> 0", request);
                return 0;
            case BINDER_WRITE_READ:
                return e3g_write_read((struct binder_write_read*)arg);
            default:
                WARN("ioctl(fd=%d, req=0x%x) -> ENOTTY", fd, (unsigned)request);
                errno = ENOTTY;
                return -1;
        }
    }
    return ((ioctl_fn)real)(fd, (unsigned long)request, arg);
}

// --------------------------------------------------------- (4) JNI bridge
static void mangle_android_register(const char* plain, char* out, size_t n) {
    snprintf(out, n, "_ZN7android%zu%sEP7_JNIEnv", strlen(plain), plain);
}

static void mangle_global_register(const char* plain, char* out, size_t n) {
    snprintf(out, n, "_Z%zu%sP7_JNIEnv", strlen(plain), plain);
}

static void* lookup_symbol(const char* plain, char* mangled, size_t n, int* which) {
    mangle_android_register(plain, mangled, n);
    void* p = dlsym(RTLD_DEFAULT, mangled);
    if (p) {
        *which = 1;
        return p;
    }
    char gmangled[256];
    mangle_global_register(plain, gmangled, sizeof(gmangled));
    p = dlsym(RTLD_DEFAULT, gmangled);
    if (p) {
        snprintf(mangled, n, "%s", gmangled);
        *which = 2;
        return p;
    }
    p = dlsym(RTLD_DEFAULT, plain);
    *which = 3;
    return p;
}

JNIEXPORT jint JNICALL Java_dev_acos_e3g_Nat_register(JNIEnv* env, jclass klass, jstring name) {
    (void)klass;
    const char* plain = (*env)->GetStringUTFChars(env, name, NULL);
    char mangled[256];
    int which = 0;
    void* p = lookup_symbol(plain, mangled, sizeof(mangled), &which);
    if (p == NULL) {
        WARN("register: missing symbol %s (%s)", plain, mangled);
        (*env)->ReleaseStringUTFChars(env, name, plain);
        return 0;
    }
    typedef void (*register_fn)(JNIEnv*);
    register_fn fn = (register_fn)p;
    DBG("register: %s @ %p", plain, p);
    fn(env);
    jint rc = 1;
    if ((*env)->ExceptionCheck(env)) {
        jthrowable t = (*env)->ExceptionOccurred(env);
        (*env)->ExceptionClear(env);
        (*env)->Throw(env, t);
        rc = -1;
    }
    (*env)->ReleaseStringUTFChars(env, name, plain);
    return rc;
}

// Attach a native thread to the VM and turn it into a binder looper:
// IPCThreadState::self()->joinThreadPool(true).  This is the thread that
// receives peer-initiated BR_TRANSACTIONs (reverse calls into a Java Binder).
struct looper_arg {
    JavaVM* vm;
    volatile int running;
};

static void* looper_main(void* p) {
    struct looper_arg* a = p;
    JNIEnv* env = NULL;
    jint rc = (*a->vm)->AttachCurrentThread(a->vm, &env, NULL);
    if (rc != JNI_OK) {
        WARN("AttachCurrentThread failed: %d", (int)rc);
        return NULL;
    }
    DBG("looper thread attached (env=%p)", env);
    typedef void* (*self_fn)(void);
    typedef void (*join_fn)(void*, int);
    self_fn self = (self_fn)dlsym(RTLD_DEFAULT, "_ZN7android14IPCThreadState4selfEv");
    join_fn join = (join_fn)dlsym(RTLD_DEFAULT, "_ZN7android14IPCThreadState14joinThreadPoolEb");
    if (!self || !join) {
        WARN("IPCThreadState symbols not found (self=%p join=%p)", (void*)self, (void*)join);
        return NULL;
    }
    void* st = self();
    DBG("IPCThreadState=%p -> joinThreadPool(true)", st);
    a->running = 1;
    join(st, 1);
    WARN("joinThreadPool returned");
    return NULL;
}

JNIEXPORT jint JNICALL Java_dev_acos_e3g_Nat_startBinderLooper(JNIEnv* env, jclass klass) {
    (void)klass;
    static struct looper_arg arg;
    JavaVM* vm = NULL;
    if ((*env)->GetJavaVM(env, &vm) != 0 || vm == NULL) {
        WARN("GetJavaVM failed");
        return -1;
    }
    arg.vm = vm;
    pthread_t t;
    if (pthread_create(&t, NULL, looper_main, &arg) != 0) {
        WARN("pthread_create(looper) failed");
        return -2;
    }
    pthread_detach(t);
    for (int i = 0; i < 500 && !arg.running; i++) usleep(1000);
    DBG("looper started=%d", arg.running);
    return arg.running ? 1 : 0;
}

// Peer diagnostics: number of nodes/handles and whether the socket is live.
JNIEXPORT jstring JNICALL Java_dev_acos_e3g_Nat_state(JNIEnv* env, jclass klass) {
    (void)klass;
    char buf[512];
    snprintf(buf, sizeof(buf),
             "sock=%d driver_fd=%d map=%p sizes=%zu nodes=%d handles=%d inq=%s",
             g.sock, g.fd, (void*)g.map, g.map_size, g.n_nodes, g.n_handles,
             g.in_head ? "non-empty" : "empty");
    return (*env)->NewStringUTF(env, buf);
}
