# PERF-REL3 — frame-lattice-aware fast-settle measurement, and the v0.2.1 launch A/B

Status: (1) and (2) done and tested; (3) done for the launch question (verdict: noise);
(3b) the manifest parse cost is not measured (see the end). Branch `perf-rel3`, base 52beccf
(int-021, the v0.2.1 candidate).

## 1. Lattice-aware bench output

The problem (results/bench/REL3-AB.md): every fast-settle sample waits for the compositor, so
values sit on a ~16 ms frame-period lattice — M4 launch fast at ~155 / ~171 / ~187 ms, M8 tap
fast at ~44 / ~49 / ~59 ms. A rep's p50 is whichever mode holds the 10th/11th of 20 samples, so
**medians of per-rep p50s flip by a whole frame between reps**. The old gate treats that flip as
signal.

What the bench records now (`tools/bench/bench_lattice.py`, wired into `acos_bench.py --merge`):

* every metric keeps its raw `samples: [...]` in the merged baseline JSON (previously only per-rep
  p50s survived the merge);
* the four fast-settle metrics (`m4_launch_fast_ms`, `m6_reset_fast_ms`,
  `m8_tap_counter_fast_ms`, `m8_tap_form_fast_ms`) additionally carry a `lattice` block:
  `trimmed_mean` (5 % per side), `median`, `frame_period_ms` + `frame_period_source`
  (`measured` from the samples by pairwise-gap mode, `compositor` when a vsync period is supplied,
  `fallback` 60 Hz), `origin_ms` (the fastest observed sample), `slots` (per-slot n/share/min/max),
  `slot_shares`, `first_slot_share`, and `per_rep` (each rep's p50, trimmed mean and slot shares on
  the same pooled lattice);
* **nothing existing changed**: `p50`, `p95`, `mean`, `n`, `rep_p50`, thresholds and the p50 compare
  gate are byte-for-byte the old shape/semantics (additive fields only). Old baseline JSONs still
  load (unit test `test_ab_compare.CliTest.test_old_schema_docs_still_parse`, plus a real merge of
  `results/bench/run-20261112T200322Z`).

Sample count: `--fast-reps N` (or `ACOS_BENCH_FAST_REPS`), default **40 per rep** for the fast
metrics; `--reps` keeps its meaning for every other metric (20). `--quick` keeps 8 fast samples so
the CI subset stays under its 5-minute budget.

**Measured cost of 40 vs 20 fast samples** (one latency rep, 52beccf, `.cache/run-smoke40`,
`checks.mode_wall_s`, extra = fast-loop wall / 41 x 20 extra samples):

| mode | strict 20 samples | fast 40 samples | extra wall for 20 more |
|---|---:|---:|---:|
| M4 launch | 7.7 s | 14.0 s | +7.0 s |
| M6 reset | 27.6 s | 51.9 s | +25.3 s |
| M8 counter tap | 20.0 s | 1.5 s | +0.7 s |
| M8 form tap | 24.6 s | 45.9 s | +22.4 s |
| **total** | | | **+55.4 s (≈ +57 s with warm-ups)** |

That is above the 30 s/rep threshold in the track brief, so it is documented rather than hidden:
the latency phase wall was **202.7 s** with 40 samples (a full 3-rep run pays ~3 min more). The M6
and M8-form loops are expensive because every sample re-reads a strict tree around a reset; that
is existing methodology and was not changed. If the orchestrator prefers the old budget, pass
`--fast-reps 20` (the lattice fields still work, on half the samples).

## 2. A/B comparison tool

`tools/bench/ab-compare.py --a JSON... --b JSON...` (one JSON per rep or per merged run; arms
interleave freely). Since old baselines carry no raw samples, the tool degrades gracefully: it
scores the old gate and says `no-samples`.

It prints, in this order:

1. **old gate** — medians of per-rep p50s against the stored per-metric threshold (unchanged rule,
   always printed, never hidden);
2. **pooled 5 %-trimmed mean** per arm, delta ms and delta %;
3. **slot shares** on one shared lattice for both arms (period measured on the union of the two
   arms' samples, origin = the fastest sample of the union), with the first-slot share and the
   per-slot percentage-point delta;
4. **verdict**.

Proposed gate rule for the fast-settle metrics (for the orchestrator to adopt or reject; the old
gate output stays):

* **REGRESSION** when the pooled 5 %-trimmed mean is worse by **> +5 % AND > +2 ms**;
* **improved** symmetrically;
* **slot-shift flag** when the first-slot share moves **>= 10 percentage points** (reported next to
  the verdict; a shift of that size means the settle path changed how often a launch lands in the
  first frame slot, even if the central value did not move);
* everything else `within-noise`. Non-fast metrics keep the old gate verdict.

Rationale: the trimmed mean does not move when a rep's middle sample hops a slot (the REL3-AB
failure mode), yet it still catches a real one-frame shift (~+10 % at launch, ~+35 % on tap). The
+2 ms absolute floor keeps a 1 ms wiggle on a 45 ms tap from tripping a percentage gate. The
slot-share flag is a second, independent signal: a distribution that moves from 59 % to 32 % in
the fastest slot is a behaviour change even when the mean is inside the band.

Unit tests: `python3 -m unittest discover -s tools/bench -p 'test_*.py'` — 29 tests, covering the
trimmed-mean/slot/period math on synthetic 60 Hz and 120 Hz lattices, the degenerate cases (one
slot, continuum, empty), the verdicts (one-frame shift, inside both floors, share-only shift,
shared-lattice comparison) and old-JSON parsing.

## 3. Attribution: is the launch fast-settle shift real?

**Verdict: no — the M4 launch fast-settle "shift" is the p50 slot flip, i.e. noise.** The
candidate and main are at parity on the pooled trimmed mean (+1.0 %, +1.7 ms on 164.3 ms) and on
the first-slot share (44 % vs 43 %, a 1-point difference, far below the 10-point flag), with 120
samples per arm. The tap-fast changes below are real but they are improvements, not a launch
regression, and they are not on the launch path.

Arms (arm identity is the binary sha256; `commit` in a baseline JSON is the *bench checkout*, not
the binary):

| arm | source | binary sha256 | reps x samples |
|---|---|---|---:|
| main74e4ee9 | 74e4ee9 built in `.cache/wt-main74` (`strings` shows only that worktree: 38 refs) | `2b193e8f9002…` | 3 x 40 = 120 |
| cand52beccf | 52beccf built in this worktree (`.cache/perf-target`) | `fb143e043bd5…` | 3 x 40 = 120 |

Interleaved `ACOS_BENCH_TASK=PERF-REL3 tools/bench/acos-bench.sh --reps 1 --groups
"a:latency:8G:300" --no-build` in the order cand, main, cand, main, (cand), main — one lock
acquisition per rep, `--fast-reps 40`. Host load1 1.3–3.5, MemAvailable 29–32 GiB, clear weather.

Per-rep numbers for the launch metric (the raw samples and the per-rep records are in
`.cache/rel3/*.json`; each rep is a merged baseline JSON):

| arm | rep | p50 | 5%-trimmed mean | first-slot share | max |
|---|---:|---:|---:|---:|---:|
| main74e4ee9 | 1 | 170.62 | 168.84 | 25 % | 263.0 |
| main74e4ee9 | 2 | 168.84 | 163.57 | 47 % | 188.2 |
| main74e4ee9 | 3 | 155.75 | 161.34 | 60 % | 173.6 |
| cand52beccf | 1 | 171.18 | 178.96 | 17 % | 283.2 |
| cand52beccf | 2 | 156.18 | 162.38 | 53 % | 188.1 |
| cand52beccf | 3 | 155.52 | 161.30 | 60 % | 172.7 |

Old gate = median of per-rep p50s: main 168.84, cand 156.18. **That −7.5 % "improvement" and the
earlier +8.7 % "regression" (same candidate, two main reps) are the same data read through a
statistic that hops one frame between reps** — exactly the REL3-AB failure mode. Each arm has reps
on *both* sides of the flip: main `{170.6, 168.8, 155.8}` (median 168.8), candidate
`{171.2, 156.2, 155.5}` (median 156.2); with two main reps the median is 169.7 and the candidate
looks worse, with three it is 168.8 and the candidate looks better. With the lattice view:

| metric | n main/cand | pooled 5%-trimmed main | cand | delta | Δ% | first slot main/cand | verdict |
|---|---:|---:|---:|---:|---:|---:|---|
| M4 launch fast | 120/120 | 164.30 | 165.99 | +1.69 ms | +1.0 % | 44 % / 43 % | within-noise |
| M4 launch strict | 60/60 | 305.67 | 306.23 | +0.56 ms | +0.2 % | — | within-noise |
| M6 reset fast | 120/120 | 201.54 | 201.79 | +0.24 ms | +0.1 % | 2 % / 6 % | within-noise |
| M6 reset strict | 60/60 | 343.72 | 343.96 | +0.23 ms | +0.1 % | — | within-noise |
| M8 tap counter strict | 60/60 | 957.71 | 946.01 | −11.70 ms | −1.2 % | — | within-noise |
| M8 tap counter fast | 120/120 | 49.65 | 39.53 | −10.1 ms | −20.4 % | 0 % / 22 % | improved (slot-shift +22 pp) |
| M8 tap form strict | 60/60 | 948.75 | 930.09 | −18.67 ms | −2.0 % | — | within-noise |
| M8 tap form fast | 120/120 | 44.96 | 32.71 | −12.3 ms | −27.3 % | 0 % / 100 % | improved (slot-shift +100 pp) |

Lattice detail for M4 launch fast, pooled (period measured on the union of both arms: **16.39 ms**,
consistent with the 60 Hz compositor): main `0:44 % 1:48 % 2:6 % 3:1 % 7:1 %`, cand
`0:43 % 1:42 % 2:9 % 3:2 % 4:2 % 5:1 % 6:1 % 8:1 %` — the same distribution, same two dominant
modes, same tail of slow launches. Nothing about the launch path changed.

The M8 tap rows are a different story and are **not** a launch regression: the candidate has a
fast mode at ~33 ms that main does not have (main is ~49 ms), and the "form" fast samples collapse
into one cluster per arm (main ~45 ms, cand ~33 ms). The candidate is faster on both taps; the old
p50 gate also sees that (as "improved"), and the lattice view adds the mode evidence. This is the
rel3-int tap-visibility work, not the candidate's launch path.

**No bisect was needed** (a bisect would have started from int-021's merges: test-launch-hang-cold,
abi-gate, settle-se2-content-stable, zygote-prod-1, art-rotate-1, rn-tap-visibility,
screenshot-compress, fb4-artifacts). 74e4ee9 is an ancestor of 52beccf (72 commits), so if the
launch shift had been real the bisect range would have been those 72 commits.

### 3b. REL3b manifest parse cost on the launch path -- measured, cached

Track PERF-REL3b, branch `perf-rel3b` from 477e99c (main v0.2.1). Commit `39876dfa`.

**Call sites on the default ART launch path** (`acos launch --engine art`, `until =
application-created`, no `--activity`): five `Apk::open` calls per launch:

1. `acos-art/src/session.rs` `apk_activity` -> `launcher_activity`;
2. `acos-art/src/launch.rs` `native_libs::install` -> `Apk::open` (extracts the `.so` files; not a
   manifest parse, left as it is);
3. `acos-art/src/launch.rs` `pms::manifest_labels` -> `Apk::open` + manifest parse;
4. `acos-art/src/launch.rs` `manifest_resolve::launch_budget` -> `Apk::open`;
5. `acos-art/src/launch.rs` config changes -> `launcher_activity`.

Three of the five parse the AXML manifest. `Apk::open` reads the whole APK, copies every dex and
indexes the zip, so it is the dominant cost (Gaten: the open is ~61 ms, the AXML DOM parse is
~0.5 ms on top of it).

**Measured before the change** (`tests/manifest_launch_timing.rs`, 100 iterations, `run-exclusive
--exclusive`; `results/bench/perf-rel3b/manifest-timing-before.txt`). Sum of p50s of the five call
sites per launch:

| APK | one `Apk::open` p50 | five call sites p50 (before) |
|---|---:|---:|
| counter (12.7 KB) | 0.020 ms | **0.127 ms** |
| form-validation (12.7 KB) | 0.021 ms | **0.129 ms** |
| Gaten debug (115 MB) | 60.7 ms | **304.5 ms** |

Gaten is far above the 1 ms threshold, so a cache was added. The fixtures sit under 1 ms.

**Change** (`crates/acos-services/src/apk_facts.rs`, `manifest_resolve.rs`, `pms.rs`): one process-wide
cache of the derived facts (launcher, labels, budget) keyed by path, file length and mtime. Stat
only, no hash, so the APK is never re-hashed just for the cache. The cache keeps the facts, not the
APK bytes. Failed opens are not cached. The three public functions keep their signatures and error
text.

**Measured after the change** (`results/bench/perf-rel3b/manifest-timing-after.txt`):

| APK | cold launch (first call opens) p50 | warm launch (second launch of the APK) p50 | before p50 |
|---|---:|---:|---:|
| counter | 0.048 ms | 0.022 ms | 0.127 ms |
| form-validation | 0.049 ms | 0.023 ms | 0.129 ms |
| Gaten debug | **121.8 ms** | **61.1 ms** | 304.5 ms |

The warm Gaten cost is the one open left in `native_libs::install`. Passing it the already-opened
`Apk` would remove that too; not done here (it is not a manifest parse and touches the extraction).

**Correctness:** `tests/apk_facts_cache.rs` passes: a hit returns the same `Arc` and does not re-open
the APK; a length change and an mtime-only change each re-read; a failed open is not cached. The
`manifest_launch_timing` test checks the launcher, budget and labels on counter, form-validation and
Gaten. Full `cargo test --release -p acos-services` passes (EXIT 0), run through `run-exclusive`.

**A/B (M4 launch, cache vs base):** interleaved `ACOS_BENCH_TASK=PERF-REL3b tools/bench/acos-bench.sh
--reps 1 --groups "a:latency:8G:300"`, order cand, base, cand, base (two reps per arm,
`results/bench/perf-rel3b/ab-run-status.txt`). Binaries: cand `af9a5da5...` (39876dfa), base
`40673a49...` (477e99c), built in separate target dirs; compile paths checked (cand strings reference
only `wt-perf`, base only `perf-base`). `results/bench/perf-rel3b/ab-compare-m4.txt`:

| metric | base 477e99c | cand 39876dfa | delta | verdict |
|---|---:|---:|---:|---|
| M4 launch fast, old gate (median of per-rep p50s) | 163.01 ms | 162.87 ms | -0.1% | within-noise |
| M4 launch fast, pooled 5 %-trimmed mean (n 80/80) | 162.74 ms | 161.89 ms | -0.5% | within-noise |
| M4 launch fast, first-slot share | 50 % | 56 % | +6 pp | below the 10 pp flag |
| M4 launch strict, old gate | 305.89 ms | 305.28 ms | -0.2% | within-noise |

**Limit of the A/B:** the bench's M4 launches use the counter fixture, where the saving is ~0.08 ms
of ~160 ms. The bench cannot show a gain; it shows no regression. The gain (~180 ms per Gaten
launch) is measured by the parse harness above, not end to end. End-to-end Gaten launch wall time was
not measured.

**Decision:** keep the cache. The five parses sum to 0.13 ms on fixtures (below 1 ms, the cache
changes nothing there that the bench can see) and to 304 ms on Gaten (above 1 ms). M4 shows no
regression. Not merged into main.

**Notes for the orchestrator**

- The cache lives in the process. In the daemon (`acos serve`) the facts survive across launches of
  the same APK; a one-shot `acos` process only saves within its own launch.
- The key is (length, mtime). A rewrite that keeps both is not seen. On ext4 with ns mtimes this is
  not a practical concern, but it is a known gap.
- Pre-existing and not changed: `manifest_resolve::tests::real_apks_resolve_like_aapt2_could_not`
  opens `fixtures/build/apps/counter.apk` and `.cache/gaten/...` relative to the crate directory
  (`crates/acos-services`), so under `cargo test` both checks are silently skipped. The new
  `manifest_launch_timing` test uses `CARGO_MANIFEST_DIR/../..` and does not skip.

### Reproduction

```
# arm binaries (both under this worktree; compile paths checked with strings)
git worktree add .cache/wt-main74 74e4ee9
CARGO_TARGET_DIR=$PWD/.cache/main74-target tools/guarded.sh --mem 14G --vmem 40000000 \
  --time 1500 -- cargo build --release --offline --manifest-path .cache/wt-main74/Cargo.toml \
  -p acos-cli -p acos-art
# one lock acquisition per rep, arms alternating
ACOS_BENCH_TASK=PERF-REL3 tools/bench/acos-bench.sh --reps 1 --groups "a:latency:8G:300" \
  --target-dir .cache/main74-target --no-build --scratch-root .cache/h6-perf \
  --run-dir .cache/rel3/main-rN-run --out .cache/rel3/main-rN.json
# the A/B view (old gate + lattice), as committed in results/bench/perf-rel3/
tools/bench/ab-compare.py --a .cache/rel3/main-r{1,2,3}.json --label-a main74e4ee9 \
  --b .cache/smoke40.json .cache/rel3/cand-r{2,3}.json --label-b cand52beccf \
  --metrics m4_launch_fast_ms,m6_reset_fast_ms,m8_tap_counter_fast_ms,m8_tap_form_fast_ms
```

(`.cache/smoke40.json` is the first candidate rep, run before the bench commit; same binary
sha256. `results/bench/perf-rel3/ab-cand-vs-main.json` is the same comparison with the roles
reversed — its "REGRESSION" labels read as "the main arm is worse", i.e. the candidate's tap
improvement.)

### 3c. PERF-REL3c: one APK open per launch, test-skip fix

Track PERF-REL3c, branch `perf-rel3b` (not merged into main). Commits on `perf-rel3b`:
`421ed4d7` (TESTSKIP fix) and `a0aad161` (one open per launch). Evidence: `results/bench/perf-rel3c/`.

**(1) Correctness before and after the launch-path change**

| check | before (dd906863) | after (a0aad161) |
|---|---|---|
| ART corpus `acos-art verify` (27 programs, run-exclusive) | 27 pass, 0 fail, 1 skipped, 0 fatal logs | 27 pass, 0 fail, 1 skipped, 0 fatal logs (`corpus-before.log`, `corpus-after.log`) |
| gate `v2-art-counter` `--engine art` | pass | pass |
| gate `form-validation` `--engine art` | pass | pass |
| gate `gaten-home` `--engine art` (release-unsigned APK) | pass | pass |

The corpus baseline is `results/art-runtime/verify-27-f2.json` (27/27, 0 fatal logs). The skipped entry is the
same one the baseline records.

**(2) TESTSKIP: `manifest_resolve::tests::real_apks_resolve_like_aapt2_could_not` never ran its checks**

* Root cause: the paths were crate-relative (`fixtures/...`, `.cache/gaten/gaten-x86_64-debug.apk`), and `cargo test`
  runs with the crate as cwd. The Gaten path also named a file that does not exist (the debug APK is
  `gaten-dev-9feffd2-x86_64.apk`), so both halves were `if is_file()` no-ops. The test passed with zero assertions.
* Fix (`421ed4d7`): paths resolve from `CARGO_MANIFEST_DIR/../..`. The counter fixture is required and panics
  with its path when absent (`fixtures/build` is gitignored). The Gaten APK is optional: when absent the test prints
  `SKIP ... (Gaten half): <path> is absent; documented environment gap` and returns. The path is now the real
  release APK, `.cache/gaten/gaten-dev-f344272-x86_64-release-unsigned.apk`.
* The Gaten expectation was wrong for that APK, and it only showed once the test ran. The old assertion
  (`dex_count >= 8`, "31-dex APK") describes the debug build. The release APK has one `classes.dex` and 23 `.so`
  files (~70 MB), so it is large by the native threshold. The test now asserts that shape (`dex_count == 1`,
  `native_bytes >= 64 MiB`, `resume_ms == RESUME_MS_LARGE`). No threshold changed.
* Same class of bug in `tests/apk_facts_cache.rs` (`SKIP apk_facts_cache: fixtures missing` returned green). It now
  asserts the fixture (`a0aad161`).
* Result: `real_apks_resolve_like_aapt2_could_not` runs both halves and passes; no SKIP line is printed when the
  APK is present (`testskip-after.log`).

**(3) One APK open per launch (`a0aad161`)**

Before this change, `acos-art launch` (`launch::launch`) opened the source APK through four independent paths:
* `native_libs::install` (`Apk::open`, extraction);
* `apk_facts::launch_facts` (manifest labels, budget), a cache **miss** on a cold process;
* `pms::PmsState::from_apk` (`Apk::open` + manifest), never cached;
* the config-change lookup, which read the **staged** copy in the per-run directory. That path is new on every
  launch, so it is a cache miss and an open on every launch, warm or cold.

Counted from the code at `dd906863`: **4 opens cold, 3 opens warm** (warm = daemon, facts already cached). Each
open of the 115 MB Gaten release APK is ~61 ms (`manifest-timing-100-after.log`).

After `a0aad161`: **1 open** per launch, cold and warm.
* `apk_facts::open_shared(path)` returns `Arc<Apk>`. The cache holds a `Weak`, so the bytes live only as long as the
  launch holds the `Arc`. The cache keeps facts (kilobytes), never bytes.
* `apk_facts::prime_from(path, &apk)` derives the facts from the open the launch already holds, so the later
  `launch_facts`-based calls (`manifest_labels`, `launch_budget`, `launcher_activity`) are cache hits.
* `native_libs::install_from(&apk, ...)` and `PmsState::from_opened(path, &apk)` take that open.
  `native_libs::install` and `PmsState::from_apk` keep their signatures (`from_apk` now uses the shared open).
* `launch.rs`: one block opens, installs, primes and builds `installed` from the same open, then drops it. The
  config-change lookup reads `opts.apk` (the source; the staged file is a byte copy made earlier in the same launch).

Tests:
* `tests/launch_one_open.rs` (new, one test, `hello-jni`): one shared open; `install_from` and the facts add no
  open; after the open is dropped the facts still answer with no read; a new shared open reads the file again.
* `tests/manifest_launch_timing.rs`: the launch model is now the real sequence (`open_shared` + `prime_from` +
  the manifest calls). It asserts one cold launch opens the APK exactly once.
* `tests/apk_facts_cache.rs`: unchanged checks, now with a required fixture.
* Full run `cargo test --release --offline -p acos-services -p acos-art -p acos-cli` through run-exclusive:
  76 test-result blocks (lib, bin, integration and doc tests), **727 passed, 0 failed** (`cargo-test-summary.txt`).

Timing (`ACOS_MANIFEST_TIMING_ITERS=100`, `run-exclusive --exclusive`, `manifest-timing-100-after.log`). Gaten, p50:

| model of the launch's source-APK work | p50 |
|---|---:|
| before: five uncached call sites, five opens (the 3b base model, kept in the harness) | 378.0 ms |
| after: one launch sequence, cold (`launch-sequence-after(cold)`, one open) | **63.1 ms** |
| after: one launch sequence, warm (one open for native install; facts cached) | 62.5 ms |

The counter and form-validation fixtures stay under 0.2 ms in every row, so this change is invisible to the M4 bench,
which uses the counter.

Limits of the measurement:
* The timing harness models the sequence. It does not time the real `launch::launch`, and the open count was checked
  by the unit test and by reading `launch.rs`, not by tracing a live launch (no `strace` on this host).
* `native_libs` extraction (the 73 MB `.so` copy) is not in the model. It is the same before and after.
* In `acos-art session` (`session::session`) the launcher is still resolved through `apk_activity` with its own open.
  That path is not the `launch` path this track measures, and it is left as is.

**(4) M4 A/B vs 477e99c: no regression** (2 reps per arm, interleaved cand, base, cand, base;
`ACOS_BENCH_TASK=PERF-REL3c tools/bench/acos-bench.sh --reps 1 --groups "a:latency:8G:300" --no-build`, each rep
its own lock acquisition; `results/bench/perf-rel3c/ab-m4.txt`, raw runs in `results/bench/perf-rel3c/ab/`)

* Arms: base = `477e99c` built in a linked worktree with its own target (binary sha256 `ccc43f31…`; compile paths
  checked: only `wt-base477` strings). Cand = `a0aad161` (binary sha256 `255440b3…`; only `wt-perf` strings).
  The `commit` field in each bench JSON is the checkout's HEAD for every arm, so the binary hash is the arm's
  identity. The base worktree and its target were deleted after the runs.
* `ab-compare.py`, M4 (counter fixture, the bench's launch):

| metric | base 477e99c | cand a0aad161 | delta | verdict |
|---|---:|---:|---:|---|
| M4 warm launch fast, old gate (median of per-rep p50s) | 168.83 ms | 162.73 ms | -3.6 % | within-noise |
| M4 warm launch fast, pooled 5 %-trimmed mean (n 80/80) | 163.73 ms | 164.54 ms | +0.5 % | within-noise |
| M4 warm launch fast, first-slot share | 45 % | 38 % | -7 pp | below the 10 pp flag |
| M4 warm launch strict, old gate | 305.82 ms | 305.05 ms | -0.3 % | within-noise |

* The bench's M4 launch is the counter fixture. Its open saving is ~0.1 ms of ~160 ms, so the bench can show no gain,
  only the absence of a regression. The Gaten gain is the open count in (3): three fewer opens cold (about 180 ms at
  ~61 ms each) and two fewer warm (about 120 ms). The bench does not measure it, and end-to-end Gaten launch wall time
  was not measured.

### Tests / validation (PERF-REL3 bench tooling; 3b tests are under 3b above)

* `python3 -m unittest discover -s tools/bench -p 'test_*.py'` — 29 pass.
* Old baselines: `acos_bench.py --merge` on `results/bench/run-20261112T200322Z` produces the same
  p50/p95/threshold fields plus `samples`/`lattice`; `--compare` against `results/bench/BASELINE.json`
  prints the unchanged gate; `ab-compare.py` on two pre-PERF-REL3 baselines reports `no-samples` for
  the fast rows and scores the old gate.
* PERF-REL3 itself changed no Rust. The 3b Rust changes and their tests are described in §3b.
