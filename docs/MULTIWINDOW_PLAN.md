# Multi-window plan (v0.4.1, track MULTIWIN1)

Status: **plan complete; slice 1 designed, not built** (stopped by the disk
guard, see §9). One owner decision is open (§7 item 5); no ADR conflict.

Inputs: `docs/OWNER_DIRECTION_2026-11-18.md` (near-total compatibility, reuse
AOSP behaviour, modules are attachable building blocks), ADR-0029 (modules,
window layer stack in C1, host-built launch transactions), ADR-0026 item 7 (C1
stays; stock SurfaceFlinger is not adopted), the compositor decision of
2026-11-12 (B1: C1 + the vendored AOSP geometry kernel), LAYER1 (window layer
stack, focus, per-window tree/screenshot, tap routed by the stack),
CONFKIT §4 ("multi-window: none; would need WMS split-screen/task stacks in the
compositor — **L, 4–6 days**, depends on the layer stack"), MPROC3/MPROC7
(second process, host-built `ClientTransaction` launches), ROT1/ROT3/ROT4 (the
existing configuration-change delivery path), and the AOSP r75 cache under
`.cache/aosp` (`base/core/java/android/{app,content/res,content/pm}`,
`base/services/core/java/com/android/server/wm`).

The wire-format rule applies to everything that crosses the peer: the image's
framework dex is authoritative for parcel field order; r75 source is the
behaviour reference. Nothing in this plan changes an existing codec.

---

## 1. AOSP's model, from the cache (the behaviour ACOS must reproduce)

Multi-window is **not a new window system**. It is three existing ACOS concepts
put together: a task that has bounds, a `Configuration` that describes those
bounds, and more than one resumed activity.

1. **`WindowConfiguration` is part of `Configuration`** and carries the task's
   `bounds`, `appBounds`, `maxBounds`, `windowingMode`, `activityType`,
   `alwaysOnTop`, `rotation`, `displayWindowingMode`, `displayRotation`
   (`base/core/java/android/app/WindowConfiguration.java:100-180`).
   `Configuration.updateFrom` folds a `WindowConfiguration` delta into
   `CONFIG_WINDOW_CONFIGURATION` (`Configuration.java:1775-1777`), and
   `Configuration.diff` reports it as `0x20000000`
   (`ActivityInfo.java:1081`).
2. **Windowing modes**: `UNDEFINED=0`, `FULLSCREEN=1`, `PINNED=2`,
   `FREEFORM=5`, `MULTI_WINDOW=6` (`WindowConfiguration.java:107-116`).
   `WindowConfiguration.inMultiWindowMode(mode)` is
   `mode != FULLSCREEN && mode != UNDEFINED` (`:866-869`), so split, freeform
   and PiP all answer `true` for `isInMultiWindowMode()`.
3. **The multi-window callback is driven by the delivered `Configuration`, not
   by a separate transaction.** `ActivityThread.performActivityConfigurationChanged`
   calls `handleWindowingModeChangeIfNeeded` **before** the configuration
   callback (`ActivityThread.java:6330-6345`); that method compares
   `r.mLastReportedWindowingMode` with the new
   `Configuration.windowConfiguration.getWindowingMode()` and dispatches
   `Activity.dispatchMultiWindowModeChanged` only when the in/out-of-multi-window
   answer flips (`:6547-6570`). PiP's callback is dispatched before the MW one.
   This is why ACOS does not need a new `ClientTransaction` item for
   `onMultiWindowModeChanged`: **the `Configuration` it already delivers is the
   trigger** (the same path ROT3/ROT4 use).
4. **A `WindowConfiguration` change alone does not relaunch.**
   `ActivityRecord.getConfigurationChanges` strips
   `CONFIG_WINDOW_CONFIGURATION` before the relaunch decision
   (`ActivityRecord.java:10012-10024`, comment: *"We don't want window
   configuration to cause relaunches"*). What *does* relaunch is a change the
   activity does not declare in `android:configChanges` — in split screen that
   is usually `CONFIG_SCREEN_SIZE`, because `screenWidthDp`/`screenHeightDp`
   are **relative to the task bounds and density**:
   `TaskFragment.computeConfigResourceOverrides`
   (`TaskFragment.java:2213`, dp values at `:2360-2370`) computes them from the
   task's stable-inset bounds (`(stable.width() / density + 0.5f)`, capped at
   the parent's dp), and `smallestScreenWidthDp` *inherits* the display's for a
   split task (only floating/PiP recompute it, `:2384-2393`).
   `ActivityRecord.java:8389-8397` states the same in prose. So:
   * an activity declaring `screenSize` gets `onConfigurationChanged` +
     `onMultiWindowModeChanged` and relays out in place;
   * an activity that declares nothing is recreated through
     `ActivityRelaunchItem` (the path ROT4 already drives);
   * `smallestScreenWidthDp`, `screenLayout`, density and orientation stay
     display-level, so a pure split (no rotation) does not change them.
5. **`appBounds` is the task bounds minus the insets the app is expected to
   fit**; `maxBounds` stays the display. `Configuration.windowConfiguration`
   for an activity is the *merged override* (`global` + task override), which
   is what `MergedConfiguration` carries in `relayout` and in the launch
   `ActivityClientRecord`.
6. **Two visible resumed activities (multi-resume, Android 10+).**
   `Activity.onTopResumedActivityChanged(boolean)` exists precisely because more
   than one activity can be `RESUMED` (`Activity.java:2145-2220`). AOSP resumes
   the top activity of every visible task fragment; only the focused one is
   *top-resumed*. `onPause` is not sent to the other split activity.
7. **The display-level gate.** `ActivityTaskManager.supportsMultiWindow`
   (`ActivityTaskManager.java:252-260`) = not low-RAM **and**
   `config_supportsMultiWindow`; `supportsSplitScreenMultiWindow` additionally
   needs `config_supportsSplitScreenMultiWindow` and a display whose long side
   is ≥ `DEFAULT_MINIMAL_SPLIT_SCREEN_DISPLAY_SIZE_DP`. A non-resizable activity
   (`ActivityInfo.resizeMode`, manifest `android:resizeableActivity=false`) is
   not put in split by the server.
   **Measured on this image** (`aapt2 dump resources
   .cache/boot/android-34/framework-res.apk`): `bool/config_supportsMultiWindow
   = true`, `bool/config_supportsSplitScreenMultiWindow = true` (watch: false),
   `integer/config_supportsNonResizableMultiWindow = 0`. So the app-side gate
   already answers `true` in an ACOS guest (nothing sets `ro.config.low_ram`),
   and a non-resizable activity must be refused — no module has to fake the
   feature flags.
8. **Input and focus are per window.** AOSP has one focused window and one
   focused task; the other split activity is resumed but not focused. The
   window stack already models this in ACOS (LAYER1): `focus_target()` is the
   topmost focusable window, and `InputHost::tap_window` routes a display tap to
   the topmost touchable window under the point.

---

## 2. What app developers actually test (and therefore what ACOS must verify)

Ranked by what a real app's test suite asserts, and by what Gaten-class apps
needed (the ACOS feedback log's "no second app / no multi-window" gap).

| # | behaviour | what the app observes | ACOS today |
|---|---|---|---|
| MW-1 | **Split screen, two activities side by side** (top/bottom on a phone, left/right on a tablet) | two tasks visible at once, each activity laid out in its half; both `RESUMED`; one focused | **no** — one display-global frame, one task stack, one resumed activity |
| MW-2 | **Enter/leave split** | `onMultiWindowModeChanged(true/false, newConfig)` before `onConfigurationChanged`; `isInMultiWindowMode()` flips; a `screenSize`-handling activity keeps its instance, the others are recreated | **no** — the configuration never carries a windowing mode other than `FULLSCREEN` |
| MW-3 | **Resize (divider drag)** | `onConfigurationChanged` with new `screenWidthDp`/`screenHeightDp`, new `windowConfiguration.bounds`/`appBounds`; `View.onSizeChanged`; `WindowMetrics.currentWindowMetrics.bounds`; insets recomputed per task | **no** |
| MW-4 | **Relayout to the new bounds** | the window's surface/frame is the task rect (not the display), the tree's root bounds equal the task rect, the composited pixels are at the task origin | partly — a window's frame already comes from `relayout` (per window), but it is always the display's app bounds |
| MW-5 | **Focus / top-resumed** | `onWindowFocusChanged(false)` for the unfocused half, `onTopResumedActivityChanged(false)`; keyboard/IME goes to the focused one | focus: yes (LAYER1); top-resumed: **no** |
| MW-6 | **Input in both halves** | a tap in either half hits that activity; the divider drag is the system's gesture (host-side in ACOS) | tap routing: yes (LAYER1); a second task: no |
| MW-7 | **`isInMultiWindowMode()` in the tree/log** | an app prints it or gates behaviour on it | **no** |
| MW-8 | **Picture-in-picture** (a later slice) | `enterPictureInPictureMode`, `onPictureInPictureModeChanged`, a small floating window above the full-screen one, `isInPictureInPictureMode` | **no** |
| MW-9 | **Freeform** (optional) | arbitrary task bounds, `isInFreeform`, move/resize the task | **no** |
| MW-10 | **Non-resizable activity refusal** | the server refuses to put `resizeableActivity=false` in split; the activity stays full-screen | **no** (no split at all) |

An app developer's *automated* tests (Espresso/UIAutomator) mostly assert
MW-2/3/4/5: the mode flag, the new configuration's dp size, the root view's
bounds after a resize, and which activity is focused. That is the order this
plan implements in.

---

## 3. ACOS today: the exact seams that have to change

Everything below is measured on this tree (branch `multiwin1`, base `modules`).

### 3.1 WMS (`crates/acos-wm`)

* `DisplaySpec` (`src/display.rs:281-302`) is **display-global**: `frame`,
  `app_bounds`, bars, rotation, density. Rotation is the only "configuration
  geometry" operation (`DisplaySpec::rotated`, `:400-470`).
* `compute_frame_for_requested` (`src/wms.rs:1028-1090`) takes the parent frame
  from `display.window_frame()`; `relayout` (`:489`) stores one frame per
  window. There is **no task rect** and no window→task binding.
* `reference_merged_configuration(&display)` (`src/wms.rs:1497-1560`) builds the
  **one** display `Configuration`; `window_configuration.windowing_mode` is
  hard-coded `1` (`WINDOWING_MODE_FULLSCREEN`) and `bounds`/`app_bounds` are the
  display's.
* `WindowRecord` (`:80-130`) has `frame`, `display_frame`, `parent_frame`,
  `params`, `z`, `seq`, `focusable` — no task id. The layer stack, focus and
  tap routing (`layers.rs`, `InputHost::tap_window`) are already per window and
  need no change for split (they were built for dialogs/overlays).
* `WmsState::rotate` (used by the live session) mutates the display; a
  per-window configuration push does not exist.

### 3.2 AMS/ATMS (`crates/acos-services`)

* `Task { id, activities }` (`src/ams.rs:935-942`) and `TaskStack`
  (`src/atms.rs:3374-3400`) are a back stack with **no bounds, windowing mode,
  z-order or visibility**. `AmsState.tasks: Vec<Task>` is a flat list.
* Exactly one activity is resumed at a time on the path the launcher drives:
  `activity_resumed` (`atms.rs:1324`) sets one flag and `top_process`
  (`registry.rs:196-207`, `LiveHandles`) names one foreground process.
  `ResumeActivityItem` is sent to the top activity; the one below is paused.
* The launch `Configuration` (`client_transaction.rs:690-760`) is
  `process_configuration()` — the framework's empty `WindowConfiguration`
  (all-zero bounds, mode `0`), which is correct for a full-screen first launch
  and **wrong for a task with bounds**. This is the host-built-launch seam:
  the launch transaction must carry the *task's* configuration.
* `MergedConfiguration` in `relayout` is the display config (§3.1), so a guest
  that is told about a task rect by some other means would immediately have it
  overwritten by the next relayout reply.

### 3.3 Guest (ART launcher)

* `LaunchMain.applyRotation` (`crates/acos-art/guest/java/dev/acos/art/LaunchMain.java:1610-1780`)
  is the existing configuration-change driver: it builds a `Configuration` from
  the host's display, calls `ActivityThread.handleConfigurationChanged(cfg, 0)`,
  and lets the framework choose `ActivityRelaunchItem` vs
  `handleActivityConfigurationChanged` from `ActivityInfo.getRealConfigChanged()`.
  **A windowing-mode/bounds change is the same operation with a different
  `Configuration`** — this is the single most reusable piece of the track.
* The control channel is `stdin` (`PROBE`, `ROTATE`), handled by the launcher's
  main loop (`launch.rs:8144-8237` host side, `LaunchMain.java:1812` guest side).
* The guest session has a real `ActivityThread`, `ViewRootImpl` and
  `IWindowSession.relayout` for the ART engine (unlike the VM session, which
  has no WMS) — so tree bounds, `onSizeChanged` and a real relayout follow from
  the WMS reply without guest-side geometry code.

### 3.4 Compositor (C1)

* `Compositor` (`crates/acos-wm/src/compositor.rs`) composites a display-sized
  canvas from layers that each have a `position`, `z`, `crop`, `transform` and a
  buffer. Two windows at different positions already work (LAYER1's dialog and
  overlay gates). `capture_composite` (`surface_client.rs:1020`) blits each
  layer at `position` with its buffer size.
* What split screen needs from C1 is **nothing new**: task origin = layer
  position, task size = buffer/frame size. `destinationFrame`/crop already
  exist for BLAST and are the freeform/PiP scaling path if it is ever needed.

---

### 3.5 Seams found while sizing slice 1 (MULTIWIN1b, measured on this tree)

1. **The activity override, not the process configuration, carries the task
   rect.** `applyRotation` calls `handleActivityConfigurationChanged(r, <empty
   override>, ...)` because a rotation is a *display* change. A task rect is the
   opposite: AOSP keeps the process/global `Configuration` display-level and
   puts `windowConfiguration.{bounds,appBounds,windowingMode}` plus the
   task-relative `screenWidthDp/HeightDp` in the **activity's override
   configuration** (`MergedConfiguration.overrideConfiguration`,
   `ActivityThread.performActivityConfigurationChanged` merges
   `global + override`). So `applyWindow` must pass a *non-empty* override and
   leave `handleConfigurationChanged` alone (otherwise every other activity in
   the process would also be told it is in multi-window mode).
2. **`rotatedConfiguration` is built from guest statics** (`PANEL_W/H`,
   `APP_W/H`, `STATUS_INSET`, `NAV_INSET`, `LaunchMain.java:1373-1405`), not
   from the host's WMS. `applyWindow` must take the rect from the host
   (`WINDOW l t r b mode`) and never recompute geometry guest-side; the dp
   values use the display density the guest already knows
   (`(px / density + 0.5)`, `TaskFragment.java:2360-2370`).
3. **The control thread cannot answer a module control.** A module's `control`
   runs on the *peer* thread (`launch.rs` `services`/`ctl` → `modules.control`),
   while the guest's `GuestProbeChannel` (stdin/FIFO) and the "reply bound to a
   fresh probe sample" wait live in the *launcher* command loop (`rotate`,
   `launch.rs:8148-8240`). A multi-window op needs both halves (mutate
   `WmsState` + write `WINDOW …` to the owning guest + wait for a fresh probe),
   so the module needs one new seam; see decision 5 in §7.
4. **Per-process guest channel.** `ROTATE` writes to the primary guest only.
   For split, `WINDOW` must go to the process that owns the task's top activity
   (`top_process`, MPROC5; secondary guests have their own probe channel).
   Slice 1 is single-task and single-process, so it is unaffected; slice 2
   must route by task owner.
5. **Fixtures exist.** `fixtures/apps/two-activity` declares
   `configChanges="orientation|screenSize|keyboardHidden"` on both activities,
   so it is the slice-1/2 subject (in-place `onConfigurationChanged`); the
   relaunch half of slice 3 needs a sibling that declares nothing (`counter`
   already declares nothing and is what ROT4 relaunches).

---

## 4. Target design

### 4.1 One geometry owner: a task stack in the WMS

Add a **task stack to `acos-wm`**, because the WMS is the only place that
already computes window frames and configuration, and because ADR-0029 item 8
puts the window layer stack in C1:

```
TaskRecord { id: i32, bounds: RectI, app_bounds: RectI, windowing_mode: i32,
             z: i64, visible: bool, display: i32 }
```

* `WmsState` gains `tasks: Vec<TaskRecord>` and `window_task: HashMap<WindowId, i32>`.
* `relayout` looks up the window's task and computes the frame inside
  `task.app_bounds` (instead of `display.window_frame()`). A window with no task
  (a dialog, an overlay, an IME) keeps today's display-parent behaviour; its
  parent frame is the task it is attached to when it has an activity token.
* `MergedConfiguration` becomes `merged_configuration_for_task(&display, task)`:
  the display fields stay display-level (`screenLayout`, `orientation`,
  `smallestScreenWidthDp`, density), while
  `windowConfiguration.{bounds, appBounds, maxBounds, windowingMode,
  displayWindowingMode}` and `screenWidthDp`/`screenHeightDp` come from the task
  rect (`Task.computeConfigResourceOverrides`, AOSP r75).
  Full-screen tasks keep today's bytes exactly (`mode=1`, display bounds) so no
  existing gate moves.
* The host session command `multiwindow enter|resize|exit` mutates the task
  record, then re-runs the existing "configuration changed" path for the
  affected windows (queue `Resized` with the new `MergedConfiguration`).

This is a **WMS-layer change, not a compositor architectural change**: C1 keeps
its role, its layer stack, and the one-layer fast path (LAYER1). It is the
"WMS split-screen/task stacks" that CONFKIT §4 already named as the missing
piece. If a future slice needs per-layer scaling (PiP), that *would* touch C1's
placement; it is flagged in §7 rather than assumed.

### 4.2 AMS/ATMS: per-task configuration and multi-resume

* `Task` gains the same fields as `TaskRecord` (bounds, windowing mode, z,
  visible) and a `task_id` already exists; the WMS task id and the AMS task id
  are the same number.
* **Multi-resume**: `resume_task_top(task_id)` resumes the top activity of a
  task without pausing the other task's top activity. Only the focused task's
  top activity is *top-resumed*: the ATMS sends
  `TopResumedActivityChangedItem` (or the AIDL equivalent the image has — the
  implementer must confirm the item's parcel shape against the image dex,
  wire-format rule) to the old and new top-resumed activity.
  `activity_paused` must stop being sent to the other split activity on focus
  change; `onWindowFocusChanged` already follows the WMS stack (LAYER1).
* **Launch**: `PendingLaunch` carries the task's `Configuration` (bounds, mode)
  so `LaunchActivityItem` is built with the task config, not
  `process_configuration()`. `process_configuration()` stays the process-level
  `bindApplication` config (display-level), which is what AOSP sends.
* **Relaunch vs config change**: the ATMS applies AOSP's rule — a diff that is
  only `CONFIG_WINDOW_CONFIGURATION` never relaunches
  (`ActivityRecord.getConfigurationChanges`), a `screenSize` diff relaunches
  unless `ActivityInfo.configChanges` declares it. The relaunch path is ROT4's
  `ActivityRelaunchItem`; the config-change path is ROT4's
  `handleActivityConfigurationChanged`.
* **Refusals**: a non-resizable activity (`resizeableActivity=false`, or the
  PMS's `ActivityInfo.resizeMode` when the manifest omits it) asked to enter
  split answers a structured `UNSUPPORTED_ACTIVITY_NOT_RESIZABLE` naming the
  activity and the manifest attribute — never a silent full-screen "split".

### 4.3 The `multiwindow` module (ADR-0029)

A **hot**, `in-process` module. It owns no `ServiceManager` name; it owns the
task-stack *controls* and the session's windowing behaviour, which is exactly
what the module model is for ("a set of Android system pieces" — here: the
windowing mode of the tasks in the session).

| field | value |
|---|---|
| id | `multiwindow` |
| profiles | `off`, `real` (no `stub`/`scripted`: a fake split would be a fake success) |
| attach | `hot` |
| security | `in-process` |
| provides | window type `TYPE_APPLICATION` (documentation row; no new service name), `broadcast` `android.intent.action.CONFIGURATION_CHANGED` is framework-driven and stays in `core` |
| requires | `core` (WMS/ATMS) |
| budget | 1 MiB, 5 ms |
| controls | `enter_split`, `resize`, `exit`, `status` |

Wire shape (`acos_service_ctl {module:"multiwindow", op:..., args:...}` and the
scenario `service_ctl:` step; both already exist):

| op | args | effect |
|---|---|---|
| `enter_split` | `{top: "<pkg>/.<Activity>", bottom: "<pkg>/.<Activity>", orientation?: "horizontal"\|"vertical", ratio?: 0.5}` | put `top` in the upper half and `bottom` in the lower half (horizontal) or left/right (vertical); both tasks visible, both activities resumed, `top` focused; both get `onMultiWindowModeChanged(true)` + `onConfigurationChanged`; reply carries both task rects and the mode |
| `resize` | `{ratio: 0.3..0.7}` | move the divider: both tasks get new bounds + `onConfigurationChanged`; reply carries both rects |
| `exit` | none | restore full-screen: the focused task keeps its activity, the other task's activity is finished (AOSP `dismissSplitScreen` semantics for ACOS's single-back-stack model) and gets `onMultiWindowModeChanged(false)` before it goes; reply carries the restored rect |
| `status` | none | every task with `{id, activity, bounds, app_bounds, windowing_mode, resumed, focused, visible}`, plus the last transition and the probe sample it was bound to |

Slice 1 (`enter`/`exit` for **one** activity with smaller bounds, no second
task) uses the same op names with only `top` set; `bottom` is required from
slice 2 on. The module refuses `enter_split` with no `bottom` before slice 2
lands, by name (`UNSUPPORTED_ANDROID_API` naming slice 2) — never a silent
single-window "split".

**Honesty**: the module's `report()` says `real` only when the task stack is
live and the guest confirmed the new configuration; a control that cannot reach
the live session (VM engine, no WMS) answers
`UNSUPPORTED_ANDROID_API` naming the ART engine.

### 4.4 Verdict and tree shape

* The tree already carries `windows[]` (LAYER1). Split adds nothing new: each
  task's root window has its own `bounds`, so `assert_windows order` + a bounds
  assertion is enough to prove MW-1/MW-4.
* A verdict that relied on the module says `multiwindow:real` in its `modules`
  row (ADR-0029 item 5). `core` keeps `WINDOWING_MODE_FULLSCREEN` when the
  module is `off`.

---

## 5. Slices

The estimate in CONFKIT §4 (L, 4–6 days) assumed the layer stack was not done.
It is (LAYER1), so the remaining work is WMS task geometry + ATMS multi-resume +
the guest configuration push. Sizes below are for one implementer with this
tree.

| slice | content | size | acceptance |
|---|---|---|---|
| **1** | one task, smaller bounds: `multiwindow enter`/`exit`; WMS task rect + per-task `MergedConfiguration`; guest `WINDOW` command → `handleConfigurationChanged`; `onMultiWindowModeChanged(true)` + `onConfigurationChanged`; tree bounds = task rect; exit restores | **S–M** (1–1.5 d) | probe scenario asserts mode flag, config dp size, root bounds, restore; `cargo test -p acos-services -p acos-wm` green |
| **2** | second task + second activity visible (same process, `two-activity` fixture); multi-resume (`TopResumedActivityChangedItem`); focus split; input in both halves; `enter_split {top, bottom}` | **M** (1.5–2 d) | `mproc3-two-process`-class scenario with two windows, both resumed, tap each; `onTopResumedActivityChanged` asserted from the app log |
| **3** | divider resize: `resize {ratio}`; both tasks get new config; relaunch-vs-config-change decision from `ActivityInfo.configChanges`; `WindowMetrics` assertion | **S–M** (1 d) | scenario drags the ratio 0.5 → 0.3 → 0.7, asserts both trees' bounds and both apps' new dp size |
| **4** | freeform (optional): arbitrary rect, `isInFreeform`, move/resize | **S** (0.5–1 d, after 3) | scenario enters a 600×800 freeform rect and asserts bounds |
| **5** | PiP (later slice, separate track): `enterPictureInPictureMode`, `onPictureInPictureModeChanged`, a small window above the full-screen task | **M–L** (2–3 d) | needs a PiP window layer and per-layer scaling in C1 — see §7 |

Total for 1–4: **~4–5 days**, which matches CONFKIT's L estimate once the
already-built layer stack is credited.

---

## 6. Test plan

Probe scenarios (each a `scenarios/multiwindow-*.yaml`, ART engine, real
framework):

1. `multiwindow-shrink.yaml` (slice 1): launch `two-activity` (declares
   `screenSize`), assert full-screen root bounds; `service_ctl
   multiwindow.enter_split {top}`; assert `windows[0].bounds` = the task rect,
   the app's own `onMultiWindowModeChanged(true)` / `onConfigurationChanged`
   line (a fixture that prints them into a `TextView`), and the new dp size;
   `exit`; assert the original bounds and the `false` callbacks.
2. `multiwindow-split.yaml` (slice 2): two activities, `assert_windows order`
   with two `TYPE_APPLICATION` roots, both `resumed:true`, one `focus:true`; a
   tap in each half; `onTopResumedActivityChanged` from the log.
3. `multiwindow-resize.yaml` (slice 3): ratio 0.5 → 0.3 → 0.7, bounds and
   `screenWidthDp`/`screenHeightDp` asserted at each step.
4. `multiwindow-refusal.yaml`: `resizeableActivity=false` fixture → structured
   `UNSUPPORTED_ACTIVITY_NOT_RESIZABLE`; module `off` → `UNSUPPORTED_ANDROID_API`
   naming the attach call.

Regression set that must stay green: `back-layers`, `mproc3-two-process`,
`sysui1-shade` (the brief's list), plus `rot1-rotate` (the configuration path
this reuses) and `layer0-overlay-stack` (the layer stack this must not disturb).

Fixture work: one small activity that prints
`onMultiWindowModeChanged(...)` / `onConfigurationChanged(...)` /
`isInMultiWindowMode()` / root bounds into a `TextView` (so the tree can assert
it without log scraping), and a `resizeableActivity="false"` variant. Extend
`fixtures/apps/two-activity` (it already declares the config changes) or add
`fixtures/apps/multiwindow-probe`; `tools/build/build-apps.sh` builds it.

---

## 7. ADR conflicts and decisions the orchestrator must take

**No conflict** with:
* ADR-0029 item 8 (window layer stack in C1) — the layer stack is untouched;
* ADR-0026 item 7 / the 2026-11-12 compositor decision (C1 stays, B1 geometry
  kernel) — no stock SurfaceFlinger, no new compositor;
* LAYER1's D3 ("multi-window pixels stay C1-only") — the split is composited by
  C1 exactly like two layers today; ACOS still paints no host-side window;
* the host-built launch transaction amendment — the task configuration becomes
  part of the same host-built `LaunchActivityItem`.

**Decisions needed** (each is small, but they are architecture, not
implementation detail):

1. **Where the task stack lives**: `acos-wm` (recommended: it already owns
   frames/configuration and the layer stack) vs `acos-services` (next to
   AMS/ATMS). The plan assumes `acos-wm` with the AMS mirroring the ids. This is
   the one structural choice that touches two crates; if the orchestrator
   prefers a services-owned stack, slice 1's seam moves but the slices stand.
2. **`exit` semantics**: AOSP keeps both tasks in the back stack and the
   dismissed one goes to full screen. ACOS has one back stack today; the plan
   finishes the non-focused activity on `exit` (documented, asserted) until a
   recents/launcher app exists. Confirm this is acceptable for v0.4.1.
3. **PiP scaling** (slice 5 only): a PiP window is a *scaled* layer. C1 has
   `crop`/`transform`/`destinationFrame` hooks but the one-layer fast path and
   the pixel gates assume 1:1. If PiP must scale, that is a C1 placement change
   and would need the owner's sign-off under the compositor rule; slice 5 is
   therefore **not** in this track's scope and is flagged, not assumed.
4. **Non-resizable refusal code**: the plan uses
   `UNSUPPORTED_ACTIVITY_NOT_RESIZABLE`; confirm the code name fits
   `docs/ERROR_CODES.md` conventions (it is a structured `UNSUPPORTED_*`, not a
   new family).

5. **How a module control reaches the launcher's guest channel** (new, found
   in MULTIWIN1b, §3.5.3). This is a seam between the module framework and the
   launcher, not a compositor/ADR change, but it adds a host→guest command path
   that modules do not have today, so the owner picks:
   * **(A) `ServiceHost` guest-command outbox (recommended).** The module's
     `control` mutates `WmsState` (a `wms` handle published like `atms`) and
     pushes a typed `GuestCommand::Window{…}` into an outbox on the host; the
     launcher's `services` handler drains it after `control.submit`, writes the
     line to the owning guest, waits for a fresh probe (the `rotate` rule) and
     returns the tree in the same reply. *Pros:* one reusable seam (rotation,
     PiP, future guest-driven controls can use it), the module stays the owner
     of its behaviour and its honest report. *Cons:* touches `host.rs` and the
     launcher loop (two crates), ~0.5 d extra.
   * **(B) A launcher `window` command beside `rotate`; the module is only the
     attach gate/status.** `service_ctl multiwindow.enter_split` is forwarded to
     the launcher command. *Pros:* smallest diff, no framework change.
     *Cons:* the module's `control` would be a forwarder with no behaviour of
     its own, which the honest-report check (ADR-0029 item 7) is awkward about;
     every later windowing feature grows the launcher loop instead of a module.
   * **(C) Guest-side polling of the WMS task record** (the guest asks the host
     for its task rect on every probe). *Pros:* no host→guest write path.
     *Cons:* latency and a new always-on request on the hot path; rejected by
     the performance track.

---

## 8. What slice 1 changes (concrete)

1. `crates/acos-wm/src/display.rs`: a `TaskRect`/`WindowingMode` pair (or
   `TaskRecord`) and `DisplaySpec::with_task_bounds(task) -> DisplaySpec` —
   the geometry half, reusing `custom_with_insets`-style bar math so a task
   rect keeps the display's density and rotation.
2. `crates/acos-wm/src/wms.rs`:
   * `WmsState::{set_task_bounds, clear_task_bounds, task_bounds}`;
   * `compute_frame_for_requested` takes an optional task rect as the parent
     frame (full-screen = today's bytes);
   * `merged_configuration_for_task` (the per-task `Configuration`), used by
     `relayout` and by the launcher when it pushes the config;
   * a test that a task rect yields the task frame and a `Configuration` whose
     `windowingMode` is the task's, and that clearing restores the exact
     full-screen bytes.
3. `crates/acos-art/src/launch.rs`: a `WINDOW <mode> <l> <t> <r> <b>` guest
   line next to `ROTATE`, written by the launcher after it drains the module's
   guest-command outbox (decision 5 A); the reply is bound to a fresh probe
   sample (the same rule as `rotate`: never answer with a stale tree).
4. `crates/acos-art/guest/java/dev/acos/art/LaunchMain.java`:
   `applyWindow(mode, l, t, r, b)` = build the **activity override**
   `Configuration` (not the process one, §3.5.1): `windowConfiguration.{bounds,
   appBounds, windowingMode}` + `screenWidthDp/HeightDp`, then
   `handleActivityConfigurationChanged(record, override, …)` when
   `getRealConfigChanged()` covers `CONFIG_SCREEN_SIZE`, or the ROT4
   `ActivityRelaunchItem` path (extract the relaunch block of `applyRotation`
   into a shared helper) when it does not. `exit` passes the display-level
   override (mode 1, display bounds).
5. `crates/acos-services/src/modules/multiwindow.rs` + registry row + generated
   `docs/MODULES.md`: the module, its manifest and its controls (slice 1:
   `enter`, `exit`, `status`; `enter_split` refuses `bottom` by name until
   slice 2).
6. `scenarios/multiwindow-shrink.yaml` + the fixture activity that prints the
   callbacks.

Nothing in slice 1 changes a wire codec, the compositor, or the peer protocol.

## 9. Slice 1 status on this branch

**Slice 1 is not built.** The plan is complete and reviewed against the tree
(§3.5 lists what the continuation found), but the build/ART step was stopped by
the host disk guard (`df -h /` = 87–88 % at the start and still rising while the
other tracks run, against the 85–86 % stop rule), so no code was written that
could not be verified. `results/handoff/MULTIWIN1/README.md` holds the exact
build order, files and acceptance commands for whoever runs slice 1 next, plus
the one open owner decision (§7 item 5) that selects its shape. Slices 2–5 start
from this plan either way.
