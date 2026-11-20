| Metric | ACOS | Emulator | Unit | Better | Delta | Verdict |
|---|---:|---:|---|---|---:|---|
| Cold boot -> first app frame | 4200 | 13632 | ms | ACOS | -69.2 % | better |
| Warm launch -> app frame | 288.4 | 286.1 | ms | emulator | +0.8 % | worse |
| Tap -> settled UI result (counter) | 949.2 | 1931 | ms | ACOS | -50.8 % | better |
| Tap -> settled UI result (form) | 920.0 | 1946 | ms | ACOS | -52.7 % | better |
| UI tree read with text | 41.4 | 1928 | ms | ACOS | -97.9 % | better |
| Fresh PNG screenshot | 11.7 | 527.1 | ms | ACOS | -97.8 % | better |
| Reset -> first correct screen | 308.8 | 530.8 | ms | ACOS | -41.8 % | better |
| Instance memory, PSS | 234.9 | 3578.0 | MiB | ACOS | -93.4 % | better |
| Instance memory, anon+shmem | 57.1 | 3531.6 | MiB | ACOS | -98.4 % | better |
| Idle CPU | 0.4 | 13.1 | % of one core | ACOS | -96.7 % | better |
| Verified tasks / GiB-minute | 62.84 | 2.54 | tasks/GiB-min | ACOS | -96.0 % | better |

ACOS side: `results/release/v0.3.0-gate/timing/02-ab/v030.json` (relgate2, commit 169bfa5e, 2026-11-16T11:39:48+00:00). Emulator side: `results/bench/emubench-2026-11-18.json` (2026-11-18T06:30:50Z, phases n1-cold, n1-warm, n2, n4).
