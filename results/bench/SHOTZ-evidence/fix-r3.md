# ACOS bench baseline -- 2026-11-14T18:42:52+00:00

Command: `tools/bench/acos-bench.sh --reps 1` (branch `screenshot-compress`, commit `638a0afb3c7c`).

Mode `full`; `ACOS_STRICT_SETTLE=default`; `animations=on`; binary sha256 `b53f4d77e7839b6e`.

Host: AMD Ryzen 9 9950X, 32 threads, 61908 MiB. Noisy phases: 0/1 (load1 > 4 or MemAvailable < 20 GiB; recorded, not deleted).

## Metrics (p50 / p95, pooled across reps)

| metric | label | p50 | p95 | n | threshold | prev best | method |
|---|---|---:|---:|---:|---:|---|---|
| `m4_launch_strict_ms` | M4 warm launch (strict) | 305.5 ms | 307.1 | 20 | ±5.0% / 2.000 | 440 (P9) | warm daemon, app not running -> acos_launch settle=strict -> reply tree count=0 + post-frame strict tree count |
| `m4_launch_fast_ms` | M4 warm launch (fast) | 155.7 ms | 172.0 | 20 | ±5.0% / 2.000 | 184 (P9) | acos_launch settle=fast (probe-fresh label recorded) -> reply tree count=0 + post-frame strict tree count=0 |
| `m5_screenshot_ms` | M5 fresh screenshot (PNG) | 11.82 ms | 12.23 | 20 | ±5.0% / 2.000 | 24.4 (P2/W1) | acos_observe {png:true} on a held session; PNG magic + newer frame_seq + meaningful frame required |
| `m6_reset_strict_ms` | M6 reset -> first correct screen (strict) | 342.4 ms | 352.4 | 20 | ±5.0% / 2.000 | 463 (P9) | count=1 strictly read -> acos_reset settle=strict -> reply tree count=0 + following strict tree count=0 |
| `m6_reset_fast_ms` | M6 reset -> first correct screen (fast) | 195.6 ms | 209.6 | 20 | ±5.0% / 2.000 | 195 (P9) | acos_reset settle=fast; state_reset + data_cleared + count=0 in the reply and in a following strict tree |
| `m7_tree_strict_ms` | M7 UI tree with text (strict) | 41.34 ms | 41.68 | 20 | ±5.0% / 2.000 | 151 (P10) | acos_observe_tree settle=strict on a held session; tree has nodes and text, not truncated |
| `m7_tree_fast_ms` | M7 UI tree with text (fast) | 0.559 ms | 0.781 | 20 | ±5.0% / 2.000 | 0.2 (P10) | acos_observe_tree settle=fast; the fast tree must show the same count text as a strict read (no stale tree) |
| `m8_tap_counter_strict_ms` | M8 tap -> settled, counter (strict) | 945.1 ms | 969.6 | 20 | ±5.0% / 2.000 | 955 (P11 event) | acos_tap {id:inc} settle=strict on counter, one session; every tap must report state_changed and the final str |
| `m8_tap_counter_fast_ms` | M8 tap -> settled, counter (fast) | 50.54 ms | 65.42 | 20 | ±5.0% / 2.000 | 70.9 (P1) | acos_tap settle=fast; same state_changed/final-count checks |
| `m8_tap_form_strict_ms` | M8 tap -> settled, form (strict) | 929.8 ms | 941.4 | 20 | ±5.0% / 2.000 | - | empty form, tap submit settle=strict; following strict tree must read error='Name is required'; fast reset (un |
| `m8_tap_form_fast_ms` | M8 tap -> settled, form (fast) | 32.31 ms | 33.15 | 20 | ±5.0% / 2.000 | - | empty form, tap submit settle=fast; same strict post-check |
| `tap_ack_ms` | tap acknowledgement (settle.ack_ms) | 5.000 ms | 5.000 | 80 | ±5.0% / 2.000 | 5 (P1) | input ack latency reported in the tap reply's settle block (counter + form, strict and fast) |
| `teardown_ms` | acos_destroy teardown_ms (GL1) | 13.00 ms | 14.00 | 44 | ±5.0% / 2.000 | 18-20 (GL1) | reply teardown.teardown_ms for a live counter session (lifecycle label recorded); 20 samples |
| `teardown_wall_ms` | acos_destroy wall | 19.21 ms | 20.87 | 44 | ±5.0% / 2.000 | - | client wall of acos_destroy incl. teardown evidence |
| `cpu_active_tap_strict_pct` | active CPU, 20 strict counter taps | 45.72 % of one core | 45.72 | 1 | ±10.0% / 0.200 | - | utime+stime delta over the serve tree during the M8 strict counter cohort (20 taps, ~20 s) |

## Phases and host load

| rep | phase | ok | load1 before -> after | MemAvailable | queue wait s | wall s | noisy |
|---:|---|---|---:|---:|---:|---:|---|
| 1 | latency | yes | 1.250 -> 1.250 | 33.28 GiB | - | 145.4 | no |

## Notes

* value = p50 pooled over every sample in every rep; p95 pooled the same way; per-rep p50s are kept for the threshold derivation.
* threshold = max(category floor, 2 x worst rep-to-rep deviation), capped at 40 %; a delta must exceed both the relative and the absolute threshold to be called a regression.
* memory rows are label-matched (cgroup anon+shmem vs Pss); they are never mixed.
* M3 is measured with the launch working set evicted per rep; the warm-cache variant is M4 + daemon spawn (P7).
