### D1. Cohort results (median of the reps that verified every task)

| N | arm | reps | verified tasks | failures | wall s | GiB·min (PSS) | tasks/GiB·min | range |
|---:|---|---:|---|---:|---:|---:|---:|---|
| 1 | exec | 3 | 3/3 | 0 | 4.35 | 0.0150 | **198.3** | 195.4–202.3 |
| 2 | exec | 2 | 6/6 | 0 | 4.32 | 0.0140 | **418.3** | 407.3–429.2 |
| 5 | exec | 3 | 15/15 | 0 | 5.19 | 0.0270 | **556.8** | 543.5–573.8 |
| 10 | exec | 3 | 30/30 | 0 | 4.54 | 0.0350 | **862.8** | 700.6–865.8 |
| 10 | zygote-on | 3 | 30/30 | 0 | 4.48 | 0.0270 | **1,094.9** | 1,079.9–1,104.2 |
| 20 | exec | 3 | 60/60 | 0 | 5.70 | 0.0680 | **881.4** | 805.2–903.3 |
| 20 | zygote-on | 2 | 60/60 | 0 | 5.66 | 0.0540 | **1,116.4** | 1,036.4–1,196.4 |

### D2. Memory per session (settled state: every session holding a live guest)

| N | arm | per-session PSS MiB | range | guest PSS | guest USS (private) | host-side (serve) | tree PSS total MiB | peak tree PSS MiB | host used % peak | CPU % of one core |
|---:|---|---:|---|---:|---:|---:|---:|---:|---:|---:|
| 1 | exec | **233.9** | 232.6–234.7 | 211.7 | 208.9 | 21.9 | 233.9 | 239.7 | 12.2 | 3 |
| 2 | exec | **109.7** | 108.7–110.7 | 96.5 | 33.6 | 12.9 | 219.4 | 233.1 | 15.5 | 8 |
| 5 | exec | **75.4** | 73.4–77.9 | 67.8 | 31.6 | 7.4 | 377.0 | 438.0 | 14.0 | 15 |
| 10 | exec | **56.1** | 54.9–57.0 | 50.4 | 32.1 | 5.5 | 561.3 | 585.6 | 15.1 | 37 |
| 10 | zygote-on | **40.3** | 38.1–42.2 | 34.6 | 18.2 | 5.4 | 403.0 | 456.9 | 17.3 | 48 |
| 20 | exec | **46.7** | 44.4–47.2 | 42.0 | 32.8 | 4.5 | 934.6 | 1,033.7 | 15.5 | 94 |
| 20 | zygote-on | **32.3** | 30.4–34.1 | 27.6 | 19.1 | 4.4 | 645.0 | 745.2 | 19.4 | 164 |

### D3. Marginal per-session memory (exec arm, settled tree PSS)

| from N | to N | MiB per additional session |
|---:|---:|---:|
| 1 | 2 | **-14.5** |
| 2 | 5 | **52.5** |
| 5 | 10 | **36.9** |
| 10 | 20 | **37.3** |

### D4. Where the memory goes (settled, per session, median rep)

**N=1 exec** (1 sessions)

| # | mapping class | total PSS MiB | per session MiB | private MiB | shared MiB | % of tree PSS |
|---:|---|---:|---:|---:|---:|---:|
| 1 | `boot-image` | 66.0 | 66.0 | 66.0 | 0.0 | 28.22 |
| 2 | `shared-lib` | 44.6 | 44.6 | 42.1 | 16.1 | 19.03 |
| 3 | `framework-dex` | 25.3 | 25.3 | 25.2 | 0.1 | 10.80 |
| 4 | `odex-vdex-oat` | 20.5 | 20.5 | 20.5 | 0.0 | 8.75 |
| 5 | `other-file` | 19.1 | 19.1 | 18.8 | 0.7 | 8.21 |
| 6 | `fonts` | 15.4 | 15.4 | 15.4 | 0.0 | 6.56 |
| 7 | `apk-dex` | 13.8 | 13.8 | 13.7 | 0.2 | 5.93 |
| 8 | `anon-other` | 10.1 | 10.1 | 10.1 | 0.1 | 4.35 |
| 9 | `hwui-skia` | 8.5 | 8.5 | 8.5 | 0.0 | 3.62 |
| 10 | `dalvik-other` | 8.4 | 8.4 | 8.4 | 0.0 | 3.59 |
| 11 | `binder` | 0.9 | 0.9 | 0.9 | 0.0 | 0.40 |
| 12 | `dalvik-heap` | 0.8 | 0.8 | 0.8 | 0.0 | 0.36 |

**N=10 exec** (10 sessions)

| # | mapping class | total PSS MiB | per session MiB | private MiB | shared MiB | % of tree PSS |
|---:|---|---:|---:|---:|---:|---:|
| 1 | `boot-image` | 158.2 | 15.8 | 102.2 | 552.7 | 28.12 |
| 2 | `anon-other` | 98.6 | 9.9 | 98.1 | 1.3 | 17.59 |
| 3 | `shared-lib` | 85.3 | 8.5 | 45.0 | 494.2 | 15.19 |
| 4 | `dalvik-other` | 63.2 | 6.3 | 63.2 | 0.0 | 11.26 |
| 5 | `odex-vdex-oat` | 53.8 | 5.4 | 37.9 | 164.1 | 9.59 |
| 6 | `framework-dex` | 25.7 | 2.6 | 0.2 | 247.5 | 4.59 |
| 7 | `other-file` | 22.6 | 2.3 | 12.8 | 98.2 | 4.04 |
| 8 | `fonts` | 16.2 | 1.6 | 0.4 | 155.5 | 2.88 |
| 9 | `apk-dex` | 14.5 | 1.4 | 0.3 | 135.4 | 2.56 |
| 10 | `hwui-skia` | 10.2 | 1.0 | 1.9 | 82.6 | 1.82 |
| 11 | `dalvik-heap` | 5.6 | 0.6 | 5.6 | 0.0 | 1.00 |
| 12 | `binder` | 2.0 | 0.2 | 1.1 | 8.5 | 0.35 |

**N=20 exec** (20 sessions)

| # | mapping class | total PSS MiB | per session MiB | private MiB | shared MiB | % of tree PSS |
|---:|---|---:|---:|---:|---:|---:|
| 1 | `boot-image` | 259.7 | 13.0 | 203.7 | 1,112.0 | 27.79 |
| 2 | `anon-other` | 196.1 | 9.8 | 195.2 | 2.6 | 20.96 |
| 3 | `dalvik-other` | 141.6 | 7.1 | 141.6 | 0.0 | 15.15 |
| 4 | `shared-lib` | 128.8 | 6.4 | 89.2 | 988.8 | 13.78 |
| 5 | `odex-vdex-oat` | 91.4 | 4.6 | 75.9 | 331.9 | 9.78 |
| 6 | `other-file` | 25.9 | 1.3 | 16.1 | 196.2 | 2.77 |
| 7 | `framework-dex` | 25.9 | 1.3 | 0.3 | 501.9 | 2.77 |
| 8 | `fonts` | 15.5 | 0.8 | 0.3 | 310.8 | 1.66 |
| 9 | `apk-dex` | 14.5 | 0.7 | 0.3 | 275.5 | 1.56 |
| 10 | `hwui-skia` | 12.1 | 0.6 | 3.8 | 165.5 | 1.29 |
| 11 | `dalvik-heap` | 10.8 | 0.5 | 10.8 | 0.0 | 1.16 |
| 12 | `binder` | 3.1 | 0.1 | 2.3 | 17.0 | 0.33 |

**N=10 zygote (warm-fork)** (10 sessions)

| # | mapping class | total PSS MiB | per session MiB | private MiB | shared MiB | % of tree PSS |
|---:|---|---:|---:|---:|---:|---:|
| 1 | `anon-other` | 109.0 | 10.9 | 102.2 | 81.4 | 27.03 |
| 2 | `dalvik-other` | 91.7 | 9.2 | 54.9 | 880.6 | 22.76 |
| 3 | `shared-lib` | 71.8 | 7.2 | 36.5 | 777.3 | 18.31 |
| 4 | `framework-dex` | 31.9 | 3.2 | 5.5 | 309.3 | 7.92 |
| 5 | `other-file` | 23.5 | 2.4 | 13.6 | 121.5 | 5.84 |
| 6 | `apk-dex` | 17.4 | 1.7 | 6.0 | 113.3 | 4.31 |
| 7 | `fonts` | 12.9 | 1.3 | 0.0 | 154.0 | 3.29 |
| 8 | `boot-image` | 12.5 | 1.2 | 0.7 | 140.6 | 3.19 |
| 9 | `hwui-skia` | 9.9 | 1.0 | 2.1 | 122.6 | 2.45 |
| 10 | `dalvik-heap` | 6.0 | 0.6 | 6.0 | 0.0 | 1.48 |
| 11 | `binder` | 2.0 | 0.2 | 1.2 | 16.9 | 0.49 |
| 12 | `odex-vdex-oat` | 1.4 | 0.1 | 0.2 | 20.2 | 0.36 |

**N=20 zygote (warm-fork)** (20 sessions)

| # | mapping class | total PSS MiB | per session MiB | private MiB | shared MiB | % of tree PSS |
|---:|---|---:|---:|---:|---:|---:|
| 1 | `anon-other` | 215.1 | 10.8 | 203.9 | 162.1 | 33.45 |
| 2 | `dalvik-other` | 163.3 | 8.2 | 127.2 | 1,786.0 | 25.07 |
| 3 | `shared-lib` | 112.7 | 5.6 | 75.6 | 1,531.6 | 17.53 |
| 4 | `framework-dex` | 35.0 | 1.8 | 8.2 | 591.3 | 5.45 |
| 5 | `other-file` | 27.9 | 1.4 | 17.6 | 242.3 | 4.34 |
| 6 | `apk-dex` | 20.9 | 1.0 | 11.6 | 209.6 | 3.25 |
| 7 | `fonts` | 13.6 | 0.7 | 0.0 | 313.3 | 2.12 |
| 8 | `boot-image` | 12.9 | 0.6 | 0.6 | 282.5 | 2.00 |
| 9 | `hwui-skia` | 12.5 | 0.6 | 4.1 | 239.2 | 1.95 |
| 10 | `dalvik-heap` | 11.3 | 0.6 | 11.3 | 0.0 | 1.76 |
| 11 | `binder` | 3.2 | 0.2 | 2.4 | 33.2 | 0.49 |
| 12 | `gl` | 1.5 | 0.1 | 1.2 | 19.9 | 0.23 |

