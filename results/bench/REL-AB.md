# Release A/B: rel1c-release (REL1a + REL1b + REL1c) vs main cb5bd04 — 2026-11-14

Interleaved, `acos-bench.sh --reps 1 --groups "a:latency:8G:300"`, `--exclusive`, 2 reps per arm. Main arm built in
place (`.cache/wt-abmain3` at cb5bd04). Branch 145230/150025, main 145003/145628. Medians of per-rep p50s; gate +5 %
and +2 ms.

| metric | main | release | Δ |
|---|---:|---:|---:|
| M4 launch strict | 306.45 | 306.55 | 0.0 % |
| M4 launch fast | 163.16 | 163.94 | +0.5 % |
| M5 screenshot | 24.26 | 24.33 | +0.3 % |
| M6 reset strict | 329.06 | 334.23 | +1.6 % |
| M6 reset fast | 186.61 | 191.94 | +2.9 % |
| M7 tree strict | 41.36 | 41.32 | −0.1 % |
| M8 tap counter strict | 956.91 | 960.20 | +0.3 % |
| M8 tap counter fast | 44.94 | 46.78 | +4.1 % (+1.84 ms, under both gates) |
| M8 tap form strict | 940.88 | 947.51 | +0.7 % |
| M8 tap form fast | 45.70 | 45.12 | −1.3 % |
| teardown | 13.0 | 13.0 | 0 |

No regression. Release rep 2 had one fail-closed launch (`m4-fast-13`: `NO_FRESH_FRAME` / `POST_RESUME_NOT_READY`,
`draw_skip=traversal:view_not_visible`, 1 of ~80 launches) — the S3 view-not-visible class (results/bench/S3.md),
labelled, never a pass; not attributable to path resolution or the memory admission check.

Correctness (`.cache/int/verify-rel.out` on rel+main 1ead474, `verify-rel2.out` on rel+main cb5bd04): acos-protocol 64,
acos-apk 39, acos-wm 80, acos-cli 173, acos-services 331, acos-verify 80, acos-mcp 79 (after the test helper honours
ACOS_BIN/CARGO_TARGET_DIR), acos-art 167 pass; 15 gates (10 pass + 5 designed failures with their codes), 5 Gaten flows
pass + 5 negatives ASSERTION_FAILED, GR4 4 + 4. acos-vm 1 and acos-runtime 6 failures are identical on main cb5bd04
(frozen legacy Rust VM crates; queued LEGACYVM). 0 false passes.

Installed-preview checks (~/.local, 2026-11-14): `acos doctor` healthy; `acos verify scenarios/gaten-home.yaml --engine
art` from ~/Projects/gaten pass (14 steps); two concurrent installed `acos-mcp` clients (default session id s_mcp each,
separate dirs) each launched counter, tapped 0→1 and destroyed cleanly, no leftover guest.
