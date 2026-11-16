# MODA11Y6 — the `getWindows`-after-overlay "deadlock", the capability gate, gestures and screenshots

Branch `moda11y6` (from `modules` = `9eb1b8ee`, i.e. MODA11Y5 + MODNOTIF5), 2026-11-16.
Worktree `.t3/worktrees/acos/t3-2c775ebe-044d741e708e27d8`.

## Status

**IN PROGRESS** — this file is updated as each brief step lands. The
per-step table below always states what is *proven on ART*, not what is
intended.

| Brief step | State |
|---|---|
| 1. WM lock: no host state lock across a guest round trip; test: concurrent relayout + `getWindows`; restore the guest-side type-4 overlay assertion | **DONE, passes on ART (13/13)** (§1) |
| 2. `getWindows` capability gate from the service's `accessibility-service` meta-data XML | not started |
| 3. `dispatchGesture` (tap + swipe) and `takeScreenshot` | not started |
| 4. Gaten bubble (`TYPE_ACCESSIBILITY_OVERLAY` owned by `GatenAccessibilityService`) | not started |

## 1. The step-1 symptom is not a lock deadlock — it is a stale guest cache

MODA11Y5 reported that a **second** `getWindows()` after the probe's overlay
add "never returns" and blamed the window-session critical section (the
window-manager state lock held for the whole transaction vs
`A11yState::window_snapshot`). Two runs here disprove that and locate the real
cause.

### What the host saw

`ACOS_WMS_TRACE` + a trace file (`ACOS_TRACE_FILE`, added to
`crates/acos-services/src/trace.rs`) with `ACOSA11Y`/`ACOSINPUT` probes gives
the host's own order of events for the failing run
(`wms-trace-repro4.log`):

```
ACOSWMS enter code=2 (addToDisplayAsUser) … exit      # activity
ACOSWMS enter code=5 (relayout) … exit
ACOSWMS enter code=9 (finishDrawing) … exit
ACOSA11Y getWindows enter / snapshot=1                 # probe #1 (1 window)
ACOSWMS enter code=2 (addToDisplayAsUser) … exit       # the a11y overlay
ACOSWMS enter code=5 (relayout) … exit
ACOSWMS enter code=9 (finishDrawing) … exit
ACOSWMS enter code=5 (relayout) … exit
```

**There is no second `ACOSA11Y getWindows` line at all.** The guest never
issued the transaction, so nothing in the host was blocking it; every window
transaction in the run completed (`enter` and `exit` both present, lock
acquired and released). The host's window stack did have the overlay by then —
`assert_windows` (step 9 of the same scenario) reports
`[application, accessibility_overlay]` with `TYPE_ACCESSIBILITY_OVERLAY` and the
overlay's real bounds `[40,368,280,488]`.

### Why the guest returns 1 window (and then hangs)

The image's own framework source is authoritative here
(`.cache/aosp`, `android-14.0.0_r75`):

* `AccessibilityService.getWindows()` →
  `AccessibilityInteractionClient.getWindows(connectionId)`
  (`AccessibilityService.java:1060-1062`);
* `AccessibilityInteractionClient.getWindowsOnAllDisplays`
  (`AccessibilityInteractionClient.java:489-532`) asks the **guest-side
  `AccessibilityCache` first** and returns it on a hit without any IPC:
  `windows = cache.getWindowsOnAllDisplays(); if (windows != null) return
  windows;`
* the probe's first `getWindows()` (before the overlay) populated that cache
  with the one application window — so the post-overlay query is a **cache
  hit** and returns the same one window. That is exactly the
  `windows2 count=1` in `repro/`.

The cache is only invalidated by an accessibility event: `AccessibilityCache
.onAccessibilityEvent` (`AccessibilityCache.java:276-288`) clears it for
`TYPE_WINDOWS_CHANGED` (any change other than
`WINDOWS_CHANGE_ACCESSIBILITY_FOCUSED`) and `TYPE_WINDOW_STATE_CHANGED`.
**ACOS delivers no window-change events**, so a bound service's window cache
can never learn about a window that was added after its first query. In AOSP
`AccessibilityManagerService`/`AccessibilityWindowManager` sends
`TYPE_WINDOWS_CHANGED` to the bound services whenever the window stack
changes.

### The "hang"

The probe's real-world query loop is 25 × 200 ms (a service retries while the
window registers). Against a stale cache every attempt returns the same cached
list with no IPC, so the loop spins for its full 5 s while the scenario's
`assert_log` for `overlay_probe_done` (bounded, earlier) gives up first: the
run ends with the guest log stopped at `windows2_start`. Repro 2, 4, 5, 7 all
show that ending. No thread is deadlocked — the guest simply keeps answering
from a cache that AOSP would have cleared, and the scenario stops the session
before the loop finishes.

### The fix (step 1)

1. **`TYPE_WINDOWS_CHANGED` delivery on a window-stack change.** The window
   session (`window_session.rs`) hands a `WindowStackChange`
   (`Added`/`Removed`) to a sink after `addToDisplayAsUser`/`remove`; the sink
   (`registry::a11y_window_changes` → `a11y_bind::windows_changed_deliveries`)
   queues one `AccessibilityEvent.TYPE_WINDOWS_CHANGED` per change and returns
   the `IAccessibilityServiceClient.onAccessibilityEvent(event,
   serviceWantsEvent)` deliveries, addressed at the bound service's client
   binder. The event parcel is written in the image's own order
   (`AccessibilityEvent.writeToParcel` / `writeAccessibilityRecordToParcel`);
   two wire details were read out of the image dex, not guessed:
   * the AIDL `in AccessibilityEvent` argument is a `writeTypedObject`, so a
     presence `int 1` precedes the body (`IAccessibilityServiceClient$Stub`
     unmarshals it with `Parcel.readTypedObject`);
   * a null `CharSequence` is `int 1` (the *not-spanned* kind) followed by a
     null `String8`, not `int 0` — `int 0` selects `TextUtils`'
     `Spanned` branch and desynchronises the whole record
     (`TextUtils.writeToParcel` at `framework/classes3.dex` 0x455fda:
     `writeInt(1)`, `writeString8(cs == null ? null : cs.toString())`).
   The first attempt at the parcel made the guest abort (SIGABRT) — the dex
   read is what fixed it, and the run below is the proof.
2. **The lock discipline.** The sink is called **after**
   `raw_transact` drops the window-manager state guard, so the two states are
   never held in both orders (`window_snapshot` takes the window state while
   the accessibility state is held). The compiler now enforces it: the state
   guard borrows `self`, so calling the `&mut self` notifier under it does not
   build.

### Proof on ART

`scenarios/a11y-probe.yaml` passes **13/13** with the guest-side assertion
restored (`results/handoff/MODA11Y6/step1/a11y-verdict.json`,
`status=pass`), and the probe's own log is the evidence:

```
I AcosA11yProbe: overlay_added type=2032 token=set
I AcosA11yProbe: windows2_start
I AcosA11yProbe: onAccessibilityEvent type=4194304      <- TYPE_WINDOWS_CHANGED
I AcosA11yProbe: windows2 count=2 attempts=2            <- the retry, not the cache
I AcosA11yProbe: window2[0] id=2 type=4 layer=1 active=false focused=false
I AcosA11yProbe: window2[1] id=1 type=1 layer=0 active=true focused=true
I AcosA11yProbe: windows2 overlay=true
```

`type=4` is `AccessibilityWindowInfo.TYPE_ACCESSIBILITY_OVERLAY` and it is the
topmost window — asserted by the service itself, above the `assert_windows`
step that reads the host's own stack.

### Tests

```
crates/acos-services/tests/moda11y6_windows_changed.rs (new, 3 tests)
  a_window_add_and_remove_deliver_type_windows_changed_to_the_bound_service
    (fails with the sink unwired: "one TYPE_WINDOWS_CHANGED per added window;
     got []") — parses the event parcel field by field: sealed, type,
     windowChanges mask, connection id, source window id, INVALID_DISPLAY
  no_service_no_window_change_delivery
  a_stalled_window_change_sink_does_not_block_the_window_snapshot
    (the brief's concurrency test: a sink stalled for 1.5 s must not delay
     `A11yState::window_snapshot`; asserts < 500 ms)
```

### Evidence

```
results/handoff/MODA11Y6/wms-trace-repro4.log      host-side trace of the failing run
results/handoff/MODA11Y6/repro/                     first run: windows2 count=1 (cache hit)
results/handoff/MODA11Y6/repro4/                    the traced run (no second getWindows IPC)
```

Tooling added for that diagnosis (kept: an opt-in sink the orchestrator can
point any run at):

* `crates/acos-services/src/trace.rs::line` — stderr **and** `$ACOS_TRACE_FILE`;
* `ACOS_WMS_TRACE` is now also implied by `ACOS_TRACE_FILE`; the a11y
  `getWindows` handler and the input ack wait log one line each.
