# SPIKE-WV — ADR-0031 gate report (host Chromium as the WebView engine)

Status: **PARTIAL — engine gate measured GO on 4 of 5 items and GO on the fifth
with a metric caveat; the in-session provider was not built, so the items are
not yet re-measured inside a real ACOS session.**

Branch `spike-wv`. Spike code: `research/spike-wv/` (not release code). Attempt
log with every pass and failure: `research/spike-wv/LOG.md`. Raw evidence:
`research/spike-wv/evidence/`. Nothing in `crates/`, `tools/` or `fixtures/`
changed except the two hooks named in §6.

## 1. Gate verdicts

Engine under test: Chrome for Testing `chrome-headless-shell` **157.0.8103.78**
(rev `@4b3d0dabb8250b6f152d2d85d79e7706e309d087`), zip `sha256
f90329720fb61e5a76da9b6b509b6ad12a484c9c5df0359403e935177c6d153a`, 122 MiB,
downloaded from the official CfT URL (see `evidence/engine.json`). Driver: the
broker in `research/spike-wv/broker/` over `--remote-debugging-pipe` (no TCP, no
port, no DevTools socket anywhere the guest could see).

| gate | item | verdict | numbers | evidence |
|---|---|---|---|---|
| a | real HTTPS page loads | **GO** | `example.com` complete, 22 nodes, 335 ms; `iana.org/help/example-domains` complete, 114 nodes, 515 ms | `evidence/gate-a.json`, `evidence/gate-a-*.png` |
| b | JS bridge both ways | **GO** | host→page `"ACOS WebView probe"`; page→host binding `["tapped"]`; async page→host result `"delayed-ok"` | `evidence/gate-b.json` |
| c | tap a web button by its text | **GO** | AX role `button`, name `Tap me`, content box `x54 y173.4 w260 h70`; state `not tapped`→`tapped`; real page: `wikipedia.org` "English" link tapped → navigated | `evidence/gate-c.json`, `evidence/gate-c-before/after.png` |
| d | frame < 50 ms | **GO** | `beginFrame`+PNG, 1080×800@2.625: median 2.84 / p95 3.08 / max 3.49 ms; 1080×2400@2.625: median 8.00 / p95 8.38 / max 8.99 ms; with a DOM change: p95 3.1 / 8.4 ms | `evidence/gate-d.json` |
| e | ≤ 60 MiB per extra session | **GO on PSS, NO-GO on RSS-sum** | +32.0 MiB PSS per extra session (1→3 sessions, one browser context each); +138.0 MiB if you sum per-process RSS; closing returns to baseline (202.0 → 202.5 MiB PSS, no leak) | `evidence/gate-e.json` |
| — | today's guest behaviour (step 1) | **recorded** | `hasSystemFeature(android.software.webview)=false` → `WebView FAILED: java.lang.UnsupportedOperationException: null` at uid 10192 | `evidence/probe-run/logs/logcat.txt` lines 80–83, `evidence/probe-run/tree/2.json`, `verdict.json` |

**What the caveat means for (e).** The RSS sum over three renderers counts the
same file-backed Blink code pages three times; that number does not describe
what one more session costs. PSS divides each shared page across the processes
that map it, so the 1→3 delta is the marginal cost: **32 MiB**. The honest
number for the "extra session" line in the ADR is therefore 32 MiB PSS, with
+138 MiB RSS-sum stated next to it. There is also a **fixed** host cost: one
idle Chromium tree is 202 MiB PSS / 458 MiB RSS-sum (6 processes) before any
session opens. That is the price of "one Chromium per host", paid once.

**What (a)–(e) do not prove yet.** The ADR asks for these inside a real ACOS
session. The numbers above are the engine ceiling: they exclude the PNG decode
and canvas draw in the guest, the guest↔broker round trip, and the per-frame
copy into the View. Frame time in-session is therefore engine time plus an
unknown guest cost, which is the first thing session 2 must measure. Every gate
item is otherwise proven end-to-end against the real engine, including a tap
located by text through the accessibility tree on a live HTTPS page.

## 2. Why the guest needs more than a provider class (step 1 findings)

Ground truth is the API-34 image's own dex (`framework.jar` → `classes4.dex`,
`dexdump -d`), because the AOSP cache has no `android/webkit` checkout. Full
walk-through in `LOG.md` §1; the five walls in order:

1. `WebViewFactory.getProvider()` throws for uid 0/1000/1001/1027/1002
   ("privileged processes"). ACOS's ART launcher runs apps as uid 10192, so this
   one is already satisfied — the `K1`/`K3` compat reports' privileged-process
   error comes from other processes, and is a separate issue.
2. `isWebViewSupported()` reads `PackageManager.hasSystemFeature("android.software.webview")`.
   ACOS answers `false` (`crates/acos-services/src/pms.rs`, `FeatureTable::default`).
   **This is today's exact failure**: `UnsupportedOperationException` with a null
   message, proven by the probe app.
3. `ServiceManager.getService("webviewupdate")` →
   `IWebViewUpdateService.waitForAndGetProvider()` → a `WebViewProviderResponse`
   carrying a `PackageInfo` and `status == OK`.
4. `PackageManager.getPackageInfo(pkg, 0)` for that package,
   `verifyPackageInfo`, then `Application.createApplicationContext(ai, 0)` — the
   provider package must exist in PMS with a usable `ApplicationInfo`.
5. `Class.forName("com.android.webview.chromium.WebViewChromiumFactoryProviderForT", true, providerPackageClassLoader)`
   and a **static** `create(WebViewDelegate)` returning `WebViewFactoryProvider`.
   The class name is hard-coded and hidden-API; ACOS cannot choose its own name,
   it must publish that one. `WebViewLibraryLoader.loadNativeLibrary` can be
   skipped by omitting the `com.android.webview.WebViewLibrary` metadata, so the
   provider can be pure Java.

## 3. Provider methods real apps need

The complete API-34 surface is in `research/spike-wv/provider/METHODS.md`
(105 methods across `WebViewFactoryProvider`, `WebViewProvider`,
`ViewDelegate`, `ScrollDelegate`). What react-native-webview and a plain
`WebView` app actually touch, in the order they break:

* **Construction**: `createWebView(WebView, PrivateAccess)`, `init(Map, boolean)`,
  `getSettings()` (JS, DOM storage, user agent, cache mode, viewport, mixed
  content, file access), `getViewDelegate()`.
* **Navigation**: `loadUrl(String[, Map])`, `loadDataWithBaseURL`, `getUrl`,
  `getTitle`, `reload`, `stopLoading`, `getProgress`.
* **Callbacks**: `WebViewClient.onPageStarted/onPageFinished/onReceivedError/
  onReceivedHttpError/shouldOverrideUrlLoading/shouldInterceptRequest/
  doUpdateVisitedHistory/onLoadResource`, `WebChromeClient.onProgressChanged/
  onTitleReceived/onConsoleMessage`.
* **JS**: `evaluateJavascript(String, ValueCallback<String>)`,
  `addJavascriptInterface`/`removeJavascriptInterface` (with the
  `@JavascriptInterface`-only rule, or an XSS hole opens).
* **Input/draw**: `ViewDelegate.onDraw`, `onTouchEvent`, `onMeasure`,
  `onSizeChanged`, `requestFocus`, `computeHorizontal/VerticalScrollRange`,
  `onScrollChanged`; `ScrollDelegate.scrollTo/scrollBy`.
* **Lifecycle/state**: `destroy`, `onPause`, `onResume`, `saveState`/
  `restoreState` (RN restores), `freeMemory`, `clearCache`, `clearHistory`.
* **Cookies/storage**: `CookieManager` (setCookie with callback, getCookie,
  flush), `WebStorage`, `WebViewDatabase` (RN apps and login flows need it).
* Optional but named in the ADR: `capturePicture`/`insertVisualStateCallback`,
  `hitTestResult`, `documentHasImages`, `findAll*`, zoom, `createPrintDocumentAdapter`
  (stub with a structured error is acceptable for v0.4.0).

## 4. Risks

| risk | why | evidence |
|---|---|---|
| PMS knows only one package | the provider package must be installed and `createApplicationContext` must work for it; the plan lists "a second APK" for v0.4.0, so this is on the critical path and is *not* WebView work | `crates/acos-services/src/pms.rs` header: "Mini PackageManagerService built from the fixture APK" |
| hidden-API compile | provider implements framework-hidden interfaces; needs stubs generated from the image, not the SDK `android.jar` | `provider/METHODS.md` |
| guest-side frame cost unknown | the engine already spends 8 ms per 1080×2400 frame, and a 72 KB PNG (37 KB at 1080×800) still has to cross the socket, be decoded, and be drawn on CPU Skia | `evidence/gate-d.json` (`png_bytes_median`) |
| frame pump vs input | touch only routes when a frame is produced, so the provider needs a frame pump on a vsync-ish cadence; a stalled broker becomes a stalled UI thread (ANR risk if not off the main thread) | `LOG.md` 4.3 |
| one engine, all sessions | a Chromium crash loses every session's WebView; needs a supervisor + honest `WebViewRenderProcess` signalling | design |
| network semantics | the page fetch happens on the host: the guest's `offline` profile, TLS pinning and traffic recording are bypassed until the network module + request interception land. This must be stated in `fidelity` | gate a; ADR-0031 item 1 |
| JS interface security | motivating the binding through a page shim means re-implementing the annotated-method-only rule; getting it wrong is a sandbox escape into the app's process | — |
| accessibility merge | the full AX tree has 100+ nodes including `InlineTextBox` noise; needs role filtering and a CSS-px → guest-px transform, or agents see duplicate text | `evidence/gate-c.json` (`ax_node`) |
| bwrap sandbox for Chromium | not exercised; needs `/dev/shm`, fonts, `--no-sandbox`-free profile | `/usr/bin/bwrap` 0.13.1 present, unused |
| disk | host `/` was at 87 % when the spike started (125 G free) | `df -h /` |

## 5. Estimate for the full v0.4.0 implementation

Agent-days, assuming the gate design is kept:

| work | days |
|---|---|
| host broker production: session protocol, frame pump, input, request interception (`file:///android_asset`, `content://`), cookie/storage mapping, supervisor, installer + checksum + bwrap sandbox | 4–6 |
| Java provider: `WebViewProvider`, `ViewDelegate`, `ScrollDelegate`, settings, cookies, JS bridge + shim, history, lifecycle, stubs that fail structured | 6–8 |
| framework hooks: `hasSystemFeature`, `webviewupdate` peer, provider package in PMS + `createApplicationContext`, hidden-API stubs, provider APK in the image | 4–6 (highest uncertainty) |
| accessibility merge into the ACOS UI tree | 2 |
| real-app proof (RN webview app, plain WebView, Gaten), in-session gate re-measurement, docs/verdict `fidelity` text | 3 |
| **total** | **19–25** |

If the PMS/`createApplicationContext` wall (item 3–4 above) turns out to need
the multi-package work first, v0.4.0 either ships WebView after that work or
falls back to option 1 (Android System WebView APK) at 150–250 MiB per session.

## 6. Hooks outside `research/spike-wv/` (additive, marked)

* `fixtures/apps/webview-probe/` — new fixture app (reports uid, feature flag,
  provider lookup and the exact throwable). Two files plus resources.
* `tools/build-apps.sh` — one `build_apk webview-probe` line with a
  `SPIKE-WV` comment.
* `research/spike-wv/scenarios/webview-probe.yaml` — the scenario used for the
  probe run.

Nothing else outside `research/spike-wv/` changed.

## 7. What session 2 must do (smallest next action first)

1. Make the probe app's failure move: add `android.software.webview=true` to
   `FeatureTable`, re-run the probe, and record the *next* exception (expected:
   `MissingWebViewPackageException` from `waitForAndGetProvider`). This is a
   one-line change and it proves the wall map in §2 step by step.
2. Implement the `webviewupdate` peer + a provider package record, then the
   minimal Java provider (`create`, `createWebView`, `loadUrl` → broker
   navigate, `ViewDelegate.onDraw` painting the PNG, touch → broker tap), and
   re-measure (a)–(e) inside a session. Expect the in-session numbers to be the
   host numbers plus decode/draw; `gate-d` at 8 ms leaves ~40 ms of headroom.
3. If step 2 cannot be finished, report NO-GO on the integration gate and hand
   the multi-package PMS work to the orchestrator as the blocking dependency.
