# T-42 — core snapshot/restore (proof of concept): results

Run on 2026-11-10, release profile, isolated git worktree at commit `0cdfacd`
(HEAD) + this task's changes. Commands are given verbatim in §7.

## 1. Verdict

`Vm::snapshot()` / `Vm::restore()` are implemented in
`crates/acos-vm/src/snapshot.rs`. A VM snapshotted after core boot restores into
a fresh `Vm` that runs the 23 `dexprogs` with **byte-identical stdout and exit
code versus a fresh VM**, normal and GC-stress, on both the bootstrap and the
real Android-14 boot classpaths. The invariants (determinism, idempotence,
fingerprint refusal, live-thread / open-fd / pending-exception refusal) are
individual tests.

This is the measured proof of concept behind ADR-0010 gate 2 ("correct restore of
an initialised state"). It is **not** a framework snapshot (T-43): see §6.

## 2. What the format contains

Versioned, little-endian, self-describing byte stream (`ACOSSNAP`, format
version 1):

| section | contents |
|---|---|
| header | format version, corelib, heap limit, **boot-DEX fingerprint** (Adler-32 + SHA-1 + size of every loaded provider, in order), **property hash** (`ACOS_*` env + corelib + locale + boot dir, sorted), locale, clock anchor, boot dir, class/object counts |
| classes | every loaded class by **descriptor** + `InitState` + static-field slots + `Class` mirror handle. Method bodies, vtables, field layouts and native bindings are **not** copied; they are re-linked from the same boot DEX on restore |
| heap | the whole slot table, **slot-preserving** (handle index == slot index), so object identity and `identityHashCode` survive. Encodes `Instance`/`Array`/`String`/`Class`/`Builtin` payloads |
| roots | interned strings (ordered), native roots, integer cache, string-hash cache, throwable stacks, fatal-error and `ThreadGroup` handles, output/exit state, heap & metric counters, quiescent thread table |

Maps are serialised in sorted key order and the intern table is rebuilt from the
ordered interned list, so **HashMap iteration order never enters the bytes**.

## 3. Acceptance (1): 23 dexprogs through restore

`cargo test --release -p acos-vm --test snapshot` — **16/16 pass** (1.95 s):

* `snapshot_restore_bootstrap_dexprogs_normal` — 20/20 (regex excluded: bootstrap
  has no `java.util.regex`), fresh vs restore, identical stdout+exit.
* `snapshot_restore_bootstrap_dexprogs_gc_stress` — 19/19 (19_threads_sync
  excluded for runtime, matching `tests/dexprogs.rs`), a GC before every
  allocation.
* `snapshot_restore_real_all_dexprogs` — **23/23**, real boot classpath, fresh vs
  restore.
* `snapshot_restore_real_dexprogs_gc_stress` — 22/22 GC-stress, real.
* `snapshot_restore_real_regex` — 3/3.
* `snapshot_restore_100_cycles_is_stable` — 100 restores of one snapshot, each
  running `01_hello`.

Independently, the repo's own differential script run unmodified in the isolated
tree — `tools/diff-dexprogs.sh --corelib real` — reports **23/23 PASS** against
the real-ART expectations (`results/snapshot/diff-dexprogs-real.txt`; the script
then errors on a 24th fixture `24_zip_roundtrip` added concurrently by another
agent with no expected-art file — not a T-42 failure).

## 4. Acceptance (2): measured numbers (release, 20 restores, p50/p95)

`crates/acos-vm/examples/snapshot_probe.rs`, output saved verbatim in
`results/snapshot/probe-{bootstrap,real}.json`.

| metric | bootstrap | real (Android 14 boot) |
|---|---:|---:|
| snapshot size | **3,328 B** | **6,633 B** |
| `Vm::new` (fresh) | 461 µs | 14.71 ms |
| fresh first run (`01_hello`) | 55.9 µs | 6.54 ms |
| **restore p50** | **222 µs** | **13.34 ms** |
| restore p95 | 249 µs | 13.82 ms |
| restore copy (memcpy lower bound) p50 | 180 ns | 581 ns |
| restore + run p50 | 16.7 µs | 6.08 ms |
| RSS per restored VM | ~345 KiB* | **21.6 MiB** |
| identical stdout/exit | 20/20 | 20/20 |

\* bootstrap state is tiny; the RSS delta is allocator noise at this scale.

**Copy-light finding (important).** The crate is `#![forbid(unsafe_code)]`, so a
real `mmap(MAP_PRIVATE)`/CoW arena is not available. The measured `memcpy`
surrogate of the whole stream is **180–581 ns** (3–7 KB) — i.e. copying the arena
is free at this size, so CoW buys nothing for the core boot state. The dominant
restore cost is **re-parsing the boot DEX** (≈13 ms of the 13.34 ms real
restore), and each restored VM owns its own parsed boot DEX, which is why per-VM
RSS is ~21.6 MiB. T-43/T-44 must share one parsed `Rc<DexFile>` provider across
restored VMs (or snapshot the parsed provider), not just the arena.

## 5. Acceptance (3): invariants

| invariant | test | result |
|---|---|---|
| determinism: two snapshots of the same state byte-equal | `snapshot_is_deterministic_and_idempotent` | pass |
| idempotence: `snapshot(restore(s)) == s` (and twice) | same | pass |
| refuse bad magic / format version | `snapshot_refuses_bad_magic_and_version` | `SNAPSHOT_FORMAT` |
| refuse boot-DEX fingerprint mismatch | `snapshot_refuses_fingerprint_mismatch` | `SNAPSHOT_MISMATCH` |
| refuse property-hash mismatch | same | `SNAPSHOT_MISMATCH` |
| refuse locale mismatch | header check in `restore` | `SNAPSHOT_MISMATCH` |
| refuse a live extra thread | `snapshot_refuses_extra_live_thread` | `SNAPSHOT_PRECONDITION` |
| refuse an open fd | `snapshot_refuses_open_fd` (`vfs.pipe2`) | `SNAPSHOT_PRECONDITION` |
| refuse pending exception | `snapshot_refuses_pending_exception` | `SNAPSHOT_PRECONDITION` |
| refuse held monitor | `snapshot_refuses_held_monitor` | `SNAPSHOT_PRECONDITION` |
| refuse app DEX (no code in stream) | `snapshot_refuses_app_dex` | `SNAPSHOT_PRECONDITION` |

Preconditions also refuse: >1 green thread, any live Java frame, wait monitor,
wake deadline, joiner, ICU converter native handle, held monitor.

## 6. What still blocks a framework-level snapshot (T-43)

1. **Shared boot-DEX provider.** Restore calls `Vm::new`, which re-reads and
   re-parses the boot DEX. Needed: a process-shared `Rc<DexFile>` set so N
   restored workers share one copy (target: per-worker RSS ≪ N × full RSS).
2. **Application DEX / class-load closure.** The stream stores descriptors only;
   app DEX and lazily loaded boot jars must be re-attached after restore, and the
   preload/`<clinit>` closure (T-43) must be computed and validated against the
   dynamic-trace `first_order` witness.
3. **Re-seed hooks are not implemented.** Restore currently inherits the fresh
   VM's pid/tid, RNG (none yet), clock (`clock_anchor_millis` stored but the VM
   does not yet read a clock), properties, locale, resources, binder identity and
   fd table. `SystemProperties`/`Build` caches, `ThreadLocalRandom`, `SystemClock`
   anchor, `LocaleList`/ICU default, `Resources`/`Configuration` and
   `ActivityThread.currentActivityThread()` all still need the single tested
   re-seed list from REPORT §6.3.
4. **Native handles.** ICU converter handles (`converters`) and any
   `AssetManager`/`ApkAssets` fd-backed object are refused today; they need a
   reopen-or-recreate policy (store the path, not the fd).
5. **Per-process structures.** `Looper`/`MessageQueue`/`Choreographer`/
   `InputEventReceiver`/`Binder` caller stacks must be excluded and rebuilt after
   restore; the precondition (quiescent VM) is what the PoC uses in their place.
6. **GC generation/mark state.** Mark bits are not snapshotted (there is no
   generational collector yet); the free list is rebuilt from empty slots, which
   is adequate for the current non-moving mark-sweep but must be revisited for a
   compacting/generational heap (ADR-0009).
7. **Real CoW.** `#![forbid(unsafe_code)]` blocks `mmap`; if CoW per worker is
   required, it must live behind a small audited module with its own review, or
   move to a fork-based model.

## 7. Exact commands

```bash
export CARGO_TARGET_DIR=/home/Qui/Projects/acos/.cache/target-w19e \
       CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_BUILD_JOBS=6

# unit + snapshot tests (debug; the full-real corpus is skipped in debug)
tools/guarded.sh --mem 4G --time 300 -- cargo test -p acos-vm

# all 16 snapshot tests incl. the full 23-program real corpus
tools/guarded.sh --mem 6G --time 600 -- cargo test --release -p acos-vm --test snapshot
tools/guarded.sh --mem 6G --time 600 -- cargo clippy -p acos-vm --lib --tests --examples -- -D warnings

# measurements (release, >=20 restores)
tools/guarded.sh --mem 6G --time 400 -- cargo run --release -p acos-vm \
  --example snapshot_probe -- --corelib bootstrap \
     --dex fixtures/build/dexprogs/01_hello/classes.dex --iters 20 --rss-vms 8
tools/guarded.sh --mem 6G --time 400 -- cargo run --release -p acos-vm \
  --example snapshot_probe -- --corelib real \
     --dex fixtures/build/dexprogs/01_hello/classes.dex --iters 20 --rss-vms 8

# real differential (unmodified script)
tools/diff-dexprogs.sh --corelib real
```

## 8. Files and hooks

* owned: `crates/acos-vm/src/snapshot.rs`, `crates/acos-vm/tests/snapshot.rs`,
  `crates/acos-vm/examples/snapshot_probe.rs`, `results/snapshot/`.
* minimal listed hooks in shared files:
  * `crates/acos-vm/src/lib.rs` — `pub mod snapshot;`
  * `crates/acos-vm/src/error.rs` — `SnapshotPrecondition`/`SnapshotFormat`/
    `SnapshotMismatch` variants + codes + `Display`.
  * `crates/acos-vm/src/heap.rs` — `free`/`field_pool` made `pub(crate)`.
  * `crates/acos-vm/src/vm.rs` — `gc_threshold` `pub(crate)`; new
    `clock_anchor_millis` + `snapshot_locale` fields; `ThreadRT::new` made `pub`.

## 9. Risks

* The two full-real-corpus tests boot the Android-14 DEX set 46 times and are
  therefore **release-only in debug** (they skip unless `ACOS_SNAPSHOT_FULL_REAL=1`),
  so `cargo test -p acos-vm` stays tractable. The bootstrap tests, the 100-cycle
  test and the real regex subset still run in debug. `cargo test --release -p
  acos-vm --test snapshot` runs all 16.
* The differential / measurement commands were first validated in an isolated
  worktree (`.cache/wt-w19e`, commit `0cdfacd` + only this task's diff) because a
  concurrent agent's `natives/fwk_surface.rs` did not compile in the shared tree.
  They were then re-run in the shared tree after it compiled again (snapshot
  tests 16/16 in debug; lib tests 83/83).
* `cargo clippy -p acos-vm --lib --tests --examples -- -D warnings` is clean for
  this task's files (verified in the isolated tree). At hand-off the **shared**
  tree fails the same command on 8 lints in another agent's
  `crates/acos-vm/src/natives/fwk_input.rs` (untouched by T-42).
* `24_zip_roundtrip` appeared in `fixtures/build` during the run (concurrent
  agent) and aborts `diff-dexprogs.sh` after the 23 expected programs; it is not
  a T-42 artifact.
* The bootstrap per-restored-VM RSS number is below allocator resolution and
  should not be quoted as a memory result.
