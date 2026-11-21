# MULTIWIN3 (v0.4.1) — Slice 1 Implementation & Evidence (`MULTIWIN2`)

- **Branch**: `multiwin3` (base `80cc10e83` = `docs/MULTIWINDOW_PLAN.md` merged with `v0.4.0`).
- **Implementation commit**: `2f91c2c0b` (`multiwin3: slice 1 multiwindow module, ServiceHost guest-command outbox, and ART split probe`).
- **Compositor changes**: None (C1 unchanged).
- **ADR amendment**: `DECISIONS.md` ADR-0029 amendment (2026-11-18, MULTIWIN3) records **Option A — `ServiceHost` guest-command outbox** as the reusable module→guest control seam.

## Summary of Slice 1 Changes

1. **Host geometry & WMS (`crates/acos-wm`)**:
   - Added `WINDOWING_MODE_*` constants (`UNDEFINED=0`, `FULLSCREEN=1`, `PINNED=2`, `MULTI_WINDOW=6`), `SplitPosition`, `TaskRecord`, and `DisplaySpec::{px_to_dp, task_app_bounds, split_rect, task_record}` in `crates/acos-wm/src/display.rs`.
   - Added task-rect tracking on `WindowManager` (`DEFAULT_TASK_ID = 10`, `set_task_bounds`, `clear_task_bounds`, `clear_all_task_bounds`, `task_bounds`, `bind_window_task`, `task_for_window` restricted to `WindowKind::Application | WindowKind::SubWindow`) and `merged_configuration_for_task` in `crates/acos-wm/src/wms.rs`.
   - Unit test `wms::tests::task_bounds_shrink_app_frame_and_clear_restores_exact_fullscreen_bytes` verifies shrunk frames and byte-identical fullscreen restore against `reference_merged_configuration`.

2. **ServiceHost guest-command outbox & `multiwindow` module (`crates/acos-services`)**:
   - Added `GuestCommand::Window { task, process, rect, app_bounds, windowing_mode }` and `ServiceHost::{push_guest_command, drain_guest_commands}` + live `wms` handle (`crates/acos-services/src/host.rs`, `crates/acos-services/src/wms.rs`).
   - Implemented `MultiwindowModule` (`id = "multiwindow"`, `off` refuses controls; `real` implements `enter_split {position, ratio}`, `resize {ratio}`, `exit`, `status`; `bottom` refused until slice 2; non-ART / missing WMS refused with `UNSUPPORTED_ANDROID_API`) in `crates/acos-services/src/modules/multiwindow.rs` and registered it in `ModuleRegistry::standard()`.
   - Added conformance kit row in `crates/acos-services/tests/module_kit.rs`, 4 module/outbox unit tests in `crates/acos-services/tests/multiwindow_module.rs`, and regenerated `docs/MODULES.md` via `cargo run -p acos-services --example gen-modules-docs > docs/MODULES.md`.

3. **Launcher drain + guest activity override `Configuration` (`crates/acos-art`)**:
   - Peer pump drains `peer.host_mut().drain_guest_commands()` into `ServiceControlReply`; launcher `"services"` arm writes `WINDOW <mode> <l> <t> <r> <b> <al> <at> <ar> <ab>` to process 0 stdin (refusing `process != 0` until slice 2), waits for a fresh probe sample (`sample.seq > before`), and returns settled `probe_seq` + `tree` in the same control reply (`crates/acos-art/src/launch.rs`).
   - `LaunchMain.java` parses `WINDOW ...`, builds an **activity override `Configuration`** (`seq=0`, `windowConfiguration` bounds/appBounds/maxBounds/windowingMode, dp computed from host `appBounds` and real display density, display-level qualifiers left undefined), dispatches `ActivityThread.handleActivityConfigurationChanged` when the activity declares `CONFIG_SCREEN_SIZE` (or ROT4 `ActivityRelaunchItem` otherwise), forces relayout (`mForceNextWindowRelayout = true`, `decor.requestLayout()`), and posts the probe wakeup after the next `Choreographer` frame so the first probe carries settled bounds.

4. **Fixture & scenarios**:
   - `fixtures/apps/two-activity`: added `@+id/mw_state` below untouched `@+id/main_state`, overrode `onMultiWindowModeChanged`, and logged `Main.onMultiWindowModeChanged` + live `isInMultiWindowMode()` and dp size.
   - `scenarios/multiwindow-split.yaml` (17 steps): enters top 50% split, resizes to 35%, exits back to fullscreen; asserts `service_ctl` reply values, `mw_state` text, `main_state` config change count (`1 -> 2 -> 3 -> 4`), exact shrunk/restored view bounds (`[42,612,1038,2295] -> [42,612,1038,1158] -> [42,612,1038,798] -> [42,612,1038,2295]`), and `assert_no_crash`.
   - `scenarios/negative/multiwindow-split-wrong.yaml` (5 steps): enters top 50% split and asserts stale fullscreen bounds `[42,612,1038,2295]`; fails with `ASSERTION_FAILED`.

## Verification Results

### 1. Rust tests (`acos-services`, `acos-wm`, `acos-art`)

- **`cargo test -p acos-services -p acos-wm`** — **PASS** (`PIPESTATUS=0`), log: `results/handoff/MULTIWIN2/cargo-test-services-wm.log`
  - Includes `acos-services` `lib` (185 passed), `module_kit` (21 passed), `modules_doc` (2 passed), `multiwindow_module` (4 passed), and `acos-wm` `lib` (79 passed) + all 7 `acos-wm` integration test binaries.
- **`cargo test --no-fail-fast -p acos-services -p acos-art -p acos-wm`** — **1117 passed, 1 failed, 3 ignored** (`PIPESTATUS=101`), log: `results/handoff/MULTIWIN2/cargo-test.log`
  - `acos-services`: **100% PASS**
  - `acos-wm`: **100% PASS**
  - `acos-art`: **15/16 test binaries PASS** (`lib` 204 passed / 1 ignored, `sandbox_s5` 7/7, `sandbox_s6` 5/5, `w31_tree_evidence`, `w37_build_cache`, `w39_stage_cache`, `w43_session_exit`, etc.).
  - **1 pre-existing harness-sandbox failure**: `acos-art::tests/spike_net_boundary.rs::guest_cannot_change_the_lan_drop_rules` failed with:
    ```text
    bwrap: setting up uid map: Operation not permitted
    ```
    Root cause: this agent runner executes with `NoNewPrivs: 1` and `Seccomp: 2`, which prevents `pasta`'s nested `bwrap --unshare-user` from writing `/proc/self/uid_map`. Unrelated to MULTIWIN3 (not touched).

### 2. ART Scenarios (via `tools/art-runtime/run-exclusive.sh` + `tools/guarded.sh`)

Log: `results/handoff/MULTIWIN2/art-scenarios.log`

| Scenario | Expected | Actual | Steps | Evidence |
| --- | --- | --- | --- | --- |
| `scenarios/multiwindow-split.yaml` | PASS | **PASS** (`ok=true`, `EXIT=0`) | 17 | `multiwindow-split/verdict.json`, `multiwindow-split/logcat.txt`, `multiwindow-split/frames/*.png` |
| `scenarios/negative/multiwindow-split-wrong.yaml` | FAIL (`ASSERTION_FAILED`) | **FAIL as expected** (`ASSERTION_FAILED: expected bounds [42,612][1038,2295], got [42,612][1038,1158]`, `EXIT=1`) | 5 | `multiwindow-split-wrong/verdict.json`, `multiwindow-split-wrong/logcat.txt` |
| `scenarios/w15-two-activity.yaml` | PASS | **PASS** (`ok=true`, `EXIT=0`) | 3 | `art-scenarios.log` |
| `scenarios/back-layers.yaml` | PASS | **PASS** (`ok=true`, `EXIT=0`) | 22 | `art-scenarios.log` |
| `scenarios/mproc3-two-process.yaml` | PASS | **PASS** (`ok=true`, `EXIT=0`) | 17 | `art-scenarios.log` |
| `scenarios/sysui1-shade.yaml` | PASS | **PASS** (`ok=true`, `EXIT=0`) | 12 | `art-scenarios.log` |
| `scenarios/rot1-rotate.yaml` | PASS | **PASS** (`ok=true`, `EXIT=0`) | 18 | `art-scenarios.log` |

Key logcat lines from `multiwindow-split/logcat.txt`:
```text
1795264905808     86  I AcosTwo: Main.onMultiWindowModeChanged inMW=true isInMW=true cb=1 dp=411x408
1795264906641     89  I AcosTwo: Main.onMultiWindowModeChanged inMW=false isInMW=false cb=2 dp=411x914
```

## Notes & What Remains (Slices 2–5)

- **Slice 2 (second process / bottom pane + dual-pane split)**:
  - Route `GuestCommand::Window` for `process != 0` in `crates/acos-art/src/launch.rs` and lift the `position == "bottom"` refusal in `crates/acos-services/src/modules/multiwindow.rs`.
  - Verify compositor clipping/buffer reallocation for the top pane: in `multiwindow-split/frames/3-after-enter-split.png`, the `DecorView` and child views cleanly relayout to `[0,0][1080,1200]` (`411x408dp`), while the initial 1080x2400 surface buffer still shows its light-grey background below `y=1200`. When a second app renders in the bottom pane `[0,1200,1080,2400]`, check z-ordering / surface resize or crop so the top pane's stale lower pixels cannot overdraw the bottom pane.
- **Slice 3**: Multi-resume / focus switching / routed touch coordinates across split panes.
- **Slice 4**: Activity relaunch path (`!CONFIG_SCREEN_SIZE` fixtures) & multiwindow + rotation interaction.
- **Slice 5**: PiP (requires owner sign-off if C1 layer scaling is needed).
