# MODA11Y4 — guest-side proof of the interaction surface, and BACK

Branch `moda11y4` (from `moda11y3` = `efe78253`), 2026-11-16.
Worktree `.t3/worktrees/acos/t3-b4cf813a-908c5c602f7981ae`.

## Status

**PARTIAL. Steps 1 (merge), 2 (guest-side proof of `getWindows` /
`getRootInActiveWindow` / `ACTION_CLICK`) and 3 (`performGlobalAction(BACK)`)
are implemented and pass on real ART. Step 4 (`TYPE_ACCESSIBILITY_OVERLAY`)
was not started** — the one-hour rule stopped the track, and the brief's own
order puts the overlay after the two proofs that were finished here.

| Brief step | State |
|---|---|
| 1. `git merge modules`, unify onto the modules control path, keep both sides' tests | **DONE** (`dcc1d6d9`) |
| 2. guest-side proof: `getWindows()` (count/types/layers), `getRootInActiveWindow()`, `ACTION_CLICK` on a known button, asserted in `scenarios/a11y-probe.yaml` (`engine: art`), app screen shows the click | **DONE, passes on real ART** (the commit that adds this README; see "Evidence") |
| 3. `performGlobalAction(GLOBAL_ACTION_BACK)` routed to the session's Back path, asserted | **DONE, passes on real ART** — with one honest limit: the app handles Back and presses back, but the *destroy transaction* after `finish()` is not delivered by the host (see "What is not proven") |
| 4. `TYPE_ACCESSIBILITY_OVERLAY`: WMS accepts `addWindow` for a bound service's token, window lands in LAYER1, tagged in the observe tree, probe overlay view, asserted | **NOT STARTED** (the LAYER1 layer 31 policy and the `getWindows` type-4 mapping already exist from LAYER1/MODA11Y3; the a11y-side acceptance and the tree tag do not) |
| Deferred: `dispatchGesture`, `takeScreenshot`, Gaten bubble, orchestrator notes (a)(b)(c) | untouched, except (a) which was **diagnosed as a pre-existing red test** (see below) |

Nothing in this handoff claims behaviour that was not run.

## 1. Merge (`dcc1d6d9`)

`git merge modules` (sensors module + the modules control path) into
`moda11y3`. Three content conflicts, all in the module registry:

* `core_module.rs` — both sides filter service names out of `core`'s manifest.
  Resolved by keeping `modules`' `FILLED_BY_MODULE` mechanism and adding
  `"accessibility"` to it next to `"sensorservice"`: `core` still *publishes*
  the disabled accessibility fallback node (so a session that does not attach
  the module boots unchanged) and does not claim the name.
* `module.rs` — `ModuleRegistry::standard()` now ships `accessibility`,
  `sensors`, `vibrator`.
* `modules/mod.rs` — both `pub mod`s.

Both sides' tests are kept and pass: `cargo test -p acos-services` (102 lib,
11 `module_kit`, 4 `moda11y2_bind`, 7 `moda11y3_windows`, the 938-line
`sensors_module` suite, 3 `moda11y4_back`) and `cargo test -p acos-wm`
(76 lib + 6 integration binaries).

## 2. The probe's own interaction surface, on ART

`fixtures/apps/a11y-probe` (service + activity + layout) now does, from
`onServiceConnected`:

1. `getWindows()` (retried up to 25 × 200 ms while the window registers),
   logging count + every window's id/type/layer/active/focused/title;
2. `getRootInActiveWindow()`, logging the root's class and text;
3. `root.findAccessibilityNodeInfosByViewId("dev.acos.fixtures.a11yprobe:id/click_target")`
   and `performAction(ACTION_CLICK)` on the result;
4. the activity's own `OnClickListener` increments a counter that is rendered
   into the `status` text, and the scenario asserts that text — so the frame,
   not the probe's bookkeeping, shows the click.

Real log (`results/handoff/MODA11Y4/a11y-probe-logcat.txt`):

```
I AcosA11yProbe: windows count=1 attempts=1
I AcosA11yProbe: window[0] id=1 type=1 layer=0 active=true focused=true title=null
I AcosA11yProbe: root class=android.widget.FrameLayout text=null view_id=null clickable=false
I AcosA11yProbe: by_view_id count=1
I AcosA11yProbe: node class=android.widget.Button text=Click target view_id=null clickable=true
I AcosA11yProbe: click_target_pressed count=1
I AcosA11yProbe: click handled=true
I AcosA11yProbe: status a11y enabled=true installed=1 enabled_list=1 sent=0 first=… a11y_click=1
```

**The relay worked on its first real run**: no host change was needed for
step 2. The MODA11Y3 design point — reply first, queue the oneway delivery, let
the callback run guest-local — is what makes it work.

### The one design decision the run forced: interrogate off the main thread

The probe queries from a worker thread, deliberately, and the reason is worth
carrying forward:

* the guest is **one process**, so the app-side
  `AccessibilityInteractionController` schedules the request on the *app's
  main-thread* handler (`AccessibilityInteractionController.java:140-167`);
* AOSP's same-process fast path (`scheduleMessage`: `interrogatingPid ==
  myPid && interrogatingTid == myLooperThreadId` → the waiting client handles
  the message inline, `AccessibilityInteractionClient.setSameThreadMessage`)
  needs the **interrogating pid**, which ACOS's binder does not model at all
  (no caller pid/uid anywhere; `IAccessibilityServiceConnection` API 34 does
  not even send one, so the host would have to know the guest's pid);
* therefore a main-thread interrogation would block the thread that must run
  the request and wait out the framework's 5 s `TIMEOUT_INTERACTION_MILLIS`.
  The guest's own `Process.myPid()` is `2` in this session (bwrap
  `--unshare-pid`), which is exactly the kind of value the host must not
  hard-code.

Querying from a worker thread is what a real service does (never block the
main thread on a synchronous a11y query) and it exercises the cross-thread
relay path. The observation `view_id=null` is AOSP behaviour, not a defect:
`View` reports the id resource name only when the fetch asked for
`FLAG_REPORT_VIEW_IDS`; the by-view-id *query* itself resolved (count=1).

## 3. `performGlobalAction(GLOBAL_ACTION_BACK)`

AOSP dispatches Back as a **key** into the focused window, so the app's own
`Window` decides; "dialog first, then activity" follows from *which* window is
focused, not from a host-side rule. The module now:

* accepts `IAccessibilityServiceConnection.performGlobalAction(1)` and
  publishes a real `KEYCODE_BACK` down/up pair on the session's
  `InputChannel` — the same `InputChannel` producer a scenario `back` /
  `acos key back` uses (`A11yState::back_token` / `publish_back`, wired in
  `registry.rs` next to the LAYER1 window stack);
* answers AOSP's boolean *after* the publish, and answers it `true` only when
  the key actually left the host: no token, no live window or no input host is
  a structured `ModuleRejected`, never a `true`;
* keeps HOME/RECENTS/NOTIFICATIONS as structured refusals (they need system
  UI), recorded in the module report's `unsupported`.

### The root cause the first ART run exposed

The call returned `true` and the key arrived — and nothing happened. Real
cause, found in the framework source: `Activity.onKeyUp` handles Back only when
`event.isTracking()` (`Activity.java:4117-4136`), and the flag is set by
`InputDispatcher` on the **up** event of a tracked press
(`AKEY_EVENT_FLAG_TRACKING` = `KeyEvent.FLAG_TRACKING` = `0x200`). ACOS
publishes its own key events, so it has to set the same flag: a host-side gap
for **every** Back in a session, not just the accessibility one. Fix in
`input_channel.rs` (`key_at` and `key_stroke` both set `KEY_FLAG_TRACKING` on
the up event), with `tests/moda11y4_back.rs` asserting it on the wire.

Real log after the fix:

```
I AcosA11yProbe: back_requested
I AcosA11yProbe: key_down code=4 tracking=false
I AcosA11yProbe: global_action back=true
I AcosA11yProbe: key_up code=4 tracking=true
I AcosA11yProbe: back_pressed
```

## What is not proven (say it plainly)

* **The activity's destroy transaction after `finish()` is missing.** The app
  handles Back (`back_pressed` runs) and calls `finish()`, but no
  `onPause`/`onStop`/`onDestroy` follows: the host's activity manager does not
  drive the lifecycle after a guest `finishActivity`. The scenario therefore
  asserts `key_down` / `key_up tracking=true` / `back_pressed` and **not**
  `activity_destroyed`. This is pre-existing for the control plane's `back`
  too (nothing in the tree claimed otherwise); it is AMS work, not a11y work.
* **No capability gate on `getWindows`.** AOSP's
  `AccessibilityServiceConnection.getWindows()` requires
  `canRetrieveWindowContent` (`android:canRetrieveWindowContent` in the
  service's `accessibility-service` XML) plus
  `FLAG_RETRIEVE_INTERACTIVE_WINDOWS`. The relay answers from the LAYER1 stack
  without checking either, and the manifest parser does not read those
  attributes yet (MODA11Y2 limit). The fixture therefore does not claim a
  capability it is not checked for; closing this needs the meta-data resource
  parser.
* **The module report in a launch reply is a pre-install snapshot** (MODA11Y2
  limit, unchanged): the a11y `details` block in the verdict still says
  `service_bound: false`, `windows: []`. The app-side log is the authority in
  this handoff.

## Evidence

```
results/handoff/MODA11Y4/a11y-verdict.json          status=pass, 10/10 steps
results/handoff/MODA11Y4/artifacts/verdict.json     the same file (canonical copy)
results/handoff/MODA11Y4/artifacts/logs/logcat.txt  full guest log
results/handoff/MODA11Y4/a11y-probe-logcat.txt      the probe's lines only
results/handoff/MODA11Y4/artifacts/tree|frames|steps
```

Command (private runtime dir, shared lock):

```bash
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export ACOS_RUNTIME_DIR=$PWD/.cache/moda11y4-run
export ACOS_SESSION_SOCKET=$ACOS_RUNTIME_DIR/session.sock
export ACOS_ART_BIN=$PWD/.cache/moda11y4-target/release/acos-art
tools/art-runtime/run-exclusive.sh --task MODA11Y4 -- \
  tools/guarded.sh --mem 8G --time 600 -- \
  .cache/moda11y4-target/release/acos verify scenarios/a11y-probe.yaml \
  --engine art --artifacts results/handoff/MODA11Y4/artifacts
```

## Tests

```
crates/acos-services/tests/moda11y4_back.rs      3/3 pass
  back_is_a_real_key_on_the_focused_window_with_the_tracking_flag
  back_without_a_live_window_is_a_structured_refusal
  other_global_actions_stay_structured_refusals
cargo test -p acos-services --no-fail-fast        all green except the
  pre-existing parcelable drift (below)
cargo test -p acos-wm --no-fail-fast              76 lib + 6 binaries green
cargo test -p acos-art                            146 lib pass, 7 fail =
  worktree path length (`stream socket path …/2178128-stream/stream.sock is
  too long for a unix socket`), same environmental failure MODSENS documented;
  all integration binaries green
```

**One red test is inherited, not caused here:**
`acos-services --test parcelable::codec_field_orders_match_the_framework_dex`
fails on `android.accessibilityservice.AccessibilityServiceInfo` (image dex:
`consumed 22/22 calls, spec 23 fields, all_fields=false`). It is
orchestrator note (a) — the dex scanner misses the last `writeToParcel` call —
and it fails identically on `moda11y`/`moda11y2`/`moda11y3` (the test and the
spec are untouched by this branch: `git diff moda11y3 -- tests/parcelable.rs
src/parcelable.rs` is empty). It was **not** weakened, ignored or deleted.

## Remaining (smallest next action first)

1. **Step 4, `TYPE_ACCESSIBILITY_OVERLAY`.** The LAYER1 policy already returns
   layer 31 for `TYPE_ACCESSIBILITY_OVERLAY` and `getWindows` already reports it
   as type 4 (MODA11Y3, unit-proven). Left to do: WMS `addWindow` acceptance for
   a **bound** a11y service's token (the session knows
   `state.bound_service`), the window tag in the observe tree
   (`acos-verify`/tree `windows[]` carries `kind`/`type_name`, so the tag is a
   `kind: "accessibility_overlay"` mapping), the probe's overlay
   `LayoutParams(TYPE_ACCESSIBILITY_OVERLAY)` view, and
   `assert_windows: {order: [application, accessibility_overlay], types:
   [TYPE_BASE_APPLICATION, TYPE_ACCESSIBILITY_OVERLAY]}` in the scenario.
   `scenarios/a11y-probe.yaml` is the file to extend; keep the BACK steps last.
2. **The guest `finish()` lifecycle gap** (AMS): deliver the destroy
   transaction so `activity_destroyed`/`onStop` run; that also makes a
   `back`-based navigation assertion possible for every scenario.
3. Orchestrator note (a): the parcelable scanner's missing last call — this is
   the only red test in `acos-services`.
4. Orchestrator note (b) `AccessibilityWindowAttributes` codec and note (c) the
   AMS `serviceDoneExecuting` test (unchanged, still deferred).

## Environment notes

* Target `.cache/moda11y4-target`, built with `
  CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0`,
  and deleted at the end of the session.
* `fixtures/build` was a hardlink copy of the main checkout's and was **deleted**
  at the end (the a11y-probe APK was rebuilt here; the main checkout's copy is
  untouched).
* Host: disk at 84 % before the last run (145 G free), MemAvailable ≥ 40 G
  throughout; every ART run went through `run-exclusive.sh` with `--mem 8G`.
