# ACOS bench baseline -- 2026-11-18T05:23:28+00:00

Command: `tools/bench/acos-bench.sh --reps 5` (branch `backsettle`, commit `5f6553be05b2`).

Mode `full`; `ACOS_STRICT_SETTLE=default`; `animations=on`; binary sha256 `17da6bd331e2276d`.

Host: AMD Ryzen 9 9950X, 32 threads, 61908 MiB. Noisy phases: 1/5 (load1 > 4 or MemAvailable < 20 GiB; recorded, not deleted).

## Metrics (p50 / p95, pooled across reps)

| metric | label | p50 | p95 | n | threshold | prev best | method |
|---|---|---:|---:|---:|---:|---|---|
| `m4_launch_strict_ms` | M4 warm launch (strict) | 300.5 ms | 362.0 | 100 | ±6% / 2.000 | 440 (P9) | warm daemon, app not running -> acos_launch settle=strict -> reply tree count=0 + post-frame strict tree count |
| `m4_launch_fast_ms` | M4 warm launch (fast) | 162.1 ms | 224.4 | 200 | ±13% / 2.000 | 184 (P9) | acos_launch settle=fast (probe-fresh label recorded) -> reply tree count=0 + post-frame strict tree count=0 |
| `m5_screenshot_ms` | M5 fresh screenshot (PNG) | 12.10 ms | 13.05 | 100 | ±10% / 2.000 | 24.4 (P2/W1) | acos_observe {png:true} on a held session; PNG magic + newer frame_seq + meaningful frame required |
| `m6_reset_strict_ms` | M6 reset -> first correct screen (strict) | 319.2 ms | 342.4 | 100 | ±9% / 2.000 | 463 (P9) | count=1 strictly read -> acos_reset settle=strict -> reply tree count=0 + following strict tree count=0 |
| `m6_reset_fast_ms` | M6 reset -> first correct screen (fast) | 178.9 ms | 213.6 | 200 | ±21% / 2.000 | 195 (P9) | acos_reset settle=fast; state_reset + data_cleared + count=0 in the reply and in a following strict tree |
| `m7_tree_strict_ms` | M7 UI tree with text (strict) | 41.46 ms | 42.00 | 100 | ±5.0% / 2.000 | 151 (P10) | acos_observe_tree settle=strict on a held session; tree has nodes and text, not truncated |
| `m7_tree_fast_ms` | M7 UI tree with text (fast) | 0.526 ms | 0.852 | 100 | ±38% / 2.000 | 0.2 (P10) | acos_observe_tree settle=fast; the fast tree must show the same count text as a strict read (no stale tree) |
| `m8_tap_counter_strict_ms` | M8 tap -> settled, counter (strict) | 946.9 ms | 958.2 | 100 | ±5.0% / 2.000 | 955 (P11 event) | acos_tap {id:inc} settle=strict on counter, one session; every tap must report state_changed and the final str |
| `m8_tap_counter_fast_ms` | M8 tap -> settled, counter (fast) | 29.74 ms | 48.45 | 200 | ±40.0% / 2.000 | 70.9 (P1) | acos_tap settle=fast; same state_changed/final-count checks |
| `m8_tap_form_strict_ms` | M8 tap -> settled, form (strict) | 931.3 ms | 954.5 | 100 | ±5.0% / 2.000 | - | empty form, tap submit settle=strict; following strict tree must read error='Name is required'; fast reset (un |
| `m8_tap_form_fast_ms` | M8 tap -> settled, form (fast) | 28.30 ms | 31.52 | 200 | ±7% / 2.000 | - | empty form, tap submit settle=fast; same strict post-check |
| `tap_ack_ms` | tap acknowledgement (settle.ack_ms) | 5.000 ms | 5.000 | 600 | ±5.0% / 2.000 | 5 (P1) | input ack latency reported in the tap reply's settle block (counter + form, strict and fast) |
| `teardown_ms` | acos_destroy teardown_ms (GL1) | 22.00 ms | 38.00 | 320 | ±19% / 2.000 | 18-20 (GL1) | reply teardown.teardown_ms for a live counter session (lifecycle label recorded); 20 samples |
| `teardown_wall_ms` | acos_destroy wall | 23.26 ms | 39.32 | 320 | ±18% / 2.000 | - | client wall of acos_destroy incl. teardown evidence |
| `cpu_active_tap_strict_pct` | active CPU, 20 strict counter taps | 42.91 % of one core | 47.10 | 5 | ±27% / 0.200 | - | utime+stime delta over the serve tree during the M8 strict counter cohort (20 taps, ~20 s) |

## Fast-settle lattice (PERF-REL3)

Every fast-settle sample waits for the compositor, so it lands on a frame-period lattice: a rep's p50 is whichever slot holds the middle sample. These numbers do not flip.

| metric | n | frame ms (source) | median | 5%-trimmed mean | first-slot share | slot shares |
|---|---:|---:|---:|---:|---:|---|
| `m4_launch_fast_ms` | 200 | 16.67 (fallback) | 162.1 | 161.0 | 10% | 0:10%, 1:36%, 2:41%, 3:5%, 4:2%, 5:1%, 6:2%, 7:2%, 8:0%, 9:0%, 10:0% |
| `m6_reset_fast_ms` | 200 | 16.67 (fallback) | 178.9 | 180.3 | 20% | 0:20%, 1:38%, 2:34%, 3:2%, 5:1%, 7:1%, 8:1%, 9:2%, 10:0% |
| `m8_tap_counter_fast_ms` | 200 | 16.67 (fallback) | 29.74 | 26.48 | 45% | 0:45%, 1:46%, 2:9% |
| `m8_tap_form_fast_ms` | 200 | 16.67 (fallback) | 28.30 | 28.61 | 96% | 0:96%, 1:1%, 2:2%, 3:2% |

## Phases and host load

| rep | phase | ok | load1 before -> after | MemAvailable | queue wait s | wall s | noisy |
|---:|---|---|---:|---:|---:|---:|---|
| 1 | latency | yes | 4.920 -> 4.920 | 50.30 GiB | - | 203.4 | yes |
| 2 | latency | yes | 2.860 -> 2.860 | 50.18 GiB | - | 198.7 | no |
| 3 | latency | yes | 3.200 -> 3.200 | 48.40 GiB | - | 198.8 | no |
| 4 | latency | yes | 3.120 -> 3.120 | 44.36 GiB | - | 195.6 | no |
| 5 | latency | yes | 3.250 -> 3.250 | 46.12 GiB | - | 203.5 | no |

Noisy phases (recorded): latency/rep1

## Notes

* value = p50 pooled over every sample in every rep; p95 pooled the same way; per-rep p50s are kept for the threshold derivation.
* threshold = max(category floor, 2 x worst rep-to-rep deviation), capped at 40 %; a delta must exceed both the relative and the absolute threshold to be called a regression.
* memory rows are label-matched (cgroup anon+shmem vs Pss); they are never mixed.
* M3 is measured with the launch working set evicted per rep; the warm-cache variant is M4 + daemon spawn (P7).
* PERF-REL3: every metric now carries its raw `samples`; the fast-settle metrics (M4 launch fast, M6 reset fast, M8 tap fast) also carry `lattice`: a 5 %-trimmed mean, the frame period measured from the samples (phase coherence), and the share of samples per frame slot (slot = round((x - origin) / period), origin = the fastest observed sample). p50/p95 and the p50 compare gate are unchanged; the lattice fields are additive.
