# V1 gate — HEAD `f0c9fdb` (2026-11-11)

Verification only: no source edits, no commits, no stash. Evidence lives in
`.cache/v1gate-f0c9fdb/` (gitignored; survives the worktree removal).

## Setup

- Clean detached worktree of `f0c9fdb` at `.cache/wt-v1gate` (removed after the run).
  The main checkout's 41 uncommitted entries are **not** part of this gate.
- Release build: `cargo build --release -p acos-cli -p acos-mcp --bins` plus
  `--example vmbench` and `--example bench_render`, `CARGO_TARGET_DIR=.cache/target-v1gate`,
  `CARGO_PROFILE_DEV_DEBUG=0`, `CARGO_BUILD_JOBS=2`, under `tools/guarded.sh`. Builds clean (~1.5 min).
- `fixtures/build` and `.cache/boot/android-34` symlinked from the main checkout.
  `ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts`; `ACOS_BOOT_DIR` unset.
- `tools/build-negative-apps.sh` wrote the three broken APKs through the `fixtures/build` symlink,
  so they sit in the main checkout's gitignored `fixtures/build/apps/`.

## 1. Checklist (docs/V1.md)

| # | Item | Result | Evidence (`.cache/v1gate-f0c9fdb/`) |
|---|---|---|---|
| 1.1 | hello-views real Activity, acos-native | **PASS** 13/13, 4.6 s | `hv-core/verdict.json` |
| 1.2 | Lifecycle order | **PASS** (launch order in v1-hello-views; recreate order 17/17) | `recreate/verdict.json` |
| 1.3 | Real frame, stable hash | **PASS** static counter frames 10 vs 11 `IDENTICAL` (SSIM 1.0); a tap changes 342 px; PNGs ~92 KB. Non-black by content, no explicit luminance test | `counter/art/frames/`, `frame-diff` output |
| 1.4 | UI tree text/ids/bounds | **FAIL** text/ids pass; title `[42,317][232,368]` vs ART `[42,317][231,368]` (1 px) | `bounds/hv-art.json` |
| 1.5 | Tap + type + back | **PASS** | `hv-core`, `recreate` |
| 1.6 | Text assertions | **PASS** `Hello, World! clicks: 1` / `clicks: 2` | `hv-core`, `recreate` |
| 1.7 | Save/restore on recreate | **PASS** 17/17, `clicks: 2` survives rotate | `recreate/verdict.json` |
| 1.8 | Reset + destroy | **PASS** reset 11/11 (doc says 12/12; file has 11 steps). Destroy: `count 1 -> 0`, `destroyed: true` | `reset/verdict.json`, `destroy-*.json` |
| 1.9 | ART comparison (`--reference`) | **FAIL** documented path `fixtures/expected-art/hello-views` does not exist. Real path with `--reference` returns `UNSUPPORTED_BACKEND` (acvm-reference cannot tap). Scenario bounds vs ART miss by 1 px | `bounds/hv-ref.json`, `bounds/hv-art.json` |
| 2.1 | hello-jni `.so` loaded | **PASS** 13/13 `--native` | `jni/verdict.json` |
| 2.2 | JNI both directions | **PASS** (`add`, `sumArray`, `javaCallbackResult`, `voidCallback` asserted) | `jni/verdict.json` |
| 2.3 | RegisterNatives | **PASS** `registeredEcho=echo!` | `jni/verdict.json` |
| 2.4 | ThrowNew | **PASS** `throwFromJNI caught=thrown from native` | `jni/verdict.json` |
| 2.5 | Displayed results vs ART | **PASS** scenario asserts ART bounds `[0,275][1080,2337]` and all 7 strings. The `--reference` form is unsupported (see 1.9) | `jni/verdict.json` |
| 3.1 | dexprogs, real corelib, normal | **PASS** 27/27 (corpus has 27 programs, not 20) | `dexprogs/dexprogs-real-normal.json` |
| 3.2 | dexprogs, GC-stress | **PASS** 27/27 | `dexprogs/dexprogs-real-gc-stress.json` |
| 4.1 | Wrong-text negative | **PASS** `fail` / `ASSERTION_FAILED` | `negative_verify` test output |
| 4.2 | Crash negative | **PASS** `crash` / `APP_CRASHED` | same |
| 4.3 | Missing-symbol negative | **PASS** `unsupported` / `UNSUPPORTED_NATIVE_SYMBOL` | same |
| 4.4 | Zero false passes | **PASS** 0 of 3 broken APKs pass (test asserts it) | same |
| 5.1 | N=1,2,5,10,20 concurrent workers | **PARTIAL** the literal `acos verify --concurrency N` flag does not exist. Substitute: N parallel `acos verify` processes, all pass at every N, no `clicks=2` leakage. Inputs are identical, so state/input independence is **not proven** | `conc/N*/`, `conc/run-n.sh` |
| 5.2 | Per-worker memory | **PASS** PSS p50 534.0 / 529.2 / 528.0 MiB at N=5/10/20 (RSS 734.6 / 732.8 / 732.8). Incremental anon+shmem N=1 133.9, N=2 74.9, N=5 39.4, N=10 27.6 MiB | `bench-v1-20r.json` `concurrent_sessions`, `sessions_incremental_mib` |
| 6.1 | M1–M11, ≥20 reps, p50/p95 | **PASS** run at `--rounds 20` (13 min) | `bench-v1-20r.json`, `bench-v1-20r.log` |
| 6.2 | Worse-than-legacy list with plans | **PASS** (list below) | §2 |
| 6.3 | Composite verified tasks/GiB-min | **PARTIAL** 958.3 derived from **one** verified task, not the T01–T12 suite. False passes 0 | `tasks_per_gib_minute` |

## 2. Metrics M1–M11 (ACOS vs legacy ACVM)

Times are p50 at n=20 unless noted. Legacy values are from BENCHMARKS.md §5 and §3.

| # | Metric | ACOS | Legacy | Ratio | Verdict |
|---|---|---:|---:|---:|---|
| M1 | Fixed idle core memory (anon+shmem) | 133.9 MiB | 206.0 MiB | 0.650 | **PASS** |
| M2 | Incremental memory/session, N=5 | 39.41 MiB | 78.7 MiB | 0.501 | **PASS** |
| M3 | Cold start → first correct frame | 1.281 s (default warm path, 20/20 reach `first_frame`) | 3.620 s | 0.354 | **PASS** |
| M4 | Worker create → correct frame | 1.142 s (20/20 fresh frames) | 1.297 s | 0.880 | **PASS** (narrow) |
| M5 | Fresh screenshot | 24.14 ms | 66.5 ms | 0.363 | **PASS** |
| M6 | Reset-to-ready | 1.141 s (20/20 fresh frames) | 0.748 s | 1.525 | **FAIL** |
| M7 | UI tree with text | 1.5 ms (warm kept session) | 538 ms one-shot (persistent 4.1 ms) | 0.003 | **PASS** |
| M8 | Action-to-state latency | 66.0 ms, `state_changed=true` | not published | — | **PASS** (capability) |
| M9 | Concurrent workloads | N=20 ok, PSS 528 MiB/session; input independence not proven | N=5 scorecard | — | **FAIL** |
| M10 | End-to-end verify wall | 1.627 s, one task, state change + frame-hash change | ~19 s | 0.086 | **PASS** |
| M11 | Verified tasks / GiB-min | 958.3 (n=1) | not published | — | **PASS** by script rule; not the suite |

**Worse than legacy, with plans**
- **M6 reset (1.141 s vs 0.748 s):** the reset path drops the session and rebuilds it from the boot cache.
  It was 714.7 ms at W176-A (`fd1a887`). The regression is not investigated here.
- **M9 concurrency:** 528 MiB PSS per session at N=20. Independence of input and state is unproven, since the
  verify workers use identical inputs. Plan: per-session transports plus differing inputs per worker.
- **L-rows (launch snapshot):** the `bench launch` row failed once in the 20-round run with
  `Vm::new: host RSS limit exceeded: 2067 MiB > 2048 MiB` (intermittent). Three standalone
  5-sample reruns passed: before-snapshot p50 871–877 ms, restore after p50 36–39 ms. The L-rows in the
  table are blank for this run only.

## 3. Verdict

V1 is **not done**. The core loop passes on both fixtures with evidence at `f0c9fdb`: hello-views 13/13, the
counter 13/13, recreate 17/17, reset 11/11, hello-jni 13/13 with `--native`. Dexprogs pass 27/27 in both modes, the
negative suite gives zero false passes, and M1–M5, M7, M8 and M10 pass. Exact remaining failures:
(1) the hello-views title is 1 px narrow (`[232,368]` vs ART `[231,368]`), which keeps 1.4 and 1.9 open. It is a
text-metric issue, not a layout issue. (2) The documented `--reference` ART comparison is unusable: the path is stale and
the backend cannot tap (1.9). (3) There is no `--concurrency` flag, and the 5.1 independence claim is unproven (5.1).
(4) M6 is 1.5× legacy and M9 fails (6.2). (5) M11 is one task, not the suite (6.3). (6) The launch RSS guard
trips intermittently at 2067 MiB against 2048 MiB.

## 4. Doc and command discrepancies

- `acos verify --concurrency N` and `acos verify --suite dexprogs` do not exist. Substitutes were used as stated above.
- `fixtures/expected-art/hello-views` does not exist; the ART reference is `fixtures/reference/hello-views/art-api34-x86_64`.
- dexprogs corpus is 27 programs, not 20. The reset scenario has 11 steps, not 12.
- `docs/V1.md` lists 2.5 as "in progress", but its own scenario evidence passes; it is marked PASS here.
