# P3d — the M7 tree: pixel-free, and fast when asked

Worktree `perf-p3b-tree`, base `6f70cf5` (includes P1). Time box 60 min,
started 2026-11-12T15:50Z. P3b.md already exists (the older `render-app`
delivery), so this report is P3d.

## 1. Result

One fresh `acos_observe_tree` on a held `acos serve` ART session
(counter.apk, release, n=30, 2 warm-ups discarded):

| mode | before (P3, `ecf7fa2`) | after p50 | after p95 | min / max | good |
|---|---:|---:|---:|---:|---:|
| **strict** (default, unchanged rule) | **186.42 ms** | **151.31 ms** | **151.70 ms** | 151.04 / 151.94 | 30/30 |
| **fast** (new, opt-in, labelled) | — (tree was always strict) | **0.20 ms** | **0.22 ms** | 0.20 / 0.25 | 30/30 |

The strict saving is the capture P3 identified: **35.11 ms** p50 (buffer clone,
PNG encode/write, md5, and the client PNG read-back). What remains is exactly
the 150 ms quiet window P1 left strict as its evidence rule.

Fast is not "a smaller quiet window": on an idle screen it reuses the newest
probe sample when that sample was parsed at the C1 generation current at request
entry. After an input/frame store it wakes the guest for one immediate probe.
The worst fast tree measured in the 20 post-tap freshness reps was **2.646 ms**
(p50 **2.53 ms**), because those are the requests that actually have to ask the
guest.

## 2. Changes

Frame identity without pixels:

* `crates/acos-services/src/surface_client.rs:235` adds `FrameMetadata` and
  `:933` `latest_frame_metadata()`: a metadata-only read of the newest C1
  buffer (generation, producer frame number, buffer/layer ids, dimensions). It
  does not clone `rgba`, encode, write or hash a PNG.
* `crates/acos-art/src/launch.rs:2575` splits the `tree` command into
  `pixels:true` (legacy capture reply) and `pixels:false` (`:2716`, tree-only).
  The tree-only reply carries `buffer_generation`, `frame_number`, `buffer_id`,
  `layer_id`, `width`, `height` and `frame_identity`, plus the probe sequence
  and `settle` block. No `png` or `md5` field is written.
* `crates/acos-cli/src/engine/art.rs:1715` adds `request_tree_only`; `:1963`
  `tree_with_settle`. `absorb` (`:1699`) keeps the last *pixel-bound* observation
  in `last_pixel` (`:112`), so a tree-only reply cannot erase the frame that
  `last_frame`/`bound_frame`/`observe_session` describe. `:1000`
  `frame_identity_meta()` exposes the C1 identity; `serve.rs:933` puts it in the
  reply's `frame` object.
* `crates/acos-cli/src/engine/serve.rs:1066` passes P1's `settle` vocabulary
  through `acos_observe_tree` and `:1102` through `acos_observe {tree|text}`;
  `:1095` returns the guest's `settle` block. `acos_assert` still rejects fast
  (`serve.rs` assert arm, unchanged).

Fast tree and its invalidation rule:

* `crates/acos-art/src/launch.rs:2269` `probe_usable_for_fast_tree`: usable iff
  `probe.seq > min_probe_seq` and `probe.generation >= entry_generation`, where
  `generation` is the C1 store counter observed when the probe line was parsed
  (`ProbeSample`, `:1148`; filled by the stdout drain at `:1650`). A sample from
  before the frame store that was current at request entry is never served.
* `crates/acos-art/src/launch.rs:2299` `wait_for_fast_tree` returns that sample
  immediately, or asks the guest for one probe and waits a bounded 750 ms. On
  timeout it returns `NO_FRESH_TREE` (`:2621`), never the old tree.
* `crates/acos-art/src/launch.rs:2280` `request_guest_probe` writes one `PROBE`
  line to the guest's stdin (interactive sessions have a piped stdin, `:2379`).
* `LaunchMain.java:594` adds a daemon control thread that reads `PROBE` and
  wakes the probe thread; `:673` replaces the blind 50 ms `Thread.sleep` with a
  wait that still times out at 50 ms (the poll is the fallback). The wake only
  schedules the existing read-only probe; it never serialises per draw.

Documentation: `docs/AGENT_API.md:516` §8.6.2 documents the tree-only reply,
the `settle` argument, the `NO_FRESH_TREE` failure and the reply fields.

Not touched: `acos_reset`/stop/relaunch (P4), the screenshot/md5 path (P2), boot
arguments (B3), assertions, animations.

## 3. Freshness evidence

20 reps on the same session, each: fast tap `inc` (state 0→1→…→20) → **fast**
tree → **strict** tree (`results/art-runtime/evidence/P3d/tree-latency.json`):

* fast count matched the post-tap value **20/20**; strict matched **20/20**;
* no fast tree returned the pre-tap count (`0` never appeared after a tap);
* every fast reply was labelled `mode:"fast", settled:true,
  bound:"probe-fresh"` with its own `probe_seq` and `tree_age_ms`;
* fast request wall p50 **2.53 ms**, max **2.646 ms**; strict p50 **151 ms**.
* Unit test `acos-art launch::probe_line_tests::
  fast_tree_rejects_a_probe_from_before_the_entry_generation` covers the
  generation/sequence invalidation rule (old-generation rejected, same/newer
  accepted, sequence floor honoured, generation 0 with no frame accepted).

## 4. Tests and gates

* `cargo test -p acos-cli -p acos-art -p acos-services --offline --
  --test-threads=1` under `run-exclusive` + `guarded --mem 8G --time 1800`:
  **rc=0, 64 test binaries "test result: ok", 0 failed**
  (`evidence/P3d/tests.txt`; full log `.cache/p3b-tests-final.log`).
* Gate scenarios, real ART, release, final binary:
  `v2-art-counter` **11/11 pass**, `form-validation` **28/28 pass**,
  `w54-dialog-pixels` **14/14 pass**, `w15-dialog-menu` **3/3 pass**,
  `w42-back-frame` **17/17 pass**, `T03-render-good` **5/5 pass**,
  `T03-render-wrong` **fail `ASSERTION_FAILED`** (2 pass, 1 fail) — the
  false-pass guard is intact (`evidence/P3d/gates.txt`).
* Agent loop `bench-agent-loop.sh --loops 6 --runs 1 --settle fast`:
  **10/10 tasks verified, 0 false passes, 0 unclassified**
  (`evidence/P3d/agent-loop-fast.json`). At n=10 the per-verb percentile gates
  report INSUFFICIENT (they need n≥20); the runtime gates
  `task_reaches_verdict`, `tree_reached` and `round_trips<=12` all PASS.
* Build/run rails: release binaries sha256 in `evidence/P3d/commands.txt`;
  every ART run under `tools/art-runtime/run-exclusive.sh --task P3b` and
  `tools/guarded.sh --mem 8G`; host `contaminated=false`. The harness's
  `survivors` list contains only its own bash/timeout ancestors (their cmdline
  names the scratch directory); no guest or ART process survived.

## 5. What remains

1. **Strict tree is still 151 ms by design.** Its quiet window is P1's evidence
   rule. Fast is the opt-in navigation/inspection path, and an assertion cannot
   request it. If strict itself must get cheaper, the decision is whether a
   tree-only observation needs a 150 ms quiet window at all; this task did not
   change that semantic.
2. **View-dirty event probe (P3 objective 3) is not implemented.** The
   host-requested immediate probe removes the 0–50 ms wait for fast trees, but
   the guest still walks every 50 ms when idle. An `OnGlobalLayoutListener` +
   window add/remove dirty bit with a debounce would remove that idle work and
   make a future fast path independent of the poll. The 50 ms poll stays the
   canvas/background fallback.
3. **Strict tree reuse (P3 objective 4) is not implemented.** Fast already
   reuses a current sample cheaply; a strict second request with the same frame
   generation and no input/store could reuse the first strict tree. That needs
   the same invalidation plumbing extended to the strict reply and was left out
   to keep strict semantics byte-identical.
4. **Large trees.** A 12-node probe is <1 ms and 3.8 KiB. Diff delivery is still
   not worth building until a ≥500-node measurement shows parse/serialise cost.

**Files:** `results/art-runtime/P3d.md`; evidence under
`results/art-runtime/evidence/P3d/` (`tree-latency.json`, `summary.json`,
`agent-loop-fast.json`, `p3d_tree.py`, `commands.txt`, `gates.txt`, `tests.txt`,
`agent-loop-summary.txt`).
