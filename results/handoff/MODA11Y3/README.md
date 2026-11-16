# MODA11Y3 — the service-facing interaction surface (steps 1 and part of 2)

Branch `moda11y3` (from `moda11y2` = 2a08342d), 2026-11-16.
Worktree `.t3/worktrees/acos/t3-1914926f-51e00d0db3fd77f0`.

## Status

**PARTIAL — step 1 and the `performAction` half of step 2 are implemented and
unit-proven; no ART proof was run.** The one-hour rule stopped the track here.

| Brief step | State |
|---|---|
| 1. `getWindows` / `getWindow` + `findAccessibilityNodeInfoByAccessibilityId` / `ByText` / `ByViewId` relayed to each registered `IAccessibilityInteractionConnection`; window list from the LAYER1 stack (type, layer, bounds, focused/active) | **DONE (unit-proven)** |
| 2. `performAction` on nodes | **DONE (unit-proven, relay half)** |
| 2. `performGlobalAction(BACK)`, `dispatchGesture`, `takeScreenshot` | **NOT DONE** |
| 3. `TYPE_ACCESSIBILITY_OVERLAY` windows | **NOT DONE** (the LAYER1 policy already assigns layer 31; the a11y relay/tree side is untouched) |
| 4. a11y-probe bubble scenario + Gaten proof | **NOT DONE** |
| Orchestrator note (a) parcelable dex scanner ("last `writeToParcel` call of `AccessibilityServiceInfo`") | **NOT DONE** |
| Orchestrator note (b) `AccessibilityWindowAttributes` codec | **NOT DONE** |
| Orchestrator note (c) AMS test for the `serviceDoneExecuting` void reply | **NOT DONE** |

Nothing claims ART behaviour: no `acos verify` run was made on this branch.

## What landed

### Step 1 — windows and node queries (`d3b6c7cc`)

* `A11yState` now holds the LAYER1 window source
  (`Option<Arc<Mutex<crate::wms::WmsState>>>`, wired in
  `registry.rs::install` next to the package manager) and an accessibility
  window-id table (`window_id_for`, monotonic per `IWindow`, never reused).
  `addAccessibilityInteractionConnection` returns that id, so
  `AccessibilityWindowInfo.getId()` and the `accessibilityWindowId` a service
  passes back are the same number for the life of the window.
* `A11yState::window_snapshot` / `window_list_readonly` read
  `wm.layer_stack().ordered()` (bottom-first), reverse it (**AOSP `getWindows`
  is topmost first**) and set `layer = count - 1 - index` — the same inversion
  `AccessibilityWindowManager.updateWindowsLocked` does. Type mapping is
  `AccessibilityWindowInfo.TYPE_*` (application 1, input method 2, system 3,
  accessibility overlay 4, magnification 6). Focused/active come from
  `wm.focus`.
* `A11yConnectionNode` answers `getWindows` (code 10) with the framework's own
  `WindowListSparseArray` layout and `getWindow` (code 9) with a typed
  `AccessibilityWindowInfo`, both transcribed from the API-34
  `framework.jar` (see "Framework facts").
* `findAccessibilityNodeInfoByAccessibilityId` (3), `...ByText` (4) and
  `...ByViewId` (5) resolve the window (including `ACTIVE_WINDOW_ID` /
  `ANY_WINDOW_ID`), queue a **oneway** `IAccessibilityInteractionConnection`
  request (codes 1/3/2) against the app's own binder (`JAVA_TAG` handle), and
  answer the service with the window's reported package names — the reply
  shape AOSP's `AbstractAccessibilityServiceConnection` returns.
* `A11yState::interactions` records each relayed request (bounded 128) and the
  module report gains `windows[]` and `interactions[]`.

### Step 2 (part) — `performAction` (`f8967384`)

* `performAccessibilityAction` (8) relays code 6 with the node id, action id,
  the caller's `Bundle` (byte-forwarded with its binder objects via
  `Parcel::append_from`), interaction id and callback, and answers the
  "request went out" boolean. The real result arrives on
  `setPerformAccessibilityActionResult`, which the guest dispatches locally.

### Why the relay can reply before the app answers (the key design point)

The image's `AccessibilityInteractionClient` **waits** for the callback
(`waitForResultTimedLocked`, 5 s) *after* the AIDL call returns, and treats a
`null` `String[]`/`false` as "no request went out". The peer is single-threaded,
and reverse deliveries are only framed after `ServiceHost::transact` returns, so
the host **must** reply first and queue the delivery. The app then invokes the
service's callback; both endpoints are guest-local binders (the driver rewrites
them to `BINDER_TYPE_BINDER` on the way in, `binder_driver.c` `xform_objects`
dir 1), so the callback is a local dispatch inside the guest and needs no host
round trip.

## Framework facts (transcribed from `.cache/boot/android-34` / `.cache/aosp`)

`IAccessibilityServiceConnection` (API 34) transaction codes and signatures —
the image returns `String[]` from the three `find*` methods:
`3 findAccessibilityNodeInfoByAccessibilityId(int,long,int,callback,int,long,Bundle)`,
`4 ...ByText(int,long,String,int,callback,long)`,
`5 ...ByViewId(int,long,String,int,callback,long)`,
`8 performAccessibilityAction(int,long,int,Bundle,int,callback,long) -> boolean`,
`9 getWindow(int) -> AccessibilityWindowInfo`,
`10 getWindows() -> AccessibilityWindowInfo.WindowListSparseArray`,
`33 dispatchGesture(int, ParceledListSlice, int)`,
`35 getOverlayWindowToken(int) -> IBinder`,
`37 takeScreenshot(int, RemoteCallback)`.

`IAccessibilityInteractionConnection` codes: 1 by-id, 2 by-view-id, 3 by-text,
4 findFocus, 5 focusSearch, 6 performAccessibilityAction, 7 clearAccessibilityFocus,
8 notifyOutsideTouch, 9 takeScreenshotOfWindow, 10 attachAccessibilityOverlayToWindow.
The by-id order is `(long nodeId, Region bounds, int interactionId, callback,
int flags, int pid, long tid, MagnificationSpec, float[] matrix, Bundle)`.

`AccessibilityWindowInfo.writeToParcel` (API 34): displayId, type, layer,
booleanProperties (ACTIVE=1, FOCUSED=2, ACCESSIBILITY_FOCUSED=4, PIP=8), id,
parentId, taskId, `Region` (int count + left/top/right/bottom per rect), title
(`writeInt(span)` + `writeString`), anchorId, transitionTime, child ids
(int count + int each), connectionId, LocaleList (`writeParcelable`, null =
`writeString(null)`).
`WindowListSparseArray.writeToParcel`: display count, then per display
`writeParcelableList` (**int N + N × [class name + payload]**), then the
display id.

## Tests

```
crates/acos-services/tests/moda11y3_windows.rs   7/7 pass
results/handoff/MODA11Y3/windows-tests.log
  get_windows_reports_the_layer1_stack_topmost_first
  a_find_request_is_relayed_to_the_window_connection
  a_find_by_text_request_uses_the_text_connection_code
  a_window_without_a_connection_is_not_routed
  the_report_names_the_window_stack
  a_direct_state_without_a_window_source_reports_no_windows
  a_perform_action_request_is_relayed_with_the_action_payload

cargo test -p acos-services --lib      101/102 — the 1 failure is
  manifest_resolve::tests::real_apks_resolve_like_aapt2_could_not, which needs
  fixtures/build (not present in this worktree); unrelated to this branch.
cargo test -p acos-services --test module_kit   11/11
cargo test -p acos-services --test moda11y2_bind  4/4
```

Every test runs through `ServiceHost::transact` (the real dispatch path) against
a real `WmsState` window add and a real
`IAccessibilityManager.addAccessibilityInteractionConnection`.

## Remaining (smallest next action first)

1. **ART proof of step 1**: extend `fixtures/apps/a11y-probe` so the service
   logs `windows=N first_type=… layer=…` from `getWindows()` and
   `root=<class>` from `getRootInActiveWindow()` (which is
   `findAccessibilityNodeInfoByAccessibilityId(ACTIVE_WINDOW_ID, ROOT_NODE_ID)`),
   then assert both in `scenarios/a11y-probe.yaml`. This is the first place the
   guest-side callback actually runs; watch for the `interrogatingPid`/Region
   arguments the framework validates.
2. **Step 2 rest**: `performGlobalAction(BACK)` through the session's existing
   Back path (HOME/RECENTS/NOTIFICATIONS stay structured unsupported);
   `dispatchGesture` — the base has no REL3S gesture stream, so implement taps
   through `input_dispatch` and say so; `takeScreenshot` from the compositor
   frame honouring `FLAG_SECURE`.
3. **Step 3**: `TYPE_ACCESSIBILITY_OVERLAY`. The LAYER1 policy already returns
   31 for it (`crates/acos-wm/src/layers.rs`), and `getWindows` already reports
   it as type 4, so the work is: the a11y overlay's `WindowManager.addView`
   must be accepted for a bound service (token/permission) and the tree must
   carry the window tag.
4. **Step 4**: the a11y-probe bubble scenario (`assert_windows` for type/z,
   drag, tap handled by the overlay, `takeScreenshot` frame), then Gaten.
5. Orchestrator notes (a), (b), (c) — untouched (see the table above).

## Environment notes

* Built with `CARGO_TARGET_DIR=$PWD/.cache/moda11y3-target`,
  `CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0`;
  the target directory is deleted.
* `fixtures/build` was **not** created in this worktree (no ART run), so the
  one `--lib` failure above is environmental.
