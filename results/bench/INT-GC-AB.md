# Integration A/B: int-gc (ARTGC artifact retention) vs main 1ead474 — 2026-11-14

Interleaved, `acos-bench.sh --reps 1 --groups "a:latency:8G:300"`, `--exclusive`, 2 reps per arm, all ok, no noisy
phases. Main arm built in place (`.cache/wt-abmain3` at 1ead474). Branch 142557/143152, main 142314/142900.
Medians of per-rep p50s; gate +5 % and +2 ms.

| metric | main | int-gc | Δ |
|---|---:|---:|---:|
| M4 launch strict | 307.01 | 307.02 | 0.0 % |
| M4 launch fast | 163.73 | 156.07 | −4.7 % |
| M5 screenshot | 24.54 | 23.76 | −3.2 % |
| M6 reset strict | 333.62 | 335.08 | +0.4 % |
| M6 reset fast | 192.71 | 195.60 | +1.5 % |
| M7 tree strict | 41.29 | 41.31 | +0.1 % |
| M8 tap counter strict | 954.14 | 953.33 | −0.1 % |
| M8 tap counter fast | 46.79 | 44.18 | −5.6 % |
| M8 tap form strict | 945.71 | 944.58 | −0.1 % |
| M8 tap form fast | 44.11 | 44.52 | +0.9 % |
| teardown | 12.5 | 12.5 | 0 |

No regression (retention runs in the verify executor after the verdict; the bench's serve path is unaffected).
Correctness (`.cache/int/verify10.out`): acos-protocol 64, acos-cli 163, acos-verify 80, acos-art 163 pass; 15 gates
(10 pass, 5 designed failures with their codes), 5 Gaten flows pass + 5 negatives ASSERTION_FAILED, GR4 4 pass +
4 negatives ASSERTION_FAILED. 0 false passes. The whole verify suite left 3.8 GB in out/ (was ~20 GB per suite).
