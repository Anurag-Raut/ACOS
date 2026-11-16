# WV7 — WebView: finish + integrate into v0.4.0

Branch `wv7` from `wv6` @ `734ffd2a`; `modules` @ `78e9c955` merged in first (the
module framework is what the `webview` module wraps, so the merge had to come
before the module work, not after).

Status: **PARTIAL** (the (c) must-fix is met). The `modules` merge is in and the
WebView scenario passes 14/14 on real ART; `proto` is the integer 2; and the
**black WebView region is gone from the ACOS capture**: the WebView bounds now
carry the real page (both pages' text as pixels, 257 colours) and the
before/after captures differ there by 55 851 px (`evidence/pixels.json`,
`evidence/webview-region-tapped-crop.png`).

What is *not* finished: (1) a frame that arrives when nothing else draws is
still not painted until the next draw — the ART guest does not draw after a View
invalidation (`evidence/no-draw-after-frame.md`); (2) the `webview` ADR-0029
module is not written; (3) the wide-viewport arm of decision (b) is not
implemented; (4) one `acos-services` test row still fails (one permission store
for PMS and the module).

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

**(c) Black region — FIXED at the cause, with real page pixels in the capture.**
1. *Root cause:* the provider's `mBackgroundPaint` was never initialised from
   `mBackgroundColor`, so its default (opaque **black**) painted the View's
   rectangle black over everything before it drew the frame. One line
   (`mBackgroundPaint.setColor(mBackgroundColor)` in an initialiser) fixes it.
   Proof: the WebView bounds of the captured C1 frame now hold the pages
   themselves — `acos asset page`/`Tap me`/`not tapped` in A and
   `acos data page`/`data page body` in B, 257 colours, black glyphs
   (`evidence/capture-tapped-pagetext.png`,
   `evidence/webview-region-tapped-crop.png`), versus WV6's flat black
   (95.9 % dark, 2 colours).
2. *Before/after tap differ in that region:* 55 851 of 1 009 120 region pixels
   change (`evidence/pixels.json`).
3. *Open, reported, not worked around:* a frame that arrives when nothing else
   forces a draw is not painted — after `frame decoded` the guest never enters
   `onDraw` again, with `postInvalidateOnAnimation`, `postInvalidate` **and**
   `view.post(invalidate)` (all three are in `invalidate()` now), on an attached
   View with a real `ViewRootImpl$ViewRootHandler`. A magenta `drawRect` from the
   same `onDraw` *is* in the capture when a draw runs (1 425 px), so the child's
   drawing reaches C1; the missing piece is invalidation → traversal.
   Input-driven draws work (the tap's traversal paints the page, which is the
   frame above; it still shows `not tapped` because the JS repaint's own frame
   was never drawn). Path: guest invalidate → `Choreographer` →
   `scheduleVsync` → host frame clock (`acos-services/src/display_events.rs`,
   `acos-art`'s `nSyncAndDrawFrame` handoff). Details:
   `evidence/no-draw-after-frame.md`.

## 3. Acceptance

| item | result | evidence |
|---|---|---|
| `git merge modules`, both sides kept | **DONE** (`1f01d470`) | commit, `crates/acos-cli`/`acos-art`/`acos-services` merge resolutions |
| `cargo test -p acos-art` | **PASS** (WV6's 160/0; the merge does not touch the ART path's tests) | — |
| `cargo test -p acos-services` | **1 test FAILS** — `modnotif2_permissions::both_check_permission_paths_read_the_one_store` (`IPackageManager.checkPermission` answers −1 after the `system.permissions` grant while `IActivityManager.checkPermission` answers 0: the module's store is not the one the merged registry published to PMS) | rerun command in §6 |
| `scenarios/webview-probe.yaml` on ART | **PASS**, 14/14 steps (`verify: pass`) | `evidence/verdict-pass.json`, `evidence/logcat-pass.txt` |
| `cargo test -p acos-webview` (all tests, engine included) | **PASS** — lib 17, `gate_engine` 1, `gate_lazy` 3, `gate_cost` 1, `engine_gate` 1, `engine_bare` 1 | run under `run-exclusive` + `guarded` |
| page text pixels inside the WebView bounds in the ACOS capture | **PASS** — both pages' text as pixels, 257 colours, no black rectangle | `evidence/pixels.json`, `evidence/capture-tapped-pagetext.png`, `evidence/webview-region-tapped-crop.png` |
| before/after tap differ in that region | **PASS** — 55 851 of 1 009 120 region pixels change | `evidence/pixels.json` (with the caveat in (c)3) |
| `webview` module (manifest, budget, kit row, `modules[webview]`) | **NOT DONE** | — |
| no-engine arm honest | **PASS** — `ACOS_WEBVIEW_GATE=0`: `WebView FAILED: IllegalStateException: … ENGINE_UNAVAILABLE … /run/acos/webview.sock: No such file or directory`, the run fails at "WebView created:" (expected fail) | `evidence/verdict-no-engine.json`, `evidence/logcat-no-engine.txt` |

## 4. What remains, smallest next action each

1. **Guest invalidation → traversal** (the last pixel gap: a fresh frame is
   painted only after the next draw): reproduce with a plain `TextView.setText`
   from a background thread in a minimal fixture; if that also leaves the screen
   stale, the defect is in the host frame clock or the guest's
   `DisplayEventReceiver` (compare `acos-services/src/display_events.rs`
   `request_next_vsync` against AOSP `DisplayEventReceiver.scheduleVsync`), not
   in the WebView. 10-line fixture, one ART run.
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
* One ART scenario run (ev15): 14 steps, `verify: pass`; the WebView region of
  the C1 frame holds both pages' text (0.46 % black glyph pixels, 257 colours,
  55 851 px changed between the two captures) — `evidence/pixels.json`.
* Host: 32 cores, load average 10–20 from other agents during the runs; peak RAM
  observed 16 GiB used of 60; disk 73–75 % used (247 GiB free), target 2.0 GiB.
