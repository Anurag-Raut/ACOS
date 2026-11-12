# Z7 mechanism evidence (smoke runs, 2026-11-12)

## EARLY_RELEASE markers (guest stdout)
### /home/Qui/Projects/acos/.cache/z7-smoke/apc1/s0/guest-0.out
ACOSLAUNCH EARLY_RELEASE start gc=true trim=true delay_ms=0 gc_count_before=3
ACOSLAUNCH EARLY_RELEASE done ms=4 gc_count_after=4

### /home/Qui/Projects/acos/.cache/z7-smoke/cpc1/s0/guest-0.out
ACOSLAUNCH EARLY_RELEASE start gc=true trim=true delay_ms=0 gc_count_before=0
ACOSLAUNCH EARLY_RELEASE done ms=0 gc_count_after=0

### /home/Qui/Projects/acos/.cache/z7-smoke/apd0/s0/guest-0.out
ACOSLAUNCH EARLY_RELEASE start gc=true trim=true delay_ms=0 gc_count_before=3
ACOSLAUNCH EARLY_RELEASE done ms=0 gc_count_after=3

### /home/Qui/Projects/acos/.cache/z7-smoke/apd2000/s0/guest-0.out
ACOSLAUNCH EARLY_RELEASE start gc=true trim=true delay_ms=2000 gc_count_before=4
ACOSLAUNCH EARLY_RELEASE done ms=2000 gc_count_after=4

## requestConcurrentGC vs System.gc
apd0  (System.gc only, GC delay 0): gc_count 3 -> 3, no Pss change
apc1  (System.gc + requestConcurrentGC): gc_count 3 -> 4, N=1 whole-system Pss 186.0 MiB
cpc1  (zygote + requestConcurrentGC): N=1 whole-system Pss 205.3 MiB
apd2000 (same hook delayed 2000 ms): gc_count 4 -> 4 at the request

## 1 Hz series
c-series.json / cp-series.json: per-process smaps_rollup Pss each second after the last ready, N=2, before any tap.
