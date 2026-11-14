# ACOS bench baseline -- 2026-11-14T18:32:02+00:00

Command: `tools/bench/acos-bench.sh --reps 1` (branch `HEAD`, commit `19db6b8a656c`).

Mode `full`; `ACOS_STRICT_SETTLE=default`; `animations=on`; binary sha256 `90b888d3f43e2bde`.

Host: AMD Ryzen 9 9950X, 32 threads, 61908 MiB. Noisy phases: 0/1 (load1 > 4 or MemAvailable < 20 GiB; recorded, not deleted).

## Metrics (p50 / p95, pooled across reps)

| metric | label | p50 | p95 | n | threshold | prev best | method |
|---|---|---:|---:|---:|---:|---|---|
| `m4_launch_strict_ms` | M4 warm launch (strict) | 305.3 ms | 307.1 | 20 | ±5.0% / 2.000 | 440 (P9) | warm daemon, app not running -> acos_launch settle=strict -> reply tree count=0 + post-frame strict tree count |
| `m4_launch_fast_ms` | M4 warm launch (fast) | 154.7 ms | 170.5 | 20 | ±5.0% / 2.000 | 184 (P9) | acos_launch settle=fast (probe-fresh label recorded) -> reply tree count=0 + post-frame strict tree count=0 |
| `m5_screenshot_ms` | M5 fresh screenshot (PNG) | 29.42 ms | 32.07 | 20 | ±5.0% / 2.000 | 24.4 (P2/W1) | acos_observe {png:true} on a held session; PNG magic + newer frame_seq + meaningful frame required |
| `m6_reset_strict_ms` | M6 reset -> first correct screen (strict) | 339.8 ms | 349.9 | 20 | ±5.0% / 2.000 | 463 (P9) | count=1 strictly read -> acos_reset settle=strict -> reply tree count=0 + following strict tree count=0 |
| `m6_reset_fast_ms` | M6 reset -> first correct screen (fast) | 198.5 ms | 212.3 | 20 | ±5.0% / 2.000 | 195 (P9) | acos_reset settle=fast; state_reset + data_cleared + count=0 in the reply and in a following strict tree |
| `m7_tree_strict_ms` | M7 UI tree with text (strict) | 41.39 ms | 41.56 | 20 | ±5.0% / 2.000 | 151 (P10) | acos_observe_tree settle=strict on a held session; tree has nodes and text, not truncated |
| `m7_tree_fast_ms` | M7 UI tree with text (fast) | 0.524 ms | 0.678 | 20 | ±5.0% / 2.000 | 0.2 (P10) | acos_observe_tree settle=fast; the fast tree must show the same count text as a strict read (no stale tree) |
| `m8_tap_counter_strict_ms` | M8 tap -> settled, counter (strict) | 950.3 ms | 970.9 | 20 | ±5.0% / 2.000 | 955 (P11 event) | acos_tap {id:inc} settle=strict on counter, one session; every tap must report state_changed and the final str |
| `m8_tap_counter_fast_ms` | M8 tap -> settled, counter (fast) | 47.73 ms | 74.31 | 20 | ±5.0% / 2.000 | 70.9 (P1) | acos_tap settle=fast; same state_changed/final-count checks |
| `m8_tap_form_strict_ms` | M8 tap -> settled, form (strict) | 949.2 ms | 964.3 | 20 | ±5.0% / 2.000 | - | empty form, tap submit settle=strict; following strict tree must read error='Name is required'; fast reset (un |
| `m8_tap_form_fast_ms` | M8 tap -> settled, form (fast) | 46.14 ms | 48.55 | 20 | ±5.0% / 2.000 | - | empty form, tap submit settle=fast; same strict post-check |
| `tap_ack_ms` | tap acknowledgement (settle.ack_ms) | 5.000 ms | 5.000 | 80 | ±5.0% / 2.000 | 5 (P1) | input ack latency reported in the tap reply's settle block (counter + form, strict and fast) |
| `teardown_ms` | acos_destroy teardown_ms (GL1) | 13.00 ms | 14.00 | 44 | ±5.0% / 2.000 | 18-20 (GL1) | reply teardown.teardown_ms for a live counter session (lifecycle label recorded); 20 samples |
| `teardown_wall_ms` | acos_destroy wall | 19.43 ms | 20.63 | 44 | ±5.0% / 2.000 | - | client wall of acos_destroy incl. teardown evidence |
| `cpu_active_tap_strict_pct` | active CPU, 20 strict counter taps | 47.57 % of one core | 47.57 | 1 | ±10.0% / 0.200 | - | utime+stime delta over the serve tree during the M8 strict counter cohort (20 taps, ~20 s) |

## Phases and host load

| rep | phase | ok | load1 before -> after | MemAvailable | queue wait s | wall s | noisy |
|---:|---|---|---:|---:|---:|---:|---|
| 1 | latency | yes | 0.550 -> 0.550 | 32.78 GiB | - | 151.0 | no |

## Notes

* value = p50 pooled over every sample in every rep; p95 pooled the same way; per-rep p50s are kept for the threshold derivation.
* threshold = max(category floor, 2 x worst rep-to-rep deviation), capped at 40 %; a delta must exceed both the relative and the absolute threshold to be called a regression.
* memory rows are label-matched (cgroup anon+shmem vs Pss); they are never mixed.
* M3 is measured with the launch working set evicted per rep; the warm-cache variant is M4 + daemon spawn (P7).
