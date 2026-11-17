# RELGATE2 A/B table (v0.2.2 tag vs v0.3.0 RC 169bfa5e), 3 interleaved reps

| # | metric | unit | v0.2.2 | v0.3.0 RC | delta | delta % | gate applied | verdict |
|---|---|---|---:|---:|---:|---:|---|---|
| | M3 cold start -> first correct screen | ms | 4082.11 | 4232.06 | +149.95 | +3.7% | old gate ±5.0% / 2.0 | within-noise |
| | M4 warm launch (strict) | ms | 305.62 | 288.99 | -16.63 | -5.4% | old gate ±5.0% / 2.0 | improved |
| | M4 warm launch (fast) — fast (pooled 5%-trimmed mean, n=120/120) | ms | 162.82 | 143.39 | -19.43 | -11.9% | lattice +5%&+2ms (first slot 0%→54%, period 16.667 ms fallback) | **improved** |
| | M4 warm launch (fast) — strict (median of per-rep p50s) | ms | 169.19 | 137.37 | -31.81 | -18.8% | old gate ±21% / 2.0 | within-noise |
| | M5 fresh screenshot (PNG) | ms | 11.85 | 11.71 | -0.14 | -1.2% | old gate ±8% / 2.0 | within-noise |
| | M6 reset -> first correct screen (strict) | ms | 322.78 | 305.92 | -16.86 | -5.2% | old gate ±11% / 2.0 | within-noise |
| | M6 reset -> first correct screen (fast) — fast (pooled 5%-trimmed mean, n=120/120) | ms | 186.55 | 168.06 | -18.49 | -9.9% | lattice +5%&+2ms (first slot 0%→29%, period 16.667 ms fallback) | **improved** |
| | M6 reset -> first correct screen (fast) — strict (median of per-rep p50s) | ms | 180.84 | 161.98 | -18.86 | -10.4% | old gate ±22% / 2.0 | within-noise |
| | M7 UI tree with text (strict) | ms | 41.36 | 41.43 | +0.07 | +0.2% | old gate ±5.0% / 2.0 | within-noise |
| | M7 UI tree with text (fast) | ms | 0.44 | 0.64 | +0.20 | +45.8% | old gate ±34% / 2.0 | within-noise |
| | M8 tap -> settled, counter (strict) | ms | 947.32 | 949.16 | +1.84 | +0.2% | old gate ±5.0% / 2.0 | within-noise |
| | M8 tap -> settled, counter (fast) — fast (pooled 5%-trimmed mean, n=120/120) | ms | 35.73 | 40.82 | +5.08 | +14.2% | lattice +5%&+2ms (first slot 27%→18%, period 15.431 ms measured) | **REGRESSION** |
| | M8 tap -> settled, counter (fast) — strict (median of per-rep p50s) | ms | 32.39 | 47.18 | +14.79 | +45.7% | old gate ±40.0% / 2.0 | REGRESSION |
| | M8 tap -> settled, form (strict) | ms | 920.22 | 920.34 | +0.11 | +0.0% | old gate ±5.0% / 2.0 | within-noise |
| | M8 tap -> settled, form (fast) — fast (pooled 5%-trimmed mean, n=120/120) | ms | 32.42 | 41.09 | +8.68 | +26.8% | lattice +5%&+2ms (first slot 0%→4%, period 8.674 ms measured) | **REGRESSION** |
| | M8 tap -> settled, form (fast) — strict (median of per-rep p50s) | ms | 32.41 | 41.11 | +8.70 | +26.8% | old gate ±5.0% / 2.0 | REGRESSION |
| | tap acknowledgement | ms | 5.00 | 5.00 | +0.00 | +0.0% | old gate ±5.0% / 2.0 | within-noise |
| | acos_destroy teardown_ms | ms | 19.00 | 19.00 | +0.00 | +0.0% | old gate ±11% / 2.0 | within-noise |
| | cpu_active_tap_strict_pct | % of one core | 36.36 | 36.90 | +0.54 | +1.5% | old gate ±40.0% / 0.2 | within-noise |
| | cpu_idle_n10_pct | % of one core | 3.20 | 3.10 | -0.10 | -3.1% | old gate ±11% / 0.2 | within-noise |
| | cpu_idle_n10_per_session_pct | % of one core/session | 0.32 | 0.31 | -0.01 | -3.1% | old gate ±11% / 0.2 | within-noise |
| | cpu_idle_n1_pct | % of one core | 0.50 | 0.43 | -0.07 | -13.3% | old gate ±31% / 0.2 | within-noise |
| | cpu_idle_n1_per_session_pct | % of one core/session | 0.50 | 0.43 | -0.07 | -13.3% | old gate ±31% / 0.2 | within-noise |
| | cpu_idle_n5_pct | % of one core | 2.00 | 1.88 | -0.12 | -5.8% | old gate ±18% / 0.2 | within-noise |
| | cpu_idle_n5_per_session_pct | % of one core/session | 0.40 | 0.38 | -0.02 | -5.8% | old gate ±18% / 0.2 | within-noise |
| | m1_idle_mem_mib | MiB | 55.46 | 57.05 | +1.59 | +2.9% | old gate ±3.0% / 0.5 | within-noise |
| | m1_idle_pss_mib | MiB | 233.89 | 234.89 | +1.00 | +0.4% | old gate ±3.0% / 0.5 | within-noise |
| | m2_per_session_anon_shmem_n1 | MiB/session | 53.83 | 55.16 | +1.33 | +2.5% | old gate ±3.0% / 0.5 | within-noise |
| | m2_per_session_anon_shmem_n10 | MiB/session | 53.98 | 55.75 | +1.77 | +3.3% | old gate ±3.0% / 0.5 | REGRESSION |
| | m2_per_session_anon_shmem_n5 | MiB/session | 54.25 | 55.33 | +1.08 | +2.0% | old gate ±3.0% / 0.5 | within-noise |
| | m2_per_session_pss_n1 | MiB/session | 211.56 | 215.25 | +3.69 | +1.7% | old gate ±3.0% / 0.5 | within-noise |
| | m2_per_session_pss_n10 | MiB/session | 52.40 | 54.40 | +2.00 | +3.8% | old gate ±3.0% / 0.5 | REGRESSION |
| | m2_per_session_pss_n5 | MiB/session | 70.68 | 72.44 | +1.77 | +2.5% | old gate ±3.0% / 0.5 | within-noise |
| | teardown_wall_ms | ms | 19.73 | 19.67 | -0.06 | -0.3% | old gate ±13% / 2.0 | within-noise |
| | wakeups_idle_n10_per_s | voluntary ctxt switches/s | 3.10 | 3.25 | +0.15 | +4.8% | old gate ±10.0% / 0.2 | within-noise |
| | wakeups_idle_n1_per_s | voluntary ctxt switches/s | 1.21 | 1.25 | +0.03 | +2.8% | old gate ±10.0% / 0.2 | within-noise |
| | wakeups_idle_n5_per_s | voluntary ctxt switches/s | 2.06 | 2.20 | +0.13 | +6.5% | old gate ±10.0% / 0.2 | within-noise |
