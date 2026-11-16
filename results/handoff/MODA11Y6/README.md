# MODA11Y6 — the `getWindows`-after-overlay "deadlock", the capability gate, gestures and screenshots

Branch `moda11y6` (from `modules` = `9eb1b8ee`, i.e. MODA11Y5 + MODNOTIF5), 2026-11-16.
Worktree `.t3/worktrees/acos/t3-2c775ebe-044d741e708e27d8`.

## Status

**IN PROGRESS** — this file is updated as each brief step lands. The
per-step table below always states what is *proven on ART*, not what is
intended.

| Brief step | State |
|---|---|
| 1. WM lock: no host state lock across a guest round trip; test: concurrent relayout + `getWindows`; restore the guest-side type-4 overlay assertion | **root cause found, fix in progress** (§1) |
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

Deliver `TYPE_WINDOWS_CHANGED` to the bound service when the window stack
changes, the way AOSP does, and keep the window-session critical section free
of anything that can wait on the guest (the discipline the orchestrator asked
for, and the reason the trace above was needed: it proves the host never held
the lock across a guest round trip in this path). Then the guest-side type-4
assertion goes back into `scenarios/a11y-probe.yaml` on the first retry.

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
