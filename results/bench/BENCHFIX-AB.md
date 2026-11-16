# BENCHFIX A/B: v0.2.2 (tag) vs rel-030 HEAD -- 2026-11-16

The A/B gate (ADR-0028, `tools/bench/ab-compare.py`) could not run at all on main or rel-030.
This run is the first full one after the harness fix, and it is the gate table the release
decision needs.

* Arm A: tag `v0.2.2` (ce14b888), built from `git archive v0.2.2`; binary sha256 `50537b60cd2c4bf28e1e1ff63b9ebf4b35aad6a66f5393c9a408a67ddb0da7ad`
  (`acos 0.2.2`).
* Arm B: rel-030 HEAD `b8c30224` (product code; the bench fix is Python only); binary sha256
  `7f16d333de4157af666df6398a1be51f60ca63ee73b8376071dd633f949693d7` (`acos 0.2.3`).
* Driver: interleaved per rep (A latency+cold, A memory, B latency+cold, B memory), 3 reps, every
  acquisition through `tools/art-runtime/run-exclusive.sh --task BENCHFIX --exclusive` +
  `tools/guarded.sh` (8G/540s for latency+cold, 16G/600s for memory).
  `--reps 20 --fast-reps 40` (the ADR-0028 defaults; 20 strict samples and 40 fast samples per rep
  per metric). Host: AMD Ryzen 9 9950X, 60 GiB, load 1.5-4.5 during the run (no phase was marked
  noisy by the host gate).
* Raw JSON (kept): `results/bench/BENCHFIX-AB/{v022,rel030}.json` (merged, with raw `samples`),
  `results/bench/BENCHFIX-AB/run-{v022,rel030}/rep-{1,2,3}/*.json` (per phase),
  `results/bench/BENCHFIX-AB/ab-compare.json|txt`, `results/bench/BENCHFIX-AB/shape-probe/*.json`.
* Every metric both arms carry: M3 cold, M4 strict/fast, M5 screenshot, M6 strict/fast, M7
  strict/fast, M8 counter+form strict/fast, tap ack, GL1 teardown, M1/M2 memory (anon+shmem and
  Pss at N=1/5/10), idle CPU and wakeups -- 33 metrics, `n = 60` strict / `120` fast samples per
  metric per arm.

Meaning of the arm label in the tables: the merged docs carry the merge checkout's `commit` for
both arms (`b545b1e4`, the benchfix branch), so provenance is by `acos` +
`binary_sha256` above, not by that field.

## Gate result

**One lattice-gate regression: M8 tap -> settled, form (fast), +3.64 ms / +11.1 % (first-slot
98 % -> 85 %, slot-shift -13 pp).** The old (median-of-p50s) gate calls it within noise
(+5.8 %, threshold +-14 % / 2 ms); the trimmed-mean gate flags it, and it is consistent across all
three reps (per-rep p50 32.43/32.46/32.77 on v0.2.2 vs 34.35/34.02/36.70 on rel-030), so it is not
a one-rep artifact. No product code was changed for it (out of scope for BENCHFIX): it needs a
REL3C/REL3F reply-path look at the form submit fast tap.

Everything else is parity or better. Notable (within/improved, not regressions):
M4 launch fast -9.8 % (slot-shift +29 pp), M6 reset fast -11.3 % (slot-shift +16 pp), M4 strict
-6.0 %, M6 strict -3.5 %, M7 flat, M8 counter flat, M5 +1.6 %, M3 cold +3.0 % (rep 3 of arm A had
one 6.08 s cold start; the medians are 4131 vs 4255 ms), memory +2..4 % (all within their derived
thresholds; anon+shmem and Pss are both reported, never mixed), idle CPU/wakeups flat.

## What was broken and why (root causes, from the shipping binaries)

The probe (`shape-probe/*.json`, one launch + both capture forms per arm) records all of this:

1. **`phase_latency`: `two consecutive settled captures differ: [None, None]`.**
   `make_reference` asked for `acos_observe {tree:false, png:true, screenshot:"<scratch>/ref-N.png"}`.
   Since FB2c (v0.2.2) `screenshot` is `true` or a path **inside the session's artifacts
   directory**; any other path is `INVALID_ARGUMENT` and the reply has **no `result.frame` at
   all**, on both arms (probe: `old_capture_args.ok=false`, `has_frame=false`). Two such replies
   compare as `[None, None]`. The frame identity was never lost: the same launch and two
   frame-only captures all carry `frame.md5 = 7218b3ae...` on both arms. `d2_warm.measure_screenshot`
   (M5) used the same rejected path, which is why M5 was empty too. Fix: `screenshot: true`
   (the daemon reports `screenshot.path`) for M5, and frame-only `acos_observe` for the two
   reference captures.
2. **`phase_cold`: `reference tree is not the count-0 screen: count='0' nodes=14`.**
   P7/P9's rule was `len(nodes) == 12` (`EXPECT_NODES`), a P6 measurement of one ART revision: the
   same verified screen is **14 nodes on both arms** (probe `full_tree.nodes = 14`,
   `count_text = 0`, untruncated). And on rel-030 the launch reply's `tree` is a REL3C **compact
   page** (4 visible-text nodes + `tree_path`), so a node count and count text read from it are a
   page, not the screen. Fix: read the full tree (inline on v0.2.2-, the `tree_path` file on
   rel-030) and check what the rule means: count text 0, the fixture's `count`/`inc`/`reset` nodes
   laid out, an untruncated walk, more than a skeleton. No magic node count.
3. Harness-only defects the spike branch had already wired (now on this branch): `survivors()`
   called an undefined `_environ_has`; `Serve` recorded no pid for the run reaper
   (`_record_daemon`); `phase_loop` returned an unbound `cohort`.
4. Two more silent-rot paths found while fixing (both fail loudly now): `phase_memory` ended on
   `checks["mode_wall_s"] = mode_wall` (copy-pasted from `phase_latency`; a `NameError` **after**
   every sample was taken, so the memory group was rc=1 on every run), and the M8 tap check read
   `settle.state_changed`, which is deliberately `null` in strict mode -- so
   `settle.get("state_changed") is not False` was always true and never checked the verdict. It
   now reads the top-level `result.state_changed` (a required bool).

## Shape-drift guard (acceptance 2)

`tools/bench/bench_reply.py` is the one place that knows how the bench reads a reply, and it raises
`ReplyShapeError` naming the tool, the field and what it saw. `tools/bench/test_bench_reply.py`
(26 tests, `python3 -m unittest discover -s tools/bench -p 'test_*.py'`) fails on the exact shapes
that rotted here:

* a compact page resolved through `tree_path` (and a compact page with **no** `tree_path`, or a
  `tree_path` naming another compact page, refused);
* an inline full tree used as-is (v0.2.2 shape);
* `[None, None]` captures refused, an empty `frame.md5` refused (the pixels hint is in the error);
* a screenshot reply without `screenshot.path` refused;
* a renamed `launched`/missing `settle`/`state_changed` at the wrong level refused;
* P7's stale node count replaced by the semantic screen check (14 nodes still passes; a wrong
  count text, a missing fixture id, a skeleton or a truncated walk is refused).

## Known flakes in this run (reported, not hidden)

One launch out of 60 failed in one rep of each arm: arm A rep 1 `m4-fast-11` GUEST_EXITED ("the
guest ended before its first live frame, exit 1"), arm B rep 3 one NO_FRESH_FRAME. The step is
recorded as a failure (phase `ok=false`, sample not added), the other 59 launches and every other
metric are unaffected. This is the GUEST_EXITED flake already queued in INT-021-AB.

## Gate tables (verbatim from tools/bench/ab-compare.py)

## 1. Old gate -- medians of per-rep p50s (unchanged rule)

| metric | v0.2.2 (tag) | rel-030 HEAD | delta | delta % | threshold | verdict |
|---|---:|---:|---:|---:|---:|---|
| M3 cold start -> first correct screen | 4131.14 | 4255.26 | 124.12 | +3.0% | ±13% / 2.00 | within-noise |
| M4 warm launch (strict) | 304.45 | 286.32 | -18.13 | -6.0% | ±10% / 2.00 | within-noise |
| M4 warm launch (fast) | 168.45 | 149.97 | -18.48 | -11.0% | ±5.00% / 2.00 | improved |
| M5 fresh screenshot (PNG) | 11.83 | 12.02 | 0.19 | +1.6% | ±9% / 2.00 | within-noise |
| M6 reset -> first correct screen (strict) | 343.51 | 331.43 | -12.07 | -3.5% | ±12% / 2.00 | within-noise |
| M6 reset -> first correct screen (fast) | 208.14 | 179.27 | -28.88 | -13.9% | ±14% / 2.00 | within-noise |
| M7 UI tree with text (strict) | 41.48 | 41.46 | -0.02 | -0.1% | ±5.00% / 2.00 | within-noise |
| M7 UI tree with text (fast) | 0.59 | 0.55 | -0.04 | -6.8% | ±40.00% / 2.00 | within-noise |
| M8 tap -> settled, counter (strict) | 949.48 | 949.30 | -0.18 | -0.0% | ±5.00% / 2.00 | within-noise |
| M8 tap -> settled, counter (fast) | 48.86 | 48.12 | -0.73 | -1.5% | ±40.00% / 2.00 | within-noise |
| M8 tap -> settled, form (strict) | 934.11 | 928.40 | -5.70 | -0.6% | ±5.00% / 2.00 | within-noise |
| M8 tap -> settled, form (fast) | 32.46 | 34.35 | 1.89 | +5.8% | ±14% / 2.00 | within-noise |
| tap acknowledgement | 5.00 | 5.00 | 0.00 | +0.0% | ±5.00% / 2.00 | within-noise |
| acos_destroy teardown_ms | 20.25 | 21.00 | 0.75 | +3.7% | ±10% / 2.00 | within-noise |
| cpu_active_tap_strict_pct | 44.55 | 43.04 | -1.51 | -3.4% | ±10.00% / 0.20 | within-noise |
| cpu_idle_n10_pct | 3.89 | 4.01 | 0.12 | +3.0% | ±19% / 0.20 | within-noise |
| cpu_idle_n10_per_session_pct | 0.39 | 0.40 | 0.01 | +3.1% | ±19% / 0.20 | within-noise |
| cpu_idle_n1_pct | 0.68 | 0.73 | 0.05 | +7.4% | ±40.00% / 0.20 | within-noise |
| cpu_idle_n1_per_session_pct | 0.68 | 0.73 | 0.05 | +7.4% | ±40.00% / 0.20 | within-noise |
| cpu_idle_n5_pct | 2.37 | 2.44 | 0.07 | +2.9% | ±37% / 0.20 | within-noise |
| cpu_idle_n5_per_session_pct | 0.47 | 0.49 | 0.01 | +2.7% | ±37% / 0.20 | within-noise |
| m1_idle_mem_mib | 55.23 | 56.98 | 1.76 | +3.2% | ±17% / 0.50 | within-noise |
| m1_idle_pss_mib | 223.73 | 230.78 | 7.05 | +3.2% | ±6% / 0.50 | within-noise |
| m2_per_session_anon_shmem_n1 | 53.60 | 55.34 | 1.74 | +3.2% | ±18% / 0.50 | within-noise |
| m2_per_session_anon_shmem_n10 | 53.65 | 55.77 | 2.11 | +3.9% | ±5% / 0.50 | within-noise |
| m2_per_session_anon_shmem_n5 | 53.94 | 55.27 | 1.33 | +2.5% | ±4% / 0.50 | within-noise |
| m2_per_session_pss_n1 | 207.39 | 213.80 | 6.41 | +3.1% | ±6% / 0.50 | within-noise |
| m2_per_session_pss_n10 | 52.17 | 54.42 | 2.25 | +4.3% | ±9% / 0.50 | within-noise |
| m2_per_session_pss_n5 | 70.68 | 71.93 | 1.25 | +1.8% | ±5% / 0.50 | within-noise |
| teardown_wall_ms | 20.87 | 21.91 | 1.03 | +5.0% | ±11% / 2.00 | within-noise |
| wakeups_idle_n10_per_s | 2.98 | 2.99 | 0.01 | +0.4% | ±10.00% / 0.20 | within-noise |
| wakeups_idle_n1_per_s | 1.33 | 1.20 | -0.13 | -10.0% | ±40.00% / 0.20 | within-noise |
| wakeups_idle_n5_per_s | 1.99 | 2.00 | 0.01 | +0.3% | ±10.00% / 0.20 | within-noise |

## 2. Pooled 5 %-trimmed mean, and 3. frame-slot shares

| metric | n v0.2.2 (tag)/rel-030 HEAD | v0.2.2 (tag) trimmed | rel-030 HEAD trimmed | delta | delta % | frame ms | first slot v0.2.2 (tag)/rel-030 HEAD | slot shares v0.2.2 (tag) | slot shares rel-030 HEAD | verdict |
|---|---:|---:|---:|---:|---:|---:|---:|---|---|---|---|
| M4 warm launch (fast) | 119/119 | 163.96 | 147.98 | -15.99 | -9.8% | 16.67 | 0%/29% | 1:36% 2:55% 3:8% | 0:29% 1:55% 2:13% 3:2% | improved (slot-shift +29 pp) |
| M6 reset -> first correct screen (fast) | 120/120 | 204.40 | 181.29 | -23.11 | -11.3% | 16.67 | 0%/16% | 1:9% 2:18% 3:55% 4:13% 7:1% 9:2% 12:1% 29:1% | 0:16% 1:30% 2:42% 3:8% 4:2% 10:1% 31:1% | improved (slot-shift +16 pp) |
| M8 tap -> settled, counter (fast) | 120/120 | 42.16 | 41.12 | -1.03 | -2.5% | 16.67 | 17%/18% | 0:17% 1:16% 2:62% 3:5% | 0:18% 1:23% 2:52% 3:7% | within-noise |
| M8 tap -> settled, form (fast) | 120/120 | 32.74 | 36.38 | +3.64 | +11.1% | 16.67 | 98%/85% | 0:98% 1:2% | 0:85% 1:8% 2:2% 3:2% 4:1% 10:1% 13:1% 14:1% | REGRESSION (slot-shift -13 pp) |

## 4. Verdict

Lattice gate (proposal): pooled 5 percent-trimmed mean worse by > 5 percent and > 2 ms; a first-slot share shift >= 10 pp is flagged separately.

* M4 warm launch (fast): improved -- trimmed 163.96 vs 147.98 (-15.99 ms, -9.8%), first-slot 0% vs 29% (SLOT-SHIFT +29 pp)
* M6 reset -> first correct screen (fast): improved -- trimmed 204.40 vs 181.29 (-23.11 ms, -11.3%), first-slot 0% vs 16% (SLOT-SHIFT +16 pp)
* M8 tap -> settled, counter (fast): within-noise -- trimmed 42.16 vs 41.12 (-1.03 ms, -2.5%), first-slot 17% vs 18%
* M8 tap -> settled, form (fast): REGRESSION -- trimmed 32.74 vs 36.38 (+3.64 ms, +11.1%), first-slot 98% vs 85% (SLOT-SHIFT -13 pp)
Lattice regressions: M8 tap -> settled, form (fast)

JSON: results/bench/BENCHFIX-AB/ab-compare.json
