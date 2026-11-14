# Integration A/B: int-pt (PHONE2 + TREE1) vs main 389429a — 2026-11-14

Interleaved, `acos-bench.sh --reps 1 --groups "a:latency:8G:300"`, `--exclusive`. Main arm = main 389429a
binaries (`.cache/wt-abmain3`). Branch reps 4 (130824, 131753, 132024, 132602), main reps 3 (130536, 131755,
132334; main rep 2 was killed by SIGTERM mid-run, rc=143, no output). Medians of per-rep p50s; gate +5 % and +2 ms.

| metric | main | int-pt | Δ |
|---|---:|---:|---:|
| M4 launch strict | 323.80 | 306.80 | −5.3 % |
| M4 launch fast | 157.90 | 157.00 | −0.6 % |
| M5 screenshot | 24.47 | 25.12 | +2.7 % (+0.65 ms) |
| M6 reset strict | 333.50 | 329.90 | −1.1 % |
| M6 reset fast | 188.10 | 189.20 | +0.6 % |
| M7 tree strict | 41.28 | 41.34 | +0.1 % |
| M8 tap counter strict | 951.80 | 959.70 | +0.8 % |
| M8 tap counter fast | 44.24 | 45.56 | +3.0 % |
| M8 tap form strict | 942.70 | 948.20 | +0.6 % |
| teardown | 12.0 | 12.0 | 0 |

Rep 4 of both arms reported one failed step each, both fail-closed: branch `m4-warm launch GUEST_EXITED (exit
status 1) before the first live frame` — the pre-existing unclaimed exit-1 launch class (G3CD 1/60; G3F1 adds
its evidence plumbing); main `m8-form destroy INTERNAL`. Neither is attributable to this branch.

Correctness (`.cache/int/verify8.out`): acos-protocol 64, acos-apk 29, acos-wm 80, acos-cli 151, acos-services
331, acos-verify 70 pass; acos-art 162/163 — `launch_hang::guest_exits_before_milestone_returns_exit_code_and_
cleans_up` took 6.01 s vs its 5 s bound under host load (~30 GiB used by 6 agents); rerun alone 3/3 pass in
0.23–0.31 s (load1 3.05). 15 gates (10 pass, 5 designed failures), 5 Gaten flows pass, 5 negatives
ASSERTION_FAILED, GR4 4 pass + 4 negatives ASSERTION_FAILED. 0 false passes.
