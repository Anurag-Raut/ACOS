# Integration A/B: int-sh (S3AB + S3PERF2 + HYG1) vs main 570834c — 2026-11-14

Interleaved, 5 reps per arm, `acos-bench.sh --reps 1 --groups "a:latency:8G:300"`, both arms `--exclusive`
under the 2-slot lock. Main arm = main 570834c binaries (`.cache/wt-abmain3`). Medians of per-rep p50s;
gate = +5 % and +2 ms.

| metric | main | int-sh | Δ |
|---|---:|---:|---:|
| M4 launch strict | 307.40 | 307.60 | +0.1 % |
| M4 launch fast | 156.90 | 157.00 | +0.1 % |
| M5 screenshot | 24.62 | 24.91 | +1.2 % |
| M6 reset strict | 331.30 | 320.90 | −3.1 % |
| M6 reset fast | 175.20 | 181.00 | +3.3 % |
| M7 tree strict | 41.33 | 41.23 | −0.2 % |
| M8 tap counter strict | 957.20 | 955.90 | −0.1 % |
| M8 tap counter fast | 47.66 | 45.10 | −5.4 % |
| M8 tap form strict | 942.50 | 939.30 | −0.3 % |
| M8 tap form fast | 45.16 | 44.87 | −0.6 % |
| teardown | 13.0 | 13.0 | 0 |

After 3 reps M8 counter fast read +5.8 % (single-rep outliers on both arms: main 49.08, branch 63.18 ms);
2 extra reps per arm settle it at −5.4 %. That metric's outliers are queued as TAPNOISE. Main rep 1 ran at
load1 14 (agent builds; non-ART load is not excluded by the lock).

Correctness on the same tree (`.cache/int/verify7.out`): acos-protocol 64, acos-apk 29, acos-wm 80,
acos-cli 150, acos-services 331, acos-verify 70, acos-art 163 + isolated cgroup test — all pass; 15 gates
(10 pass, 5 fail with their designed codes); 5 Gaten flows pass, 5 negatives ASSERTION_FAILED; GR4 4 pass +
4 negatives ASSERTION_FAILED. S3 reset smoke N=10 × 10 reps: measured 100, verified 100, false_pass 0,
failed_reps [], relaunch_retries 0. 0 false passes.
