# v0.2.1 A/B: int-021 vs main 74e4ee9 (v0.2.0) — 2026-11-15

Interleaved `acos-bench.sh --reps 1 --groups "a:latency:8G:300"`. Main arm built in place in
`.cache/wt-abmain3` at 74e4ee9 (compile paths checked with `strings`). Candidate: 3 reps at a3ce9f2
(before RNTAP/SHOTZ/FB4), 5 reps at 52beccf (final code before the version bump); main 9 reps.
Comparison: `tools/bench/ab-compare.py` (PERF-REL3; these JSONs predate raw samples, so the old gate only).

| metric | main (n=9) | v0.2.1 (n=5) | Δ | verdict |
|---|---:|---:|---:|---|
| M4 launch strict | 306.34 | 306.01 | −0.1 % | parity |
| M4 launch fast | 163.07 | 170.96 | +4.8 % (+7.9 ms) | within gate; per-rep p50 flips 155↔171↔187 on the frame lattice; PERF-REL3 40-sample A/B: +1.0 % trimmed, first-slot 44 vs 43 % → noise |
| M5 screenshot | 26.28 | 11.24 | −57.2 % | improved (SHOTZ) |
| M6 reset strict / fast | 339.78 / 198.40 | 343.62 / 195.30 | +1.1 % / −1.6 % | parity |
| M7 tree strict / fast | 41.33 / 0.52 | 41.32 / 0.52 | 0 % | parity |
| M8 tap counter strict / fast | 961.09 / 48.41 | 950.74 / 50.42 | −1.1 % / +4.1 % (+2.0 ms) | parity |
| M8 tap form strict / fast | 948.59 / 45.10 | 933.62 / 32.52 | −1.6 % / −27.9 % | improved |
| active CPU (strict taps) | 46.41 % | 44.49 % | −4.1 % | parity |
| teardown / wall | 13.0 / 18.95 | 13.0 / 19.30 | 0 / +1.8 % | parity |

No regression under either gate (ADR-0028). One candidate rep ran at host load 11.75 (other agents);
it is included.

Correctness on the final commit (3d492e0 + lock): all crate tests green (protocol 77, apk 47, wm 88,
cli 203, services 334, verify 117, mcp 102, vm 214, runtime 116, art 186); 15 gates exactly as
expected; Gaten 5 flows pass + 5 negatives ASSERTION_FAILED; GR4 4 + 4 (one gr4-recognition
GUEST_EXITED "guest ended before first live frame (exit 0)" under load, 2/2 reruns pass — flake
queued); rot1-rotate pass, rot1-rotate-wrong ASSERTION_FAILED, todo-list pass; installed-prefix MCP
smoke counter + gaten ok from an empty dir, private daemon = the install's own
`share/acos/0.2.1/bin/acos`.
