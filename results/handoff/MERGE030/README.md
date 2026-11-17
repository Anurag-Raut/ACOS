# MERGE030 — v0.4.0 integration (modules ← main/v0.3.0)

Branch `merge030` from `modules` @ `7ad8dcb0`; merged `main` @ `02421e17` (v0.3.0).
Merge base `3c2e0750`. 115 commits from main, 190 from modules, **45 conflicted files**.

## Status: PARTIAL

The merged tree **builds** (`cargo check --workspace --all-targets`) and the whole
non-ART unit/integration sweep of the brief is **green**. The ART corpus is mostly
green; three ART items are open (below), two of them are feature seams that need a
decision, and one is an ordered-`assert_log` cursor question. One more (`net-check`)
is the known "real network arms need pasta" environment arm.

## Commits on `merge030`

- `3e8daac7` merge main (v0.3.0) into modules — one implementation per seam, schemas
  regenerated, handoff README + evidence.
- `62792fd4` probe-sample staleness per process (SETTLE5 evidence), capabilities reply
  cap keeps `commands[]`, capabilities schema accepts `session_id`, LaunchMain brace.
- `7fe7cd57` policy focus stays with the layer stack; ATMS launch/resume items are
  replays for an already-live record (merged lifecycle model).
- (HEAD after the ART fixes: see `git log --oneline modules..merge030`.)

## One implementation per seam — what was kept and why

### Settle / input / back (the false-pass-sensitive seams)

| File | Seam | Resolution (intent kept) |
|---|---|---|
| `acos-services/src/input_dispatch.rs` | modules' LAYER1 `tap_window` (hit test, window-local point) vs main's `publish_on_live` retry + `swipe` | `tap` uses `publish_on_live` (REL3I's retry when a channel's peer closed); `tap_window` keeps the LAYER1 explicit-token path; `swipe` restored from main. **Focus is policy's** (LAYER1 `set_focus`), never registration order: main's two swipe tests drain only the TOUCH_MODE registration message. |
| `acos-verify/src/executor.rs` | modules' single-sample `observation_assertion` vs main's REL3A wait+settle rule | main's version verbatim (timeout is a real wait; `settle: content` needs the content window; a settle timeout is never a pass). Modules' `StepRef::AssertWindows` arm restored on top (no `timeout_ms`, `settle: None` — its assertion type has no settle field). `ServiceCtlStep` + `SwipeDirection` imports both kept. |
| `acos-verify/src/verify.rs` | modules' `services:` map vs main's `engine`/`warnings` | both: `RunOptions` carries `services`, `engine`, `warnings`; provenance warnings computed by main's block, `services` still filled from the scenario. |
| `acos-art/src/launch.rs` | modules' `serve_with_control` (tagged conns, spawns, control pump, timed module deliveries) vs main's single-conn `serve` with REL3I3 idle wait + PERF4 settle wake | **One loop**: `serve_with_control` now takes the delivery-wake fd and a `change` callback. Idle is unbounded (`poll(-1)`) when no module deadline, wakes on the launcher's control pipe **or** the delivery wake, drains it, runs the control pump, then `flush_deliveries()` + `tick()`; every handled frame calls `change.notify()` (PERF4's own-draw bind still falls back to the shipped rule — that lives in the settle/own-draw predicate, unchanged). `service_request` (live attach/detach/ctl) kept from modules; main's local `serve` deleted. |
| `acos-cli/src/engine/art.rs` | placeholder verdict shape | main's `first_frame_placeholder_verdict` (the **window** is required: `window_spent == false` yields neither placeholder nor pass). modules' `SessionBackend::modules/module_rows_handle` + main's `bound_tree/attach_input_target` both kept. |
| `acos-cli/src/engine/serve.rs` | modules' `acos_services`/`acos_service_ctl` vs main's `acos_swipe` + `selector_of` + `reset(logs)…` | union: services + service_ctl + reset(artifacts) + swipe; `selector_of` shared by input and `until`. |
| `acos-wm/src/replay.rs` | `InsetsControlChanged` shape | main's (`ime: false`). |
| `acos-services/src/window_session.rs` | modules' `WindowStackSink` vs main's `ime_inset_delivery`/`take_ime_insets_deliveries` | both (independent additions; imports were already main's). |

### Process / module / binder

| File | Resolution |
|---|---|
| `acos-services/src/wms.rs` | modules' per-call `SessionFactory` + session-death methods, **plus** main's `attach_delivery_wake` (IME insets queue/wake). No `host_session` (modules replaced the one-session model). |
| `acos-services/src/registry.rs`, `host.rs` | modules' `SessionModules`/`NodeRegistrar`/a11y/permission/webview state **plus** main's `DeliveryWake` (host + WMS attached). `delivery_wake` field lives on `ServiceHost` only (it had been spliced into `WebviewGate` by the merge). |
| `acos-services/src/atms.rs` | modules' multi-process `processes`/`tops`/`result_to` **plus** main's `back_pressed` + `pending_reverse_calls` + `on_back_pressed`; `finishActivity` keeps modules' `(token, result_code, result_data)` signature (main's 1-arg call sites/tests updated). `schedule()` treats `LaunchActivity`/`ResumeActivity` as **replays** for a record the guest already reported live (merged lifecycle model: modules' `activityResumed` creates/starts/resumes; main's ROT7/ROT8b tests asserted "no invented transition"). |
| `acos-cli/src/verify.rs` | `explicit` from main (`args.engine_explicit()`), `modules` summary line from modules. |
| `acos-binder-peer` | `serve.rs` merged (above). `txn_heap.c/.h` + `tests/txn_heap.rs` **modules'** (it already contains FLAKEWIN's lock and adds BINDERREF's reclaim; main's is the same file without reclaim). `binder_driver.c` comments/seq/reply-block from modules (superset). `peer.rs`: modules' `take_delivery_frames`/`tick`/`next_timed_deadline` + main's `flush_deliveries` (raw IME queue) + `IdleWake::Wake` doc. |
| `acos-art/guest/java/.../LaunchMain.java` | both additions (`windowMeta` from LAYER1, `taskFinished`/`liveRecordActivity` from ROT8); the shared tail's `}` closes `windowMeta`. |
| protocol | `Engine::{Art,RustVm}` (main's vocabulary + aliases for the legacy names), bionic error codes **both** (`NETWORK_UNAVAILABLE` + `DAEMON_VERSION_MISMATCH`), `LaunchStep` carries `idle_kill_seconds` **and** `settle_timeout_ms`. Schemas regenerated with `UPDATE_SCHEMA=1`. Verdict keeps typed `input`, `warnings`, `attempts[]`, `modules[]`. |
| docs | `DECISIONS.md`, `EXPERIMENTS.md`, `AGENT_GUIDE.md`, `AOSP_REFERENCE.md`, `MCP.md` unions (no behaviour dropped). |

### Two post-merge fixes worth naming

1. **Capabilities reply cap** (`acos-protocol/src/reply.rs`): after the module catalog
   was added, `acos_capabilities` overflowed the 20 KiB reply cap and step 4 replaced the
   *whole* result with `{reply_capped}`, so `commands[]` vanished (`the commands are the
   answer`). New step 3b: when `result.commands` exists, drop `result.modules` (it has its
   own query paths: `acos_services {"op":"list"}`, `acos_help topic:limits`) and record
   `modules_omitted`/`modules_total`.
2. **`FLAG_ACTIVITY_RESET_TASK_IF_NEEDED`** (`acos-services/src/ams.rs`): the gaten
   launcher starts its next activity with `0x10200000` (`NEW_TASK|RESET_TASK_IF_NEEDED`),
   which the merged launch-flag whitelist rejected (`UNSUPPORTED … startActivity flags
   0x10200000`) so every gaten scenario crashed on launch. AOSP's reset pass has nothing
   to do in ACOS's one-task-per-process model, so the flag is honoured as that no-op
   (never silently ignored: documented in the code).

## Conflicts: 45 files, all resolved (no `<<<<<<<` left)

`git grep -n '^<<<<<<<'` is empty; `cargo check --workspace --all-targets` is green.

## Verification

### Build + unit/integration sweep (brief's command)
`cargo check --workspace --all-targets` — green.
`cargo test -p acos-art -p acos-verify -p acos-protocol -p acos-services -p acos-wm -p acos-binder-peer -p acos-mcp -p acos-cli -p acos-webview -p acos-netd`
— run under `tools/art-runtime/run-exclusive.sh` + `tools/guarded.sh`;
log: `nonart-tests.log`. **All targets green** except the leak scan
(`acos-art --test cleanup::leak_scan_is_scoped_to_this_tests_own_processes`), which
enumerates host `acos` processes and fails when another agent's session is live; it
passes in isolation (env-only). No test was ignored/weakened. The one test I *changed*
asserted the pre-MPROC5 lifecycle (`Uncreated` after a resume); it now asserts the
merged model's intent — the Back/finish call invents no transition (lifecycle stays the
`Resumed` the resume established).

### ART corpus (release `acos` + `acos-art`, `run-exclusive`, one acquisition)
Runner: `results/handoff/MERGE030/art/run-art.sh` (positiv-ART list from the brief).
Per-scenario envelopes: `art/corpus2/`, fixed reruns `art/corpus3/`, `art/corpus4/`.

| Scenario | Envelope |
|---|---|
| back-layers (22/22), back-layers-ime, rel3h-unhandled, rel3h2-expect-false, touchdisp-* (3), form-validation, rel3a-content-settle, looperwake-bg-post | pass |
| sensors-scripted/off, notif-granted/denied, location-probe/geocoder/detach, tail-telephony/biometric/bluetooth/vibrator/detached, webview-probe, net-check-offline | pass |
| gaten-backstack, gaten-lesson | pass (corpus4, after the launch-flag fix; corpus2 crashed) |
| **negatives** rel3h-unhandled-expect-change, rel3h2-expect-handled-true, rel3a-content-pixel, rel3a-content-sticky | fail/timeout **as designed** (`expectation: non_pass`); `run-matrix.sh` is the expectation-aware runner and is the place to pin `expect_error` |
| **a11y-probe** | FAIL (open): step 14 `assert_log` — the ordered cursor resumes after the previous assert's `service_connected` match, but the first pattern (`button_text …`) is printed *before* that line. The log itself has every line (`out/a11y-probe/logs/logcat.txt`). Decide the ordered-`assert_log` cursor rule (per-step scan vs global cursor) and update the scenario or the cursor. |
| **mproc3-two-process** | TIMEOUT (open): `wait_for id=="finish"` 30 s; proc-b's activity never finished. Logs: `out/mproc3-two-process/`. Next: rerun with `ACOS_ATMS_TRACE` + `ACOS_SERVICE_CTL_LOG` and compare the cross-process stop/finish deliveries. |
| **net-check / net-network-off** | environment: the real network arms need `pasta` (and `acos-netd`); `net-network-offline` passes. Not a merge verdict. |

### M8 (indicative, 5 reps) — NOT RUN here
The brief asks for an indicative shared-slot 5-rep `counter/form fast` comparison of the
merged tree against the `v0.3.0` tag. Timed A/Bs are batched by the bench owner
(COMMON_RULES), and a `v0.3.0` release tree of this worktree does not exist, so no number
is claimed. The arms are the standard ones (`tools/art-runtime/bench-*.sh`, `form fast`),
merged-tree binary already at `.cache/merge030-target/release/acos` before cleanup.

## Known environment-only facts
- ART requires `acos-art` **and** `acos` from the same tree; a stale `acos-art` silently
  keeps the old `acos-services` (this bit the gaten fix for one rerun).
- `fixtures/build/apps/*` had to be built for the module corpus (`tools/build-apps.sh`
  with `ACOS_APP_ONLY=…`): main's shared fixture cache lacks the module fixtures
  (`a11y-probe`, `tail-probe`, `location-probe`, `scroll-list`, `back-layers`,
  `webview-probe`, …).
- `webview-probe` needs `webview-provider.apk` too (built).
- The leak scan and `view` socket tests are checkout-path/process sensitive.

## Next actions (smallest first)
1. `assert_log` ordered-cursor decision → scenario or executor (a11y-probe).
2. mproc3 finish delivery: trace the cross-process finish (ATMS `pending_lifecycle_calls`
   vs MPROC8 service delivery) — one scenario, one trace.
3. Bench owner: M8 5-rep merged vs `v0.3.0` (prepare a `v0.3.0` release build in a
   scratch worktree if the comparison is required before shipping v0.4.0).
