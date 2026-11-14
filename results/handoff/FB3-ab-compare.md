# FB3 A/B: final 57c3c91 vs 05ba66b base (interleaved, 3 reps per arm)

Replaces the draft `FB3-ab-compare.md`, which compared 0ed96bf against itself and is wrong.

## Method

* Bench: `tools/bench/acos-bench.sh --reps 1 --groups "a:latency:8G:300" --no-build`,
  `ACOS_BENCH_TASK=FB3`, alternating **B A B A B A** ordering as run (`bench/bench-run.log`),
  one run per arm per rep, 20 latency reps per run. Each run reports
  `Noisy phases: 0/1`.
* Arm **A** = final FB3 build, commit `57c3c91` (binary sha256 `15bc0fe7…`, built in
  `.cache/g3f1-target`). Arm **B** = base build of `05ba66b` in its own linked worktree
  (binary sha256 `d316068c…`), with only the `executor.rs` compile repair copied in (no FB3
  code; `strings | grep "must be a boolean"` = 0 in B, 1 in A).
* Two data-handling corrections to the raw bench output, both applied before scoring:
  1. The bench JSON `commit` field is the worktree HEAD, so it reads `57c3c91` for both
     arms. Arm B's binary is `05ba66b`; `merge-samples.py` rewrites the field to match the binary.
  2. The bench baseline JSON carries no raw samples. `merge-samples.py` copies each rep's
     raw values (`run-*/rep-1/latency.json`, `samples[key].values`, 20 per rep per metric)
     into the baseline JSON so `tools/bench/ab-compare.py` can compute the pooled
     5 %-trimmed mean and the frame-slot shares.
* **Sign convention.** `ab-compare.py` computes `delta = b − a` and calls `b` the candidate,
  so `a` must be the base. The first attempt passed FB3 as `--a`, which inverted every
  verdict (it reported a false REGRESSION on the counter fast tap). The final run below uses
  base = `--a`, FB3 = `--b`. Positive delta = FB3 slower.

Raw: `bench/A{1,2,3}.json`, `bench/B{1,2,3}.json` (bench output), `bench/A{1,2,3}.lat.json`,
`bench/B{1,2,3}.lat.json` (with merged samples), `bench/ab.json`. Under
`results/handoff/FB3-evidence/bench/`.

## Result (`tools/bench/ab-compare.py --a B*.lat.json --b A*.lat.json`)

# 05ba66b base (a) vs FB3 final 57c3c91 (b) -- lattice-aware A/B (PERF-REL3)

Arms: 05ba66b base (a) = 3 file(s) [05ba66b, 05ba66b, 05ba66b]; FB3 final 57c3c91 (b) = 3 file(s) [57c3c91, 57c3c91, 57c3c91]

## 1. Old gate -- medians of per-rep p50s (unchanged rule)

| metric | 05ba66b base (a) | FB3 final 57c3c91 (b) | delta | delta % | threshold | verdict |
|---|---:|---:|---:|---:|---:|---|
| M4 warm launch (strict) | 305.98 | 306.28 | 0.30 | +0.1% | ±5.00% / 2.00 | within-noise |
| M4 warm launch (fast) | 155.72 | 155.62 | -0.10 | -0.1% | ±5.00% / 2.00 | within-noise |
| M5 fresh screenshot (PNG) | 11.39 | 11.67 | 0.28 | +2.5% | ±5.00% / 2.00 | within-noise |
| M6 reset -> first correct screen (strict) | 343.84 | 343.92 | 0.08 | +0.0% | ±5.00% / 2.00 | within-noise |
| M6 reset -> first correct screen (fast) | 197.48 | 200.20 | 2.71 | +1.4% | ±5.00% / 2.00 | within-noise |
| M7 UI tree with text (strict) | 41.32 | 41.41 | 0.09 | +0.2% | ±5.00% / 2.00 | within-noise |
| M7 UI tree with text (fast) | 0.59 | 0.60 | 0.01 | +1.9% | ±5.00% / 2.00 | within-noise |
| M8 tap -> settled, counter (strict) | 947.66 | 950.68 | 3.01 | +0.3% | ±5.00% / 2.00 | within-noise |
| M8 tap -> settled, counter (fast) | 50.49 | 49.73 | -0.76 | -1.5% | ±5.00% / 2.00 | within-noise |
| M8 tap -> settled, form (strict) | 933.13 | 932.91 | -0.22 | -0.0% | ±5.00% / 2.00 | within-noise |
| M8 tap -> settled, form (fast) | 32.38 | 32.77 | 0.38 | +1.2% | ±5.00% / 2.00 | within-noise |
| tap acknowledgement | 5.00 | 5.00 | 0.00 | +0.0% | ±5.00% / 2.00 | within-noise |
| acos_destroy teardown_ms | 13.00 | 13.00 | 0.00 | +0.0% | ±5.00% / 2.00 | within-noise |
| cpu_active_tap_strict_pct | 46.07 | 44.70 | -1.37 | -3.0% | ±10.00% / 0.20 | within-noise |
| teardown_wall_ms | 18.92 | 18.78 | -0.14 | -0.8% | ±5.00% / 2.00 | within-noise |

## 2. Pooled 5 %-trimmed mean, and 3. frame-slot shares

| metric | n 05ba66b base (a)/FB3 final 57c3c91 (b) | 05ba66b base (a) trimmed | FB3 final 57c3c91 (b) trimmed | delta | delta % | frame ms | first slot 05ba66b base (a)/FB3 final 57c3c91 (b) | slot shares 05ba66b base (a) | slot shares FB3 final 57c3c91 (b) | verdict |
|---|---:|---:|---:|---:|---:|---:|---:|---|---|---|---|
| M4 warm launch (fast) | 60/60 | 161.47 | 158.85 | -2.62 | -1.6% | 16.39 | 55%/58% | 0:55% 1:42% 2:3% | 0:58% 1:35% 2:5% 3:2% | within-noise |
| M6 reset -> first correct screen (fast) | 60/60 | 201.97 | 202.18 | +0.21 | +0.1% | 16.67 | 3%/2% | 0:3% 1:48% 2:48% | 0:2% 1:48% 2:48% 3:2% | within-noise |
| M8 tap -> settled, counter (fast) | 60/60 | 49.98 | 43.76 | -6.22 | -12.4% | 15.60 | 0%/15% | 1:12% 2:78% 3:10% | 0:15% 1:15% 2:63% 3:7% | improved (slot-shift +15 pp) |
| M8 tap -> settled, form (fast) | 60/60 | 32.40 | 32.78 | +0.38 | +1.2% | 16.67 | 100%/100% | 0:100% | 0:100% | within-noise |

## 4. Verdict

Lattice gate (proposal): pooled 5 percent-trimmed mean worse by > 5 percent and > 2 ms; a first-slot share shift >= 10 pp is flagged separately.

* M4 warm launch (fast): within-noise -- trimmed 161.47 vs 158.85 (-2.62 ms, -1.6%), first-slot 55% vs 58%
* M6 reset -> first correct screen (fast): within-noise -- trimmed 201.97 vs 202.18 (+0.21 ms, +0.1%), first-slot 3% vs 2%
* M8 tap -> settled, counter (fast): improved -- trimmed 49.98 vs 43.76 (-6.22 ms, -12.4%), first-slot 0% vs 15% (SLOT-SHIFT +15 pp)
* M8 tap -> settled, form (fast): within-noise -- trimmed 32.40 vs 32.78 (+0.38 ms, +1.2%), first-slot 100% vs 100%

No regression under either gate.

JSON: ab.json

## Reading the fast-metric verdicts

* **M8 tap → settled, counter (fast).** The pooled trimmed mean says FB3 is 6.2 ms lower
  (−12 %), and the tool labels this "improved". I do **not** claim an improvement. The
  first-slot share moved by 15 points (0 % → 15 %), and the per-rep means spread widely on
  both arms (base 45.2 / 52.1 / 52.7 ms; FB3 49.8 / 32.0 / 48.8 ms). That is the frame-lattice
  phase, not a verified speedup. The old median gate (49.73 vs 50.49, −0.8 ms, −1.5 %) is
  within noise. Verdict: **parity**.
* **M8 tap → settled, counter (strict)** is not in the fast-metric table. Recomputed from the
  raw samples: base trimmed 946.33 ms, FB3 952.06 ms (+5.7 ms, +0.6 %). The old median gate is
  +3.0 ms (+0.3 %). Regression needs both more than 5 % and more than 2 ms, and this is far
  below 5 %. Verdict: **parity**.
* **M8 tap → settled, form (fast):** +0.38 ms (+1.2 %), within noise. Verdict: **parity**.
* **M4 and M6 (fast):** −2.6 ms / +0.2 ms trimmed, within noise. **Unchanged.**
* **M5, M7 (fast and strict), tap ack, teardown:** all within the gate. **Unchanged.**

The old per-rep median gate reports no regression on any metric. The pooled lattice gate
flags nothing as a regression after correcting the sign convention.
