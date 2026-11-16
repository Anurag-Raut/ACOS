//! FLAKEWIN regression: the transaction-buffer heap must be safe under the
//! concurrency the driver actually has.
//!
//! The binder driver allocates transaction buffers from **two threads at once**
//! by design: the reply to a synchronous transaction is allocated by the thread
//! blocked in `transact` (`e3g_write_read` → `build_reply`), while a
//! peer-initiated `BR_TRANSACTION` is allocated by the receiving looper thread
//! (`deliver_incoming`). Both call `txn_alloc` with `g.mu` unlocked.
//!
//! The pre-FLAKEWIN allocator was a lock-free bump heap: two concurrent callers
//! read the same `g_bump` and got **the same offset**, so the second `memcpy`
//! overwrote the first parcel in place. In a guest that is the
//! `addToDisplayAsUser` reply being overwritten by the
//! `IWindow.dispatchAppVisibility` request that follows it, which AOSP reports
//! as `RuntimeException: Adding window failed`.
//!
//! This test compiles the real `native/txn_heap.c` for the host (it has no
//! bionic/JNI dependency) and checks the two properties the driver needs:
//!
//!  1. **no two live blocks overlap** (FLAKEWIN): race several threads with no
//!     frees and look for a duplicated offset. Before the lock it reports
//!     thousands of them; after it, zero.
//!  2. **freed space is reusable, and reuse never hands a live block to a second
//!     owner** (BINDERREF): race the same threads with a free after every block
//!     and verify each block's contents are still intact after a yield. A
//!     monotonic heap never reuses (the registration wall); a reuse bug shows up
//!     as a block whose pattern another thread overwrote.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The stress program. It records every block it is handed and looks for
/// overlaps after all threads join, so it detects a duplicated offset even when
/// the two writers did not happen to land in the same window.
const STRESS: &str = r#"
#include <pthread.h>
#include <sched.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include "txn_heap.h"

#define THREADS 4
#define ITERS   50000
#define REGION  (128u * 1024u * 1024u)

struct rec { size_t off, size; };

static struct rec g_rec[THREADS][ITERS];
static unsigned char g_pattern[THREADS][ITERS];
static pthread_barrier_t g_start;
// Phase B only: how many pattern checks failed (another owner wrote the block
// while we still held it).
static int g_mismatch;
static long g_reused;

static size_t block_size(unsigned seed) {
    // A delivered transaction is a few hundred bytes.
    return 64 + (size_t)((seed >> 16) % 512);
}

// Phase A: every thread allocates and holds. No frees, so a duplicated offset is
// a block handed to two live owners -- the FLAKEWIN overwrite-in-place race.
static void* worker_hold(void* arg) {
    long t = (long)arg;
    unsigned seed = 0x9e3779b9u * (unsigned)(t + 1);
    pthread_barrier_wait(&g_start);
    for (int i = 0; i < ITERS; i++) {
        seed = seed * 1664525u + 1013904223u;
        size_t n = block_size(seed);
        void* p = txn_alloc(n);
        if (p == NULL) {
            fprintf(stderr, "txn_alloc returned NULL at iteration %d (capacity %zu)\n",
                    i, txn_heap_capacity());
            exit(2);
        }
        g_rec[t][i].off = (size_t)((uint8_t*)p - (uint8_t*)0);
        g_rec[t][i].size = n;
        g_pattern[t][i] = (unsigned char)(t * 31 + i * 7 + 1);
        memset(p, g_pattern[t][i], n);
    }
    return NULL;
}

// Phase B: allocate, stamp, yield (so another thread has every chance to be
// handed the same block), verify the stamp, then free. This is the lifetime the
// driver has: `BC_FREE_BUFFER` arrives when the receiver's Parcel is gone, and
// after that another transaction may take the block.
static void* worker_reuse(void* arg) {
    long t = (long)arg;
    unsigned seed = 0x85ebca6bu * (unsigned)(t + 3);
    pthread_barrier_wait(&g_start);
    for (int i = 0; i < ITERS; i++) {
        seed = seed * 1664525u + 1013904223u;
        size_t n = block_size(seed);
        void* p = txn_alloc(n);
        if (p == NULL) {
            fprintf(stderr, "reuse phase: txn_alloc NULL at %d (in use %zu of %zu)\n", i,
                    txn_heap_in_use(), txn_heap_capacity());
            exit(2);
        }
        unsigned char pat = (unsigned char)(t * 17 + i * 5 + 1);
        memset(p, pat, n);
        sched_yield();
        for (size_t k = 0; k < n; k++) {
            if (((unsigned char*)p)[k] != pat) {
                __sync_fetch_and_add(&g_mismatch, 1);
                break;
            }
        }
        txn_free(p);
    }
    return NULL;
}

static int cmp(const void* a, const void* b) {
    const struct rec* x = a;
    const struct rec* y = b;
    if (x->off != y->off) return x->off < y->off ? -1 : 1;
    return 0;
}

static void race(void* (*worker)(void*)) {
    pthread_barrier_init(&g_start, NULL, THREADS);
    pthread_t th[THREADS];
    for (long t = 0; t < THREADS; t++) pthread_create(&th[t], NULL, worker, (void*)t);
    for (int t = 0; t < THREADS; t++) pthread_join(th[t], NULL);
    pthread_barrier_destroy(&g_start);
}

int main(void) {
    void* map = mmap(NULL, REGION, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (map == MAP_FAILED) { perror("mmap"); return 2; }
    txn_heap_init(map, REGION);
    if (txn_heap_capacity() != REGION) { fprintf(stderr, "capacity mismatch\n"); return 2; }

    // ---- phase A: concurrent allocation, nothing freed -> no shared block ----
    race(worker_hold);
    size_t total = (size_t)THREADS * ITERS;
    struct rec* all = malloc(total * sizeof(*all));
    size_t k = 0;
    for (int t = 0; t < THREADS; t++)
        for (int i = 0; i < ITERS; i++) all[k++] = g_rec[t][i];
    qsort(all, total, sizeof(*all), cmp);
    size_t dup = 0, overlap = 0;
    for (size_t i = 1; i < total; i++) {
        if (all[i].off == all[i - 1].off) dup++;
        else if (all[i].off < all[i - 1].off + all[i - 1].size) overlap++;
    }
    free(all);

    // ---- phase B: same races, but every block is freed -> reuse, no aliasing --
    txn_heap_init(map, REGION);
    size_t before_water = txn_heap_used();
    race(worker_reuse);
    size_t after_water = txn_heap_used();
    size_t live = txn_heap_in_use();
    // Reuse proof: the whole phase ran in a region far smaller than its traffic
    // (4 * 50k blocks, average ~320 bytes = ~64 MB of allocations). A monotonic
    // heap needed 64 MB; reuse needs a few KB.
    g_reused = (after_water <= 256u * 1024u) ? 1 : 0;

    printf("allocations=%zu distinct_offsets=%zu duplicates=%zu overlaps=%zu used=%zu\n",
           total, total - dup, dup, overlap, after_water);
    printf("reuse: mismatches=%d high_water=%zu in_use=%zu reused=%ld\n", g_mismatch,
           after_water, live, g_reused);
    if (dup != 0 || overlap != 0) {
        fprintf(stderr, "FAIL: the transaction heap handed out the same block twice "
                        "(%zu duplicate offsets, %zu overlaps)\n", dup, overlap);
        return 1;
    }
    if (g_mismatch != 0) {
        fprintf(stderr, "FAIL: %d blocks were overwritten while still owned by a caller\n",
                g_mismatch);
        return 1;
    }
    if (g_reused != 1) {
        fprintf(stderr, "FAIL: 64 MB of freed blocks were not reused (high-water %zu)\n",
                after_water);
        return 1;
    }
    if (before_water != 0 || live != 0) {
        fprintf(stderr, "FAIL: heap not empty after the reuse phase (in use %zu)\n", live);
        return 1;
    }
    return 0;
}
"#;

fn native_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("native")
}

/// Compile `native/txn_heap.c` + the stress program with the host C compiler.
fn build_stress(dir: &Path) -> PathBuf {
    let src = dir.join("txn_heap_stress.c");
    std::fs::write(&src, STRESS).expect("write stress source");
    let bin = dir.join("txn_heap_stress");
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let out = Command::new(&cc)
        .args(["-O2", "-pthread", "-Wall", "-Wextra", "-Wno-unused-parameter", "-o"])
        .arg(&bin)
        .arg(&src)
        .arg(native_dir().join("txn_heap.c"))
        .arg("-I")
        .arg(native_dir())
        .output()
        .unwrap_or_else(|e| panic!("cannot run the C compiler {cc:?}: {e}"));
    assert!(
        out.status.success(),
        "C compile failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    bin
}

#[test]
fn concurrent_transaction_buffer_allocations_never_overlap() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("txn_heap");
    std::fs::create_dir_all(&dir).expect("create test scratch dir");
    let bin = build_stress(&dir);
    let out = Command::new(&bin).output().expect("run stress binary");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "the transaction heap gave two live buffers the same range\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("duplicates=0 overlaps=0"),
        "unexpected stress output: {stdout}"
    );
    assert!(
        stdout.contains("reuse: mismatches=0") && stdout.contains("reused=1"),
        "the heap did not reclaim freed space under load: {stdout}"
    );
}

/// The driver also needs `txn_free` to be a no-op for pointers it does not own
/// (it sees the guest's `BC_FREE_BUFFER` for buffers from other threads) and to
/// never hand out a block that is still mapped.
#[test]
fn free_of_a_foreign_pointer_is_ignored_and_exhaustion_is_named() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("txn_heap_edges");
    std::fs::create_dir_all(&dir).expect("create test scratch dir");
    let src = dir.join("txn_heap_edges.c");
    std::fs::write(
        &src,
        r#"
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include "txn_heap.h"
int main(void) {
    void* map = mmap(NULL, 8192, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (map == MAP_FAILED) return 2;
    txn_heap_init(map, 8192);
    void* a = txn_alloc(64);
    if (a == NULL) return 2;
    void* b = txn_alloc(64);
    if (b == NULL) return 2;
    if (b == a || (size_t)b < (size_t)a + 64) { fprintf(stderr, "overlapping blocks\n"); return 1; }
    txn_free((void*)((uint8_t*)a + 3));      // not a block start: ignored
    txn_free((void*)0x10);                   // outside the mapping: ignored
    txn_free(NULL);                          // ignored
    // A stale free of a live block must not make it reusable: `b` is still owned.
    txn_free((void*)((uint8_t*)b + 3));
    txn_free(a);                             // BC_FREE_BUFFER: `a` is free now
    txn_free(a);                             // a double free is a no-op
    void* c = txn_alloc(64);
    if (c != a) {
        fprintf(stderr, "the freed block was not reused (a=%p c=%p)\n", a, c);
        return 1;
    }
    txn_free(c);
    txn_free(b);
    // Exhaust the region: the driver must see a named NULL, never a short block.
    size_t got = 0;
    while (txn_alloc(256) != NULL) got++;
    if (txn_alloc(256) != NULL) return 1;
    printf("edges ok, exhausted after %zu further blocks\n", got);
    return 0;
}
"#,
    )
    .expect("write edge-case source");
    let bin = dir.join("txn_heap_edges");
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let out = Command::new(&cc)
        .args(["-O2", "-pthread", "-o"])
        .arg(&bin)
        .arg(&src)
        .arg(native_dir().join("txn_heap.c"))
        .arg("-I")
        .arg(native_dir())
        .output()
        .unwrap_or_else(|e| panic!("cannot run the C compiler {cc:?}: {e}"));
    assert!(
        out.status.success(),
        "C compile failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&bin).output().expect("run edge-case binary");
    assert!(
        run.status.success(),
        "edge cases failed:\n{}\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
}
