# SPIKE-WV — ADR-0031 gate report (host Chromium as the WebView engine)

Status: **NO-GO for the full gate** (session 2, 2026-11-16). The engine half is
GO (host, measured in session 1). The in-session half moved three walls and is
**blocked on one measured defect**: the provider load path now reaches
`PackageManager.getPackageInfo(com.android.webview, …)`, and the reply for that
call desynchronises the framework's `PackageInfo` reader, so no provider is ever
loaded and **no gate item (a–e) has been re-measured inside a session yet**.

Branch `spike-wv`. Spike code: `research/spike-wv/` + three hooks in
`crates/acos-services/` marked `SPIKE-WV`, all behind `ACOS_SPIKE_WV=1`
(§3). Attempt log with every pass and failure: `research/spike-wv/LOG.md`.
Raw evidence: `research/spike-wv/evidence/`.

## 1. Gate verdicts

| gate | item | host engine (session 1) | in a real ACOS session (session 2) |
|---|---|---|---|
| a | real HTTPS page loads | **GO** — `example.com` 335 ms, `iana.org` 515 ms (`evidence/gate-a.json`) | **NOT MEASURED** — provider never loads (§2), so there is no page in a View |
| b | JS bridge both ways | **GO** — host→page, page→host binding, async result (`evidence/gate-b.json`) | **NOT MEASURED** — same block |
| c | tap web text by AX bounds | **GO** — AX `button "Tap me"` → touch → state change; real `wikipedia.org` link tap (`evidence/gate-c.json`) | **NOT MEASURED** — same block |
| d | frame < 50 ms end-to-end | **GO (engine only)** — 2.84 / 8.00 ms median at 1080×800 / 1080×2400@2.625 (`evidence/gate-d.json`) | **NOT MEASURED** — no frame ever crosses guest→View |
| e | ≤ 60 MiB per extra session | **GO on PSS** — +32.0 MiB PSS per extra session (1→3), +138 MiB RSS-sum for information, 202 MiB fixed idle cost (`evidence/gate-e.json`) | **NOT MEASURED** — needs (a) working first |

Per the orchestrator's decision, the memory line for ADR-0031 is **+32 MiB PSS
per extra session**, with the **~202 MiB PSS / 458 MiB RSS-sum fixed idle
Chromium tree paid once per host reported separately**, and RSS-sum marked
information-only.

**Verdict: NO-GO.** ADR-0031 asks for (a)–(e) *inside a real ACOS session*; the
session-side half is zero of five. The engine ceiling is proven, the integration
is not. Nothing in this session suggests the design cannot work — the blocker is
one reply encoding plus the provider package/classloader foundation — but the
gate is not passed and must not be reported as passed.

## 2. Wall map, walked by measurement

`WebViewFactory.getProvider()` was walked step by step in a real ART session
(`acos verify --engine art`, uid 10192). Each row is a run whose logcat line is
quoted; evidence dirs are under `research/spike-wv/evidence/`.

| # | wall | before | after session 2 |
|---|---|---|---|
| 1 | uid guard (0/1000/1001/1027/1002) | already satisfied | unchanged (`probe-run`) |
| 2 | `hasSystemFeature("android.software.webview")` | `false` → `UnsupportedOperationException: null` | **PASSED** — `true` → `AndroidRuntimeException: … waitForAndGetProvider() on a null object reference` (`probe-run-flag`) |
| 3 | `ServiceManager.getService("webviewupdate")` → `IWebViewUpdateService.waitForAndGetProvider()` | owner `null` → NPE → `MissingWebViewPackageException` | **PASSED** — `WebView.getCurrentWebViewPackage()=PackageInfo{… com.android.webview}` (`probe-run-webviewupdate`) |
| 4 | `PackageManager.getPackageInfo(pkg, 0x100024c0)` → `verifyPackageInfo` → `createApplicationContext` | — | **BLOCKED** — reply desyncs the framework's `PackageInfo` reader: `OutOfMemoryError: Failed to allocate a 7676737936 byte allocation` at `Parcel.createString8Array ← PackageInfo.<init> ← PackageInfo$1.createFromParcel` (`probe-run-webviewupdate/logs/logcat.txt` 81–87) |
| 5 | `Class.forName("com.android.webview.chromium.WebViewChromiumFactoryProviderForT", …)` + static `create(WebViewDelegate)` | — | **NOT REACHED** — no provider package is installed in the session image, no dex mount, no provider Java written |

### 2.1 The defect at wall 4, and the pre-existing bug it exposed

Two facts, both measured:

1. **The `Signature[]`/`String[]` encoding in the PMS `PackageInfo` reply is
   wrong (pre-existing, not introduced here).** The probe asks for *its own*
   package with the flags `WebViewFactory` uses (`0x100024c0`):
   `own getPackageInfo(0x100024c0) -> dev.acos.fixtures.webview sigs=7209071`
   (`probe-run-flags-isolation/logs/logcat.txt` line 82). A signature array
   length of 7 209 071 is a desynchronised parcel: `PackageInfo.writeToParcel`
   writes `dest.writeString8Array(splitNames)` and
   `dest.writeString8Array(requestedPermissions)` (AOSP
   `base/core/java/android/content/pm/PackageInfo.java:585,608`), i.e. **String8**
   arrays, while `crates/acos-services/src/parcelable.rs` encodes
   `FieldOp::StringArray` with `Parcel::write_string16_array` (String16). For a
   **non-empty** array the two encodings differ by the per-element length
   representation, so every field after `requestedPermissions` is misread — the
   framework then happily builds a `Signature[]` of 7 209 071 entries. The
   fixture app's own record hides this today only because its launch-time query
   uses flags without `GET_META_DATA`/`GET_SIGNATURES` and the array it *does*
   have is short enough to land in allocatable memory.
2. **The provider record's reply desyncs at `splitNames` (the second field).**
   The same query for `com.android.webview` fails 4 bytes earlier, which points
   at the reply *head* rather than at a late array. That is the next thing to
   isolate; the two candidate causes are the `StringArray`/`String8Array`
   mismatch above (if `null`/empty is not encoded as the framework's `-1`) and a
   presence/`allowSquashing` marker difference in the nested
   `ApplicationInfo`. It needs a byte-level diff of one reply, not another guess.

The orchestrator should treat (1) as a **release defect** independent of
WebView: any app that calls `getPackageInfo(pkg, GET_SIGNATURES|GET_META_DATA)`
gets a corrupted `PackageInfo` (or an OOM) today. It is a one-line spec fix plus
a regression test (see §7).

## 3. What was built, and what the real foundation must provide

Spike code, gated on `ACOS_SPIKE_WV=1` (env). Nothing here is the v0.4.0 design.

| file | change | why |
|---|---|---|
| `crates/acos-services/src/pms.rs` | `FeatureTable::default` answers `android.software.webview = true` under the flag; `PmsState.spike_webview` (one extra `PackageInfo`), `lookup` consults it, `spike_webview_provider_package()`; `getPackageInfo` gained the `ACOS_PMS_TRACE` hook the `getApplicationInfo` path already had | wall 2, wall 4's package record, and observability |
| `crates/acos-services/src/spike_webview.rs` (new) | `IWebViewUpdateService` as a dynamic raw node: `1 notifyRelroCreationCompleted`, `2 waitForAndGetProvider`, `6 getCurrentWebViewPackageName`, `7 getCurrentWebViewPackage`, `8 isMultiProcessEnabled`. Codes taken from the image's own `IWebViewUpdateService$Stub$Proxy`; every other method returns `Ok(false)` so the dispatcher raises the structured `UNSUPPORTED_ANDROID_API` (never a silent no-op) | wall 3 |
| `crates/acos-services/src/{lib,registry}.rs` | module + `host.register_dynamic_named("webviewupdate", …)` under the flag | wall 3 |
| `fixtures/apps/webview-probe/…/MainActivity.java` | one diagnostic query (own package, `0x100024c0`) | isolates §2.1(1) |
| `docs/AOSP_REFERENCE.md`, `tools/fetch-aosp-reference.sh` | `core/java/android/webkit` added to the `base` sparse set (and fetched into the shared cache) | the provider load path no longer has to be read out of `framework.dex` |

**What the real foundation must provide (decision ii — the provider package is a
system package in the image, like `com.android.webview` on a phone):**

1. **A real multi-package PMS.** `PmsState` holds exactly one package
   (`app` + `package`, built from the APK being launched); `lookup` is a name
   comparison. The provider needs a real package table with per-package
   `ApplicationInfo`, uid allocation and `packages_for_uid` — this is the same
   work v0.4.0's second-app support needs, which is why the Android-System-
   WebView-APK fallback was rejected.
2. **A provider APK in the image and a mount for it.** The provider's
   `sourceDir` must be a guest path (`/data/app/com.android.webview.apk`) that
   the session's sandbox actually binds, exactly like the launched app's APK
   (`crates/acos-art/src/launch.rs` `sb.ro_bind(&apk, "/data/app/<name>")`).
   Nothing mounts a second APK today.
3. **`Application.createApplicationContext(ai, 0)` must work for that
   package** — it needs `ActivityThread.getPackageInfoNoCheck` to accept a
   package the process did not launch, i.e. a `LoadedApk` + `PathClassLoader`
   over the provider's `sourceDir`. Untested; wall 5 is where it would fail
   first.
4. **The hard-coded provider class name** in that classloader:
   `com.android.webview.chromium.WebViewChromiumFactoryProviderForT`, static
   `create(WebViewDelegate)`, plus the hidden-API stubs to compile it (the SDK
   `android.jar` has none of `WebViewFactoryProvider`, `WebViewProvider`,
   `ViewDelegate`, `WebViewDelegate`; 105 methods, `provider/METHODS.md`).
5. **The guest↔broker transport** (Unix socket created by ACOS and bind-mounted
   into the session). The broker's `--serve` mode exists in
   `research/spike-wv/broker/wv_broker.py` but was **not** exercised from inside
   a session in this spike: there is no provider to connect to it yet.

## 4. Risks (updated)

| risk | why | evidence |
|---|---|---|
| PMS reply encoding (§2.1) | blocks the provider load path *and* corrupts `getPackageInfo` for any app today | `probe-run-flags-isolation` logcat 82 |
| multi-package PMS + `createApplicationContext` | two of the five walls are foundation work the ADR assumed would exist; without them the provider cannot be loaded at all | §3 items 1–3 |
| hidden-API compile of the provider | 105 methods against classes the SDK `android.jar` does not have; stubs must come from the image | `provider/METHODS.md` |
| guest-side frame cost still unknown | the engine spends 8 ms per 1080×2400 frame; the PNG (72 KB) still has to cross the socket, be decoded and drawn on CPU Skia, and the frame pump must not stall the UI thread | `evidence/gate-d.json`, `LOG.md` 4.3 |
| one engine, all sessions | a Chromium crash loses every session's WebView; needs a supervisor + honest `WebViewRenderProcess` signalling | design |
| network semantics | the page fetch happens on the host, so the guest's `offline` profile, TLS pinning and traffic recording are bypassed until request interception lands | gate a; ADR-0031 item 1 |
| JS interface security | `addJavascriptInterface` must re-implement the annotated-method-only rule; getting it wrong is a sandbox escape into the app's process | — |
| accessibility merge | the full AX tree has 100+ nodes including `InlineTextBox` noise; needs role filtering and a CSS-px → guest-px transform | `evidence/gate-c.json` |
| disk | host `/` was at 87 % when the spike started; 83 % (155 G free) at the end of session 2 | `df -h /` |

## 5. Estimate for the full v0.4.0 implementation

| work | days |
|---|---|
| host broker production: session protocol, frame pump, input, request interception (`file:///android_asset`, `content://`), cookie/storage mapping, supervisor, installer + checksum + bwrap sandbox | 4–6 |
| Java provider: `WebViewProvider`, `ViewDelegate`, `ScrollDelegate`, settings, cookies, JS bridge + shim, history, lifecycle, structured stubs (105 methods), hidden-API stubs | 7–9 (+1 vs session 1: the hidden-API stub generation is now the only unknown on this line, and `core/java/android/webkit` is in the AOSP cache so it is no longer a reverse-engineering task) |
| framework hooks: real multi-package PMS, provider package in the image + mount, `webviewupdate` peer (the spike's version is ~90 lines and can be kept), `hasSystemFeature` follow-through, `createApplicationContext` for a non-launched package | 5–8 (+1: measured, not assumed — `createApplicationContext` for a package the process did not launch is now a named, unproven step) |
| fix `PackageInfo` parcel encoding (`String8Array` for `splitNames`/`requestedPermissions`) + regression test | 0.5 (independent of WebView; do it now) |
| accessibility merge into the ACOS UI tree | 2 |
| real-app proof (RN webview app, plain WebView, Gaten), in-session gate re-measurement, docs/verdict `fidelity` text | 3 |
| **total** | **21–28** (was 19–25) |

## 6. Commits

| sha | what |
|---|---|
| `2d2c6cfe` | session 1: wall map, host gate numbers, probe app |
| `1214c8f1` | session 1: exact PNG frame sizes in the risk table |
| `6601a1f6` | session 2: `ACOS_SPIKE_WV` flag + `webviewupdate` peer (walls 2–3 pass, wall 4 reached) |

## 7. Smallest next actions

1. **Fix the `StringArray` encoding** (0.5 day): `splitNames`, `requestedPermissions`
   and `ApplicationInfo.sharedLibraryFiles`/`splitSourceDirs` are String8 arrays
   in the image; add a `String8Array` `FieldOp`, use it for those fields, and add
   a test that decodes the reply with the framework's own reader semantics (the
   probe's `sigs=7209071` line is the regression assertion).
2. **Byte-diff one `getPackageInfo(com.android.webview, 0x100024c0)` reply**
   (2 hours) to close §2.1(2); then wall 4's remaining work is the provider
   package record (§3 items 1–2).
3. **Then** the provider Java + socket bridge + (a)–(e) in-session, which is the
   bulk of the estimate above.

Nothing in this session changes the ADR's *design*; it changes the *order*:
WebView cannot ship before the multi-package PMS and the `PackageInfo` encoding
fix.
