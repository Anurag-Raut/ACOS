# Integration A/B: int-nhp (NET1 + H13B + PHONE-REF-1) vs main — 2026-11-14

Interleaved, 5 reps per arm, `acos-bench.sh --reps 1 --groups "a:latency:8G:300"`, both arms under the
2-slot lock with `--exclusive`. Main arm = main 863def1 binaries (wt-abmain2; parity with 0c0b322 shown in
the int-sew7 A/B). Branch = int-nhp f6c418d. Medians of the per-rep p50s; gate = +5 % and +2 ms.

| metric | main | int-nhp | Δ |
|---|---:|---:|---:|
| M4 launch strict | 309.10 | 307.60 | −0.5 % |
| M4 launch fast | 174.90 | 156.80 | −10.3 % |
| M5 screenshot | 24.79 | 24.58 | −0.8 % |
| M6 reset strict | 322.70 | 319.40 | −1.0 % |
| M6 reset fast | 187.90 | 173.40 | −7.7 % |
| M7 tree strict | 41.20 | 41.29 | +0.2 % |
| M7 tree fast | 0.48 | 0.56 | +15 % (+0.08 ms, under the 2 ms floor) |
| M8 tap counter strict | 962.50 | 959.80 | −0.3 % |
| M8 tap counter fast | 44.47 | 45.11 | +1.4 % |
| M8 tap form strict | 941.40 | 937.90 | −0.4 % |
| M8 tap form fast | 46.02 (n=4) | 45.76 | −0.6 % |
| teardown | 12.0 | 13.0 | +1 ms (under the floor) |

Noise notes: after 3 reps M8 counter fast read +8.5 % from one 71.42 ms outlier (rep 1) and a rep at load1
7.18 during a T3 server restart (rep 2); reps 3–5 were clean (44.90/45.11/44.44 ms), so 2 extra reps per arm
were run before deciding. Main rep 2 lost its form-fast cohort (`group a rep 1 FAILED rc=1`) in the same
restart window. Verdict: no regression; merge.

Correctness on the same tree (`.cache/int/verify6.out`): acos-protocol 64, acos-apk 29, acos-wm 80,
acos-cli 147, acos-services 314, acos-verify 70, acos-art 156 + isolated cgroup test — all pass; 15 gates
(10 pass, 5 fail with their designed codes), 5 Gaten negatives ASSERTION_FAILED, GR4 4 pass + 4 negatives
ASSERTION_FAILED, 5 Gaten flows pass. 0 false passes.
