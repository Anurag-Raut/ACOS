# MODA11Y5 — overlay windows, the tracking-flag root cause, the dex codec drift

Branch `moda11y5` (from `moda11y4` = `d78fded0`), 2026-11-16.

## Status

**PARTIAL — steps 1–4 are implemented and pass on real ART; step 5 (the
`getWindows` capability gate) is not implemented** and the brief's stop rule
applies (see §5: the parsers exist, the resource-resolution glue does not).

| Brief step | State |
|---|---|
| 1. `git merge modules` (cfed3eea: the single module control path), unify, keep both sides' tests | **DONE** |
| 2a. The only red `acos-services` test (`parcelable::codec_field_orders_match_the_framework_dex`) at its root cause | **DONE** — the image dex was right and the *spec* (and the runtime encoder) were wrong, see §2 |
| 2b. `AccessibilityWindowAttributes` codec from the dex | **DONE** (spec + `writeCharSequence` field kind) |
| 3. `FLAG_TRACKING`: verify AOSP, remove the sender-side flag if the wire never carries it, fix the real cause | **DONE, proven on ART** — the wire never carries it; the real defect was **which window** got the key (§3) |
| 4. `TYPE_ACCESSIBILITY_OVERLAY`: WMS acceptance on the bound service's token, LAYER1 layer 31, `accessibility_overlay` in the observe tree, probe overlay view, both window types asserted (BACK last) | **DONE, passes on ART** (§4) |
| 5. `getWindows` gate from the service's `accessibility-service` meta-data XML | **NOT DONE — stopped at the documented point** (§5) |

`scenarios/a11y-probe.yaml` passes 12/12 steps on ART
(`results/handoff/MODA11Y5/a11y-verdict.json`, `status=pass`).

Commits: `3bd552b4` (the `modules` merge — it also carries the step-2 codec
fix, because the two touched the same file and were staged together),
`317dcfae` (steps 3-4: the focused-window Back fix, overlay windows,
probe/scenario, tests).

## 1. Merge `modules` (`cfed3eea`)

Three content conflicts, all resolved by keeping both sides:

* `modules/mod.rs` — `accessibility` **and** `biometric`/`bluetooth`;
* `tools/build-apps.sh` — both `a11y-probe` and `tail-probe` are built;
* `parcelable.rs` — the module side's richer `read_dynamic` (`VAL_LIST`,
  `VAL_OBJECTARRAY` with `check_value_length`) supersedes this branch's
  one-line `STRINGARRAY` arm; nothing from HEAD was unique in that hunk.

Then the two forced adaptations of `modules`' control path:

* `AccessibilityModule::control(&mut self, _host: &mut ServiceHost, op, args)`
  — the trait now passes the session host so a control op can deliver what the
  app observes; the accessibility ops (`status`, `list_events`,
  `global_action`) deliver nothing through the host, so the parameter is
  unused and named `_host` rather than silently dropped;
* `ModuleManifest { features: &[], process_initializers: &[] }` — the
  accessibility module publishes no system feature and needs no framework
  initializer (its state is written by `attach`).

Both sides' tests are kept and pass: 64 test binaries green
(`cargo test -p acos-services -p acos-wm --no-fail-fast`), including the
938-line `sensors_module` suite, `module_kit`, `moda11y2_bind`,
`moda11y3_windows`, `moda11y4_back` and the new `moda11y5_overlay`.

## 2. The red parcelable test: the image dex was right, the spec was wrong

The failure was `AccessibilityServiceInfo consumed 22/22 calls, spec 23
fields, all_fields=false`. The disassembly of the image's own
`classes.dex` (`AccessibilityServiceInfo.writeToParcel(Parcel;I)V`) ends:

```
006a: iget v0, v2, …AccessibilityServiceInfo;.mMotionEventSources:I
006c: invoke-virtual {v3, v0}, Landroid/os/Parcel;.writeInt:(I)V
006f: return-void
```

22 calls, and its `initFromParcel` stops at the same field. The AOSP tag in
`.cache/aosp` (`android-14.0.0_r75`,
`AccessibilityServiceInfo.java:1392-1393`) writes one more int,
`mObservedMotionEventSources` — the image build does not. Per the wire-format
rule the **image dex is authoritative**, so:

* the spec in `parcelable.rs` drops the 23rd field (with the dex + source
  citation and the difference spelled out);
* `misc::service_info_value` (what the module actually encodes) drops the
  trailing `Value::Int(0)` — leaving it would have put 4 stray bytes in every
  `getInstalledAccessibilityServiceList` reply.

The scanner was *not* touched: it was right.

The test is green now, and the `AccessibilityWindowAttributes` spec was added
from the same dex (`writeCharSequence(mWindowTitle)` + `writeParcelable(
mLocales)`, 2 calls), with `FieldOp::CharSequence` extended to accept a direct
`Parcel.writeCharSequence` (same wire form as `TextUtils.writeToParcel`).

## 3. `FLAG_TRACKING`: not a host-side flag — and the real bug was the target window

### What AOSP does

`KeyEvent.FLAG_TRACKING` is decided entirely inside the app process
(`.cache/aosp/base/core/java/android/view/KeyEvent.java`):

* down: `KeyEvent.dispatch(down, state, target)` → `receiver.onKeyDown`; when
  that returns `true` and set `FLAG_START_TRACKING` through
  `event.startTracking()`, `state.startTracking(event, target)` records the key
  code (2960-2975);
* `Activity.onKeyDown(KEYCODE_BACK)` is exactly that handler
  (`Activity.java:4018-4028`);
* up: `KeyEvent.dispatch` runs `state.handleUpEvent(event)` **before**
  `receiver.onKeyUp`, and that sets `FLAG_TRACKING` when the down was tracked
  (3088-3100).

So `Activity.onKeyUp`'s `event.isTracking()` test (`Activity.java:4117-4136`)
is satisfied by a plain up event. Nothing in the Java input path demonstrates
InputDispatcher putting the bit on the wire, and the dex/source both keep the
tracking state in `DispatcherState`, which is per-`AttachInfo` and reset only by
`ViewRootImpl.handleWindowFocusChanged` (`ViewRootImpl.java:4359`) and
`View.dispatchDetachedFromWindow` (`View.java:8618`). MODA11Y4's sender-side
flag therefore papered over a defect instead of fixing it: **it made
`onKeyUp` see `isTracking()` even when the app had never tracked the down.**

### The real defect the ART run exposed

With the flag removed the first ART run showed the key *arriving nowhere*: no
`key_down` line at all. `A11yState::back_token` used
`InputHost::newest_token()` — "the most recently registered window channel" —
so once the probe's service added its `FLAG_NOT_FOCUSABLE`
`TYPE_ACCESSIBILITY_OVERLAY` window, Back went to the overlay's channel and the
activity never saw it. AOSP only ever dispatches a key to the **focused**
window (`InputDispatcher::dispatchKeyLocked` → `getFocusedWindowHandle`).

Fix (`misc.rs` + `input_dispatch.rs`): `back_token` is now
`InputHost::focused_token()` (the channel `InputDispatch::set_focus` last
published `FOCUS(true)` to), and "no focused window" is a structured refusal,
never a guess.

Real ART log (`results/handoff/MODA11Y5/a11y-probe-logcat.txt`):

```
I AcosA11yProbe: back_requested
I AcosA11yProbe: global_action back=true
I AcosA11yProbe: key_down code=4 tracking=false repeat=0 flags=0
I AcosA11yProbe: key_up code=4 tracking=true
I AcosA11yProbe: back_pressed
```

`key_up tracking=true` is now the app's own `DispatcherState`, and
`global_action back=true` remains honest: the key was published to the focused
window.

Tests:

```
crates/acos-services/tests/moda11y4_back.rs
  back_is_a_real_key_on_the_focused_window_without_a_tracking_flag  (asserts
    down flags == 0, repeat == 0, up FLAG_TRACKING == 0, shared downTime)
  back_goes_to_the_focused_window_not_the_newest_one   (new: fails before the
    fix — the overlay's channel got the pair)
  back_without_a_live_window_is_a_structured_refusal
  other_global_actions_stay_structured_refusals
```

Still unproven (unchanged, MPROC5's track): the destroy transaction after
`finish()` — no `onPause`/`onStop`/`onDestroy`, so the scenario does not assert
`activity_destroyed`.

## 4. `TYPE_ACCESSIBILITY_OVERLAY`

### The acceptance rule (AOSP)

`sanitizeWindowType` only assigns `TYPE_ACCESSIBILITY_OVERLAY` to a client whose
`windowToken` *is* an accessibility overlay token
(`WindowManagerService.java:9914-9928`), and `addWindow` answers
`ADD_BAD_APP_TOKEN` for an accessibility overlay added on a token of another
type (`WindowManagerService.java:1630-1640`). The token itself is created by the
accessibility manager service and reaches the app through
`IAccessibilityServiceClient.init(connectionId, windowToken)`
(`AccessibilityService.java:2803-2812`), which the framework then installs as
the default token of the service's `WindowManager`
(`AccessibilityService.java:2660-2670`) — so `wm.addView(view, params)` with
`TYPE_ACCESSIBILITY_OVERLAY` and *no explicit token* is the AOSP way to add one.

### What ACOS does now

* `acos-wm`/`wms.rs`: `WindowManager` keeps an `overlay_tokens: BTreeSet<u64>`
  with `add_overlay_token`/`remove_overlay_token`/`is_overlay_token`
  (`WindowManagerService.addWindowToken`/`removeWindowToken`), and
  `add_to_display_as_user` refuses a `TYPE_ACCESSIBILITY_OVERLAY` window whose
  `LayoutParams.token` is not registered — `ADD_BAD_APP_TOKEN`, no window
  created. Other window types keep the previous behaviour (no token policy yet),
  which the test states explicitly.
* `acos-services`: the bind registers the token (`on_publish_service` →
  `A11yState::register_overlay_token`) and `disable()` releases it with the
  service (`WindowManagerService.removeWindowToken`).
* the observe tree already tagged the window: `WindowKind::name()` maps
  `AccessibilityOverlay` → `accessibility_overlay`, so `tree/*.json` shows
  `{"kind": "accessibility_overlay", "type_name": "TYPE_ACCESSIBILITY_OVERLAY",
  "z": 1, "focus": false}` above the activity; `getWindows`' type 4 mapping
  (`misc::a11y_window_type`) landed in MODA11Y3.
* `fixtures/apps/a11y-probe`: `A11yProbeService` adds a 240×120
  `TYPE_ACCESSIBILITY_OVERLAY` `TextView` on the main thread after its click
  probe, using the framework's default token, and logs
  `overlay_added type=2032 token=set`.
* `scenarios/a11y-probe.yaml`: asserts `overlay_added type=2032 token=set` +
  `overlay_probe_done`, then

  ```yaml
  - assert_windows:
      order: [application, accessibility_overlay]
      types: [TYPE_BASE_APPLICATION, TYPE_ACCESSIBILITY_OVERLAY]
      focus: application
      focus_type: TYPE_BASE_APPLICATION
  ```

  and keeps the BACK steps last (brief requirement).

### The one thing the guest cannot assert (and why), stated plainly

A **second** `getWindows()` after the overlay add never returns: the run's log
ends at `overlay_added` and the scenario's next step proceeds, so the probe's
worker thread is stuck inside the transaction. The window-session path holds the
window-manager state lock for the whole transaction
(`window_session.rs::raw_transact` takes `self.lock()` before dispatch and keeps
it while the overlay's own `relayout`/`finishDrawing` is in flight), and the
`getWindows` handler wants the same lock (`A11yState::window_snapshot` →
`wms.lock()`), i.e. the two block each other while one of them waits on the
guest. So the guest-side `getWindows` type-4 assertion is **not** in the
scenario; the overlay's type is asserted from the host's own stack
(`assert_windows`, types `TYPE_BASE_APPLICATION`/`TYPE_ACCESSIBILITY_OVERLAY`)
and the type-4 mapping stays unit-proven (`moda11y3_windows`). Root-causing the
lock (shorten the critical section, or serve `getWindows` from a snapshot) is
queued, not worked around with a sleep.

## 5. `getWindows` capability gate — stopped at the documented point

The brief allows stopping here if nothing can read a compiled XML resource. The
pieces that exist:

* `acos-apk/src/axml.rs::parse` — a binary-XML (compiled resource) parser;
* `acos-apk/src/arsc.rs::Resources::resolve(id)` — resource-id lookup;
* `acos-apk/src/manifest.rs` — `MetaDataValue::Reference(u32)` for
  `android:resource="@xml/accessibility_service_config"`.

What does **not** exist and would have to be written: resolve the meta-data
reference to the resource entry's *file path*, read that zip entry from the APK,
parse it as AXML, and map `android:canRetrieveWindowContent` +
`android:accessibilityFlags` (`FLAG_RETRIEVE_INTERACTIVE_WINDOWS` =
`0x2`) onto the connection. Until then `getWindows` answers from the LAYER1
stack with **no** capability check, and the fixture's manifest does not claim a
capability that is not enforced — the module report still says so. Not
implemented, not faked.

## Evidence

```
results/handoff/MODA11Y5/a11y-verdict.json          status=pass, 12/12 steps
results/handoff/MODA11Y5/artifacts/verdict.json     the same file (canonical)
results/handoff/MODA11Y5/artifacts/logs/logcat.txt  full guest log
results/handoff/MODA11Y5/a11y-probe-logcat.txt      the probe's lines only
results/handoff/MODA11Y5/artifacts/tree/*.json      the observe tree (windows[])
results/handoff/MODA11Y5/artifacts/frames|steps
```

Command (private runtime dir, shared lock):

```bash
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export ACOS_RUNTIME_DIR=$PWD/.cache/moda11y5-run
export ACOS_SESSION_SOCKET=$ACOS_RUNTIME_DIR/session.sock
export ACOS_ART_BIN=$PWD/.cache/moda11y5-target/release/acos-art
tools/art-runtime/run-exclusive.sh --task MODA11Y5 -- \
  tools/guarded.sh --mem 8G --time 700 -- \
  .cache/moda11y5-target/release/acos verify scenarios/a11y-probe.yaml \
  --engine art --artifacts results/handoff/MODA11Y5/artifacts
```

## Tests

```
cargo test -p acos-services -p acos-wm --no-fail-fast   64 binaries, all green
  (new) acos-wm wms::tests::accessibility_overlay_needs_the_accessibility_overlay_token
  (new) acos-services tests/moda11y5_overlay.rs
  (new) acos-services tests/moda11y4_back.rs::back_goes_to_the_focused_window_not_the_newest_one
  (fixed) acos-services tests/parcelable.rs::codec_field_orders_match_the_framework_dex
  (updated) tests/moda11y3_windows.rs carries the overlay token its fixture needs
```

## Remaining (smallest next action first)

1. **Step 5**, the capability gate: `arsc` resolve → zip read → `axml::parse` →
   `canRetrieveWindowContent` / `FLAG_RETRIEVE_INTERACTIVE_WINDOWS`, then gate
   `getWindows`/`getRootInActiveWindow` and let the fixture declare the flags.
2. The `getWindows`-after-overlay lock (window-session critical section vs the
   a11y window snapshot) — then the guest-side type-4 assertion goes back into
   the scenario.
3. The `finish()`/destroy lifecycle after Back (MPROC5's track) — unchanged.

## Environment notes

* Target `.cache/moda11y5-target`, built with
  `CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0`;
  deleted at the end of the session together with `fixtures/build` (hardlink
  copy) and `.cache/moda11y5-run`.
* Every ART/gate command went through `tools/art-runtime/run-exclusive.sh`
  (`--mem 8G`; builds `--mem 12G/14G`). Disk 77→78 % throughout, well below
  the 86 % stop line.
