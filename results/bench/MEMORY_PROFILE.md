# Memory profile of a booted ACOS session (and the shared-boot fix)

Run: `run-2026-11-10-201637.json` (commit `ced0d70` + this task's changes),
`release`, N=1..20 (`sessions-shared`), 5 fresh processes per N, host 32 threads
/ 60 GiB, kernel 7.2.11-arch1-1, rustc 1.90.0. Fixture
`fixtures/build/apps/hello-views.apk`, corelib `real` (Android 14 boot
classpath). Method: BENCHMARKS.md §2; memory from
`/proc/self/smaps_rollup` + `/proc/self/status`; structural bytes are exact
`Vec::capacity()` / `String::capacity()` / `DexFile` slice lengths, with the
`_approx` rows carrying a one-byte-per-entry control estimate (never mixed).

## 1. Where the ~155–158 MiB of one booted session goes

A single booted framework app process (attach handshake done), measured with the
new `sessions-shared` footprint walk (`acos bench sessions-shared`, N=1):

| Category | MiB | What it is | Sharing today |
|---|---:|---|---|
| boot-classpath raw DEX bytes | **50.169** | the 12 `classes*.dex` files loaded during boot (core-oj/core-libart/core-icu4j + 9 lazy jars touched by the framework), held as `Arc<[u8]>` | **shared**, one copy/process |
| parsed `DexFile` tables | **25.347** | id tables, lazily-materialised string table, compact class index | **shared** |
| decoded method bodies | **9.237** | 6 404 `MethodCode` bodies (`insns`, `raw`, `pc_to_idx`, tries, line tables) | **shared** |
| per-VM DEX caches | **9.581** | `string_cache`/`field_cache`/`method_cache`/`class_cache` (handles + ClassIds are per-VM) | **per session** |
| class table | 1.387 | `Class` descriptors, field lists, vtables, maps, statics slots | per session |
| method table | 1.439 | `Method` names/signatures/params | per session |
| type table | 0.041 | `TypeInfo` descriptors | per session |
| interner | 0.163 | `string -> Handle` map + `interned` vector | per session |
| guest heap objects | 0.499 | `Vm::heap.live_bytes` (class mirrors, static refs, strings) | per session |
| thread stacks | 0.006 | parked threads, frame registers | per session |
| lookup maps / natives+misc (approx) | 0.124 | `class_by_desc`, `type_by_desc`, natives, monitors, caches | per session |
| **structural total** | **98.041** | | |
| process `anon+shmem` (p50, N=1) | **157.602** | `RssAnon + RssShmem` | |
| process PSS / private / shared | 162.83 / 161.73 / 4.97 | PSS and anon+shmem are never summed | |

The **59.6 MiB gap** between the structural sum (98.0 MiB) and the process
`anon+shmem` (157.6 MiB) is *not* attributed by this walk. It is the real
allocator overhead of thousands of `HashMap`/`HashSet` buckets (the structural
row only charges 1 B/entry), allocator size classes/retained arena pages, the
`ServiceHost`/ATMS/Binder service state that is not part of `Vm`, the APK/VFS
buffers, Rust std runtime state, and the process's own file-backed/private
pages. It is reported as a residual, never guessed into a category.

Before this task the raw DEX, parsed tables and decoded bodies were **all
per-VM**: every session read and parsed its own copy with `std::fs::read`, so
the N=1 profile above was also the *marginal* cost of every extra session.

## 2. What sharing changes: flat shared bytes, ~14 MiB marginal

`sessions-shared` boots N independent sessions (separate heaps) in one process
and boots a fresh process per N. The shared categories are held **once**:

| N | anon+shmem total (MiB) | incremental / session | marginal / session | raw DEX (MiB) | parsed DEX (MiB) | decoded code (MiB) |
|---:|---:|---:|---:|---:|---:|---:|
| 1 | 157.60 | 157.60 | — | 50.17 | 25.35 | 9.24 |
| 2 | 171.75 | 86.10 | 14.59 | 50.17 | 25.35 | 9.24 |
| 5 | 214.94 | 43.03 | 14.31 | 50.17 | 25.35 | 9.24 |
| 10 | 286.73 | 28.68 | 14.32 | 50.17 | 25.35 | 9.24 |
| 20 | 429.99 | 21.50 | 14.33 | 50.25 | 25.43 | 9.24 |

At N=20 the structural walk holds **350.7 MiB** for 20 sessions
(17.5 MiB/session) versus an **unshared equivalent of 1960.8 MiB**
(98.0 MiB/session): **1610 MiB saved**, ~80.5 MiB/session, while `shared` pages
stay flat at ~5 MiB (the binary + libs — the sharing is `Arc`, not `mmap`).

The remaining ~14.3 MiB marginal is dominated by the **per-VM DEX caches**
(9.58 MiB/session: `string_cache`/`field_cache`/`method_cache`/`class_cache`
store per-VM handles/ClassIds), then class+method+type tables (~2.9 MiB) and the
guest heap/interner (~0.7 MiB). Those are genuinely per-session and are the next
lever (e.g. interned-index tables instead of handle tables); they are **not**
shared today.

## 3. Before / after (same harness, `sessions` metric, fresh process per N)

Before: `run-2026-11-10-193539.json`, commit `ced0d70` (unmodified boot path).
After: `run-2026-11-10-201637.json`, same commit + this task.

| Metric | Before | After | Verdict |
|---|---:|---:|---|
| N=1 `anon+shmem` | 155.06 MiB | 157.60 MiB | +1.6 % (adds the once-per-process class index; within the run-to-run spread, no speed cost) |
| N=2 incremental | 154.28 MiB/session | 86.10 MiB/session | **−44.2 %** |
| N=5 incremental | 153.82 MiB/session | 43.03 MiB/session | **−72.0 %** |
| N=10 incremental | 153.67 MiB/session | 28.68 MiB/session | **−81.3 %** |
| N=5→10 marginal | 153.52 MiB/session | 14.31 MiB/session | **−90.7 %** |
| N=10→20 marginal | not measured | 14.33 MiB/session | new |
| `Vm::new` cold (first Vm, empty cache) | 16.415 ms | 15.60–16.12 ms | unchanged / ≤ |
| `Vm::new` warm (later Vm in same process) | 16.4 ms (re-parse) | **1.31 ms** | shared parsed DEX |
| cold `boot_app_process` (first Vm) | 95.1 ms | 82.9–84.8 ms | ≤ |
| warm `boot_app_process` (later Vm) | 95.1 ms | **12.6–13.1 ms** | shared classpath + code |
| process-start (fresh process) | 20.841 ms | 16.782 ms | better |
| interpreter `02_int_arith` wall | 22.823 ms | 7.914 ms | see §6 (warm cache) |

**Legacy comparison now:** M2 at N=5 is **43.03 MiB/session vs 78.7 MiB (v9) =
−45.3 % (better)**; marginal N=5→10 is **14.31 vs ~99.3 MiB = −85.6 %
(better)**. Per-session N=1 is 157.6 vs 130.3 MiB (**+21 % worse**): the first
session still pays the whole boot classpath. The legacy goal ("marginal
per-session memory far below legacy at N=5/10/20") is met at N≥2; N=20 total is
430 MiB for 20 sessions (21.5 MiB/session).

## 4. What is shared, and why it is safe

* `acos_dex::cache::BootCache` (new): a process-global
  `Mutex<HashMap<fingerprint, Arc<DexFile>>>`, keyed by
  `(canonical path, size, mtime)` so a hit costs one `stat` and never re-reads
  the DEX. It shares the immutable `Arc<[u8]>` bytes, the parsed id tables, the
  lazily-materialised string table and the lazily-built compact class index.
* `acos_vm::codecache` (new): `Mutex<HashMap<(DexFile::id, code_off),
  Arc<MethodCode>>>`. A decoded body is a pure function of the DEX bytes and
  `code_off`; `DexFile::id` is process-unique and shared with the parsed file.
* Nothing mutable crosses a `Vm`: handles, `ClassId`s, statics, monitors,
  interner maps, stacks and heaps stay per-VM. `DexCtx`'s caches stay per-VM
  because they hold per-VM `Handle`s/`ClassId`s.
* The crates stay `#![forbid(unsafe_code)]` (no `mmap`, no raw pointers);
  `DexFile` is `Send + Sync` (`Arc<[u8]>` + `OnceLock`), and both caches use
  `std::sync` primitives. Parsing/decoding happens outside the lock, so a race
  discards the duplicate and shares the winner.
* Lazy: unused classes still cost nothing. Only files a session asks for are
  parsed; the class index and each method body are built on first use. A lazy
  boot jar is still parsed on the first class miss that needs it.

Correctness evidence: `cargo test --release -p acos-dex` (13+1 pass) and
`-p acos-vm` (83 unit + all integration suites, including 16 snapshot tests)
pass; `tools/diff-dexprogs.sh --corelib real` and `--gc-stress` each report
**23/23** on the 23 target programs (24/25 are another agent's pending
fixtures; normal and GC-stress both fail them identically, unrelated to this
change). `ACOS_GC_STRESS=1` and `--corelib real` are covered by those runs.

## 5. Isolation trade-off (explicit)

**Processes remain the isolation unit.** The in-process sharing above is safe
for *trusted, agent-owned* sessions because the shared data is immutable DEX
text and decoded code; it does not share heaps, statics, handles or capabilities.
It is **not** a claim that one process can host N untrusted tenants:

* `Runtime::with_host` installs one process-global Binder transport, so N live
  runtimes are currently a memory experiment, not N working isolated sessions
  (unchanged by this task; `sessions`' `limit` field still says so).
* A bug or escape in one session can read the shared DEX/decoded code (not a
  secret) but must not reach another session's heap — that is enforced by the
  separate `Vm` heaps, not by the cache.

Recommended design for untrusted N: a **zygote-like template process**. One
warm parent parses the boot classpath (the shared `Arc<DexFile>` set) and then
creates isolated child `Vm`s/processes from that template — fork/CoW where the
host permits it, or a re-exec into a pre-warmed cache where it does not. Keep
one transport/capability namespace per child and never share mutable statics.
That gets the same flat shared-page cost while restoring process-level
isolation. In-process `sessions-shared` remains the measurement/telemetry mode
for trusted agent workloads.

## 6. Cold vs warm labels (honesty note)

The `boot-attach` and `interpreter` harness metrics run several samples inside
one process. Before sharing, every sample re-read and re-parsed the boot
classpath, so they were cold. After sharing, only the **first** `Vm` is cold;
later samples reuse the process-global cache (1.3 ms `Vm::new`, 12.6 ms boot).
The run document now records `first_vm_in_process` (cold: 15.6 ms `Vm::new`,
84.8 ms boot) next to the warm p50, and the `cold_or_warm` labels say so. The
`process-start` metric still spawns a fresh process per sample and is the
authoritative cold number. The warm interpreter wall time is therefore not an
interpreter-loop speed claim; it is the shared-boot effect.

## 7. Reproduce

```sh
export CARGO_TARGET_DIR=$PWD/.cache/target-w21b CARGO_PROFILE_DEV_DEBUG=0 \
       CARGO_PROFILE_TEST_DEBUG=0 CARGO_BUILD_JOBS=6
tools/guarded.sh --mem 6G --time 900 -- \
  env ACOS_BENCH_RUNS=20 tools/bench-acos.sh sessions sessions-shared
# structural profile of one session:
tools/guarded.sh --mem 6G --time 300 -- \
  .cache/target-w21b/release/acos bench sessions-shared --child sessions-shared \
  --n 1 --corelib real --apk fixtures/build/apps/hello-views.apk --settle-ms 1000
```
