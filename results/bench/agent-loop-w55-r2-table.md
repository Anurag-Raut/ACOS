# Agent-loop benchmark (acos serve, engine art)

Run: e06fa779-3d52-42dc-afda-58fac8e7815a  UTC 2026-11-12T06:04:09+00:00  profile release  engine art
Git be9896acb8073ffc8d88e5a8bd9e9dc6c17e332e (dirty hash 0e1276cfb22950ef), binary /home/Qui/Projects/acos/.cache/w55-target/release/acos sha256 19ef0e851d4f16f8

Fixed script: launch -> tree -> tap -> tree -> type -> key -> screenshot -> assert -> logs -> reset
Two runs per app; one warm-up iteration per run; 20 samples per verb.

## Per-verb cost

**Read the `ok/total` column with the gate.** A latency gate can PASS
on a verb that never succeeded: that is a *fast error path*, not a
working verb. `runtime:tree_reached` / `runtime:task_reaches_verdict`
are the functional gates and they are reported separately.

| app | verb | n | p50 ms | p95 ms | response bytes p50 | tree bytes | png bytes | ok/total | ok rate | gate |
|---|---|---|---|---|---|---|---|---|---|---|
| counter | launch | 20 | 0.2 | 0.3 | 5222.0 | - | - | 20/20 | 100% | EXCLUDED |
| counter | tree | 40 | 253.5 | 255.3 | 5060.0 | 4101.0 | - | 40/40 | 100% | PASS |
| counter | tap | 20 | 1140.1 | 1142.1 | 1447.0 | - | - | 20/20 | 100% | FAIL |
| counter | type | 20 | 0.1 | 0.1 | 903.0 | - | - | 0/20 | 0% | PASS |
| counter | key | 20 | 385.3 | 387.3 | 1450.0 | - | - | 20/20 | 100% | PASS |
| counter | screenshot | 20 | 102.2 | 104.4 | 1127.0 | - | 9545882.0 | 20/20 | 100% | PASS |
| counter | assert | 20 | 253.0 | 254.3 | 5059.0 | 4100.0 | - | 20/20 | 100% | PASS |
| counter | logs | 20 | 0.4 | 0.5 | 30708.0 | - | - | 20/20 | 100% | PASS |
| counter | reset | 20 | 717.7 | 728.9 | 1049.0 | - | - | 20/20 | 100% | EXCLUDED |
| form-validation | launch | 20 | 0.2 | 0.2 | 5698.5 | - | - | 20/20 | 100% | EXCLUDED |
| form-validation | tree | 40 | 236.3 | 254.8 | 5529.0 | 4538.0 | - | 40/40 | 100% | PASS |
| form-validation | tap | 20 | 288.4 | 309.0 | 1470.5 | - | - | 20/20 | 100% | PASS |
| form-validation | type | 20 | 263.5 | 287.3 | 1739.5 | - | - | 20/20 | 100% | PASS |
| form-validation | key | 20 | 263.6 | 284.4 | 1471.5 | - | - | 20/20 | 100% | PASS |
| form-validation | screenshot | 20 | 82.5 | 86.0 | 1166.5 | - | 9545882.0 | 20/20 | 100% | PASS |
| form-validation | assert | 20 | 234.9 | 405.0 | 5528.5 | 4537.0 | - | 20/20 | 100% | PASS |
| form-validation | logs | 20 | 0.2 | 0.3 | 13858.5 | - | - | 20/20 | 100% | PASS |
| form-validation | reset | 20 | 687.6 | 726.9 | 1082.5 | - | - | 20/20 | 100% | EXCLUDED |

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
- evidence: `{"n": 20, "p50_ms": 1140.146, "p95_ms": 1142.1256999999998}`
- biggest cost: p50 1140.1 ms; error-path samples 0/20; response bytes p50 1447.0; transport floor ping p50 0.01 ms; control tree p50 235.59 ms, frame-only p50 80.86 ms; biggest cost is the guest input dispatch plus its probe

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
- tree_ms: n=5 p50=235.6 p95=251.1 ms
- frame_ms: n=5 p50=80.9 p95=82.1 ms
- frame available: True

**form-validation**

- ping_ms: n=5 p50=0.0 p95=0.0 ms
- tree_ms: n=5 p50=230.7 p95=232.4 ms
- frame_ms: n=5 p50=79.0 p95=82.4 ms
- frame available: True

## Host

loadavg1 0.56, MemAvailable 42175.9 MiB, host gate ok=True
