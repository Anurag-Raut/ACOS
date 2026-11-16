# PERF3 — the v0.3.0 M8 form-fast latency regression: root cause, fix, A/B

Branch `perf3` from `rel-030` HEAD `9d339cc7` (the BENCHFIX-measured rel-030 is
`b8c30224`; the extra commits after it are docs, the live-view feature and the
REL3B/REL3E/REL3P merges). Host: AMD Ryzen 9 9950X, load 2–9 during these runs
(other agents' builds; every ART acquisition was `run-exclusive --exclusive`).
Raw bench JSON is kept under `run-v022/` and `run-fix/`, the merged documents are
`v022.json` / `fix.json`, and the gate output is `ab-compare.txt`.

## 1. Symptom

BENCHFIX measured M8 tap -> settled, form (fast) +3.64 ms / +11.1 % on rel-030
(v0.2.2 32.74 ms trimmed, rel-030 36.38 ms), first-slot share 98 % -> 85 %.

## 2. Root cause (measured, not guessed)

The extra time is **not in the tap reply's own shaping or diff**. The bench's tap
reply for both arms has `timing.input.absorb_ms = 0` and no tree; the whole
extra delay is inside `guest_command_ms`, the acos-art round trip.

The M8 form loop is `acos_reset {settle:fast}` -> `acos_tap {settle:fast}` ->
`acos_observe_tree {settle:strict}`, repeated. REL3C made **every tree-bearing
reply** call `shape_tree`, which serializes the full tree as pretty JSON and
writes it to `artifacts/tree/<frame_seq>.json` synchronously (including
`create_dir_all` and `canonicalize`). The form screen does not change between
these iterations, so the reset and the observation rewrite the same two trees,
under a new frame sequence, on every iteration. Those writes dirty the
filesystem immediately before the next fast tap; the tap's own settle then takes
one extra frame or a few extra milliseconds. v0.2.2 inlined the tree and never
wrote a file (`write_full_tree` is new in REL3C).

Evidence, in `diagnosis/`:

| experiment | arms | tap p50 / trimmed | conclusion |
|---|---|---:|---|
| `diag-v022-benchform.json` vs `diag-rel030-benchform.json` | v0.2.2 vs rel-030, exact bench-form loop | 32.20 / — vs 34.61 / — | reproduces the BENCHFIX regression in a 60 s loop; `guest_command_ms` carries it |
| `blocks-write-vs-nowrite.json` | rel-030, same binary, `ACOS_PERF3_WRITE_MODE=full` vs `nowrite` | 33.85 / 35.58 vs 33.70 / 34.12 | the full-tree file write costs ~1.5 ms on the following fast tap |
| `blocks-v022-vs-nowrite.json` | v0.2.2 vs rel-030 with the write disabled | 32.67 / 32.72 vs 32.68 / 32.74 | with the writes gone, rel-030 is at parity (+0.02 ms, +0.06 %) |
| `blocks-v022-vs-fix.json` | v0.2.2 vs this fix | 32.71 / 32.76 vs 32.55 / 32.55 | the fix restores parity (−0.21 ms) |

The candidate commits named in the brief were therefore narrowed by measurement
to REL3C's reply-side artifact write: REL3F's `/proc` RSS read is only on
`session list`, int-022's live view only on attach, REL3P is guest parcel code —
none is on this loop. The toggle experiment (same rel-030 binary, write on/off)
is a direct measurement of that one code path, and the fix below is measured
against v0.2.2.

## 3. Fix

`acos_protocol::reply::write_full_tree` now keeps a process-wide cache of the
full-tree artifacts already written for each artifacts directory:

* byte-identical content is **not written again** — the reply names the existing
  complete artifact (the common reset/observe case, and the reason the dirty
  writes disappeared);
* a cache entry is only used while the file still exists;
* different content for a frame sequence that was already used is written to
  `tree/<seq>-<hash>.json`, never over a file an earlier reply named;
* writes are temp-file + rename, so a reader never sees a partial tree;
* the canonical `tree/` directory is resolved once per artifacts directory.

The reply still names a full tree for every tree-bearing reply; no field was
removed and no test was weakened. `crates/acos-protocol/src/reply.rs` has two
new tests for the cache (reuse, and sequence-collision preservation), alongside
the existing REL3C tests.

Also on the branch (required to build/test rel-030 at `9d339cc7`, which had a
merge defect): pass `args` to the 5-argument `tool_result` at the invalid-argument
call site, and accept `full`/`cursor` on `acos_verify`/`acos_observe` so the
REL3C paging test and the CLI's own paged observe are valid. This mirrors the
merge fixes later integrated as REL3K; it is not on any timing path.

## 4. Proof

Arms: v0.2.2 tag, binary sha256
`50537b60cd2c4bf28e1e1ff63b9ebf4b35aad6a66f5393c9a408a67ddb0da7ad`;
fix, sha256 `cacf5e28071e6565c79970feb231f769b45f6c5673c79cf97f27c98dd51bbced`.
Driver `run-ab.sh`: 3 interleaved reps, 40 fast / 20 strict samples per rep,
`tools/bench/acos_bench.py --bundle latency`, every acquisition through
`run-exclusive --task PERF3 --exclusive` + `guarded.sh --mem 8G --time 540`.

ADR-0028 gate (`ab-compare.txt`, section 2):

| metric | v0.2.2 trimmed | fix trimmed | delta | first-slot | verdict |
|---|---:|---:|---:|---:|---|
| M8 form fast | 33.57 | 33.75 | +0.18 ms (+0.5 %) | 92 % -> 99 % | **within-noise — PASS** |
| M8 counter fast | 39.06 | 39.93 | +0.87 ms (+2.2 %) | 21 % -> 17 % | within-noise |
| M4 launch fast | 165.95 | 149.01 | -16.94 ms (-10.2 %) | 0 % -> 35 % | improved |
| M6 reset fast | 204.91 | 200.25 | -4.66 ms (-2.3 %) | 0 % -> 21 % | within-noise |

The old median-of-per-rep-p50 gate prints M8 counter fast as a regression
because one rep per arm landed a whole 16.7 ms frame later (v0.2.2 rep p50s
32.48/32.86/50.56, fix 49.39/33.06/47.56). That is exactly the lattice flip
ADR-0028 replaces; the pooled trimmed mean is +2.2 % / +0.87 ms, inside the
+5 % / +2 ms gate, and M8 counter was already within-noise in BENCHFIX
(-2.5 %).

Correctness:

* `cargo test --release -p acos-protocol reply::tests` — 7 passed (including the
  two new cache tests).
* `cargo test --release -p acos-mcp --test compact_reply` — 1 passed (compact
  default, paging, full tree file exists at `tree_path` with all 400 nodes).
* `cargo test --release -p acos-cli engine::serve::tests::shaped_tree` — 1 passed
  (compact default, full tree written and read back).

## 5. Files

* `crates/acos-protocol/src/reply.rs` — cache + tests.
* `crates/acos-mcp/src/server.rs`, `crates/acos-mcp/src/descriptors.rs` — rel-030
  build/REL3C paging merge fixes.
* `results/bench/PERF3/run-*/` raw phase JSON; `v022.json`, `fix.json`,
  `ab-compare.json|txt`; `diagnosis/` the toggle/parity experiments and their
  scripts.
