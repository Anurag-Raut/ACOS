# ACOS bench baseline -- 2026-11-18T05:23:30+00:00

Command: `tools/bench/acos-bench.sh --reps 5` (branch `backsettle`, commit `5f6553be05b2`).

Mode `full`; `ACOS_STRICT_SETTLE=default`; `animations=on`; binary sha256 `09866ce283562233`.

Host: AMD Ryzen 9 9950X, 32 threads, 61908 MiB. Noisy phases: 4/5 (load1 > 4 or MemAvailable < 20 GiB; recorded, not deleted).

## Metrics (p50 / p95, pooled across reps)

| metric | label | p50 | p95 | n | threshold | prev best | method |
|---|---|---:|---:|---:|---:|---|---|
| `m4_launch_strict_ms` | M4 warm launch (strict) | 298.8 ms | 312.7 | 100 | ±8% / 2.000 | 440 (P9) | warm daemon, app not running -> acos_launch settle=strict -> reply tree count=0 + post-frame strict tree count |
| `m4_launch_fast_ms` | M4 warm launch (fast) | 151.6 ms | 169.6 | 200 | ±5.0% / 2.000 | 184 (P9) | acos_launch settle=fast (probe-fresh label recorded) -> reply tree count=0 + post-frame strict tree count=0 |
| `m5_screenshot_ms` | M5 fresh screenshot (PNG) | 12.18 ms | 14.69 | 100 | ±11% / 2.000 | 24.4 (P2/W1) | acos_observe {png:true} on a held session; PNG magic + newer frame_seq + meaningful frame required |
| `m6_reset_strict_ms` | M6 reset -> first correct screen (strict) | 318.9 ms | 338.7 | 100 | ±9% / 2.000 | 463 (P9) | count=1 strictly read -> acos_reset settle=strict -> reply tree count=0 + following strict tree count=0 |
| `m6_reset_fast_ms` | M6 reset -> first correct screen (fast) | 179.7 ms | 236.2 | 200 | ±20% / 2.000 | 195 (P9) | acos_reset settle=fast; state_reset + data_cleared + count=0 in the reply and in a following strict tree |
| `m7_tree_strict_ms` | M7 UI tree with text (strict) | 41.54 ms | 42.04 | 100 | ±5.0% / 2.000 | 151 (P10) | acos_observe_tree settle=strict on a held session; tree has nodes and text, not truncated |
| `m7_tree_fast_ms` | M7 UI tree with text (fast) | 0.618 ms | 0.989 | 100 | ±38% / 2.000 | 0.2 (P10) | acos_observe_tree settle=fast; the fast tree must show the same count text as a strict read (no stale tree) |
| `m8_tap_counter_strict_ms` | M8 tap -> settled, counter (strict) | 947.9 ms | 959.1 | 100 | ±5.0% / 2.000 | 955 (P11 event) | acos_tap {id:inc} settle=strict on counter, one session; every tap must report state_changed and the final str |
| `m8_tap_counter_fast_ms` | M8 tap -> settled, counter (fast) | 31.15 ms | 50.20 | 200 | ±40.0% / 2.000 | 70.9 (P1) | acos_tap settle=fast; same state_changed/final-count checks |
| `m8_tap_form_strict_ms` | M8 tap -> settled, form (strict) | 930.3 ms | 940.9 | 100 | ±5.0% / 2.000 | - | empty form, tap submit settle=strict; following strict tree must read error='Name is required'; fast reset (un |
| `m8_tap_form_fast_ms` | M8 tap -> settled, form (fast) | 28.23 ms | 32.28 | 200 | ±5.0% / 2.000 | - | empty form, tap submit settle=fast; same strict post-check |
| `tap_ack_ms` | tap acknowledgement (settle.ack_ms) | 5.000 ms | 5.000 | 600 | ±5.0% / 2.000 | 5 (P1) | input ack latency reported in the tap reply's settle block (counter + form, strict and fast) |
| `teardown_ms` | acos_destroy teardown_ms (GL1) | 22.00 ms | 26.00 | 320 | ±19% / 2.000 | 18-20 (GL1) | reply teardown.teardown_ms for a live counter session (lifecycle label recorded); 20 samples |
| `teardown_wall_ms` | acos_destroy wall | 22.82 ms | 26.62 | 320 | ±19% / 2.000 | - | client wall of acos_destroy incl. teardown evidence |
| `cpu_active_tap_strict_pct` | active CPU, 20 strict counter taps | 43.11 % of one core | 52.42 | 5 | ±40.0% / 0.200 | - | utime+stime delta over the serve tree during the M8 strict counter cohort (20 taps, ~20 s) |

## Fast-settle lattice (PERF-REL3)

Every fast-settle sample waits for the compositor, so it lands on a frame-period lattice: a rep's p50 is whichever slot holds the middle sample. These numbers do not flip.

| metric | n | frame ms (source) | median | 5%-trimmed mean | first-slot share | slot shares |
|---|---:|---:|---:|---:|---:|---|
| `m4_launch_fast_ms` | 200 | 16.67 (fallback) | 151.6 | 154.2 | 12% | 0:12%, 1:56%, 2:30%, 3:2%, 4:1% |
| `m6_reset_fast_ms` | 200 | 16.67 (fallback) | 179.7 | 180.8 | 26% | 0:26%, 1:28%, 2:35%, 3:4%, 4:2%, 5:2%, 6:1%, 7:1%, 8:0%, 9:0%, 10:0%, 11:0%, 13:0% |
| `m8_tap_counter_fast_ms` | 200 | 16.67 (fallback) | 31.15 | 27.38 | 40% | 0:40%, 1:50%, 2:10%, 3:0% |
| `m8_tap_form_fast_ms` | 200 | 16.67 (fallback) | 28.23 | 28.58 | 96% | 0:96%, 1:4% |

## Phases and host load

| rep | phase | ok | load1 before -> after | MemAvailable | queue wait s | wall s | noisy |
|---:|---|---|---:|---:|---:|---:|---|
| 1 | latency | yes | 2.870 -> 2.870 | 48.95 GiB | - | 196.4 | no |
| 2 | latency | yes | 5.820 -> 5.820 | 49.88 GiB | - | 196.7 | yes |
| 3 | latency | yes | 10.69 -> 10.69 | 42.40 GiB | - | 200.8 | yes |
| 4 | latency | yes | 9.970 -> 9.970 | 45.15 GiB | - | 197.9 | yes |
| 5 | latency | yes | 5.500 -> 5.500 | 44.54 GiB | - | 196.7 | yes |

Noisy phases (recorded): latency/rep2, latency/rep3, latency/rep4, latency/rep5

## Notes

* value = p50 pooled over every sample in every rep; p95 pooled the same way; per-rep p50s are kept for the threshold derivation.
* threshold = max(category floor, 2 x worst rep-to-rep deviation), capped at 40 %; a delta must exceed both the relative and the absolute threshold to be called a regression.
* memory rows are label-matched (cgroup anon+shmem vs Pss); they are never mixed.
* M3 is measured with the launch working set evicted per rep; the warm-cache variant is M4 + daemon spawn (P7).
* PERF-REL3: every metric now carries its raw `samples`; the fast-settle metrics (M4 launch fast, M6 reset fast, M8 tap fast) also carry `lattice`: a 5 %-trimmed mean, the frame period measured from the samples (phase coherence), and the share of samples per frame slot (slot = round((x - origin) / period), origin = the fastest observed sample). p50/p95 and the p50 compare gate are unchanged; the lattice fields are additive.
