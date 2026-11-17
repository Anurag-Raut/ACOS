# RELGATE3 A/B — v0.2.2 (tag ce14b888) vs v0.3.0 RC (rel-030 c7962f71)

Generated from `ab-compare.json`. 3 interleaved reps, `--exclusive` lock, `--reps 20 --fast-reps 40`: n = 60 strict / 120 fast samples per metric per arm, memory n = 3 fresh-core pairs per arm.

## Lattice metrics (ADR-0028: pooled 5 %-trimmed mean, +5 % AND +2 ms; >=10 pp slot shift flagged)

| metric | n A/B | v0.2.2 trimmed | v0.3.0 trimmed | delta ms | delta % | first slot A/B | verdict |
|---|---:|---:|---:|---:|---:|---:|---|
| M4 warm launch (fast) | 120/120 | 159.74 | 143.59 | -16.15 | -10.1% | 0%/57% | improved (slot-shift +57.5 pp) |
| M6 reset -> first correct screen (fast) | 120/120 | 197.75 | 185.46 | -12.29 | -6.2% | 0%/5% | improved |
| M8 tap -> settled, counter (fast) | 120/120 | 46.37 | 24.93 | -21.44 | -46.2% | 8%/49% | improved (slot-shift +75.8 pp) |
| M8 tap -> settled, form (fast) | 120/120 | 32.40 | 29.60 | -2.81 | -8.7% | 100%/99% | improved |

## All other tracked metrics (old gate: medians of per-rep p50s)

| metric | v0.2.2 | v0.3.0 | delta | delta % | threshold | verdict |
|---|---:|---:|---:|---:|---:|---|
| M3 cold start -> first correct screen | 4048.68 | 4148.25 | +99.57 | +2.5% | +-5.00% / 2.00 | within-noise |
| M4 warm launch (strict) | 304.82 | 293.26 | -11.55 | -3.8% | +-6.00% / 2.00 | within-noise |
| M5 fresh screenshot (PNG) | 11.55 | 11.69 | +0.14 | +1.2% | +-8.00% / 2.00 | within-noise |
| M6 reset -> first correct screen (strict) | 343.94 | 324.95 | -18.99 | -5.5% | +-5.00% / 2.00 | improved |
| M7 UI tree with text (strict) | 41.37 | 41.57 | +0.20 | +0.5% | +-5.00% / 2.00 | within-noise |
| M7 UI tree with text (fast) | 0.56 | 0.64 | +0.08 | +13.6% | +-11.00% / 2.00 | within-noise |
| M8 tap -> settled, counter (strict) | 950.65 | 950.32 | -0.34 | -0.0% | +-5.00% / 2.00 | within-noise |
| M8 tap -> settled, form (strict) | 933.22 | 924.32 | -8.90 | -1.0% | +-5.00% / 2.00 | within-noise |
| tap acknowledgement | 5.00 | 5.00 | +0.00 | +0.0% | +-5.00% / 2.00 | within-noise |
| acos_destroy teardown_ms | 19.00 | 20.00 | +1.00 | +5.3% | +-10.00% / 2.00 | within-noise |
| cpu_active_tap_strict_pct | 45.51 | 43.37 | -2.14 | -4.7% | +-10.00% / 0.20 | within-noise |
| cpu_idle_n10_pct | 3.83 | 3.60 | -0.23 | -6.0% | +-10.00% / 0.20 | within-noise |
| cpu_idle_n10_per_session_pct | 0.38 | 0.36 | -0.02 | -6.0% | +-10.00% / 0.20 | within-noise |
| cpu_idle_n1_pct | 0.56 | 0.65 | +0.08 | +14.5% | +-21.00% / 0.20 | within-noise |
| cpu_idle_n1_per_session_pct | 0.56 | 0.65 | +0.08 | +14.5% | +-21.00% / 0.20 | within-noise |
| cpu_idle_n5_pct | 2.39 | 2.31 | -0.08 | -3.4% | +-20.00% / 0.20 | within-noise |
| cpu_idle_n5_per_session_pct | 0.48 | 0.46 | -0.02 | -3.3% | +-20.00% / 0.20 | within-noise |
| m1_idle_mem_mib | 55.44 | 57.94 | +2.50 | +4.5% | +-3.00% / 0.50 | REGRESSION |
| m1_idle_pss_mib | 230.80 | 233.49 | +2.69 | +1.2% | +-3.00% / 0.50 | within-noise |
| m2_per_session_anon_shmem_n1 | 53.81 | 56.04 | +2.24 | +4.2% | +-3.00% / 0.50 | REGRESSION |
| m2_per_session_anon_shmem_n10 | 54.19 | 55.82 | +1.62 | +3.0% | +-3.00% / 0.50 | within-noise |
| m2_per_session_anon_shmem_n5 | 54.21 | 55.62 | +1.41 | +2.6% | +-3.00% / 0.50 | within-noise |
| m2_per_session_pss_n1 | 213.74 | 214.87 | +1.13 | +0.5% | +-3.00% / 0.50 | within-noise |
| m2_per_session_pss_n10 | 52.88 | 54.62 | +1.73 | +3.3% | +-3.00% / 0.50 | REGRESSION |
| m2_per_session_pss_n5 | 71.20 | 72.67 | +1.48 | +2.1% | +-3.00% / 0.50 | within-noise |
| teardown_wall_ms | 19.87 | 20.65 | +0.78 | +3.9% | +-9.00% / 2.00 | within-noise |
| wakeups_idle_n10_per_s | 3.11 | 3.24 | +0.13 | +4.1% | +-10.00% / 0.20 | within-noise |
| wakeups_idle_n1_per_s | 1.21 | 1.23 | +0.02 | +1.3% | +-10.00% / 0.20 | within-noise |
| wakeups_idle_n5_per_s | 2.05 | 2.18 | +0.13 | +6.4% | +-10.00% / 0.20 | within-noise |
