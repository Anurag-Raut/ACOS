# RELGATE4 A/B — v0.3.0 (tag 02421e17b) vs v0.4.0 RC (origin/modules afad66858)

Generated from `ab-compare.json`. 3 interleaved reps, `--exclusive` lock, `--reps 20 --fast-reps 40`: n = 60 strict / 120 fast samples per metric per arm, memory n = 3 fresh-core pairs per arm.

## Lattice metrics (ADR-0028: pooled 5 %-trimmed mean, +5 % AND +2 ms; >=10 pp slot shift flagged)

| metric | n A/B | v0.3.0 trimmed | v0.4.0 RC trimmed | delta ms | delta % | first slot A/B | verdict |
|---|---:|---:|---:|---:|---:|---:|---|
| M4 warm launch (fast) | 120/120 | 143.11 | 145.40 | +2.30 | +1.6% | 59%/38% | within-noise (slot-shift -20.8 pp) |
| M6 reset -> first correct screen (fast) | 120/120 | 184.93 | 187.70 | +2.77 | +1.5% | 2%/1% | within-noise |
| M8 tap -> settled, counter (fast) | 120/120 | 24.81 | 24.85 | +0.04 | +0.1% | 50%/50% | within-noise |
| M8 tap -> settled, form (fast) | 120/120 | 29.79 | 29.65 | -0.14 | -0.5% | 100%/100% | within-noise |

## All other tracked metrics (old gate: medians of per-rep p50s)

| metric | v0.3.0 | v0.4.0 RC | delta | delta % | threshold | verdict |
|---|---:|---:|---:|---:|---:|---|
| M3 cold start -> first correct screen | 4020.62 | 4039.76 | +19.13 | +0.5% | +-5.00% / 2.00 | within-noise |
| M4 warm launch (strict) | 287.58 | 287.01 | -0.57 | -0.2% | +-5.00% / 2.00 | within-noise |
| M4 warm launch (fast) | 137.30 | 149.62 | +12.31 | +9.0% | +-5.00% / 2.00 | REGRESSION |
| M5 fresh screenshot (PNG) | 11.75 | 11.70 | -0.04 | -0.4% | +-5.00% / 2.00 | within-noise |
| M6 reset -> first correct screen (strict) | 331.35 | 332.06 | +0.71 | +0.2% | +-5.00% / 2.00 | within-noise |
| M6 reset -> first correct screen (fast) | 188.14 | 189.61 | +1.47 | +0.8% | +-5.00% / 2.00 | within-noise |
| M7 UI tree with text (strict) | 41.45 | 41.55 | +0.09 | +0.2% | +-5.00% / 2.00 | within-noise |
| M7 UI tree with text (fast) | 0.60 | 0.61 | +0.01 | +1.7% | +-40.00% / 2.00 | within-noise |
| M8 tap -> settled, counter (strict) | 951.02 | 948.94 | -2.08 | -0.2% | +-5.00% / 2.00 | within-noise |
| M8 tap -> settled, counter (fast) | 24.76 | 24.68 | -0.08 | -0.3% | +-5.00% / 2.00 | within-noise |
| M8 tap -> settled, form (strict) | 923.47 | 923.07 | -0.40 | -0.0% | +-5.00% / 2.00 | within-noise |
| M8 tap -> settled, form (fast) | 30.20 | 29.96 | -0.24 | -0.8% | +-5.00% / 2.00 | within-noise |
| tap acknowledgement | 5.00 | 5.00 | +0.00 | +0.0% | +-5.00% / 2.00 | within-noise |
| acos_destroy teardown_ms | 21.25 | 21.00 | -0.25 | -1.2% | +-15.00% / 2.00 | within-noise |
| cpu_active_tap_strict_pct | 44.13 | 43.84 | -0.29 | -0.7% | +-10.00% / 0.20 | within-noise |
| cpu_idle_n10_pct | 3.47 | 3.58 | +0.11 | +3.3% | +-10.00% / 0.20 | within-noise |
| cpu_idle_n10_per_session_pct | 0.35 | 0.36 | +0.01 | +3.2% | +-10.00% / 0.20 | within-noise |
| cpu_idle_n1_pct | 0.58 | 0.56 | -0.02 | -2.9% | +-30.00% / 0.20 | within-noise |
| cpu_idle_n1_per_session_pct | 0.58 | 0.56 | -0.02 | -2.9% | +-30.00% / 0.20 | within-noise |
| cpu_idle_n5_pct | 2.16 | 2.10 | -0.07 | -3.1% | +-23.00% / 0.20 | within-noise |
| cpu_idle_n5_per_session_pct | 0.43 | 0.42 | -0.01 | -3.0% | +-22.00% / 0.20 | within-noise |
| m1_idle_mem_mib | 58.01 | 59.33 | +1.32 | +2.3% | +-3.00% / 0.50 | within-noise |
| m1_idle_pss_mib | 236.95 | 240.85 | +3.90 | +1.6% | +-21.00% / 0.50 | within-noise |
| m2_per_session_anon_shmem_n1 | 56.15 | 57.38 | +1.22 | +2.2% | +-3.00% / 0.50 | within-noise |
| m2_per_session_anon_shmem_n10 | 55.81 | 56.14 | +0.34 | +0.6% | +-3.00% / 0.50 | within-noise |
| m2_per_session_anon_shmem_n5 | 55.77 | 56.32 | +0.55 | +1.0% | +-3.00% / 0.50 | within-noise |
| m2_per_session_pss_n1 | 218.34 | 222.01 | +3.67 | +1.7% | +-23.00% / 0.50 | within-noise |
| m2_per_session_pss_n10 | 55.19 | 57.27 | +2.08 | +3.8% | +-7.00% / 0.50 | within-noise |
| m2_per_session_pss_n5 | 73.83 | 74.73 | +0.89 | +1.2% | +-13.00% / 0.50 | within-noise |
| teardown_wall_ms | 21.79 | 21.74 | -0.04 | -0.2% | +-16.00% / 2.00 | within-noise |
| wakeups_idle_n10_per_s | 3.23 | 3.18 | -0.05 | -1.5% | +-10.00% / 0.20 | within-noise |
| wakeups_idle_n1_per_s | 1.21 | 1.21 | +0.00 | +0.0% | +-10.00% / 0.20 | within-noise |
| wakeups_idle_n5_per_s | 2.10 | 2.10 | +0.00 | +0.0% | +-10.00% / 0.20 | within-noise |
