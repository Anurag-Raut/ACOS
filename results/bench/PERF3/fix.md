# ACOS bench baseline -- 2026-11-16T06:48:37+00:00

Command: `tools/bench/acos-bench.sh --reps 3` (branch `perf3`, commit `9d339cc76355`).

Mode `full`; `ACOS_STRICT_SETTLE=default`; `animations=on`; binary sha256 ``.

Host: AMD Ryzen 9 9950X, 32 threads, 61908 MiB. Noisy phases: 2/3 (load1 > 4 or MemAvailable < 20 GiB; recorded, not deleted).

## Metrics (p50 / p95, pooled across reps)

| metric | label | p50 | p95 | n | threshold | prev best | method |
|---|---|---:|---:|---:|---:|---|---|
| `m4_launch_strict_ms` | M4 warm launch (strict) | 287.5 ms | 379.2 | 60 | ±15% / 2.000 | 440 (P9) | warm daemon, app not running -> acos_launch settle=strict -> reply tree count=0 + post-frame strict tree count |
| `m4_launch_fast_ms` | M4 warm launch (fast) | 150.1 ms | 167.7 | 120 | ±20% / 2.000 | 184 (P9) | acos_launch settle=fast (probe-fresh label recorded) -> reply tree count=0 + post-frame strict tree count=0 |
| `m5_screenshot_ms` | M5 fresh screenshot (PNG) | 12.35 ms | 14.71 | 60 | ±17% / 2.000 | 24.4 (P2/W1) | acos_observe {png:true} on a held session; PNG magic + newer frame_seq + meaningful frame required |
| `m6_reset_strict_ms` | M6 reset -> first correct screen (strict) | 327.6 ms | 503.5 | 60 | ±6% / 2.000 | 463 (P9) | count=1 strictly read -> acos_reset settle=strict -> reply tree count=0 + following strict tree count=0 |
| `m6_reset_fast_ms` | M6 reset -> first correct screen (fast) | 184.2 ms | 348.6 | 120 | ±12% / 2.000 | 195 (P9) | acos_reset settle=fast; state_reset + data_cleared + count=0 in the reply and in a following strict tree |
| `m7_tree_strict_ms` | M7 UI tree with text (strict) | 41.53 ms | 41.82 | 60 | ±5.0% / 2.000 | 151 (P10) | acos_observe_tree settle=strict on a held session; tree has nodes and text, not truncated |
| `m7_tree_fast_ms` | M7 UI tree with text (fast) | 0.468 ms | 0.696 | 60 | ±9% / 2.000 | 0.2 (P10) | acos_observe_tree settle=fast; the fast tree must show the same count text as a strict read (no stale tree) |
| `m8_tap_counter_strict_ms` | M8 tap -> settled, counter (strict) | 949.6 ms | 959.2 | 60 | ±5.0% / 2.000 | 955 (P11 event) | acos_tap {id:inc} settle=strict on counter, one session; every tap must report state_changed and the final str |
| `m8_tap_counter_fast_ms` | M8 tap -> settled, counter (fast) | 47.54 ms | 52.56 | 120 | ±40.0% / 2.000 | 70.9 (P1) | acos_tap settle=fast; same state_changed/final-count checks |
| `m8_tap_form_strict_ms` | M8 tap -> settled, form (strict) | 932.0 ms | 940.8 | 60 | ±5.0% / 2.000 | - | empty form, tap submit settle=strict; following strict tree must read error='Name is required'; fast reset (un |
| `m8_tap_form_fast_ms` | M8 tap -> settled, form (fast) | 33.43 ms | 35.92 | 120 | ±10% / 2.000 | - | empty form, tap submit settle=fast; same strict post-check |
| `tap_ack_ms` | tap acknowledgement (settle.ack_ms) | 5.000 ms | 5.000 | 360 | ±5.0% / 2.000 | 5 (P1) | input ack latency reported in the tap reply's settle block (counter + form, strict and fast) |
| `teardown_ms` | acos_destroy teardown_ms (GL1) | 22.00 ms | 33.90 | 192 | ±28% / 2.000 | 18-20 (GL1) | reply teardown.teardown_ms for a live counter session (lifecycle label recorded); 20 samples |
| `teardown_wall_ms` | acos_destroy wall | 22.79 ms | 34.74 | 192 | ±26% / 2.000 | - | client wall of acos_destroy incl. teardown evidence |
| `cpu_active_tap_strict_pct` | active CPU, 20 strict counter taps | 41.89 % of one core | 48.86 | 3 | ±37% / 0.200 | - | utime+stime delta over the serve tree during the M8 strict counter cohort (20 taps, ~20 s) |

## Fast-settle lattice (PERF-REL3)

Every fast-settle sample waits for the compositor, so it lands on a frame-period lattice: a rep's p50 is whichever slot holds the middle sample. These numbers do not flip.

| metric | n | frame ms (source) | median | 5%-trimmed mean | first-slot share | slot shares |
|---|---:|---:|---:|---:|---:|---|
| `m4_launch_fast_ms` | 120 | 16.67 (fallback) | 150.1 | 149.0 | 35% | 0:35%, 1:36%, 2:28%, 5:1%, 7:1% |
| `m6_reset_fast_ms` | 120 | 16.67 (fallback) | 184.2 | 200.3 | 21% | 0:21%, 1:31%, 2:26%, 3:3%, 5:1%, 6:3%, 7:2%, 8:1%, 9:2%, 10:2%, 11:4%, 13:2%, 14:1%, 18:1%, 19:1%, 23:1% |
| `m8_tap_counter_fast_ms` | 120 | 16.67 (fallback) | 47.54 | 39.93 | 17% | 0:17%, 1:31%, 2:48%, 3:5% |
| `m8_tap_form_fast_ms` | 120 | 16.67 (fallback) | 33.43 | 33.75 | 99% | 0:99%, 2:1% |

## Phases and host load

| rep | phase | ok | load1 before -> after | MemAvailable | queue wait s | wall s | noisy |
|---:|---|---|---:|---:|---:|---:|---|
| 1 | latency | yes | 4.380 -> 4.380 | 45.96 GiB | - | 198.8 | yes |
| 2 | latency | yes | 6.940 -> 6.940 | 45.95 GiB | - | 198.4 | yes |
| 3 | latency | yes | 3.800 -> 3.800 | 46.27 GiB | - | 202.1 | no |

Noisy phases (recorded): latency/rep1, latency/rep2

## Notes

* value = p50 pooled over every sample in every rep; p95 pooled the same way; per-rep p50s are kept for the threshold derivation.
* threshold = max(category floor, 2 x worst rep-to-rep deviation), capped at 40 %; a delta must exceed both the relative and the absolute threshold to be called a regression.
* memory rows are label-matched (cgroup anon+shmem vs Pss); they are never mixed.
* M3 is measured with the launch working set evicted per rep; the warm-cache variant is M4 + daemon spawn (P7).
* PERF-REL3: every metric now carries its raw `samples`; the fast-settle metrics (M4 launch fast, M6 reset fast, M8 tap fast) also carry `lattice`: a 5 %-trimmed mean, the frame period measured from the samples (phase coherence), and the share of samples per frame slot (slot = round((x - origin) / period), origin = the fastest observed sample). p50/p95 and the p50 compare gate are unchanged; the lattice fields are additive.
