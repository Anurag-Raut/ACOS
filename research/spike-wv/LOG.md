# SPIKE-WV attempt log (ADR-0031 gate, branch `spike-wv`)

Every attempt, pass or fail, in order. Facts only; numbers come from the files
named here. Session 1 = 2026-11-16, worktree
`/home/Qui/.t3/worktrees/acos/t3-72bee58b-be51fd2693b3e40f`.

## 0. Environment

| # | attempt | result |
|---|---|---|
| 0.1 | `df -h /` before starting | **87 % used, 125 G free.** Above the 85 % "stop and report" line in COMMON_RULES. Proceeded because the spike footprint is ~1.2 G (128 MiB engine + 1.6 G build dir) and 125 G is free; flagged for the orchestrator instead of blocking. Peak observed: see FINAL REPORT. |
| 0.2 | worktree test env from BRIEF_COMMON_0316 (boot/art/gaten symlinks, `cp -al fixtures/build`) | ok |
| 0.3 | `ACOS_FONT_DIR` exported for every ART run | ok |

## 1. What API-34 `WebViewFactory` actually does

The AOSP source cache (`/home/Qui/Projects/acos/.cache/aosp`) has **no**
`frameworks/base/core/java/android/webkit` checkout, so the ground truth was
taken from the API-34 image itself: `framework.jar` → `classes4.dex` →
`dexdump -d`, class section extracted with
`research/spike-wv/dexsec.py`. Dumps and the extractor live in
`.cache/spike-wv/` (not committed, gitignored).

| # | finding | exact evidence |
|---|---|---|
| 1.1 | `WebViewFactory.getProvider()` refuses five uids: 0, 1000, 1001, 1027, 1002, with `UnsupportedOperationException("For security reasons, WebView is not allowed in privileged processes")` | bytecode `WebViewFactory.getProvider` insns `0x0015`–`0x0025` (const/16 1000/1001/1027/1002 + `if-eq`), message at `+0x94` |
| 1.2 | then `isWebViewSupported()` → `PackageManager.hasSystemFeature("android.software.webview")` | `isWebViewSupported` insns `0x000c`–`0x0011` |
| 1.3 | if unsupported → `new UnsupportedOperationException()` **with no message** | `getProvider` `+0x8c`–`+0x91` |
| 1.4 | provider discovery: `getUpdateService()` → `ServiceManager.getService("webviewupdate")` → `IWebViewUpdateService.waitForAndGetProvider()` → `WebViewProviderResponse{packageInfo, status}`; status != OK → `MissingWebViewPackageException("Failed to load WebView provider: <reason>")` | `getWebViewContextAndSetProvider` `+0x0e`–`+0x3e` |
| 1.5 | then `IActivityManager.addPackageDependency(pkg)`, `PackageManager.getPackageInfo(pkg, 0)`, `verifyPackageInfo(provided, resolved)`, `Application.createApplicationContext(ai, 0)` | same method `+0x42`–`+0x91` |
| 1.6 | native library: `WebViewLibraryLoader.loadNativeLibrary(cl, libName)` where `libName` = `ApplicationInfo.metaData["com.android.webview.WebViewLibrary"]` (absent → no `.so` needed) | `getWebViewLibrary` `+0x00`–`+0x0e`, `getProviderClass` `+0x9b` |
| 1.7 | **provider class name is hard-coded**: `Class.forName("com.android.webview.chromium.WebViewChromiumFactoryProviderForT", true, providerPackageClassLoader)` | `getWebViewProviderClass` insns `0x0000`–`0x0007`; constant is `MAX-TARGET-O` hidden API |
| 1.8 | the factory is a **static** method `create(WebViewDelegate)` looked up reflectively, result cast to `WebViewFactoryProvider`, cached in `sProviderInstance` | `getProvider` `+0x3c`–`+0x5f` |
| 1.9 | provider surface size: `WebViewFactoryProvider` 14 methods, `WebViewProvider` 62, `WebViewProvider$ViewDelegate` ~50, `WebViewProvider$ScrollDelegate` 3, `WebViewDelegate` 15 | `research/spike-wv/broker/sigs.py` over the dex dump (list in `provider/METHODS.md`) |

Consequence for ACOS (not a design change, just the wall map): a provider can be
pure Java (no `.so`) by omitting `com.android.webview.WebViewLibrary`, but it
must (a) report `android.software.webview=true`, (b) answer the
`webviewupdate` binder service with a `PackageInfo` for a package PMS also
knows, (c) get a working `createApplicationContext` for that package's
`ApplicationInfo`, and (d) publish the hard-coded class name in that package's
classloader.

## 2. What the guest does today (probe app, real ART session)

New fixture `fixtures/apps/webview-probe/` (build: `ACOS_APP_ONLY=webview-probe
tools/build-apps.sh`, 12 691 bytes), scenario
`research/spike-wv/scenarios/webview-probe.yaml`.

| # | attempt | result |
|---|---|---|
| 2.1 | `acos verify --engine art` without `ACOS_ART_BIN` | **FAIL `UNSUPPORTED_BACKEND`**: `cannot spawn acos-art … (os error 2)`. Fixed by building `acos-art` into the same target dir. |
| 2.2 | same run, artifacts dir reused | **FAIL `INVALID_ARGUMENT`**: `artifacts directory … already holds run req_…`. Fixed by `rm -rf` of the run dir. |
| 2.3 | `run-exclusive.sh --task SPIKE-WV -- guarded.sh --mem 8G --time 420 -- env ACOS_ART_BIN=…/acos-art …/acos verify --engine art …` | **PASS (harness)**, and it captured the failure: `evidence/probe-run/` |

Exact behaviour today (`research/spike-wv/evidence/probe-run/logs/logcat.txt`
lines 80–83, tag `WVProbe`):

```
hasSystemFeature(android.software.webview)=false
WebView.getCurrentWebViewPackage()=null
myUid=10192
WebView FAILED: java.lang.UnsupportedOperationException: null
```

So the failure is **not** the uid check (the ART launcher runs the app as uid
10192): it is finding 1.3 — `WebViewFactory.isWebViewSupported()` is false
because ACOS's `FeatureTable` answers `android.software.webview = false`
(`crates/acos-services/src/pms.rs`, `FeatureTable::default`). The
`K1`/`K3` compat reports' "not allowed in privileged processes" came from other
processes/uid paths and is a *second*, separate wall for those apps.

## 3. Pinned engine

| # | attempt | result |
|---|---|---|
| 3.1 | `LATEST_STABLE` from the official CfT feed (`googlechromelabs.github.io/chrome-for-testing/last-known-good-versions.json`) | `157.0.8103.78`, revision `1717349` |
| 3.2 | `HEAD` on `chrome-for-testing-public/157.0.8103.78/linux64/chrome-headless-shell-linux64.zip` | HTTP 200, `content-length: 127965683` (122 MiB, under the 300 MiB cap) |
| 3.3 | download + `sha256sum` | see `evidence/engine.json`: `sha256 = f90329720fb61e5a76da9b6b509b6ad12a484c9c5df0359403e935177c6d153a` (zip), engine self-reports `HeadlessChrome/157.0.8103.78` rev `@4b3d0dabb8250b6f152d2d85d79e7706e309d087` |
| 3.4 | `--user-data-dir` | throwaway dir under `.cache/spike-wv/`; the owner's Chrome profile is never referenced. Host `/usr/bin/google-chrome-stable` (156.0.8078.47) used for nothing. |
| 3.5 | `bwrap` available for the provider's own sandbox | `/usr/bin/bwrap` 0.13.1 present |

## 4. Broker and the gate numbers

`research/spike-wv/broker/`: `cdp.py` (DevTools over `--remote-debugging-pipe`,
fds 3/4, stdlib only), `wv_broker.py` (one Chromium, one browser context per
session, own framed JSON protocol for the guest), `measure.py` (gate harness).
Full run: `python3 wv_broker.py --selfcheck --out ../evidence` →
`evidence/gates.json`, `evidence/SUMMARY.md`, per-gate JSON + PNGs.

| # | attempt | result |
|---|---|---|
| 4.1 | `subprocess` with `preexec_fn` dup2 to fd 3/4 | **FAIL** `ERROR … Remote debugging pipe file descriptors are not open.` Cause: CPython closes inherited fds *after* `preexec_fn`. Fixed: dup2 onto 3/4 in the parent, `pass_fds=(3,4)`, restore afterwards. |
| 4.2 | `HeadlessExperimental.beginFrame` | **FAIL** `Command is only supported with --run-all-compositor-stages-before-draw`. Fixed by adding `--run-all-compositor-stages-before-draw` and `--disable-new-content-rendering-timeout`. |
| 4.3 | `Input.dispatchTouchEvent` awaiting the reply | **FAIL** (reply never arrives while the frame pipeline is paused; and `send_raw` replies were being dropped by a pending-map bug). Fixed: `--touch-events=enabled` + `Emulation.setTouchEmulationEnabled` + send → `beginFrame` → send → `beginFrame`, and keep the pending entry until `result()` reads it. |
| 4.4 | gate a: `https://example.com/`, `https://www.iana.org/help/example-domains` | **PASS**: `readyState=complete`, 22 / 114 DOM nodes, load 335 / 515 ms, PNG frames `evidence/gate-a-*.png` |
| 4.5 | gate b: host→page `Runtime.evaluate` (`ACOS WebView probe`), page→host `Runtime.addBinding("acosBridge")` on click (`["tapped"]`), async return (`delayed-ok`) | **PASS** `evidence/gate-b.json` |
| 4.6 | gate c: `Accessibility.getFullAXTree` → button named `Tap me` → `DOM.getBoxModel` → touch at centre → `#out == "tapped"`; then the same on the real `https://www.wikipedia.org/` ("English" link → navigation) | **PASS** `evidence/gate-c.json`, `gate-c-before/after.png` |
| 4.7 | gate d: 30 `beginFrame`s at 1080×800@2.625 and 1080×2400@2.625, plus 10 with a DOM change | **PASS**: median 2.84 / 8.00 ms, p95 3.08 / 8.38 ms, max 3.49 / 8.99 ms `evidence/gate-d.json` |
| 4.8 | gate e: total RSS/PSS of the Chromium tree at 0 / 1 / 3 sessions, then closing back | **PSS 32.0 MiB per extra session (GO), RSS-sum 138.0 MiB (NO-GO)**; closing returns exactly to baseline (PSS 202.0 → 202.5) `evidence/gate-e.json`. RSS-sum triple-counts the shared Blink code pages; PSS is the metric that answers "what does one more session cost". |
| 4.9 | screencast fallback (`Page.startScreencast`) | passed in the smoke run earlier in the session (JPEG frame received); superseded by `beginFrame`, kept as fallback |

## 5. Still open after session 1

* The in-guest provider (`research/spike-wv/provider/`): class
  `com.android.webview.chromium.WebViewChromiumFactoryProviderForT`,
  `WebViewProvider`/`ViewDelegate`, frame painting, and the two host-side
  hooks the framework requires (`android.software.webview=true`, a
  `webviewupdate` peer). **Not started** — the session ended on the gate
  measurements and the probe.
* Gate items measured so far are engine-level (host broker), not inside a real
  ACOS session: the ADR wording (item 4 of the brief) is not satisfied yet for
  a/b/c/d/e. The numbers above are the upper bound the session-side result can
  reach (the session adds PNG decode + canvas draw + IPC).
* `docs/AOSP_REFERENCE.md` should gain the
  `frameworks/base/core/java/android/webkit` sparse path (it is missing today
  and forced the dex route).

## 6. Session 2 (2026-11-16, worktree `t3-d32c64fc-e7f05ac278bb63c2`)

Session 1's worktree held the `spike-wv` branch checkout, so this worktree took
it with `git checkout --ignore-other-worktrees spike-wv` (no other worktree was
modified). Session 1's `.cache/spike-wv{,-target}` were hardlink-copied
(`cp -al`) so the built binaries and the pinned engine were reused.

| # | attempt | result |
|---|---|---|
| 6.1 | `df -h /` at start | **84 %/85 % used** (141 G free). Session 1 measured the same disk as 87 %; the gate is 87 %, so the spike proceeded and was re-checked before every run. End of session: 83 % (155 G free). |
| 6.2 | worktree env (boot/art/gaten symlinks, `cp -al fixtures/build`), `ACOS_FONT_DIR` exported for every ART run | ok |
| 6.3 | `ACOS_APP_ONLY=webview-probe tools/build-apps.sh` | 12 691-byte probe APK |
| 6.4 | wall 2: `android.software.webview` → `true` under `ACOS_SPIKE_WV=1` | **PASS.** `probe-run-flag`: `hasSystemFeature(android.software.webview)=true`, then `WebView FAILED: android.util.AndroidRuntimeException: java.lang.NullPointerException: … IWebViewUpdateService.waitForAndGetProvider() … on a null object reference`. Exactly the predicted next wall (the image's `getWebViewContextAndSetProvider` has no null-service check). |
| 6.5 | `webviewupdate` peer as a `raw_transact` node; transaction codes read from the image's `IWebViewUpdateService$Stub$Proxy` (each proxy method's `transact` const): 1 notifyRelroCreationCompleted, 2 waitForAndGetProvider, 3 changeProviderAndSetting, 4 getValidWebViewPackages, 5 getAllWebViewPackages, 6 getCurrentWebViewPackageName, 7 getCurrentWebViewPackage, 8 isMultiProcessEnabled, 9 enableMultiProcess | **first attempt wrong**: I had assumed code 1 = getCurrentWebViewPackageName. The run crashed the guest (`GUEST_EXITED` before the first frame) on `UNSUPPORTED_ANDROID_API: android.webkit.IWebViewUpdateService code 7` (`probe-run-webviewupdate` before the fix, `art-frames/unsupported.jsonl`). Root cause: the probe calls `WebView.getCurrentWebViewPackage()` **before** creating the WebView, and that is code 7. Fixed by extracting all nine codes from the dump instead of assuming. |
| 6.6 | `WebViewProviderResponse` reply shape | from `WebViewProviderResponse.writeToParcel` in the same dump: `writeTypedObject(packageInfo, flags)` then `writeInt(status)`; status 0 and 3 are accepted. Second attempt: **PASS** — `WebView.getCurrentWebViewPackage()=PackageInfo{… com.android.webview}` (`probe-run-webviewupdate/logs/logcat.txt` line 80). Wall 3 done. |
| 6.7 | wall 4: `PackageManager.getPackageInfo(com.android.webview, 0x100024c0)` → `verifyPackageInfo` → `createApplicationContext` | **BLOCKED, measured**: `java.lang.OutOfMemoryError: Failed to allocate a 7676737936 byte allocation`, stack `Parcel.createString8Array ← PackageInfo.<init> ← PackageInfo$1.createFromParcel` (`probe-run-webviewupdate` logcat 81–87). |
| 6.8 | `ACOS_PMS_TRACE` on the spike run | `getApplicationInfo` is never queried; one `getPackageInfo key=dev.acos.fixtures.webview flags=0x10000000`. The provider query is therefore `getPackageInfoAsUser` (same reply path, untraced until 6.9). |
| 6.9 | extended `ACOS_PMS_TRACE` to `getPackageInfo` (it only covered `getApplicationInfo`) | trace now shows the launch-time query; the provider query still does not appear, so it goes through `getPackageInfoAsUser` and the desync is in the section of the reply it returns. |
| 6.10 | isolation: probe asks for **its own** package with `WebViewFactory`'s flags `0x100024c0` | **PASS (finding).** `own getPackageInfo(0x100024c0) -> dev.acos.fixtures.webview sigs=7209071` (`probe-run-flags-isolation` logcat 82). A signature array length of 7 209 071 is a desynchronised parcel and it is a **pre-existing PMS defect**: AOSP `PackageInfo.writeToParcel` writes `writeString8Array` for `splitNames` (line 585) and `requestedPermissions` (line 608), while `crates/acos-services/src/parcelable.rs` encodes `FieldOp::StringArray` with `Parcel::write_string16_array`. Every field after a non-empty `requestedPermissions` is misread. |
| 6.11 | AOSP reference: `core/java/android/webkit` | fetched into the shared cache (`git -C .cache/aosp/base sparse-checkout add core/java/android/webkit`, 94 MB total) and **added to `tools/fetch-aosp-reference.sh`** + `docs/AOSP_REFERENCE.md`, so the provider load path is readable instead of disassembled. This confirms every session-1 dex finding (`WebViewFactory` provider load path) and gives the exact `PackageInfo` write/read order used in 6.10. |
| 6.12 | gates a–e in a session | **NOT RUN.** No provider loads, so no page, no bridge, no tap, no frame and no per-session memory figure exists in-session. The broker's `--serve` Unix-socket mode exists but was not exercised from a session (nothing to connect to it). |

Still open after session 2 (in order): fix the `StringArray`/`String8Array`
encoding; byte-diff one `getPackageInfo(com.android.webview, 0x100024c0)` reply to
close the head desync; then the real multi-package PMS + provider mount +
`createApplicationContext`; then the provider Java (105 methods) and the socket;
then (a)–(e) in-session.
