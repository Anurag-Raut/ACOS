# CR1FIX — CodeRabbit review fixes for PR #1 (v0.4.1)

These are the owner-approved fixes from the CodeRabbit review of PR #1. The triage is recorded in the PR #1 comment and in `AGENTS.md` → "Threat model". Of 22 findings, 9 were fixed and 1 was a conditional fix that did not reproduce, so no shim change was made for it. The other 12 were declined with reasons, in the PR #1 comment.

The work was done by Hydrogen (#11, #21, #19), then Sonnet 5.5 (#22, #20, #17, #4, #5, #10, #15 probe, and a re-check of all of them). The lead verified the result on `f19de0686`.

## Findings

| # | Finding | Reproduced? (test) | Fix commit | Verified |
|---|---|---|---|---|
| 11 | finish-in-`onCreate` teardown races the launch watcher (`HOST_TRANSACTION_TIMEOUT`) | Yes. The race is timing-dependent: `crates/acos-cli/tests/cr1fix_finish_in_oncreate.rs` failed 2/3 runs with the fix disabled | `4e70512b9`, test `f19de0686` | Passes about 60 runs with the fix. The test accepts either success marker (`host-transaction-resumed` or the finish marker); the timeout assertion is unchanged. Lead: 3/3 pass |
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

- A scripted frame that cannot be built or delivered is reported as `onDeviceError(ERROR_CAMERA_REQUEST = 3)`, checked against the framework dex.
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
