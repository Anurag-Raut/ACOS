# Milestone 1: real counter.apk, live frames, tap, reset, teardown, N=1/10/20 (2026-11-11)

Commit ef861e8 (pool = 2). Evidence: W9/W10/W11/W12 reports, `results/art-runtime/evidence/`, `results/bench/W12-session-memory-postW11.json`.
Command: `acos verify --scenario scenarios/v2-art-counter.yaml --apk fixtures/build/apps/counter.apk --engine art --json` -> pass, engine art, live, 11/11 steps, ~8.8 s.
Verified: launch via ART; live frame from the Activity (C1 buffer capture, labelled not a device capture); live in-guest UI tree; tap Increment 0->1 (selector resolved from the live tree); Reset -> 0; clean teardown; structured ASSERTION_FAILED on a missing selector; 832 frames with 2 gralloc allocations.

| cohort (ART, debug build) | N | verified | per-session total MiB (cgroup peak + launcher + bwrap) | guest cgroup peak | host PSS delta | first frame / tap->frame ms |
|---|---:|---|---:|---:|---:|---|
| full interactive (Option A) | 1 | 1/1 | 171.4 | 103.0 | 59.8 | 523 / 1982 |
| full | 10 | 10/10 | 155.3 | 103.4 | 839 | 564 / 2036 |
| full | 20 | 20/20 | 155.0 | 103.1 | 1664 | 615 / 2149 |
| launch (window stack, no interaction) | 20 | 20/20 | 100.6 | 96.5 | 1563 | - |
| render (no window stack, comparator) | 20 | 20/20 | 61.9 | 60.2 | 870 | - |

Option A window/surface stack = launch minus render: +36 MiB cgroup peak per session (N=1/10/20 consistent), +14 to +25 MiB PSS; the interactive extras (probe, input, captures) add ~6 MiB cgroup. Estimate was +27 MiB: measured +36 MiB cgroup / +14-25 PSS. Host peak RAM 28.7% at N=20, 0 OOM. Legacy ACVM N=1: 130.3 MiB (BENCHMARKS.md section 3) vs 171 (N=1) / 155 (N=10-20) MiB here.
Caveats: debug binaries, one rep per cell, cgroup peak counts shared file pages once per cgroup (upper bound); stock-SurfaceFlinger cost is not measured (no libsurfaceflinger in the sysroot), only the earlier +120-300 MiB estimate exists.
Not done: key/type_text (IME EditorInfo codec), phone-parity capture, other app types, sandbox certification before untrusted APKs.
