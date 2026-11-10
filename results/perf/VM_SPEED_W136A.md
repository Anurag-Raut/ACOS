# W136-A — typed-IR superinstructions + IR quickening fast path

**Status:** measured, landed in the W136-A worktree of `5a5dc2f`
(`.cache/wt-w136a`, `CARGO_TARGET_DIR=.cache/target-w136a`,
`CARGO_PROFILE_DEV_DEBUG=0`, release, `tools/guarded.sh`). `perf` is not
installed on the host, so the profile was read from the existing facilities:
`vmbench --ops` (DEX opcode histogram), a temporary `ACOS_IR_DUMP` lowering
dump (removed before the final build), and `ACOS_PROFILE` for launch. All code
is `#![forbid(unsafe_code)]` and confined to `acos-vm` `interp` / `ir`
(`exec.rs`) / `frame` / `vm` quickening.

## 1. What the profile said

The four target programs lower to these IR op sequences (temporary dump):

| Program | loop-body IR ops (before) |
|---|---|
| `BenchLoop` | `ConstIf`, `ArithLit`, `ArrayPut`, `ArithLit`, `ArithLit`, `ArrayGet`, `Convert(i2l)`, `Arith2(add-long)`, `ArithLit`, `Goto` |
| `BenchArray` | `ConstIf`, `ArithLit`, `ArrayPut`, `ArrayGet`, `Convert(i2l)`, `Arith2(add-long)`, `ArithLit`, `Goto` |
| `BenchGc` | `ConstIf`, `ArithLit`, `ArithLit`, `NewArray`, `ArrayPut`, `ArrayGet`, `Convert(i2l)`, `Arith2(add-long)`, `ArithLit`, `Goto` |
| `BenchAlloc` | `ConstIf`, `NewInstance`, `Invoke`, `InstancePut`, `InstanceGet`, `Convert(i2l)`, `Arith2(add-long)`, `ArithLit`, `Goto` |

The dominant un-fused idiom on all three array/GC programs is
`aget; int-to-long; add-long/2addr` (`longAcc += arr[i]`) and the back edge
`<arith>/lit; goto` when the loop test is at the head of the body. `BenchAlloc`
is dominated by `new-instance` + constructor `invoke` and is allocation-bound
(ADR-0024 already recorded this: `gc_ms` is ~5 % of it, not dispatch).

The requested pairs that did **not** appear hot in these targets: `iget`+`if`,
`const`+`add`, `invoke`+`move-result` (the constructor returns void, and the
`MoveRes` that follows other calls is already one dispatch after the scheduler).
`aget`+`aput` occur, but are separated by the index arithmetic, so the useful
fusion is the `aget`→`i2l`→`add-long` chain rather than a literal
`aget;aput` pair.

## 2. Landed

1. **`Op::ArrayGetI2L` / `Op::ArrayGetI2LAdd2`** — fuse `aget` (int) +
   `int-to-long` [+ `add/sub/mul-long/2addr`]. The fused op reproduces the
   general `aget` low-word store for *every* element kind, so it is total (no
   fallback) except for null/out-of-bounds, where the general path throws
   exactly as before.
2. **`Op::ArithLitGoto`** — fuse `<arith>/lit` + unconditional `goto` (back
   edge with the loop test at the head). Only fused when the `goto` is not a
   re-entry point and has a valid target (the arith write is not idempotent).
3. **Per-frame IR-quickening flag** (`Frame::ir_quickened`) — a frame that has
   already resolved its field quick slots skips the per-slot `quicken` scan
   when the IR is re-entered after an `invoke` returns. `acquire_frame` clears
   it; `Vm::invalidate_ir_quickening` clears it on every live frame.

Two further candidates were implemented and then **removed** under the
45-minute/≥3 % rule:

* **`iget` + `int-to-long` [+ long add]** — no measurable win on `BenchAlloc`
  (see table), so it was reverted rather than kept for semantics risk.
* **invoke+move-result / const+add / iget+if** — no hot occurrence in the four
  target programs and no plausible launch-wide win from the counters; not
  landed.

## 3. Before / after (5 interleaved runs, `vmbench`, release)

`vmbench <dex> <main> 1`, alternating baseline/optimised order, same host
window, `tools/guarded.sh`; p50 and min of 5 samples. Baseline is a pristine
`5a5dc2f` worktree/binary (`.cache/wt-w136a-base`).

| Program | before p50 (ms) | after p50 (ms) | p50 Δ | before min | after min | min Δ | verdict |
|---|---:|---:|---:|---:|---:|---:|---|
| `BenchLoop` | 840.6 | 674.2 | **−19.8 %** | 835.0 | 628.8 | −24.7 % | keep |
| `BenchArray` | 694.3 | 468.4 | **−32.5 %** | 691.4 | 468.2 | −32.3 % | keep |
| `BenchGc` | 449.7 | 407.2 | **−9.4 %** | 405.6 | 385.6 | −4.9 % | keep |
| `BenchAlloc` | 1792.4 | 1770.4 | −1.2 % | 1768.7 | 1737.9 | −1.7 % | neutral¹ |

¹ `BenchAlloc` is host-allocation/invoke-bound, not dispatch-bound; the fused
array ops do not apply and the `iget` fusion that targeted it was net-neutral.
No regression is claimed or observed beyond host noise. It is unchanged, not
improved.

The second interleaved run (best-of-3 per call, min) gave −24.1 % / −32.5 % /
−4.3 % / −2.4 % for Loop/Array/Gc/Alloc, consistent with the table above.

### Launch (`counter.apk`, release, warm on-disk boot cache)

`acos launch --json fixtures/build/apps/counter.apk`, separate warm
`ACOS_CACHE_DIR` per binary, 5 interleaved fresh processes:

| Metric | before | after | Δ |
|---|---:|---:|---:|
| launch wall p50 | 921.8 ms | **888.5 ms** | **−3.6 %** |
| launch wall min | 914.0 ms | **862.1 ms** | −5.7 % |
| raw p50 samples (after) | — | 890.4, 1076.0, 888.5, 883.4, 862.1 | one host outlier |

The launch path is `<clinit>`/native-heavy (W109-A/W111-A); the IR only covers
loop bodies, so a ~1–2 % bytecode-side saving shows up as the ~3.6 % wall
result. No new launch-only code was added.

## 4. Correctness / acceptance

| Gate | Result |
|---|---|
| `tools/diff-dexprogs.sh --corelib real` | **27 / 27** |
| `tools/diff-dexprogs.sh --corelib real --gc-stress` | **27 / 27** |
| `ACOS_EXEC=interp tools/diff-dexprogs.sh --corelib real` | **27 / 27** |
| `cargo test -p acos-vm` (incl. `ir_equivalence`, 121 lib tests, snapshot 16/16) | green |
| `cargo clippy -p acos-vm --lib --tests --no-deps -- -D warnings` | clean |
| `cargo check --workspace --all-targets` | clean |

No `unsafe`; no edits outside `acos-vm/src/{exec,frame,interp,vm}.rs`.

## 5. Next levers (not landed)

* `BenchAlloc` remains allocation/invoke-bound: preallocated register windows
  and a size-class instance arena are the next real lever (ADR-0023/0024).
* `BenchGc` still pays one host `Vec` per `new int[n]`; a small-array pool is
  the analogue of `field_pool` and is the obvious next step.
* A `const`+`arith` fusion is worth revisiting only once a target program
  actually exhibits it hot (none of the four do).
