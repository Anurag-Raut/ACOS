# WV6 — WebView end to end (real ART, real engine, real page)

Branch `wv6` (from `wv5b` @ `62595b6b`, merged `wv5a` @ `c08effef`).
Status: **DONE** — the scenario `scenarios/webview-probe.yaml` passes on real ART
with the pinned host Chromium, and the negative arm fails honestly with
`ENGINE_UNAVAILABLE`.

## 1. What this branch changes

### 1.1 WIRE v1.1 (both sides)

`results/handoff/WVB/WIRE.md` is now **v1.1**; `proto` is the number `1.1` on
the guest and broker. Added and implemented end to end:

| addition | guest | host |
|---|---|---|
| `view.setVisibility` | sent on a visibility change (initial `visible` needs no message) | `Page.setWebLifecycleState` active/frozen: the renderer stops while hidden |
| `view.settings` | `WebSettingsChromium` setters after create (`javascriptEnabled`, `cacheMode`, `loadsImagesAutomatically`, `blockNetworkImage`) | applies what the engine can honour, returns `applied` + `rejected`; a rejected key makes the setter throw (never a silent setting) |
| `view.scrollTo` + render `contentWidth/Height`, `scrollX/Y` | `compute*ScrollRange/Offset/Extent` answer from the engine after the first report; guest geometry only until then | engine metrics from `Page.getLayoutMetrics`; `view.scrollTo` moves the page |
| `view.focusChanged` (+ `ax` → `AccessibilityNodeProvider`) | page focus/editable report answers `onCheckIsTextEditor`; real `AccessibilityNodeProvider` virtual children; a simple `BaseInputConnection` commits through `input.text` | focus report from `document.activeElement`, sent after tap/text/setFocus/navigation |
| connection-level error (`id:"g?"`) | fails **all pending requests** with the frame's code, never a hang | gate attach failure keeps `g?`/`ENGINE_UNAVAILABLE` (WV5b convention) |

Not accepted (unchanged): per-view engine profile. `privateBrowsing=true` is
still a structured refusal.

### 1.2 Provider seam: `WebView.PrivateAccess`

The `View.mLayoutParams` reflection is gone. The factory passes the
`WebView.PrivateAccess` it is created with into the core (`mWebViewPrivate`), and
`setLayoutParams` uses `super_setLayoutParams` exactly as Chromium's
`WebViewChromium` does. The stub generator now declares the image's 24
`PrivateAccess` methods (extracted from the image's own dex), and
`onMeasure` uses `setMeasuredDimension`.

### 1.3 Real defects the end-to-end run exposed (each fixed at the cause)

1. **Host → guest requests were never dispatched.** `AcosWebViewTransport`
   delivered events and responses but dropped `{"id":"h…","op":…}` requests, so
   `req.intercept`, `nav.shouldOverride` and `js.call` never reached the provider.
   All three now run on the transport reader and answer as normal responses.
2. **One `WebView` core owned the whole session's events.** One transport per
   session, one core per View: the last core's handler received every event
   (page A's `onPageFinished` arrived on B). A session-level router dispatches
   by the wire's `view` id.
3. **`loadUrl` blocked the UI thread.** A navigation makes the broker ask
   `nav.shouldOverride`, whose answer needs the UI thread — a blocking
   `view.loadUrl` deadlocked against it (both timeouts expired, no frame). Loads
   are asynchronous like Android's.
4. **The page's CSS space was wrong.** WIRE geometry is device pixels;
   `Emulation.setDeviceMetricsOverride` takes CSS pixels. Passing device pixels
   through made a 900 px View lay out at 900 CSS px with Chromium's wide mobile
   viewport; a full-width heading's AX box was 2530 device px and a tap by its
   bounds missed. The broker now divides once and lays out at width/density.
5. **`render` was not serialised across views.** Two WebViews asking
   `HeadlessExperimental.beginFrame` at once crashed the pinned engine in the ART
   run ("devtools pipe closed"). Frame production is serialised per browser;
   transient `beginFrame` states are retried, never turned into a fabricated
   frame.
6. **A new document dropped `addJavascriptInterface`.** The shim lived in the
   previous document; the page's JS ran but `window.acosBridge.report(...)` was
   gone after `loadUrl`. Every registered interface is re-installed on
   `Page.loadEventFired`.
7. **A down/up pair could lose an invalidation.** An invalidation that arrived
   while `render` was in flight was cleared by that frame's completion; the
   second (up) invalidation is now replayed after the frame.
8. **The ACOS View walker could not see page nodes.** `LaunchMain` now merges a
   View's `AccessibilityNodeProvider` virtual descendants (standard
   `findAccessibilityNodeInfosByText("", HOST_VIEW_ID)`), which is the
   `ax` → provider path, with the engine's display-space bounds.

## 2. Acceptance evidence

| acceptance item | result | evidence |
|---|---|---|
| `cargo test -p acos-webview -p acos-art` green | **PASS** | `acos-art --lib`: 160 passed, 0 failed, 1 ignored; `acos-webview`: 17 lib + 3 `gate_lazy` + 1 `gate_cost` + 1 `gate_engine` + `engine_gate`/`engine_bare`, 0 failed (commands in §5) |
| scenario passes on ART with evidence | **PASS** — `verify: pass`, 14/14 steps | `evidence/pass/verdict.json`, `evidence/pass/steps/`, `evidence/pass/tree/`, `evidence/pass/logs/logcat.txt` |
| local page from APK assets through broker interception | **PASS** — `req.intercept asset test.html 400 bytes`, `A onPageFinished file:///android_asset/test.html` | `evidence/pass/logs/logcat.txt` |
| data: URL page | **PASS** — `B onPageFinished data:text/html;base64,…`, `A/B evaluateJavascript -> "acos … page"` | same |
| frame captured, page content visible | **PASS** — engine PNGs `engine-first-frame.png` (not tapped) and `engine-tapped-frame.png` (`tapped`), 900×560 each; guest `frame decoded` lines | `evidence/engine-first-frame.png`, `evidence/engine-tapped-frame.png`, `evidence/engine-probe.json` |
| page text in ACOS observe tree via ax → AccessibilityNodeProvider | **PASS** — `assert_text selector {text: "acos asset page"}` (bounds `[21,716,879,790]`) and `"acos data page"` | `evidence/pass/steps/06-assert_text.json`, `07-assert_text.json`, `evidence/pass/tree/2.json` |
| tap by that text works; page JS changes text; tree shows it | **PASS** — tap `"Tap me"`; tree then has `"tapped"`; `JS->Java: tapped` | `evidence/pass/steps/10-tap.json`, `11-assert_text.json`, `12-assert_text.json`, `evidence/pass/tree/4.json` |
| JS interface both ways | **PASS** — `addJavascriptInterface` + `js.call {method:"report", argsJson:["tapped"]}`; `evaluateJavascript -> "acos asset page"` | `evidence/pass/logs/logcat.txt`, `evidence/engine-probe.json` (`jsi_calls`, `js_out`) |
| negative arm honest | **PASS (expected fail)** — `WebView FAILED: IllegalStateException: … ENGINE_UNAVAILABLE … No such file or directory`; `ASSERTION_FAILED` at "WebView created:" | `evidence/no-engine/verdict.json`, `evidence/no-engine/logs/logcat.txt`, `results/handoff/WV6/webview-probe-no-engine.yaml` |
| WV4 scenario moved | **PASS** — `scenarios/webview-probe.yaml` (expectation pass); old WV4 YAMLs deleted | `scenarios/webview-probe.yaml` |

The host-side frame probe (`evidence/frame-probe.py`, run under the shared lock)
is the direct WIRE v1.1 check: it serves the same asset page over
`req.intercept`, captures both PNGs, verifies `"acos asset page"` → tap →
`"tapped"`, the `js.call`, and `evaluateJavascript`. Verdict: `pass`; full JSON in
`evidence/engine-probe.json`.

## 3. Metrics (WV6 measurements, engine up)

| metric | value | source |
|---|---|---|
| first frame, load → PNG (host probe, 900×560) | **11.2 ms** (`engineMs` 2.8) | `evidence/engine-probe.json` `cost.first_frame_ms` |
| tap round trip (input.tap + engine frames) | **42.6 ms** | same `cost.tap_ms` |
| per-session engine cost (broker + Chromium descendants) | **142.5 MiB PSS / 10 processes** | same `cost.pss_mib`, `pss_detail_kib` |
| guest session with two WebViews | **71 MiB** cgroup peak, wall **618 ms** | `evidence/pass/art-frames/session-exit.json` |
| session without WebView | **no added start time/memory**: WV5b measured 131 vs 131 ms wall and 218 542 vs 220 009 KiB PSS (gate on/off); this branch does not touch that path | `results/handoff/WV5b/REPORT.md` §3 |
| frame latency (20 frames @1080×2400) | p50 2.5 ms, p95 4.7 ms | `cargo test … engine_gate` output (WVB gate, rerun here) |

## 4. Limitations found (for the orchestrator)

* The ACOS **C1 screen capture** of the WebView region can be black while the
  engine frame is correct (the engine PNGs and the tree are right; the guest
  paints a 343×213 CSS-sized bitmap into a 900×560 View in the ART path). This is
  a guest-compositor/layer issue, not the wire: the host probe receives
  900×560 PNGs. It does not affect the acceptance assertions (tree, tap, JS,
  frame decode) but it is why the committed engine PNGs, not the C1 screenshot,
  are the "page content" evidence.
* `view.settings` implements the engine-backed subset; the rest is still a
  structured refusal. `textZoom` is rejected (no honest DevTools equivalent).
* IME: focus is reported and a basic `BaseInputConnection` commits text; full
  composing spans/selection are not modelled.
* Frame timing is serialised browser-wide; two views get frames one after the
  other (correctness before concurrency).

## 5. Commands

```sh
# host tests (acceptance; shared slots)
ACOS_VIEW_TEST_DIR=$PWD/.cache/lv \
CARGO_TARGET_DIR=$PWD/.cache/wv6-target \
run-exclusive.sh --task WV6-acc -- guarded.sh --mem 14G --time 1800 -- \
  cargo test --offline -p acos-webview -p acos-art

# the ART scenario (shared slot; private runtime dirs)
ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts \
ACOS_RUNTIME_DIR=$PWD/.cache/wv6-run ACOS_SESSION_SOCKET=$PWD/.cache/wv6-run/session.sock \
ACOS_VIEW_TEST_DIR=$PWD/.cache/lv ACOS_WEBVIEW_CACHE=/home/Qui/Projects/acos/.cache/webview \
ACOS_ART_BIN=$PWD/.cache/wv6-target/release/acos-art \
run-exclusive.sh --task WV6 -- guarded.sh --mem 10G --time 900 -- \
  ./.cache/wv6-target/release/acos verify scenarios/webview-probe.yaml --engine art \
  --artifacts .cache/wv6-run/ev

# negative control (gate off)
ACOS_WEBVIEW_GATE=0 … ./.cache/wv6-target/release/acos verify \
  results/handoff/WV6/webview-probe-no-engine.yaml --engine art --artifacts .cache/wv6-run/ev-neg

# host-side frame evidence
WT=$PWD run-exclusive.sh --task WV6-frame -- guarded.sh --mem 8G --time 600 -- \
  python3 results/handoff/WV6/evidence/frame-probe.py
```

Peak observed: host RAM 60 GiB with ~15 GiB used during builds (44 GiB
available); disk 75% used (232 GiB free), target 1.9 GiB. No ART processes,
brokers or scratch left behind; the target and `fixtures/build` copy were removed
after the final run.
