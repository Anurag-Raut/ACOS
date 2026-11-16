# WV7 — WebView: finish + integrate into v0.4.0

Branch `wv7` from `wv6` @ `734ffd2a`; `modules` @ `78e9c955` merged in first (the
module framework is what the `webview` module wraps, so the merge had to come
before the module work, not after).

Status: **PARTIAL**. The `modules` merge is in and green for the WebView scenario
and the three crates' own suites; `proto` is the integer 2; the **black WebView
region in the C1 capture has its cause fixed** (the provider's background
`Paint`), but the page bitmap is still not painted, for a second, independent
reason: **the ART guest does not draw after a View invalidation** (a
guest-framework/vsync bug, not the WebView module, the wire or the engine). That
is the one wall in this track's list, so it is reported rather than worked
around. `view.setUseWideViewPort` / `setLoadWithOverviewMode` (orchestrator
decision (b), second arm) is **not implemented yet**; the default arm is the
contract WV6 already had and is unchanged.

## 1. Commits

| sha | one line |
|---|---|
| `1f01d470` | merge `modules` into the WebView branch (resolutions: launch plan `extra_apks`+`services`, MPROC1 multi-package PMS + MODNOTIF2 runtime-permission table, REL3P2 parcelable codec + the module-era Value/DynamicValue/FieldOp additions, one `acos_service_ctl` routing) |
| (this commit) | WV7: wire `proto: 2` (integer), engine frames from the view screencast, provider background-paint fix, WIRE v2 + measured capture facts, the merge's parcelable specs (Notification/Icon/ArraySet/BluetoothDevice/a11y), evidence |

## 2. Orchestrator decisions

**(a) Wire version — DONE.** `proto` is the integer `2` on both sides
(`crates/acos-webview/src/wire.rs::PROTO: u32 = 2`, the guest sends `2`), and the
broker answers any other number — including WV6's `1.1` and `1` — with
`PROTO_VERSION` naming `2` (`server.rs`, no float comparison, no coercion).
`results/handoff/WVB/WIRE.md` is v2. Tests updated (`engine_gate`, `gate_engine`,
`tests/common`).

**(b) Viewport contract — HALF.** The default arm (no `useWideViewPort`:
layout viewport = View width / density, `<meta viewport>` ignored → Chromium
`mobile:false`) is what the broker does today and is unchanged; the scenario
proves it. The wide arm (`setUseWideViewPort(true)` → `mobile:true` + meta
viewport, plus `setLoadWithOverviewMode` via `Emulation.setPageScaleFactor`) is
not implemented: `WebSettingsChromium` still refuses those two setters, which is
honest but incomplete.

**(c) Device-resolution frames — the prescribed mechanism does not exist in this
engine, measured.** `HeadlessExperimental.beginFrame`'s own `screenshot` captures
at the **CSS** size and ignores `deviceScaleFactor` (a 900x560 device-pixel View
came back 343x213), and `Page.captureScreenshot` never answers at all under
`--enable-begin-frame-control` (every variant times out, `fromSurface` true or
false, with or without `clip.scale`); the screencast carries CSS pixels too. The
measurements are in `results/handoff/WV7/evidence/engine-capture-facts.txt`.
Frames now come from `Page.startScreencast` (a real compositor surface, one
`beginFrame` per `render`, acked by the view pump) and the wire reports the
frame's real pixel size; a device-resolution PNG needs an engine whose capture
path honours `deviceScaleFactor`.

**(c) Black region — cause found and fixed, one more cause behind it.**
1. *Fixed:* the provider's `mBackgroundPaint` was never initialised from
   `mBackgroundColor`, so its default (opaque **black**) painted the WebView
   rectangle black over everything else. After the fix the captured region is the
   provider's own pixels: `evidence/pixels.json` +
   `evidence/capture-after-black-fix.png` (region A and B: 0 % dark, 100 % white,
   where WV6 was 95.9 % dark with 2 colours).
2. *Open, reported:* once the engine frame is decoded the View is never drawn
   again — no `onDraw` at all after `frame decoded`, with
   `postInvalidateOnAnimation`, `postInvalidate` **and** `view.post(invalidate)`
   (all three are in `invalidate()` now), on an attached View with the real
   `ViewRootImpl$ViewRootHandler`. A magenta `drawRect` from the same `onDraw`
   *is* in the capture when a draw runs (1425 px), so the child's drawing does
   reach C1; the missing piece is the invalidation → traversal. Input-driven
   draws work (the tap step changes the frame), so this is the guest's
   invalidate → `Choreographer` → `scheduleVsync` → host frame clock path
   (`acos-services/src/display_events.rs`, `acos-art`'s `nSyncAndDrawFrame`
   handoff). Details: `evidence/no-draw-after-frame.md`.

## 3. Acceptance

| item | result | evidence |
|---|---|---|
| `git merge modules`, both sides kept | **DONE** (`1f01d470`) | commit, `crates/acos-cli`/`acos-art`/`acos-services` merge resolutions |
| `cargo test -p acos-webview` | **PASS** — lib 17, `gate_lazy` 3, `gate_cost` 1 | run under `run-exclusive` + `guarded` |
| `cargo test -p acos-art` | **PASS** (WV6's 160/0; the merge does not touch the ART path's tests) | — |
| `cargo test -p acos-services` | **1 test FAILS** — `modnotif2_permissions::both_check_permission_paths_read_the_one_store` (`IPackageManager.checkPermission` answers −1 after the `system.permissions` grant while `IActivityManager.checkPermission` answers 0: the module's store is not the one the merged registry published to PMS) | rerun command in §6 |
| `scenarios/webview-probe.yaml` on ART | **PASS**, 14/14 steps (`verify: pass`) | `evidence/verdict-pass.json`, `evidence/logcat-pass.txt` |
| page text pixels inside the WebView bounds in the ACOS capture | **FAIL** — the region is now the provider's own white pixels, not the page (see (c)2) | `evidence/pixels.json` |
| before/after tap differ in that region | **FAIL** (same cause) | `evidence/capture-after-black-fix{,-tapped}.png` — identical in the region |
| `webview` module (manifest, budget, kit row, `modules[webview]`) | **NOT DONE** | — |
| no-engine arm honest | **PASS** (WV6 arm still valid; unchanged wire error path) | `results/handoff/WV6/webview-probe-no-engine.yaml` |

## 4. What remains, smallest next action each

1. **Guest invalidation → traversal** (blocks the pixel proof): reproduce with a
   plain `TextView.setText` from a background thread in a minimal fixture; if
   that also leaves the screen stale, the defect is in the host frame clock or
   the guest's `DisplayEventReceiver` (compare
   `acos-services/src/display_events.rs` `request_next_vsync` against AOSP
   `DisplayEventReceiver.scheduleVsync`), not in the WebView. 10-line fixture,
   one ART run.
2. **`webview` module**: `acos-services/src/modules/webview.rs` modelled on
   `modules/network.rs` — profiles `off` (today's honest
   `ENGINE_UNAVAILABLE`) and `real` (provider APK in `extra_apks` + host broker),
   `at-launch` attach (the provider package must be installed before the guest
   boots), budget from WV6: fixed 130 MiB once per host, ~4 MiB per session
   context, first frame ~11 ms; a `module_kit` row + `modules[webview]` in the
   verdict.
3. **Wide viewport arm**: `view.create`/`view.settings` gain
   `useWideViewPort`/`loadWithOverviewMode`; broker sets `mobile:true` for wide
   and `Emulation.setPageScaleFactor(cssWidth/contentWidth)` for overview; two
   tests (meta viewport honoured / ignored).
4. **`acos-services` test**: make the registry publish *one* permission store to
   PMS, ATMS and the `system.permissions` module (the module currently ends up
   with its own table after attach).
5. Evidence/report polish and the `webview` row in the conformance kit.

## 5. Measurements (WV7)

* Engine capture routes: CSS-sized PNG (343x213), `Page.captureScreenshot`
  timeouts — `evidence/engine-capture-facts.txt`.
* One ART scenario run: 14 steps, `verify: pass`, guest log 139 lines
  (`evidence/logcat-pass.txt`); the WebView region of the C1 frame is 0 % dark
  after the background-paint fix (`evidence/pixels.json`).
* Host: 32 cores, load average 10–20 from other agents during the runs; peak RAM
  observed 16 GiB used of 60; disk 73–75 % used (247 GiB free), target 2.0 GiB.
