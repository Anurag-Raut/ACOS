# Agent-loop benchmark (acos serve, engine art)

Run: 96d94c92-979f-4ac0-8c2a-1ba534e5383d  UTC 2026-11-12T05:55:09+00:00  profile release  engine art
Git cfea8d5b73b0de9b7d7549245ac595bf4c6262c2 (dirty hash 27552630ebd3e319), binary /home/Qui/Projects/acos/.cache/w55-target/release/acos sha256 b9e2485270a0919b

Fixed script: launch -> tree -> tap -> tree -> type -> key -> screenshot -> assert -> logs -> reset
Two runs per app; one warm-up iteration per run; 20 samples per verb.

## Per-verb cost

**Read the `ok/total` column with the gate.** A latency gate can PASS
on a verb that never succeeded: that is a *fast error path*, not a
working verb. `runtime:tree_reached` / `runtime:task_reaches_verdict`
are the functional gates and they are reported separately.

| app | verb | n | p50 ms | p95 ms | response bytes p50 | tree bytes | png bytes | ok/total | ok rate | gate |
|---|---|---|---|---|---|---|---|---|---|---|
| counter | launch | 20 | 0.2 | 0.2 | 5222.0 | - | - | 20/20 | 100% | EXCLUDED |
| counter | tree | 40 | 253.3 | 256.2 | 5060.0 | 4101.0 | - | 40/40 | 100% | PASS |
| counter | tap | 20 | 1135.7 | 1144.0 | 1447.0 | - | - | 20/20 | 100% | FAIL |
| counter | type | 20 | 0.1 | 0.1 | 903.0 | - | - | 0/20 | 0% | PASS |
| counter | key | 20 | 385.4 | 388.1 | 1450.0 | - | - | 20/20 | 100% | PASS |
| counter | screenshot | 20 | 100.9 | 103.5 | 1126.5 | - | 9545882.0 | 20/20 | 100% | PASS |
| counter | assert | 20 | 253.3 | 255.1 | 5059.0 | 4100.0 | - | 20/20 | 100% | PASS |
| counter | logs | 20 | 0.4 | 0.7 | 30708.0 | - | - | 20/20 | 100% | PASS |
| counter | reset | 20 | 709.1 | 728.3 | 1049.0 | - | - | 20/20 | 100% | EXCLUDED |
| form-validation | launch | 20 | 0.2 | 0.3 | 5698.5 | - | - | 20/20 | 100% | EXCLUDED |
| form-validation | tree | 40 | 253.0 | 256.1 | 5529.0 | 4538.0 | - | 40/40 | 100% | PASS |
| form-validation | tap | 20 | 294.5 | 314.3 | 1470.5 | - | - | 20/20 | 100% | PASS |
| form-validation | type | 20 | 284.6 | 290.2 | 1739.5 | - | - | 20/20 | 100% | PASS |
| form-validation | key | 20 | 283.9 | 286.7 | 1471.5 | - | - | 20/20 | 100% | PASS |
| form-validation | screenshot | 20 | 86.1 | 92.3 | 1166.5 | - | 9545882.0 | 20/20 | 100% | PASS |
| form-validation | assert | 20 | 403.8 | 405.3 | 5528.5 | 4537.0 | - | 20/20 | 100% | PASS |
| form-validation | logs | 20 | 0.3 | 0.3 | 13759.5 | - | - | 20/20 | 100% | PASS |
| form-validation | reset | 20 | 723.1 | 773.6 | 1082.5 | - | - | 20/20 | 100% | EXCLUDED |

## Gates

| gate | result |
|---|---|
| counter:launch:p95<500ms | **EXCLUDED** |
| counter:tree:p95<500ms | **PASS** |
| counter:tap:p95<500ms | **FAIL** |
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

### counter-tap-latency (latency)

- gate: `counter:tap:p95<500ms`  verbs: tap
- evidence: `{"n": 20, "p50_ms": 1135.7420000000002, "p95_ms": 1143.9599}`
- biggest cost: p50 1135.7 ms; error-path samples 0/20; response bytes p50 1447.0; transport floor ping p50 0.01 ms; control tree p50 231.74 ms, frame-only p50 79.11 ms; biggest cost is the guest input dispatch plus its probe

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
- tree_ms: n=5 p50=231.7 p95=243.3 ms
- frame_ms: n=5 p50=79.1 p95=79.6 ms
- frame available: True

**form-validation**

- ping_ms: n=5 p50=0.0 p95=0.1 ms
- tree_ms: n=5 p50=232.6 p95=233.1 ms
- frame_ms: n=5 p50=80.0 p95=80.2 ms
- frame available: True

## Host

loadavg1 1.88, MemAvailable 42101.7 MiB, host gate ok=True
