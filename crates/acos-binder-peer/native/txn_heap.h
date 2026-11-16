// txn_heap.h -- the ACOS userspace binder driver's transaction-buffer heap.
//
// AOSP's binder driver allocates a transaction buffer inside the target
// process's binder mapping (`binder_alloc_new_buf`), under the process's
// `alloc->mutex`; the buffer is handed to the *receiver* and stays valid until
// it sends `BC_FREE_BUFFER`. ACOS's driver stands in for that allocator over
// the guest's own memfd mapping, and the same two properties are required:
//
//   * two binder threads may ask for a buffer **at the same time** (a reply on
//     the thread that is blocked in `transact` and a peer-initiated
//     `BR_TRANSACTION` on the receiving looper thread are the launch case:
//     `addToDisplayAsUser`'s reply and `IWindow.dispatchAppVisibility`), and
//   * a delivered buffer's contents must not change while the receiver can
//     still read them.
//
// This translation unit exists separately from `binder_driver.c` so the
// allocator can be compiled and raced on the host (`crates/acos-binder-peer/
// tests/txn_heap.rs`) instead of only inside an Android guest: it has no
// bionic, JNI or binder dependency.
#ifndef ACOS_TXN_HEAP_H
#define ACOS_TXN_HEAP_H

#include <stddef.h>

// Hand the heap the region it may allocate from. `map` must stay mapped for the
// life of the process. Every allocation is 8-byte aligned and returns NULL once
// the region is exhausted.
void txn_heap_init(void* map, size_t size);

// Allocate `n` bytes of transaction buffer, or NULL when the region is
// exhausted. Thread-safe. Never returns a block that is still referenced.
void* txn_alloc(size_t n);

// Release a block returned by `txn_alloc`. Unknown pointers are ignored.
void txn_free(void* p);

// Bytes handed out so far / bytes in the region (diagnostics and tests).
size_t txn_heap_used(void);
size_t txn_heap_capacity(void);

#endif // ACOS_TXN_HEAP_H
