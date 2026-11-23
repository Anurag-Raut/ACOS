# CR1FIX — CodeRabbit review fixes for PR #1 (v0.4.1)

These are the owner-approved fixes from the CodeRabbit review of PR #1. The triage is recorded in the PR #1 comment and in `AGENTS.md` → "Threat model". Of 22 findings, 9 were fixed and 1 was a conditional fix that did not reproduce, so no shim change was made for it. The other 12 were declined with reasons, in the PR #1 comment.

The work was done by Hydrogen (#11, #21, #19), then Sonnet 5.5 (#22, #20, #17, #4, #5, #10, #15 probe, and a re-check of all of them). The lead verified the result on `f19de0686`.

## Findings

| # | Finding | Reproduced? (test) | Fix commit | Verified |
|---|---|---|---|---|
| 11 | finish-in-`onCreate` teardown races the launch watcher (`HOST_TRANSACTION_TIMEOUT`) | Yes. The race is timing-dependent: `crates/acos-cli/tests/cr1fix_finish_in_oncreate.rs` failed 2/3 runs with the fix disabled | `4e70512b9`, test `f19de0686` | **Completed in round 2** (see [Round 2](#round-2)). Round 1 fixed only the teardown ordering and then weakened the test to accept the intermediate `host-transaction-resumed` marker, which hid a remaining timeout. The weakening is reverted in round 2: the test again requires the final `finish_during_create=true` outcome |
| 21 | async `VISIBLE` after an ignored early `GONE` left the layer hidden | Yes: `relayout_async_visible_after_initial_gone_restores_compositor_visibility` | `41f3571ef` | Fails with the fix reverted, passes with it |
| 19 | rotated (180°) status bar fitted as a top inset, giving a zero-height frame | Yes: `fit_insets_fits_rotated_status_bar_on_its_actual_edge` | `8ae9c0a97` | Fails with the fix reverted, passes with it |
| 22 | fixture activity counted by the primary-activity adoption guard | Yes: `crates/acos-services/tests/cr1fix_primary_adopt.rs` | `390654bf2` | Fails with the guard reverted, passes with it |
| 20 | stale probe line accepted as the `WINDOW` command ack | Modelled: `a_stale_probe_with_a_higher_seq_does_not_ack_a_window_command` | `066d164f2` | Unit test and `multiwindow-split` pass. No full-stack failing run exists, because the race depends on timing |
| 17 | `g_cam_lock` held across a blocking `dequeue` | Yes: `crates/acos-art/tests/cam_sink_native.rs` (C harness `tests/native/cam_sink_test.c`, fake BufferQueue) hangs against the old shim | `5a0426248` | Passes. See [#17 design](#17-design) |
| 4 | scripted frame that cannot be delivered fell back to colour bars | Yes: `a_scripted_frame_that_cannot_be_built_is_a_capture_failure_never_color_bars` | `a92579104` | Passes. The control test `test_pattern` fallback still passes. See [#4 design](#4-design) |
| 5 | `FINALIZE_OUTPUT_CONFIGURATIONS` acknowledged success for deferred outputs | Yes: `finalize_output_configurations_is_refused_not_acknowledged` | `a92579104` | Passes. It now returns a structured `UnsupportedAndroidApi` refusal |
| 10 | token left in `sLocallyWatchedFinishes` after the 60 s local-watcher timeout | Yes: scenario `scenarios/multi-activity-late-finish.yaml` | `f6cb3dff6` | Passes. A late finish also resumes the previous activity, through the shared `runFinishSequence` |
| 15 | re-registering the same Surface disconnects the active producer | **Not reproduced end-to-end, so no shim change** | probe scenario only, `651ec9fbd` | See [#15](#15-not-reproduced) |

### #17 design

- A delivery pins its stream and runs without holding the lock.
- Closing a stream disconnects the producer first, which wakes a parked dequeue.
- Registration sets `NATIVE_WINDOW_SET_DEQUEUE_TIMEOUT(0)`, perform opcode 37, checked in this image's libgui.
- A full queue drops the frame and returns 0. The guest logs the first drop and every 128th one after it.
- In a real guest the blocking case could not be triggered: an ImageReader consumer never blocks there, and `SurfaceTexture` natives are not registered. So it is covered only by the native test.
- `scenarios/camera-hold-images.yaml` guards the app-holds-images flow. It passes on the old code and the new code.

### #4 design

- A scripted frame that the host cannot build or enqueue is reported as `onDeviceError(ERROR_CAMERA_REQUEST = 3)`, checked against the framework dex. `ERROR_CAMERA_REQUEST` covers failures detected **before the host enqueues `deliverFrameFd`**. Once the host has enqueued that call it counts the frame as delivered and queues the success callbacks. A later drop or error in the guest JNI is not revised by the host, so this is not a guest delivery guarantee.
- It is counted in the new `frames_failed`, never in `frames_delivered`.
- The test makes a frame unbuildable with a 70000×16 stream, because JPEG cannot encode a side above 65535. If stream sizes are ever bounded below that (declined finding #7), the test needs a different unbuildable frame.
- A scripted source with no frame sink still answers with metadata only. That behaviour is unchanged.

### #15 not reproduced

- **What was tried:** `scenarios/camera-rebind-same-surface.yaml` sends one ImageReader Surface through 4 successive sessions with different `OutputConfiguration`s, so the framework deletes and recreates the stream for the same Surface. Frames keep arriving on both the old and the new shim.
- **Why it cannot happen through the framework:** release always precedes the second register, and stream ids are unique, so the framework cannot send the same id twice.
- **Synthetic case:** registering the same stream id and Surface twice from C does break the fake producer. The framework never produces that sequence, so it is reported here and left unfixed.

## Not in scope / follow-ups (not regressions)

- **Other config calls still acknowledge success without acting:** #5 changed only `FINALIZE_OUTPUT_CONFIGURATIONS`. `TEAR_DOWN` and `UPDATE_OUTPUT_CONFIGURATION` still return success without doing anything. This is the same class of issue, and a candidate follow-up.
- **`scenarios/multi-activity.yaml` fails at step 13** (not in the verification list). It fails identically on a release build of `origin/modules`, so it predates this branch.

## Verification

### Lead, on `f19de0686`

- `cargo test -p acos-services -p acos-wm -p acos-art --lib` plus `cam_sink_native`, `camera_module` and `cr1fix_primary_adopt`, run with `ACOS_ALLOW_ART_SKIP=1`: **510 passed, 0 failed**.
- `cargo test -p acos-services` (full): **780 passed, 0 failed**. On the first run `apk_facts_cache` failed only because `fixtures/build/apps/dialogs-menus.apk` was missing from the worktree; it passed once the fixture was present.
- `cr1fix_finish_in_oncreate`: 3/3 pass.
- **Scenarios** (`--engine art`), all pass:
  - launcher-home, settings-intents, multiwindow-split
  - camera-preview, camera-capture, camera-scripted, camerax-probe
  - multi-activity-finish, w38-multi-activity, mproc3-two-process, rot1-rotate, gaten-backstack, v1-hello-views
  - new: multi-activity-late-finish, camera-hold-images
- **Negatives**, each fails as required with `ASSERTION_FAILED`: launcher-home-wrong, multiwindow-split-wrong, camera-scripted-wrong-checksum.

### Agent

- `cargo test --offline -p acos-services -p acos-wm -p acos-art --lib`, with `ACOS_ALLOW_ART_SKIP=1`:
  - acos-art: 207 passed
  - acos-services: 192 passed
  - acos-wm: 82 passed
  - **0 ART skips**. 1 test is ignored: it is a measurement test that needs `$SHOTZ_FRAMES`.
- `camera_module`: 25 tests, 4 of them new.
- `cam_sink_native`: 3 tests.
- New scenario `camera-rebind-same-surface` passes.

### Idle CPU

Release builds, `tools/bench/acos_bench.py --phase memory --animations on` under `run-exclusive.sh --exclusive`, `origin/modules` (`79fa67dae`) against this branch, interleaved, 4 reps each. Values are % of one core over 60 s idle.

| metric | origin/modules | CR1FIX |
|---|---|---|
| cpu_idle_n1 | 0.249, 0.232, 0.216, 0.216 | 0.199, 0.199, 0.199, 0.183 |
| cpu_idle_n5 | 0.940, 0.956, 1.072, 1.055 (mean 1.006) | 0.973, 0.989, 1.056, 1.022 (mean 1.010) |
| cpu_idle_n10 | 1.882, 1.930, 1.865, 1.914 (mean 1.898) | 1.832, 1.913, 1.961, 1.865 (mean 1.893) |

There is no rise beyond noise, and wakeups are the same.

## Round 2

Round 2 fixes the owner-approved items from CodeRabbit's second review of PR #1. The branch is `cr1fix2`, based on `origin/modules` at `9c9f19525`. Each fix was preceded by a failing test, committed first, except N2 (see the table).

| Item | Finding | Reproduced? (test) | Fix commit | Verification |
|---|---|---|---|---|
| 1 | Round-2 #3: the host-scheduled launch path selects the Activity as soon as `super.onCreate()` sets `mCalled`. An Activity that calls `finish()` later in the same `onCreate` is then waited on for a resume that `ActivityThread.handleResumeActivity` skips, so the launch timed out. This is the rest of round-1 #11. | Yes. `crates/acos-cli/tests/cr1fix_finish_in_oncreate.rs` now requires the FINAL outcome (`activity-resumed ... finish_during_create=true`, no timeout marker) over 10 launches. The permission-controller fixture does 40 ms of work between `super.onCreate()` and `finish()`. Repro commit `152c60671`: launch 0 printed `host-transaction-resumed` and then `watchdog: activity not resumed timeout_ms=1000`, so the test failed | `46e88c0bb` | 5 runs of the test, each 10 launches: all pass. `sysui1-permission-pregranted` passes. The fix adds no thread: the existing resume loop also exits as `finish_during_create` when the launched Activity has `mFinished` and no Activity is resumed. Idle CPU is flat (see below) |
| 2 | Round-2 #1: `WINDOW` was acknowledged as done when no relayout ran (no attached root, or the relayout threw) | Yes. `crates/acos-cli/tests/cr1fix_window_relayout.rs`, with the diagnostic seam `ACOSLAUNCH_TEST_WINDOW_NO_ROOT` (forwarded by the launcher; nothing else reads it). Repro commit `27bc6adc0`: `multiwindow-split` passed although the root was missing. A control test shows the same scenario still passes with a root | `7cb8b74be` | `finishWindow` acknowledges success only after the target decor has an attached `ViewRootImpl` and no pending layout request. An absent root or a pending traversal is re-checked after each frame for 3 s. A throw or the deadline marks the command failed (`windowCmdFinished(true)`), and the host reports `GUEST_WINDOW_FAILED`. Both tests pass. `multiwindow-split` and `rot1-rotate` pass |
| 3 | Round-2 #8: `cam_sink_native` passed silently when `include/jni.h` was missing | Yes, through the seam `CAM_SINK_NO_JDK=1`. Repro commit `bf52b4a92`: all 3 tests passed in 0.00 s without compiling the harness | `719b928ea` | Without a JDK the 3 tests fail by default and name the missing prerequisite. With `ACOS_ALLOW_ART_SKIP=1` they are 3 counted skips and the `SKIP SUMMARY` says 3. With a JDK they run and pass. The rule is in `run_case` through `acos_test_support::require_some` |
| 4 | Round-2 #10: the rotated config strip is a second `STATUS_BAR` source but TOP-only fitting ignored it | Yes. `fit_insets_fits_rotated_status_bar_on_its_actual_edge` with the new expectations failed. Repro commit `06eaa0ea4` | `639c9ec62` | AOSP agrees: `InsetsState.calculateInsets(frame, types, ignoreVisibility)` takes `Insets.max` over every source of the type, and `InsetsSource.calculateInsets` intersects each source's frame with the window. The config strip `[0,0,2400,63]` therefore gives top=63. `fit_insets_frame` now also applies `status_bar_config` through `apply_bar_inset`. The same strip exists at rotations 2 and 3, so those expectations changed in the same way: rotation 1 gives `[0,63,2400,1080]` (TOP) and `[128,63,2400,1080]` (all sides). 82 acos-wm tests pass, `rot1-rotate` passes |
| 5 | Nitpick N2: the finalize regression test did not use a real deferred output | Not applicable: the refusal was already correct, only the test input was weak | `037e92afe` | `finalize_output_configurations_is_refused_not_acknowledged` now creates the stream as a deferred output (`isDeferredConfig = 1`, no Surface) and sends a complete finalize parcel whose `OutputConfiguration` carries the Surface. It still asserts the refusal. 25 `camera_module` tests pass |
| 6 | Round-2 #12: the camera-probe rebind phases could count frames queued by the previous session | Yes, by evidence rather than by a failing test. The rewritten probe logs `stale_or_unmatched_images=` per phase: sessions 2 and 3 each had 1 image that matched no result of the current session's own request. The old counting would have counted those images toward the new phase | `8b9b146df` | A frame now counts only when an image and a capture result of the current session's own request have the same timestamp. The image listener, the capture callbacks and the phase change all run on one handler (`bgHandler`). Results of an old session are ignored once the phase moves on. `camera-rebind-same-surface` passes |
| 7 | Round-2 #13: the #4 wording in this report | n/a (documentation) | `df4da681b` (this report) | `ERROR_CAMERA_REQUEST` now says it covers failures detected before the host enqueues `deliverFrameFd`. A later JNI drop is not revised |

Notes:

- **#11 completed in round 2.** Round 1 reordered the teardown and then changed the test to accept `host-transaction-resumed` as success, which hid the remaining timeout above. That weakening (`f19de0686`) is reverted: the test again requires `finish_during_create=true` and no timeout marker, and runs 10 launches per test run.
- **Fixture change.** `GrantPermissionsActivity` now sleeps 40 ms between `super.onCreate()` and its answer on the "nothing to ask" path, to keep the race window real. `sysui1-permission-pregranted` still passes.
- **#6 is a fixture probe.** It is not product code. The old claim "not reproduced" for #15 is unchanged by it, because stale frames can only make the old phase counting pass too early. The new counting is stricter.

### Round 2 verification

- `cargo test --offline -p acos-services -p acos-wm -p acos-art --lib`, plus `--test cam_sink_native --test camera_module --test cr1fix_primary_adopt`, with `ACOS_ALLOW_ART_SKIP=1`: 510 passed, 0 failed. Per target: acos-art 207, `cam_sink_native` 3, acos-services 192, `camera_module` 25, `cr1fix_primary_adopt` 1, acos-wm 82. **0 skips**, no `SKIP` line printed. 1 test is ignored: the measurement test that needs `$SHOTZ_FRAMES`.
- `cargo test --offline -p acos-cli --test cr1fix_finish_in_oncreate`: 5 runs, 5 pass (10 launches each). `cr1fix_window_relayout`: 2 pass.
- Scenarios (`--engine art`), all pass: launcher-home, settings-intents, multiwindow-split, camera-preview, camera-capture, camera-scripted, camerax-probe, camera-hold-images, camera-rebind-same-surface, multi-activity-finish, multi-activity-late-finish, w38-multi-activity, mproc3-two-process, rot1-rotate, gaten-backstack, v1-hello-views, sysui1-permission-pregranted.
- Negatives, each fails as required (`status: fail`, exit 1): negative/launcher-home-wrong, negative/multiwindow-split-wrong, negative/camera-scripted-wrong-checksum.
- Idle CPU: release builds, `tools/bench/acos_bench.py --phase memory --animations on` under `run-exclusive.sh --exclusive`, interleaved `origin/modules` (`9c9f19525`) and this branch, 2 reps each. Values are % of one core over a 60 s idle window.

| metric | origin/modules | CR1FIX2 |
|---|---|---|
| cpu_idle_n1 | 0.216, 0.249 | 0.249, 0.232 |
| cpu_idle_n5 | 1.006, 0.989 | 1.039, 1.006 |
| cpu_idle_n10 | 2.044, 2.061 | 1.961, 2.013 |
| wakeups_idle_n10 per s | 3.108, 3.140 | 3.122, 3.174 |

  All metrics are within the rep-to-rep noise (n1 0.03, n5 0.05, n10 0.1). There is no rise.

### Not done / limits

- Round-2 #2 (counter snapshot), #4, #5, #6, #7, #9 and #11 were not in the owner-approved list for this round and were not changed.
- The `ACOSLAUNCH_TEST_WINDOW_NO_ROOT` and `CAM_SINK_NO_JDK` seams are diagnostic only. The no-JDK case is shown by hand (default fail, opt-out skip), not by a nested test run.
- Item 6 has no fail-before/pass-after test. A stale frame needs a precise queue state, so the evidence is the `stale_or_unmatched_images` log of the real scenario.

### Round 2: lead verification and one correction (`f6035c0cd`)

- **Correction.** The #3 repro had added an unconditional 40 ms sleep to `GrantPermissionsActivity`. That fixture is the shipped `com.android.permissioncontroller` system app (`modules/permissions.rs`), so every pre-granted permission request in a real session would have paid 40 ms.
  - The sleep is now gated by the test-only `ACOS_TEST_FINISH_DELAY_MS`. `acos-art` forwards it to the primary and secondary guests, and only `cr1fix_finish_in_oncreate` sets it (40). Normal sessions have no delay.
- **The gate still widens the window.** With `46e88c0bb` reverted, the test fails at `launch 0: secondary process hit a launch timeout`. With the fix, it passes 4/4.
- **Lead re-run on the corrected head:**
  - lib + `cam_sink_native` + `camera_module` + `cr1fix_primary_adopt`: **510 passed, 0 failed**.
  - `acos-services`: **780 passed, 0 failed**.
  - `cr1fix_window_relayout`: 2/2 pass.
  - 17 scenarios pass. That includes `sysui1-permission-pregranted`, `camera-rebind-same-surface` and `multi-activity-late-finish`.
  - The 3 negatives fail with `ASSERTION_FAILED`.
