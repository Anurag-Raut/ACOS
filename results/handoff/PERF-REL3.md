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

### 3b. REL3b activity-alias manifest parse cost — NOT measured

Remaining. Plan (prepared, not run — the 60-minute limit was reached): a temporary
`crates/acos-services/tests/` timing test calling
`manifest_resolve::launcher_activity()` and `launch_budget()` 100x for counter.apk,
form-validation.apk and the Gaten debug APK, run through
`run-exclusive.sh --task PERF-REL3 -- guarded.sh -- cargo test --release -p acos-services`.
Note from the code: the launch path parses the manifest at least twice per launch
(`launch_budget` + `launcher_activity`, and `acos-art --config_changes` does a third), so the
per-launch cost is 2–3 x one parse. Cache only if that sum is > 1 ms, keyed by APK sha256 (the
parse depends only on the bytes); `LauncherActivity`/`LaunchBudget` are both `Clone`-able plain
data, so a small process-local `HashMap<[u8;32], _>` behind a mutex would do. This is a proposal,
not implemented.

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

### Tests / validation

* `python3 -m unittest discover -s tools/bench -p 'test_*.py'` — 29 pass.
* Old baselines: `acos_bench.py --merge` on `results/bench/run-20261112T200322Z` produces the same
  p50/p95/threshold fields plus `samples`/`lattice`; `--compare` against `results/bench/BASELINE.json`
  prints the unchanged gate; `ab-compare.py` on two pre-PERF-REL3 baselines reports `no-samples` for
  the fast rows and scores the old gate.
* No Rust was changed, so no `cargo test` was needed. (The temporary timing test for 3b was
  created and deleted without being run; the tree is clean of it.)
