# MULTIWIN1 / MULTIWIN1b (v0.4.1) — plan done, slice 1 NOT built

Branch `multiwin1b` (base `multiwin1` 880a88598). Deliverable: `docs/MULTIWINDOW_PLAN.md`.

## Result
* (a) Plan finished: AOSP model from the r75 cache, what apps assert, ACOS seams (incl. §3.5, new),
  `multiwindow` module + control ops (`enter_split{top,bottom}`, `resize{ratio}`, `exit`, `status`),
  slices 1–5 with sizes, test plan, regression set. **No ADR conflict**: no compositor change
  (C1 stays), no wire-codec change. The only architecture-adjacent item is the module→guest control
  seam (§7 decision 5, options A/B/C, A recommended); PiP scaling (slice 5) would touch C1 placement
  and stays out of scope until the owner signs off.
* (b) Slice 1 not built: `df -h /` was 87 % at the start and 88 % twenty minutes later (other tracks
  growing it), over the 85–86 % stop rule, so no cargo/ART run was started. No code was changed.
  No target dir or fixture copy was created, so nothing to delete.

## Slice 1 build order (for the next run, once df < 85 %)
1. `acos-wm`: `TaskRect`/windowing mode on `WmsState` (`set_task_bounds/clear/task_bounds`),
   `compute_frame_for_requested` takes the task rect as parent, `merged_configuration_for_task`
   (mode, bounds, appBounds, dp). Tests: task frame + `windowingMode`; clearing restores today's exact
   full-screen bytes (`reference_merged_configuration`).
2. Guest `LaunchMain.applyWindow` — **activity override** configuration, not process config
   (plan §3.5.1); reuse/extract the ROT4 relaunch block.
3. Seam per owner decision §7.5 (A: `ServiceHost` guest-command outbox, launcher drains it in the
   `services` handler, fresh-probe binding like `rotate`).
4. `acos-services/src/modules/multiwindow.rs` (+ registry row, MODULES doc regeneration, conformance
   kit), `enter_split{top}` only; `bottom` refused by name until slice 2.
5. Fixture `two-activity` (already declares `screenSize`) prints `onMultiWindowModeChanged`,
   `onConfigurationChanged`, `isInMultiWindowMode()`; `scenarios/multiwindow-shrink.yaml`.

Env: `source tools/agent-env.sh`; `ACOS_VIEW_TEST_DIR=$PWD/.cache/lv`; `tools/build-apps.sh` first;
ART only via `tools/art-runtime/run-exclusive.sh`.
Acceptance: `cargo test -p acos-services -p acos-art -p acos-wm`; `multiwindow-shrink`,
`back-layers`, `mproc3-two-process`, `sysui1-shade`, `rot1-rotate`, `layer0-overlay-stack` pass on ART.
