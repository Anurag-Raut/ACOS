# W145-A — allocation/invoke/GC: field-array free list + amortised host guard

**Status:** measured and landed in the W145-A worktree of `2c682c7`
(`.cache/wt-w145a`, `CARGO_TARGET_DIR=.cache/target-w145a`,
`CARGO_PROFILE_DEV_DEBUG=0`, release, `tools/guarded.sh`). No `unsafe`
(`#![forbid(unsafe_code)]`); the snapshot on-disk format is unchanged.

## 1. Profile (what the counters say)

`vmbench` on the four dexprogs (release, `ACOS_CORELIB` default):

| Program | insns | invokes | allocs | gc | gc_ms | peak MB |
|---|---:|---:|---:|---:|---:|---:|
| `BenchAlloc` | 140,000,022 | 30,000,011 | 10,000,018 | 38 | 85 | 16.0 |
| `BenchGc` | 44,204,839 | 51,219 | 4,025,620 | 8 | 54 | 202.7 |

`BenchAlloc` is **allocation + invoke bound**, not dispatch bound: 10 M
`new-instance` of a 2-slot `Node`, each followed by the `Node.<init>` and
`Object.<init>` invokes (30 M invokes), and only 38 GCs whose total is ~5 % of
the run. `BenchGc` allocates 4 M small `int[]`; GC is ~13 % of it.

Per-allocation work on the `new-instance` path before this wave:

1. `exec_new_instance` → `alloc_instance` → `alloc` → `alloc_raw`.
2. `take_field_vec`: **one host `Vec<u32>` allocation per instance**. The
   `field_pool` free list only gains entries in `free_slot`, i.e. at GC, and was
   capped at a fixed `4096` entries. A collection frees ~330 k tiny instances
   per 16 MiB cycle, so 4096 entries recycled almost none of them: ~99 % of the
   10 M field arrays were fresh `malloc`s.
3. `HostGuard::check` ran on **every** allocation and evaluated the wall clock
   (`Instant::now()`) unconditionally — a vDSO call per object, ~20 ns × 14 M
   allocs ≈ 0.28 s on `BenchAlloc`.

The invoke and GC paths were inspected and are already in the target shape:
frames are looked up by `MethodId` (no `Arc<Method>` clone per call), the
per-thread `frame_pool` reuses register buffers, the virtual/method caches are
inline, GC marks into a generation bitmap with a reused worklist, and the sweep
has no allocation. No further ≥ 3 % win was found in the time box; the retained
levers are listed in §5.

## 2. Landed

1. **Byte-budgeted instance field-array free list** (`heap.rs`).
   `Heap::field_pool_words` tracks the `u32` capacity retained by
   `field_pool`; `free_slot` recycles a small (`capacity <= 64`) array while the
   pool is under `FIELD_POOL_MAX_WORDS` (32 MiB / 4). `take_field_vec` releases
   the budget on pop. This makes the pool the real free list for field storage:
   after the first collection it satisfies essentially every subsequent
   `new-instance`, removing ~10 M host allocations from `BenchAlloc`.
2. **Amortised `HostGuard::check`** (`limits.rs`). The wall-clock read and the
   `/proc/self/status` sample are now both behind the existing 64-call tick.
   The scheduler still calls `check()` once per instruction batch, so a thread
   that never allocates is still bounded; an allocating loop can overshoot the
   wall ceiling by at most 63 allocations (a few microseconds). `Instant::now`
   is no longer paid per object.
3. **Snapshot restore** (`snapshot.rs`) resets `field_pool_words` together with
   `field_pool.clear()`, so a restored VM's pool accounting starts clean.
   `Heap` is not serialised, so the on-disk format and `SNAPSHOT_FORMAT_VERSION`
   are untouched.

## 3. Before / after (5 interleaved runs, `vmbench`, `2c682c7`)

`vmbench <dex> <main> 1`, alternating base/opt order, same host window,
`tools/guarded.sh`; p50 (and min) of 5 samples. Base is a pristine `2c682c7`
worktree binary (`.cache/...` base build, `/tmp/vmbench-w145a-base2`).

| Program | before p50 (ms) | after p50 (ms) | p50 Δ | before min | after min | min Δ | verdict |
|---|---:|---:|---:|---:|---:|---:|---|
| `BenchAlloc` | 1772.8 | 1498.0 | **−15.5 %** | 1756.7 | 1472.4 | −16.2 % | keep |
| `BenchGc` | 404.9 | 314.7 | **−22.3 %** | 386.1 | 311.7 | −19.3 % | keep |

Decomposition (each change measured against the previous binary):

| Config | BenchAlloc | BenchGc |
|---|---:|---:|
| field free list only (vs pristine) | −4.4 % | −0.6 % (neutral) |
| + amortised host guard (vs field list) | −11.5 % | −18.8 % |
| both vs pristine (`0b4db83`, 7 rounds) | −13.3 % | −19.5 % |

`BenchGc` does not allocate instances, so its win comes entirely from the host
guard (4 M allocs × the per-object clock read); the field-list row for it is
within noise.

## 4. Launch (`counter.apk`, release, warm on-disk cache per binary)

`acos launch --json fixtures/build/apps/counter.apk`, 5 interleaved fresh
processes per binary, separate `ACOS_CACHE_DIR`, wall + `ru_maxrss`:

| Metric | before | after | Δ |
|---|---:|---:|---:|
| launch wall p50 | 841.4 ms | 840.3 ms | **−0.1 %** |
| launch wall min | 826.6 ms | 829.2 ms | +0.3 % |
| peak RSS p50 | 489.8 MiB | 489.8 MiB | **0.0 %** |

The launch path is native/`<clinit>`-heavy and does not allocate instances in a
tight loop, so it is neutral — no regression in wall or memory.

## 5. Correctness / acceptance (`2c682c7` + this diff)

| Gate | Result |
|---|---|
| `tools/diff-dexprogs.sh --corelib real` | **27 / 27** |
| `tools/diff-dexprogs.sh --corelib real --gc-stress` | **27 / 27** |
| `ACOS_EXEC=interp tools/diff-dexprogs.sh --corelib real` | **27 / 27** |
| `cargo test -p acos-vm` | green (121 lib incl. snapshot; all integration incl. `render_views`) |
| `cargo test -p acos-runtime` | green except `pms` (see below) |
| `cargo clippy -p acos-vm --lib --tests --no-deps -- -D warnings` | clean |
| `cargo check --workspace --all-targets` | clean |

`render_views` needs the boot cache at `<repo>/.cache/boot/android-34`; the
worktree was pointed at the main repo's copy for the run.
`pms_java_app_info_matches_the_reference_capture` fails with
`app.flags=536870912` vs the committed golden `0`; this was reproduced on a
pristine stash of these changes at `0b4db83` (and on `2c682c7`), i.e. it is
pre-existing and does not touch `heap.rs`/`limits.rs`/`snapshot.rs`.

## 6. Retained levers (not landed)

* Per-frame register windows on one contiguous `Vec<u64>` with frame-base
  offsets (rather than a `Vec<u32>` + `Vec<bool>` per frame) would cut invoke
  setup, but native handlers derive the frame base from `regs.len()` and many
  natives read `Object::Instance.fields`, so it is a cross-crate change (natives
  are owned elsewhere this wave).
* A size-class instance slab (`Object` header + offset into one `Vec<u64>`)
  removes the per-object field `Vec` entirely, but touches ~49 `Object::Instance`
  sites including natives and would change the snapshot object layout (version
  bump). The byte-budgeted free list gets most of the allocation win for now.
* GC sweep could store per-slot sizes to avoid recomputing `object_size` on
  free; the mark/bitmap/worklist machinery is already allocation-free and
  `BenchAlloc` GC is only ~5 % of its run.
