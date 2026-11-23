# ACOS interpreter performance log

Runtime numbers for `crates/acos-vm` (the DEX interpreter). Each experiment is
recorded whether kept or reverted. All timings are **best-of-N wall clock on the
host** (AMD Ryzen 9 9950X, Rust 1.90, `--release`), measured with:

* the in-tree driver `cargo run --release -p acos-vm --example vmbench -- <dex> <main> <repeats>`
  (in-process, best-of-N, optional `--ops` opcode histogram), and
* the headline table with `tools/bench/bench-vm.sh` (process start→exit, p50 of N
  runs, also runs `java` / `java -Xint`).

`perf(1)` is **not installed** on this host, so profiles were read from the
interpreter's own opcode histogram (`vmbench --ops`), targeted microbenchmarks,
and GNU `gdb` stack sampling. Comparison target is the **host JVM** (OpenJDK
17); these are *jvm-reference* numbers, **not** Android/ART.

Benchmark sources live in `crates/acos-vm/benches/java/`; the framework-shaped
programs are compiled and measured by `tools/bench/bench-vm.sh`.

## Baseline (commit `434f60c`, `results/vm/bench-2026-11-10-141049.json`)

| program | insns | acos ms | M insn/s | java -Xint ms | java JIT ms |
|---|---:|---:|---:|---:|---:|
| BenchFib    | 52,868,680  | 2201 | 24.0 | 338 | 21 |
| BenchLoop   | 550,000,024 | 5908 | 93.1 | 388 | 41 |
| BenchString | 9,000,039   | 608  | 14.8 | 606 | 47 |
| BenchAlloc  | 140,000,022 | 7551 | 18.5 | 1172 | 34 |

Framework-shaped additions (in-process `vmbench`, best-of-2, same host):

| program | insns | acos baseline ms | shape |
|---|---:|---:|---|
| BenchVirtual     | 240,000,044 | 6613 | interface call site, 3 receiver classes |
| BenchFields      | 57,400,231  | 943  | field-heavy object-graph walk (iget/iget-object/iput-wide) |
| BenchCollections | 14,147,193  | 2188 | ArrayList/HashMap/StringBuilder loop (small map: ACOS corelib backs it with a linear store) |

## Experiment log

### P1 — invoke fast path, dispatch cache, field-sign cache, no per-instruction `MethodImpl::clone` — KEPT

**Profile.** `vmbench --ops` showed invocation-heavy programs dominated:
BenchFib 7.0 M `invoke-static`, BenchVirtual 20 M `invoke-interface`,
BenchAlloc 30 M invokes, BenchString/Collections ~26 M. Reading `interp.rs`:

* `invoke()` unconditionally built `class`/`name`/`signature` `String`s
  (`format!("({}){}", …)`) **per call**, even for cached static/direct calls;
* static/direct calls re-ran `load_class` + `find_method` (`HashMap<String,_>`
  probes) every call instead of using `DexCtx::method_cache`;
* `invoke-virtual`/`-interface` re-ran `dispatch()` (more `String` allocation)
  every call;
* every field access called `field_desc()`, which allocated a `String` only to
  choose a narrow/widen operation;
* `build_frame()` cloned the whole `Method` and allocated a fresh `slots` `Vec`
  per call;
* `step()` cloned `MethodImpl` (an `Rc<MethodCode>` refcount pair) for **every
  instruction**.

**Change (kept).**
1. `step()` copies the native fn pointer / checks the DEX code flag without
   cloning `MethodImpl`.
2. `invoke()` uses `resolve_method_ref()` (cached) for static/direct/super and
   builds no strings on the fast path.
3. New global inline cache `Vm::dispatch_cache: HashMap<(dex, method_ref, class), MethodId>`
   for virtual/interface, keyed by dynamic receiver class (correct for
   polymorphic sites, no invalidation needed — classes are never unloaded).
4. `ResolvedField` caches `FieldSign`; field accesses no longer build a
   descriptor `String`.
5. `build_frame()` no longer clones `Method` or allocates a `Vec`.

**Before/after (vmbench, best-of-2).**

| program | before ms | after ms | speedup |
|---|---:|---:|---:|
| BenchFib | 2134 | 778 | **2.74x** |
| BenchString | 608 | 240 | **2.53x** |
| BenchAlloc | 7425 | 3379 | **2.20x** |
| BenchVirtual | 6613 | 3476 | **1.90x** |
| BenchFields | 943 | 664 | **1.42x** |
| BenchCollections | 2188 | 1973 | 1.11x |
| BenchLoop | 6419 | 6169 | 1.04x |

### P2 — tight fast-dispatch loop, inline register accessors, frame pool — KEPT

**Profile.** BenchLoop (550 M near-pure dataflow opcodes, 11 invokes) barely
moved in P1, so the cost was per-instruction dispatch/register access. Opcode
histograms showed only a handful of opcode families in the hot loops, and
`gdb` disassembly of the general path showed a **separate `call` to
`Frame::set_reg`/`reg_wide` for every register access** (`#[inline]` was not
enough for rustc).

**Change (kept).**
1. New `src/dispatch.rs`: `fast_run()` runs whole runs of frame-local opcodes
   (moves, consts, branches, compares, conversions, arithmetic, primitive array
   get/put, `move-result`, `array-length`) on `&mut Frame` + `&mut Heap`, with
   the instruction stream and pc/index kept as locals. Anything that can
   allocate, resolve, dispatch, enter a monitor or throw falls back to the
   general path. No duplicated semantics: the arithmetic edge cases call the
   same helpers, and the full corpus is the gate.
2. `Frame::reg`/`set_reg`/`set_reg_ref`/`set_reg_wide`/`reg_wide` are
   `#[inline(always)]` (removed a `call` per register access).
3. Guarded reference-bitmap writes in `set_reg*` (skip the store when the bit is
   already clear/set).
4. Per-thread **frame buffer pool** (`ThreadRT::frame_pool`): `regs`/`refs`
   `Vec`s are recycled instead of allocated twice per call. A shared
   thread-local empty `MethodCode` removes an `Rc` allocation per native call.
5. `run_batch()` runs a run of fast opcodes and then general instructions
   (invoke/return/native/…) before yielding, instead of round-tripping to the
   scheduler per instruction.
6. `invoke` probes `DexCtx::method_cache` and the class-init state inline.

**Before/after (vmbench, pinned, best-of-N; load ~3).**

| program | P1 ms | P2 ms | speedup |
|---|---:|---:|---:|
| BenchLoop | 6169 | ~1500–1700 | **~3.7x** |
| BenchArith | — | ~1700–1980 | dispatch floor |
| BenchArray | — | ~1060–1210 | array family |
| BenchFields | 664 | 397–420 | 1.6x |
| BenchFib | 778 | ~535–570 | 1.4x |
| BenchAlloc | 3379 | ~2150–2260 | 1.5x |
| BenchVirtual | 3476 | ~2030–2110 | 1.7x |

### P3 — class/descriptor caches, GC threshold growth, small-instance field recycling — KEPT

**Profile.** `new-instance` / `const-class` / `check-cast` / `instance-of` built
a type-descriptor `String` and ran a **global hash lookup on every execution**
even though `DexCtx::class_cache` existed and was never read. BenchAlloc was
also dominated by GC (853 collections, `gc_ms ≈ 800` of 3.4 s) and by the
per-`new-instance` builtin-allocator `HashMap` probe.

**Change (kept).**
1. Wire up `DexCtx::class_cache` for `new-instance`, `const-class`, `check-cast`
   and `instance-of` (one Vec probe after the first execution).
2. `new-instance` skips `builtin_instance`'s `HashMap` probe for DEX-defined
   classes (`Class::is_builtin == false`).
3. GC threshold grows to at least `heap_max / 16` (bounded, 16 MiB at the
   default 256 MiB heap) instead of collapsing to 1 MiB after every collection:
   BenchAlloc 853 → ~57 collections, `gc_ms` 800 → ~92, peak heap 16 MiB.
4. Small instance field arrays are recycled through a bounded pool on sweep.

**BenchAlloc (vmbench, pinned):** 3379 → ~2100 ms (**~1.6x**), `gc_ms` 800 → 92.

### E1 — unchecked register access (`get_unchecked`, `unsafe`) — REVERTED, no gain

Measured by locally converting the `Frame` accessors to `unsafe get_unchecked`
(with the crate changed to `deny(unsafe_code)` + module `allow`). Result under
the same load: BenchLoop ~1577 ms, BenchArith ~1838 ms, BenchArray ~1166 ms —
all inside the ±10 % run-to-run noise band of the safe build. The bounds checks
are predicted and not the bottleneck, so **`#![forbid(unsafe_code)]` was kept**.

### E2 — `codegen-units = 1` + thin LTO — REVERTED, no gain

`CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1 CARGO_PROFILE_RELEASE_LTO=thin`:
BenchLoop 1734 ms (vs 1743), BenchFib 542 (vs ~565), BenchFields 462 (vs 421).
No clear win; not committed.

### E3 — skipping the parallel `refs` bitmap entirely — REVERTED, no gain

Temporarily removed all reference-bitmap updates: BenchLoop/Arith/Array were
within noise. `refs` is not the bottleneck; the guarded writes are kept for
correct GC roots.

## Final comparison

`tools/bench/bench-vm.sh`, 10 repetitions, host JVM OpenJDK 17. Baseline is the
`434f60c` JSON above. New JSON: `results/vm/bench-2026-11-10-160048.json`.

| program | acos before ms | acos after ms | speedup | java -Xint ms | vs -Xint | java JIT ms | vs JIT |
|---|---:|---:|---:|---:|---:|---:|---:|
| BenchFib    | 2201 | 528  | **4.17x** | 341  | 1.55x slower | 20  | 26x slower |
| BenchString | 608  | 185  | **3.28x** | 611  | **3.30x faster** | 48  | 3.9x slower |
| BenchLoop   | 5908 | 1696 | **3.48x** | 411  | 4.13x slower | 41  | 42x slower |
| BenchAlloc  | 7551 | 2109 | **3.58x** | 1216 | 1.73x slower | 58  | 37x slower |

(Target was ≥4x Fib — met; ≥4x Alloc and ≥5x Loop — not met. BenchLoop's number
varies 1496–1700 ms with host load, i.e. up to 3.95x.)

Framework-shaped programs (baseline first measured in-process with `vmbench`;
final numbers from `tools/bench/bench-vm.sh`):

| program | baseline ms | final ms | speedup | java -Xint ms |
|---|---:|---:|---:|---:|
| BenchVirtual (interface, 3 receivers) | 6613 | 2107 | 3.14x | 1188 |
| BenchFields (field graph walk)         | 943  | 407  | 2.32x | 77  |
| BenchCollections (List/Map/StringBuilder) | 2188 | 2044 | 1.07x | 83  |

`BenchCollections` is dominated by native core-library calls (26 M
`metrics.invokes` for 14 M DEX opcodes): the ACOS core library backs `HashMap`
with a linear-scan store, so this program measures corelib, not the
interpreter.

## Microbenchmarks

All pinned, `vmbench`, best-of-N. `ns/instruction = best_ms / instructions`.

| microbenchmark | opcode family | ns/insn |
|---|---|---:|
| BenchArith (ints/longs/branches) | moves, const, 2addr arith, lit8, branches | 3.30 |
| BenchArray (int aget/aput)       | aget/aput + loop                | 2.68 |
| BenchLoop (mixed)                | array + arith + branches        | ~2.7–3.1 |

Call overhead (invoke + callee body + return; loop overhead subtracted):

| call shape | ns per call |
|---|---:|
| static `invoke-static`/`invoke-direct` + return | **~50** |
| virtual/interface `invoke-virtual`/`-interface` + return | **~63** |

Allocation / GC / memory:

| metric | value |
|---|---|
| small instance (~4 slots) | one `Vec<u32>` of `instance_slot_count` words + heap slot; field storage recycled, so steady-state cost is a slot-table store + zero fill |
| GC pause, ~100 MiB live heap (`BenchGc`) | **~6 ms** average over 9 collections (`gc_ms=55`) |
| GC trigger | `max(live*2, heap_max/16, 1 MiB)` |
| peak RSS, BenchAlloc (10 M objects) | 88,352 KiB → 26,388 KiB (GC-threshold change) |
| peak heap, BenchAlloc | 16 MiB |

## What is still slow, and why

* **Per-opcode dispatch (~2.7–3.3 ns/insn)** is the floor for BenchLoop. Each
  DEX `Insn` is 40 bytes, the `match` is an indirect jump, and every register
  access is bounds-checked. Eliminating the bounds checks with `unsafe` (E1)
  did **not** help, so the remaining cost is the indirect dispatch and the
  inherent load/branch work, not the checks.
* **Call overhead (~50–63 ns)** is still ~4–20x a native call. It is spread
  across `build_frame` (frame acquire + zero + argument copy + incoming-ref
  map), the monitor/class-init checks, and `pop_frame`/pending-result transfer.
  The frame pool removed the allocations but the zeroing and bookkeeping remain.
* **BenchAlloc vs -Xint** is 1.7x slower: it is 30 M invokes (including every
  `Object.<init>`) plus 10 M `new-instance`; the interpreter's call path is the
  limit, not GC (now 92 ms of 2.1 s).
* **vs JIT** the interpreter is 4–42x slower, as expected for no JIT.

## Proposed next steps (not done here; JIT out of scope)

1. **Compact pre-decoded ops.** Replace the 40-byte `Insn` with an 8–16 byte
   quickened form whose operands are pre-resolved (field offset, method id, vtable
   index, string/class handle), removing `class_cache`/`method_cache`/
   `field_cache` Vec probes and shrinking the instruction stream. ADR-0013 step 1
   explicitly anticipates this.
2. **Register-stack frame reuse without zeroing.** Allocate frames from a
   contiguous per-thread register stack and only zero the callee's local slots
   (the incoming slots are overwritten anyway).
3. **Direct-threaded or superinstruction dispatch** for the top opcode pairs
   (e.g. `aget`+`int-to-long`, `const`+`if`), which the histogram shows repeat.
4. **Rust intrinsics for hot leaf methods proven equal** (ADR-0013 step 3), e.g.
   `StringBuilder.append`, and a hash-backed `HashMap` in the core library
   (would fix `BenchCollections`).
5. **A template/baseline JIT (Cranelift) only if** profiling still shows
   interpretation dispatch dominant after (1)–(3). Not in scope for this task.
