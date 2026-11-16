// txn_heap.c -- see txn_heap.h.
//
// FLAKEWIN (v0.3.0 launch flake): this was the driver's `mm_alloc`/`mm_free`,
// a bump allocator with no lock. Two binder threads asking for a buffer in the
// same window both read the same `g_bump`, got **the same offset**, and the
// second `memcpy` overwrote the first parcel in place. The guest's
// `addToDisplayAsUser` reply then held the *other* transaction's data (the
// `IWindow.dispatchAppVisibility` request), libbinder could not walk its object
// table (`ipcSetDataReference: unsupported type object (7209057) at offset 16`)
// and `ViewRootImpl.setView` threw `RuntimeException: Adding window failed`
// (`Unknown exception code: -1040187388` = the `0xC2000004` strict-mode word of
// the request's interface-token prelude). The kernel driver allocates under
// `binder_alloc`'s mutex for exactly this reason; so does this heap now.
//
// BINDERREF (v0.3.0): the heap **reclaims**. FLAKEWIN's fix made allocation
// safe but kept it monotonic; `txn_free` only flipped a `used` bit, so every
// transaction consumed buffer space for the life of the process. A session with
// registered callbacks spends one buffer per registration plus one per callback,
// and the driver's mapping is 1 MB (`BINDER_VM_SIZE`), so a `--registers 100`
// session ran out after ~3000 transactions and every later oneway delivery was
// dropped (measured: 60 of 101 registrations dispatched, then none).
//
// `BC_FREE_BUFFER` is the receiver's release of the buffer
// (`IPCThreadState::freeBuffer`, run from the received `Parcel`'s destructor) --
// on Linux that is `binder_free_buf` handing the buffer back to
// `alloc->free_buffers`, and `binder_alloc_new_buf` then allocates first-fit out
// of those freed buffers before touching the remaining space. This file does the
// same: a freed block is reusable by the next allocation that fits it (and is
// split when only part of it is needed), adjacent free blocks coalesce, and the
// high-water mark is the fallback once no free block fits.
//
// Reuse is safe here because the driver only frees a buffer the *receiving*
// thread owns (`T.txn_buf`, `binder_get_txn_from_and_acq_inner` in the kernel):
// a stale or replayed `BC_FREE_BUFFER` is dropped, so a block can never be
// handed out while libbinder can still read it.
#include "txn_heap.h"

#include <pthread.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

struct blk {
    size_t off, size;
    int used;
};

static pthread_mutex_t g_heap_mu = PTHREAD_MUTEX_INITIALIZER;
// Sorted by offset. Grown on demand: a busy session has one block per live
// transaction, and the 512-entry array this heap started with silently stopped
// tracking blocks once it filled (the bump pointer kept moving, so exhaustion
// arrived with blocks that looked free).
static struct blk* g_blk;
static int g_nblk, g_blk_cap;
// High-water mark: the end of the space ever handed out.
static size_t g_bump;
static uint8_t* g_map;
static size_t g_map_size;

static int blk_cmp(const void* a, const void* b) {
    const struct blk* x = a;
    const struct blk* y = b;
    return (x->off > y->off) - (x->off < y->off);
}

// Callers must hold g_heap_mu.
static int blk_reserve_locked(int want) {
    if (want <= g_blk_cap) return 1;
    int cap = g_blk_cap ? g_blk_cap : 256;
    while (cap < want) {
        if (cap > (1 << 20)) return 0;
        cap *= 2;
    }
    struct blk* p = realloc(g_blk, sizeof(*p) * (size_t)cap);
    if (p == NULL) return 0;
    g_blk = p;
    g_blk_cap = cap;
    return 1;
}

// Insert a block keeping g_blk sorted by offset. Callers must hold g_heap_mu.
static void blk_insert_locked(int at, size_t off, size_t size, int used) {
    if (!blk_reserve_locked(g_nblk + 1)) {
        // No bookkeeping room: the allocation still stands (it was handed out
        // by the high-water path) but it will never be reusable. Say so.
        return;
    }
    memmove(&g_blk[at + 1], &g_blk[at], sizeof(g_blk[0]) * (size_t)(g_nblk - at));
    g_blk[at].off = off;
    g_blk[at].size = size;
    g_blk[at].used = used;
    g_nblk++;
}

// Index of the block starting at `off`, or -1. Callers must hold g_heap_mu.
static int blk_index_locked(size_t off) {
    int lo = 0, hi = g_nblk - 1;
    while (lo <= hi) {
        int mid = lo + (hi - lo) / 2;
        if (g_blk[mid].off == off) return mid;
        if (g_blk[mid].off < off) {
            lo = mid + 1;
        } else {
            hi = mid - 1;
        }
    }
    return -1;
}

void txn_heap_init(void* map, size_t size) {
    pthread_mutex_lock(&g_heap_mu);
    g_map = map;
    g_map_size = (map != NULL) ? size : 0;
    g_nblk = 0;
    g_bump = 0;
    pthread_mutex_unlock(&g_heap_mu);
}

// Callers must hold g_heap_mu.
static void* heap_alloc_locked(size_t n) {
    n = (n + 7) & ~(size_t)7;
    if (g_map == NULL || n == 0 || n > g_map_size) return NULL;
    // First fit: the kernel's `binder_alloc_new_buf` looks in
    // `alloc->free_buffers` (an rbtree of freed buffers) before it grows.
    for (int i = 0; i < g_nblk; i++) {
        if (g_blk[i].used || g_blk[i].size < n) continue;
        size_t off = g_blk[i].off;
        if (g_blk[i].size == n) {
            g_blk[i].used = 1;
        } else {
            // Split: the tail stays free and keeps its place in the table.
            size_t rest = g_blk[i].size - n;
            g_blk[i].size = n;
            g_blk[i].used = 1;
            blk_insert_locked(i + 1, off + n, rest, 0);
        }
        return g_map + off;
    }
    if (g_bump + n > g_map_size) return NULL;
    size_t off = g_bump;
    g_bump += n;
    int at = 0;
    while (at < g_nblk && g_blk[at].off < off) at++;
    blk_insert_locked(at, off, n, 1);
    return g_map + off;
}

void* txn_alloc(size_t n) {
    pthread_mutex_lock(&g_heap_mu);
    void* p = heap_alloc_locked(n);
    pthread_mutex_unlock(&g_heap_mu);
    return p;
}

// Callers must hold g_heap_mu.
static void heap_free_locked(void* p) {
    size_t off = (size_t)((uint8_t*)p - g_map);
    int i = blk_index_locked(off);
    if (i < 0 || !g_blk[i].used) {
        // Foreign pointer, double free, or a block the table stopped tracking:
        // a no-op, never a corruption.
        return;
    }
    g_blk[i].used = 0;
    // Coalesce with the next block, then with the previous one.
    if (i + 1 < g_nblk && !g_blk[i + 1].used &&
        g_blk[i].off + g_blk[i].size == g_blk[i + 1].off) {
        g_blk[i].size += g_blk[i + 1].size;
        memmove(&g_blk[i + 1], &g_blk[i + 2], sizeof(g_blk[0]) * (size_t)(g_nblk - i - 2));
        g_nblk--;
    }
    if (i > 0 && !g_blk[i - 1].used &&
        g_blk[i - 1].off + g_blk[i - 1].size == g_blk[i].off) {
        g_blk[i - 1].size += g_blk[i].size;
        memmove(&g_blk[i], &g_blk[i + 1], sizeof(g_blk[0]) * (size_t)(g_nblk - i - 1));
        g_nblk--;
    }
}

void txn_free(void* p) {
    if (p == NULL) return;
    pthread_mutex_lock(&g_heap_mu);
    if (g_map != NULL && (uint8_t*)p >= g_map && (uint8_t*)p < g_map + g_map_size)
        heap_free_locked(p);
    pthread_mutex_unlock(&g_heap_mu);
}

size_t txn_heap_used(void) {
    pthread_mutex_lock(&g_heap_mu);
    size_t used = g_bump;
    pthread_mutex_unlock(&g_heap_mu);
    return used;
}

size_t txn_heap_in_use(void) {
    pthread_mutex_lock(&g_heap_mu);
    size_t used = 0;
    for (int i = 0; i < g_nblk; i++)
        if (g_blk[i].used) used += g_blk[i].size;
    pthread_mutex_unlock(&g_heap_mu);
    return used;
}

size_t txn_heap_capacity(void) {
    pthread_mutex_lock(&g_heap_mu);
    size_t cap = g_map_size;
    pthread_mutex_unlock(&g_heap_mu);
    return cap;
}
