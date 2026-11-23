# METRO3b — React Native Metro dev-server loop on ACOS

**Branch**: `metro3b` (rebased/merged with `origin/modules` `6268710f6`).  
**Owner decision (A)**: Fast Refresh (`/hot`) is the deterministic default dev loop; explicit full `/reload` (`POST /reload`) is best-effort and excluded from gate matrices due to the RN 0.81 legacy-architecture reload race (`results/handoff/METRO2/REPORT.md` §2).

## Evidence files (< 200 KB total)

| File | Contents |
|---|---|
| `fast-refresh-5x.json` | 5/5 `scenarios/metro-reload.yaml` results + negative control + post-run process check |
| `metro-reload-rep1.verdict.json` | Representative full verdict JSON for `scenarios/metro-reload.yaml` |
| `metro-reload-wrong-text.verdict.json` | Verdict JSON for `scenarios/negative/metro-reload-wrong-text.yaml` (`status: fail`) |
| `full-reload-5x.json` | 5 runs of `scenarios/metro-reload-full.yaml` (honest counts, root tags, node counts, process checks) |
| `metro-reload-full-pass.verdict.json` | Representative verdict JSON for `scenarios/metro-reload-full.yaml` |
| `regressions-and-timeout.json` | Real-Metro timeout cleanup proof + 7/7 real-ART regression scenario results |
| `doctor-metro-checks.json` | `acos doctor --json` output with Metro present, `node_modules` missing, and `node` missing |

## Review verdict on WIP commit `440118360`

**Verdict: KEEP** (both parts are required and verified):

1. `crates/acos-cli/src/engine/mod.rs`: fixes a real compile error from `4d9e10152` (`acos_netd_ctl::ProtoError` does not implement `Display`; formats via `error.message`).
2. `crates/acos-netd/src/{forward.rs,main.rs,supervisor.rs}`: replaces the racy 30 ms fixed sleep in `start_forwarder` with an atomic `--ready <path>` marker written immediately after `TcpListener::bind` succeeds (`serve_ready`), polled up to 5 s with fast-fail on child exit and marker cleanup on every path, and removes dead duplicate `Relay::start_forwarder`. Required so the per-session `127.0.0.1:8081` loopback forwarder is guaranteed bound before the guest fetches the initial Metro bundle. Verified by `cargo test --offline -p acos-netd` (11/11 pass).

## Key METRO3b fix: deferred ART boot for pre-launch `dev_server`

Before METRO3b, `crates/acos-cli/src/verify.rs` spawned `ArtSession` eagerly before `acos_verify::verify` executed step 0 (`dev_server: {op: start}`). Consequently `rn-debug.apk` executed `Application.onCreate` and failed connecting to `localhost:8081` before `expose_host_port(8081)` or Metro started.

`DeferredArtSession` (`crates/acos-cli/src/engine/art.rs`) + `scenario_starts_dev_server_before_launch` (`crates/acos-cli/src/verify.rs`) defer ART guest startup until the scenario's `launch` step runs, while preserving `expose_host_port`, settle settings, metadata (`session_id`, `session_boot_ms`, `module_rows`), and teardown on every exit path.

## Acceptance results (real ART)

### 1. `dev_server` step, schema/docs, cargo tests, and orphan-process proof

- Schema & guide: `crates/acos-protocol/schema/scenario.json` (`DevServerOp`, `DevServerStep`, `TreeSizeAssertion`) and `docs/AGENT_GUIDE.md` (step table + `### React Native Metro dev loop` section).
- Cargo tests (all offline):
  - `ACOS_ALLOW_ART_SKIP=1 cargo test --offline -p acos-protocol` — **PASS** (133 tests passed across unit, schema, golden, invariants, roundtrip, scenarios; note: without `ACOS_ALLOW_ART_SKIP=1`, `every_checked_in_scenario_parses_and_validates` fails only on unrelated optional APKs not built in this worktree such as `camerax-probe.apk`, while all three `metro-reload*` scenarios parse, validate, and resolve `rn-debug.apk`).
  - `cargo test --offline -p acos-verify` — **PASS** (126 unit + 45 integration tests in `tests/scenarios.rs`, including new `process_group_teardown_kills_descendant_workers_on_drop` proving leader + descendant workers in `process_group(0)` are killed on `Drop`).
  - `cargo test --offline -p acos-netd` — **PASS** (11 tests).
  - `cargo test --offline -p acos-cli --bin acos detects_dev_server_start_before_launch` & `doctor` — **PASS**.
  - `cargo test --offline -p acos-services --test module_kit --test modules_doc` — **PASS** (22 + 2).
- Orphan-process proof (every exit path):
  - Normal exit (`dev_server: stop`): 5/5 `metro-reload` + 5/5 `metro-reload-full` -> every recorded Metro PID absent in `/proc/<pid>`, `0` `node` Metro processes remaining.
  - Failure exit (without reaching `dev_server: stop`): `scenarios/negative/metro-reload-wrong-text.yaml` -> Metro PID `115348` absent after exit, `0` remaining.
  - Timeout exit (real Metro `wait_serves` timeout, without reaching `dev_server: stop`): `timeout-cleanup-probe` -> `error_code: TIMEOUT`, Metro PID `123559` absent after exit, `0` remaining (`regressions-and-timeout.json`).
  - Drop/worker teardown unit test: `process_group_teardown_kills_descendant_workers_on_drop` -> leader and child worker both terminated, scratch directory deleted.

### 2. Fast Refresh (`scenarios/metro-reload.yaml`) — 5/5 PASS

Command (each rep):
```sh
source tools/agent-env.sh
env -u TMPDIR tools/guarded.sh --mem 8G --time 300 -- \
  .cache/target/debug/acos verify scenarios/metro-reload.yaml \
  --engine art --keep-artifacts all --artifacts <dir> --overwrite --json
```

| Rep | Status | Wall (ms) | Nodes (before -> after) | Final text | Roots | Warnings | Metro PID alive after |
|---|---|---|---|---|---|---|---|
| rep1 | `pass` (12/12) | 5443 | 29 -> 18 | `["Hello Metro"]` | 1 | 0 | `false` (`112740`) |
| rep2 | `pass` (12/12) | 5443 | 29 -> 18 | `["Hello Metro"]` | 1 | 0 | `false` (`113244`) |
| rep3 | `pass` (12/12) | 5443 | 29 -> 18 | `["Hello Metro"]` | 1 | 0 | `false` (`113892`) |
| rep4 | `pass` (12/12) | 5513 | 29 -> 18 | `["Hello Metro"]` | 1 | 0 | `false` (`114236`) |
| rep5 | `pass` (12/12) | 5419 | 29 -> 18 | `["Hello Metro"]` | 1 | 0 | `false` (`114971`) |

*(Node count drops 29 -> 18 because RN's 11-node Legacy Architecture LogBox banner is visible at initial mount and clears on HMR; single root tree with exact `"Hello Metro"` and absent `"Hello ACOS"`.)*

### 3. Negative control (`scenarios/negative/metro-reload-wrong-text.yaml`) — FAIL (expected)

- Exit code `1`, `status: "fail"`, `verdict: "fail"`, wall `20019 ms`.
- Failed at step 7 (`assert_text`): `ELEMENT_NOT_FOUND` — `selector text=="Hello Nowhere" matched no node`.
- Metro PID `115348` terminated automatically on failure despite never reaching `dev_server: stop`; `0` orphan Metro processes.

### 4. Full reload (`scenarios/metro-reload-full.yaml`) — 5 runs (best-effort)

| Rep | Status | Wall (ms) | Nodes (before -> after) | Final text | Roots | `explicit id` errors | Metro PID alive after |
|---|---|---|---|---|---|---|---|
| rep1 | `pass` (12/12) | 5730 | 29 -> 29 | `["Hello Metro"]` | 2 | 0 | `false` (`117540`) |
| rep2 | `pass` (12/12) | 5612 | 29 -> 29 | `["Hello Metro"]` | 2 | 0 | `false` (`117986`) |
| rep3 | `pass` (12/12) | 5807 | 29 -> 29 | `["Hello Metro"]` | 2 | 0 | `false` (`118384`) |
| rep4 | `pass` (12/12) | 5758 | 29 -> 29 | `["Hello Metro"]` | 2 | 0 | `false` (`119003`) |
| rep5 | `pass` (12/12) | 5766 | 29 -> 29 | `["Hello Metro"]` | 2 | 0 | `false` (`119351`) |

**Honest counts**: 5/5 passed, 0 doubled trees in this 5-run sample (assertions untouched; `CatalystInstanceImpl.destroy()` + second `Running "rnhello" with {"rootTag":21}` executed on every run). **Still marked best-effort and excluded from gate matrices** (`tools/run-matrix.sh` defaults to `v2-art-counter.yaml` + `scenarios/negative/*.yaml`) because logcat still shows RN 0.81's legacy-arch `Tried to enqueue runnable on already finished thread: 'js... dropping Runnable.` race window documented in `results/handoff/METRO2/REPORT.md` §2.

### 5. Offline deps & `acos doctor`

- All Node/Metro dependencies came from the local cache (`.cache/rn-0.81.4/jsproject/node_modules`); no network downloads occurred.
- `acos doctor --json` (`doctor-metro-checks.json`):
  - Present: `status: "ok"`, `node v24.19.0`, `node_modules .../.cache/rn-0.81.4/jsproject/node_modules`.
  - Missing `node_modules` (`ACOS_METRO_NODE_MODULES=/nonexistent/node_modules`): `status: "warn"` with explicit `fix:` naming `tools/build-rn-hello.sh fetch` and `ACOS_METRO_NODE_MODULES`.
  - Missing `node` (`PATH=/nonexistent`): `status: "warn"`, `` `node` is not runnable (...); install Node.js `` with explicit `fix:`.

### 6. Regressions (real ART + services tests) — 7/7 PASS

| Scenario | Status | Wall (ms) |
|---|---|---|
| `scenarios/v1-hello-views.yaml` | `pass` | 2964 |
| `scenarios/w15-rn-hello.yaml` | `pass` | 276 |
| `scenarios/w19-rn-wait-timeout.yaml` | `pass` | 551 |
| `scenarios/w21-rn-hello.yaml` | `pass` | 844 |
| `scenarios/w23-rn-hello-tap.yaml` | `pass` | 1488 |
| `scenarios/net-network-off.yaml` | `pass` | 687 |
| `scenarios/modfill-wifi.yaml` | `pass` | 24626 |

Plus `cargo test --offline -p acos-services --test module_kit --test modules_doc` — **22/22 + 2/2 PASS**.
