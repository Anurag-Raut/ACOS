# GLSHIP cost A/B (indicative, shared slots)

Default software GL (`angle-swiftshader`, no flag) vs `ACOS_GUEST_GLES=off`, 5 interleaved reps each, private runtime dir per rep. `m8_fast_ms` = sum of the tap/type step durations with `settle: fast` (the action→state latency); `peak_rss_mib` = the session's own `resources.peak_rss_bytes` high-water mark; `launch_ms` = the launch step (the only step that pays the provider bind/props).

## counter

| arm | verdicts | wall p50 | launch p50 | M8 fast p50 | peak RSS p50 |
|---|---|---|---|---|---|
| default ON | pass,pass,pass,pass,pass | 3086 ms | 154 ms | 246 ms | 13.9 MiB |

* provider verify cost per launch (ms, rep 1..5): [0, 0, 0, 0, 0]
| off | pass,pass,pass,pass,pass | 3100 ms | 154 ms | 243 ms | 14.0 MiB |

* `wall_ms`: on 3086 ms vs off 3100 ms (Δ -14.0 ms, -0.5 %)

* `launch_ms`: on 154 ms vs off 154 ms (Δ +0.0 ms, +0.0 %)

* `m8_fast_ms`: on 246 ms vs off 243 ms (Δ +3.0 ms, +1.2 %)

* `peak_rss_mib`: on 13.9 MiB vs off 14.0 MiB (Δ -0.1 MiB, -0.7 %)

## form

| arm | verdicts | wall p50 | launch p50 | M8 fast p50 | peak RSS p50 |
|---|---|---|---|---|---|
| default ON | pass,pass,pass,pass,pass | 2418 ms | 155 ms | 205 ms | 13.8 MiB |

* provider verify cost per launch (ms, rep 1..5): [0, 0, 0, 0, 0]
| off | pass,pass,pass,pass,pass | 2441 ms | 155 ms | 196 ms | 13.9 MiB |

* `wall_ms`: on 2418 ms vs off 2441 ms (Δ -23.0 ms, -0.9 %)

* `launch_ms`: on 155 ms vs off 155 ms (Δ +0.0 ms, +0.0 %)

* `m8_fast_ms`: on 205 ms vs off 196 ms (Δ +9.0 ms, +4.6 %)

* `peak_rss_mib`: on 13.8 MiB vs off 13.9 MiB (Δ -0.1 MiB, -0.7 %)

