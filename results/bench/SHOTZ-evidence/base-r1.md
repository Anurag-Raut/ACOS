# ACOS bench baseline -- 2026-11-14T18:23:45+00:00

Command: `tools/bench/acos-bench.sh --reps 1` (branch `HEAD`, commit `19db6b8a656c`).

Mode `full`; `ACOS_STRICT_SETTLE=default`; `animations=on`; binary sha256 `90b888d3f43e2bde`.

Host: AMD Ryzen 9 9950X, 32 threads, 61908 MiB. Noisy phases: 0/1 (load1 > 4 or MemAvailable < 20 GiB; recorded, not deleted).

## Metrics (p50 / p95, pooled across reps)

| metric | label | p50 | p95 | n | threshold | prev best | method |
|---|---|---:|---:|---:|---:|---|---|

## Phases and host load

| rep | phase | ok | load1 before -> after | MemAvailable | queue wait s | wall s | noisy |
|---:|---|---|---:|---:|---:|---:|---|
| 1 | latency | **no** | 0.960 -> 0.960 | 33.15 GiB | - | 0.153 | no |

## Notes

* value = p50 pooled over every sample in every rep; p95 pooled the same way; per-rep p50s are kept for the threshold derivation.
* threshold = max(category floor, 2 x worst rep-to-rep deviation), capped at 40 %; a delta must exceed both the relative and the absolute threshold to be called a regression.
* memory rows are label-matched (cgroup anon+shmem vs Pss); they are never mixed.
* M3 is measured with the launch working set evicted per rep; the warm-cache variant is M4 + daemon spawn (P7).
