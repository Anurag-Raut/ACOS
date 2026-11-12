# Agent-loop benchmark (acos serve, engine art)

Run: 91f36a2d-3de2-4cbe-9d4b-1bde2113f5a3  UTC 2026-11-12T09:48:01+00:00  profile release  engine art
Git 8e816048d7fbc1c8009b2ccdf4c6fb28cc10c446 (dirty hash 6a18ad86d3770d34), binary /home/Qui/.paseo/worktrees/3oxa6xao/perf-p1-input/.cache/p1-target/release/acos sha256 b78e9018b4b0f336

Fixed script: launch -> tree -> tap -> tree -> type -> key -> screenshot -> assert -> logs -> reset
Two runs per app; one warm-up iteration per run; 50 samples per verb.

## Per-verb cost

**Read the `ok/total` column with the gate.** A latency gate can PASS
on a verb that never succeeded: that is a *fast error path*, not a
working verb. `runtime:tree_reached` / `runtime:task_reaches_verdict`
are the functional gates and they are reported separately.

| app | verb | n | p50 ms | p95 ms | response bytes p50 | tree bytes | png bytes | ok/total | ok rate | gate |
|---|---|---|---|---|---|---|---|---|---|---|
| counter | launch | 50 | 0.2 | 0.4 | 5225.0 | - | - | 50/50 | 100% | EXCLUDED |
| counter | tree | 100 | 581.3 | 1002.6 | 5063.0 | 4101.0 | - | 100/100 | 100% | FAIL |
| counter | tap | 50 | 70.9 | 87.7 | 2028.0 | - | - | 50/50 | 100% | PASS |
| counter | type | 50 | 0.1 | 0.1 | 904.0 | - | - | 0/50 | 0% | PASS |
| counter | key | 50 | 70.1 | 93.4 | 2028.0 | - | - | 50/50 | 100% | PASS |
| counter | screenshot | 50 | 36.4 | 39.6 | 1156.0 | - | 9545882.0 | 50/50 | 100% | PASS |
| counter | assert | 50 | 202.6 | 221.1 | 5062.0 | 4100.0 | - | 50/50 | 100% | PASS |
| counter | logs | 50 | 0.4 | 0.7 | 30847.5 | - | - | 50/50 | 100% | PASS |
| counter | reset | 50 | 634.6 | 730.7 | 1054.0 | - | - | 50/50 | 100% | EXCLUDED |
| form-validation | launch | 50 | 0.2 | 0.4 | 5701.0 | - | - | 50/50 | 100% | EXCLUDED |
| form-validation | tree | 100 | 186.8 | 188.5 | 5531.0 | 4538.0 | - | 100/100 | 100% | PASS |
| form-validation | tap | 50 | 70.8 | 218.3 | 2053.0 | - | - | 50/50 | 100% | PASS |
| form-validation | type | 50 | 56.5 | 218.9 | 2231.0 | - | - | 50/50 | 100% | PASS |
| form-validation | key | 50 | 55.9 | 217.2 | 2053.0 | - | - | 50/50 | 100% | PASS |
| form-validation | screenshot | 50 | 34.9 | 37.6 | 1195.0 | - | 9545882.0 | 50/50 | 100% | PASS |
| form-validation | assert | 50 | 186.3 | 188.9 | 5530.0 | 4537.0 | - | 50/50 | 100% | PASS |
| form-validation | logs | 50 | 0.3 | 0.3 | 13891.5 | - | - | 50/50 | 100% | PASS |
| form-validation | reset | 50 | 653.5 | 665.4 | 1085.0 | - | - | 50/50 | 100% | EXCLUDED |

## Gates

| gate | result |
|---|---|
| counter:launch:p95<500ms | **EXCLUDED** |
| counter:tree:p95<500ms | **FAIL** |
| counter:tap:p95<500ms | **PASS** |
| counter:type:p95<500ms | **PASS** |
| counter:key:p95<500ms | **PASS** |
| counter:screenshot:p95<500ms | **PASS** |
| counter:assert:p95<500ms | **PASS** |
| counter:logs:p95<500ms | **PASS** |
| counter:reset:p95<500ms | **EXCLUDED** |
| form-validation:launch:p95<500ms | **EXCLUDED** |
| form-validation:tree:p95<500ms | **PASS** |
| form-validation:tap:p95<500ms | **PASS** |
| form-validation:type:p95<500ms | **PASS** |
| form-validation:key:p95<500ms | **PASS** |
| form-validation:screenshot:p95<500ms | **PASS** |
| form-validation:assert:p95<500ms | **PASS** |
| form-validation:logs:p95<500ms | **PASS** |
| form-validation:reset:p95<500ms | **EXCLUDED** |
| counter:tree_payload<=64KiB | **PASS** |
| task:round_trips<=12 | **PASS** |
| runtime:task_reaches_verdict | **PASS** |
| runtime:tree_reached | **PASS** |
| overall | **FAIL** |

## Gaps

### counter-tree-latency (latency)

- gate: `counter:tree:p95<500ms`  verbs: tree
- evidence: `{"n": 100, "p50_ms": 581.2845, "p95_ms": 1002.6235}`
- biggest cost: p50 581.3 ms; error-path samples 0/100; response bytes p50 5063.0; transport floor ping p50 0.01 ms; control tree p50 180.97 ms, frame-only p50 29.56 ms; biggest cost is the guest tree probe + JSON serialization (bounded by tree control minus ping)

### requested-scenario-missing (harness)

- gate: `scenario`  verbs: launch
- evidence: `{"present_alternatives": ["scenarios/v2-art-counter.yaml", "scenarios/v1-counter.yaml"], "requested": "scenarios/v2-counter.yaml"}`
- biggest cost: the counter loop used scenarios/v2-art-counter.yaml

### serve-assert-verb-missing (missing-verb)

- gate: `task:round_trips<=12`  verbs: assert
- evidence: `{"serve_session_tools": ["acos_launch", "acos_observe_tree", "acos_tap", "acos_key", "acos_text", "acos_swipe", "acos_logs", "acos_status", "acos_reset", "acos_destroy", "acos_doctor"]}`
- biggest cost: assertion needs an extra observe_tree round-trip client-side; serve has no assert/capture verb

### serve-screenshot-verb-missing (missing-verb)

- gate: `screenshot`  verbs: screenshot
- evidence: `{"dedicated_tool": null, "screenshot_via": "acos_observe {png:true, screenshot:path}"}`
- biggest cost: the PNG is written host-side by observe; encode vs guest capture cannot be separated without a frame

## Controls (cost decomposition)

**counter**

- ping_ms: n=5 p50=0.0 p95=0.0 ms
- tree_ms: n=5 p50=181.0 p95=186.3 ms
- frame_ms: n=5 p50=29.6 p95=32.2 ms
- frame available: True

**form-validation**

- ping_ms: n=5 p50=0.0 p95=0.0 ms
- tree_ms: n=5 p50=179.9 p95=184.6 ms
- frame_ms: n=5 p50=28.4 p95=30.5 ms
- frame available: True

## Host

loadavg1 0.67, MemAvailable 48871.1 MiB, host gate ok=True
