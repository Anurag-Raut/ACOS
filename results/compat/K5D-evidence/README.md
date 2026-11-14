# K5D evidence — F1 v1-block caps + F2 fail-closed KeyGenParameterSpec reflection

Real ART (API 34), host-wide lock `run-exclusive.sh --task K5D` + `guarded.sh`.
Tree: worktree `.paseo/worktrees/3oxa6xao/s3ab`, branch `compat-k5-keystore`,
target `.cache/s3ab-target`.

| file | content |
|---|---|
| `fidelity.jsonl` | 38/38 checks (`[k5b] fidelity: 38/38 checks pass`) |
| `probe.jsonl` | cross-session 8/8 (`k_a absent`, four paths, controls) |
| `default_off.jsonl` | 2/2 (`AndroidKeyStore not found`, no provider) |
| `summary.tsv` | `fidelity PASS`, `xsession PASS`, `default_off PASS`, mode `600` |
| `progress.log` | host-side snapshots taken while the guests were alive |
| `run.log` | full suite log (`[k5b] RESULT: PASS`) |

Harness: a scratch copy of `tools/keystore-k5b/run.sh` (relocated worktree,
target, output, `ACOS_ART_DATA` and the main-checkout lock path); see
`results/handoff/K5D.md`.

`cargo test --release --offline -p acos-apk`: 21/21 lib (incl. the 3 new cap
tests) + all integration suites green.
