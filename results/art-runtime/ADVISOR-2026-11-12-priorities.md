# Advisor (Opus 5.5 medium), 2026-11-12 ~00:55 UTC: priorities after W26
Most under-covered risk: metrics vs legacy ACVM (171/155 MiB vs 130 MiB; tap->frame ~2 s; debug builds, 1 rep).
Order: W29 release-build bench vs ACVM (N=1/10/20 x3, breakdown table); W30 tap->frame and window-stack memory cuts from W29 breakdown (target <130 MiB at N=10, tap->frame <300 ms);
W31 verdict correctness (matrix x3, flake root cause, todo-list row tap, host-observed evidence needed for pass, negative test).
Fifth slot: sandbox s5 gates 1,4,6 -> CERTIFIED-WITH-LIMITS. Defer ARM64/Compose, SF cost comparison, phone parity.
Decision: follow; W29 starts when W27/W28 free the ART lock (bench holds it long).
