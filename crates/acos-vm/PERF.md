# ACOS interpreter performance log

Runtime numbers for `crates/acos-vm` (the DEX interpreter). Each experiment is
recorded whether kept or reverted. All timings are **best-of-N wall clock on the
host** (AMD Ryzen 9 9950X), measured with the in-tree driver
`cargo run --release -p acos-vm --example vmbench -- <dex> <main> <repeats>`
and, for the headline table, with `tools/bench-vm.sh` (which also runs
`java` / `java -Xint`). `perf(1)` is not installed on this host, so profiles are
read from the interpreter's own opcode histogram (`vmbench --ops`) plus targeted
timings.

Comparison target is the **host JVM** (OpenJDK 17); these are *jvm-reference*
numbers, **not** Android/ART. See `results/vm/bench-*.json`.

## Baseline (commit `434f60c`, results/vm/bench-2026-11-10-141049.json)

| program | insns | acos ms | M insn/s | java -Xint ms | java JIT ms |
|---|---:|---:|---:|---:|---:|
| BenchFib    | 52,868,680  | 2201 | 24.0 | 338 | 21 |
| BenchLoop   | 550,000,024 | 5908 | 93.1 | 388 | 41 |
| BenchString | 9,000,039   | 608  | 14.8 | 606 | 47 |
| BenchAlloc  | 140,000,022 | 7551 | 18.5 | 1172 | 34 |

Framework-shaped additions (bench driver, best-of-2, same host):

| program | insns | acos baseline ms | shape |
|---|---:|---:|---|
| BenchVirtual     | 240,000,044 | 6613 | interface call site, 3 receiver classes |
| BenchFields      | 57,400,231  | 943  | field-heavy object-graph walk (iget/iget-object/iput-wide) |
| BenchCollections | 14,147,193  | 2188 | ArrayList/HashMap/StringBuilder loop (small map: ACOS corelib backs it with a linear store) |

## Experiment log

### P1 — invoke fast path, dispatch cache, field-sign cache, no per-instruction `MethodImpl::clone` — KEPT

**Hypothesis / profile.** `vmbench --ops` showed invocation-heavy programs
dominated: BenchFib has 7.0 M `invoke-static`, BenchVirtual 20 M
`invoke-interface`, BenchAlloc 30 M invokes, BenchString/Collections ~26 M.
Reading `interp.rs`:

* `invoke()` unconditionally built `class`/`name`/`signature` `String`s
  (`format!("({}){}", …)`) **per call**, even for cached static/direct calls.
* static/direct calls re-ran `load_class` + `find_method` (with
  `HashMap<String,_>` probes) every call instead of using the existing
  `DexCtx::method_cache`.
* `invoke-virtual`/`-interface` re-ran `dispatch()` (more `String` allocation +
  `HashMap<String,_>` probes) every call.
* every instance/static field access called `field_desc()`, which allocated a
  `String` for the descriptor, only to pick a narrow/widen operation.
* `build_frame()` cloned the whole `Method` (name/sig `String`, `params`/`ins_ref`
  `Vec`s) and allocated a fresh `slots` `Vec` per call.
* `step()` cloned `MethodImpl` (an `Rc<MethodCode>` increment/decrement) for
  **every instruction**.

**Change (kept).**
1. `step()` copies the native fn pointer / checks the DEX code flag without
   cloning `MethodImpl`.
2. `invoke()` uses `resolve_method_ref()` (cached in `DexCtx::method_cache`) for
   static/direct/super; builds no strings on the fast path.
3. New global inline cache `Vm::dispatch_cache: HashMap<(dex, method_ref, class), MethodId>`
   for virtual/interface. Keyed by dynamic receiver class, so it is correct for
   polymorphic sites and needs no invalidation (classes are never unloaded).
4. `ResolvedField` now caches `FieldSign`; field accesses no longer build a
   descriptor `String`.
5. `build_frame()` no longer clones `Method` or allocates a `Vec`; it copies
   argument words straight into the new frame.

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

Correctness: `cargo test -p acos-vm -p acos-cli` green,
`tools/diff-dexprogs.sh` and `--gc-stress` both 20/20.

**Why BenchLoop barely moved.** It is 550 M near-pure dataflow opcodes
(`and-int/lit16`, `const`, `if-ge`, `goto`, `aget`, `aput`, `int-to-long`,
`add-long/2addr`, `add-int/lit8`, `mul-int/lit8`) with only 11 invokes, so it is
dominated by per-instruction dispatch/register-access overhead, not call
overhead. That is P2's target.
