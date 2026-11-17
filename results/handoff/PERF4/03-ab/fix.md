# ACOS bench baseline -- 2026-11-17T07:29:47+00:00

Command: `tools/bench/acos-bench.sh --reps 5` (branch `perf4`, commit `0a088f6739aa`).

Mode `full`; `ACOS_STRICT_SETTLE=default`; `animations=on`; binary sha256 `1272ef7a72b56629`.

Host: AMD Ryzen 9 9950X, 32 threads, 61908 MiB. Noisy phases: 0/5 (load1 > 4 or MemAvailable < 20 GiB; recorded, not deleted).

## Metrics (p50 / p95, pooled across reps)

| metric | label | p50 | p95 | n | threshold | prev best | method |
|---|---|---:|---:|---:|---:|---|---|
| `m4_launch_strict_ms` | M4 warm launch (strict) | 279.1 ms | 296.2 | 100 | ±5.0% / 2.000 | 440 (P9) | warm daemon, app not running -> acos_launch settle=strict -> reply tree count=0 + post-frame strict tree count |
| `m4_launch_fast_ms` | M4 warm launch (fast) | 143.5 ms | 160.6 | 200 | ±5.0% / 2.000 | 184 (P9) | acos_launch settle=fast (probe-fresh label recorded) -> reply tree count=0 + post-frame strict tree count=0 |
| `m5_screenshot_ms` | M5 fresh screenshot (PNG) | 12.04 ms | 14.84 | 100 | ±10% / 2.000 | 24.4 (P2/W1) | acos_observe {png:true} on a held session; PNG magic + newer frame_seq + meaningful frame required |
| `m6_reset_strict_ms` | M6 reset -> first correct screen (strict) | 317.2 ms | 333.2 | 100 | ±6% / 2.000 | 463 (P9) | count=1 strictly read -> acos_reset settle=strict -> reply tree count=0 + following strict tree count=0 |
| `m6_reset_fast_ms` | M6 reset -> first correct screen (fast) | 188.7 ms | 198.0 | 200 | ±9% / 2.000 | 195 (P9) | acos_reset settle=fast; state_reset + data_cleared + count=0 in the reply and in a following strict tree |
| `m7_tree_strict_ms` | M7 UI tree with text (strict) | 31.87 ms | 32.29 | 100 | ±5.0% / 2.000 | 151 (P10) | acos_observe_tree settle=strict on a held session; tree has nodes and text, not truncated |
| `m7_tree_fast_ms` | M7 UI tree with text (fast) | 0.562 ms | 0.890 | 100 | ±27% / 2.000 | 0.2 (P10) | acos_observe_tree settle=fast; the fast tree must show the same count text as a strict read (no stale tree) |
| `m8_tap_counter_strict_ms` | M8 tap -> settled, counter (strict) | 948.9 ms | 951.2 | 100 | ±6% / 2.000 | 955 (P11 event) | acos_tap {id:inc} settle=strict on counter, one session; every tap must report state_changed and the final str |
| `m8_tap_counter_fast_ms` | M8 tap -> settled, counter (fast) | 24.50 ms | 33.52 | 200 | ±5.0% / 2.000 | 70.9 (P1) | acos_tap settle=fast; same state_changed/final-count checks |
| `m8_tap_form_strict_ms` | M8 tap -> settled, form (strict) | 931.2 ms | 933.4 | 100 | ±5.0% / 2.000 | - | empty form, tap submit settle=strict; following strict tree must read error='Name is required'; fast reset (un |
| `m8_tap_form_fast_ms` | M8 tap -> settled, form (fast) | 30.09 ms | 31.44 | 200 | ±14% / 2.000 | - | empty form, tap submit settle=fast; same strict post-check |
| `tap_ack_ms` | tap acknowledgement (settle.ack_ms) | 5.000 ms | 5.000 | 600 | ±5.0% / 2.000 | 5 (P1) | input ack latency reported in the tap reply's settle block (counter + form, strict and fast) |
| `teardown_ms` | acos_destroy teardown_ms (GL1) | 21.00 ms | 23.00 | 320 | ±10% / 2.000 | 18-20 (GL1) | reply teardown.teardown_ms for a live counter session (lifecycle label recorded); 20 samples |
| `teardown_wall_ms` | acos_destroy wall | 21.66 ms | 23.90 | 320 | ±7% / 2.000 | - | client wall of acos_destroy incl. teardown evidence |
| `cpu_active_tap_strict_pct` | active CPU, 20 strict counter taps | 65.81 % of one core | 66.36 | 5 | ±10.0% / 0.200 | - | utime+stime delta over the serve tree during the M8 strict counter cohort (20 taps, ~20 s) |

## Fast-settle lattice (PERF-REL3)

Every fast-settle sample waits for the compositor, so it lands on a frame-period lattice: a rep's p50 is whichever slot holds the middle sample. These numbers do not flip.

| metric | n | frame ms (source) | median | 5%-trimmed mean | first-slot share | slot shares |
|---|---:|---:|---:|---:|---:|---|
| `m4_launch_fast_ms` | 200 | 16.45 (measured) | 143.5 | 145.1 | 4% | 0:4%, 1:80%, 2:16% |
| `m6_reset_fast_ms` | 200 | 8.42 (measured) | 188.7 | 183.8 | 2% | 0:2%, 1:11%, 2:10%, 3:26%, 4:40%, 5:10%, 6:1% |
| `m8_tap_counter_fast_ms` | 200 | 15.36 (measured) | 24.50 | 24.85 | 50% | 0:50%, 1:50% |
| `m8_tap_form_fast_ms` | 200 | 16.67 (fallback) | 30.09 | 29.72 | 100% | 0:100%, 1:0% |

## Phases and host load

| rep | phase | ok | load1 before -> after | MemAvailable | queue wait s | wall s | noisy |
|---:|---|---|---:|---:|---:|---:|---|
| 1 | latency | yes | 1.090 -> 1.090 | 43.46 GiB | - | 195.1 | no |
| 2 | latency | yes | 0.450 -> 0.450 | 42.27 GiB | - | 194.4 | no |
| 3 | latency | yes | 0.990 -> 0.990 | 41.84 GiB | - | 193.4 | no |
| 4 | latency | yes | 0.470 -> 0.470 | 43.32 GiB | - | 194.4 | no |
| 5 | latency | yes | 1.600 -> 1.600 | 41.80 GiB | - | 193.7 | no |

## Notes

* value = p50 pooled over every sample in every rep; p95 pooled the same way; per-rep p50s are kept for the threshold derivation.
* threshold = max(category floor, 2 x worst rep-to-rep deviation), capped at 40 %; a delta must exceed both the relative and the absolute threshold to be called a regression.
* memory rows are label-matched (cgroup anon+shmem vs Pss); they are never mixed.
* M3 is measured with the launch working set evicted per rep; the warm-cache variant is M4 + daemon spawn (P7).
* PERF-REL3: every metric now carries its raw `samples`; the fast-settle metrics (M4 launch fast, M6 reset fast, M8 tap fast) also carry `lattice`: a 5 %-trimmed mean, the frame period measured from the samples (phase coherence), and the share of samples per frame slot (slot = round((x - origin) / period), origin = the fastest observed sample). p50/p95 and the p50 compare gate are unchanged; the lattice fields are additive.
