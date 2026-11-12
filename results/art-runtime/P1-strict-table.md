# Agent-loop benchmark (acos serve, engine art)

Run: 0a4816d8-7fd8-4d1a-bb93-387070bd106c  UTC 2026-11-12T09:52:48+00:00  profile release  engine art
Git 8e816048d7fbc1c8009b2ccdf4c6fb28cc10c446 (dirty hash 7317229ac818b09b), binary /home/Qui/.paseo/worktrees/3oxa6xao/perf-p1-input/.cache/p1-target/release/acos sha256 b78e9018b4b0f336

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
| counter | tree | 100 | 186.7 | 194.3 | 5063.0 | 4101.0 | - | 100/100 | 100% | PASS |
| counter | tap | 50 | 1064.7 | 1080.0 | 2040.0 | - | - | 50/50 | 100% | FAIL |
| counter | type | 50 | 0.1 | 0.1 | 904.0 | - | - | 0/50 | 0% | PASS |
| counter | key | 50 | 315.7 | 326.5 | 2037.0 | - | - | 50/50 | 100% | PASS |
| counter | screenshot | 50 | 36.3 | 42.2 | 1156.0 | - | 9545882.0 | 50/50 | 100% | PASS |
| counter | assert | 50 | 186.7 | 191.1 | 5062.0 | 4100.0 | - | 50/50 | 100% | PASS |
| counter | logs | 50 | 0.4 | 0.6 | 30847.0 | - | - | 50/50 | 100% | PASS |
| counter | reset | 50 | 659.2 | 680.7 | 1054.0 | - | - | 50/50 | 100% | EXCLUDED |

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
| counter:tree_payload<=64KiB | **PASS** |
| task:round_trips<=12 | **PASS** |
| runtime:task_reaches_verdict | **PASS** |
| runtime:tree_reached | **PASS** |
| overall | **FAIL** |

## Gaps

### counter-tap-latency (latency)

- gate: `counter:tap:p95<500ms`  verbs: tap
- evidence: `{"n": 50, "p50_ms": 1064.6605, "p95_ms": 1079.9886000000001}`
- biggest cost: p50 1064.7 ms; error-path samples 0/50; response bytes p50 2040.0; transport floor ping p50 0.02 ms; control tree p50 185.89 ms, frame-only p50 33.82 ms; biggest cost is the guest input dispatch plus its probe

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

- ping_ms: n=5 p50=0.0 p95=0.1 ms
- tree_ms: n=5 p50=185.9 p95=187.3 ms
- frame_ms: n=5 p50=33.8 p95=35.3 ms
- frame available: True

## Host

loadavg1 0.36, MemAvailable 48728.3 MiB, host gate ok=True
