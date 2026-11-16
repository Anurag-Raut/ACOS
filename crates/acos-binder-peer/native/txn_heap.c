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
#include "txn_heap.h"

#include <pthread.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

struct blk {
    size_t off, size;
    int used;
};

// Monotonic allocation: the kernel may reuse a freed transaction buffer, but a
// userspace driver cannot prove libbinder has dropped every reference to the
// old contents at the moment BC_FREE_BUFFER arrives. The mapping is 1 MB and a
// delivered transaction is a few hundred bytes, so never re-using a block is
// both safe and cheap (E3G's race came from premature re-use).
#define TXN_HEAP_MAX_BLOCKS 512

static pthread_mutex_t g_heap_mu = PTHREAD_MUTEX_INITIALIZER;
static struct blk g_blk[TXN_HEAP_MAX_BLOCKS];
static int g_nblk;
static size_t g_bump;
static uint8_t* g_map;
static size_t g_map_size;

static int blk_cmp(const void* a, const void* b) {
    const struct blk* x = a;
    const struct blk* y = b;
    return (x->off > y->off) - (x->off < y->off);
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
    if (g_map == NULL || n > g_map_size || g_bump + n > g_map_size) return NULL;
    size_t off = g_bump;
    g_bump += n;
    if (g_nblk < TXN_HEAP_MAX_BLOCKS) {
        g_blk[g_nblk].off = off;
        g_blk[g_nblk].size = n;
        g_blk[g_nblk].used = 1;
        g_nblk++;
    }
    return g_map + off;
}

void* txn_alloc(size_t n) {
    pthread_mutex_lock(&g_heap_mu);
    void* p = heap_alloc_locked(n);
    pthread_mutex_unlock(&g_heap_mu);
    return p;
}

void txn_free(void* p) {
    if (p == NULL) return;
    pthread_mutex_lock(&g_heap_mu);
    if (g_map != NULL && (uint8_t*)p >= g_map && (uint8_t*)p < g_map + g_map_size) {
        size_t off = (size_t)((uint8_t*)p - g_map);
        for (int i = 0; i < g_nblk; i++) {
            if (g_blk[i].off == off && g_blk[i].used) {
                g_blk[i].used = 0;
                // coalesce with neighbours (bookkeeping only; allocations stay
                // monotonic, see the header comment)
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
                break;
            }
        }
        // A free of an unknown/foreign buffer is a no-op, as before.
    }
    pthread_mutex_unlock(&g_heap_mu);
}

size_t txn_heap_used(void) {
    pthread_mutex_lock(&g_heap_mu);
    size_t used = g_bump;
    pthread_mutex_unlock(&g_heap_mu);
    return used;
}

size_t txn_heap_capacity(void) {
    pthread_mutex_lock(&g_heap_mu);
    size_t cap = g_map_size;
    pthread_mutex_unlock(&g_heap_mu);
    return cap;
}
