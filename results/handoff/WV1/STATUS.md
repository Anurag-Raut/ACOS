# WV1 — WebView engine inside real ACOS sessions (ADR-0031)

Status: **PARTIAL** — the provider package now exists, is built in-repo, is
installed into a real session as a package, and the framework walks to wall 4
(`getPackageInfo` of the provider record). One parcel defect on that reply still
blocks the provider class load (`WebViewChromiumFactoryProviderForT.create` is
**not** reached yet). Broker, provider methods, and gates (a)–(e) are not done.

Measured in a real ART session, `results/handoff/WV1/evidence-1/logcat.txt`
(tag `WVProbe`):

```
78  I WVProbe: hasSystemFeature(android.software.webview)=true
79  I WVProbe: WebView.getCurrentWebViewPackage()=PackageInfo{888761a com.android.webview}
80  I WVProbe: myUid=10192
81  I WVProbe: own getPackageInfo(0x100024c0) -> dev.acos.fixtures.webview sigs=1
82  W        : Throwing OutOfMemoryError "Failed to allocate a 7676737936 byte allocation…"
83  I WVProbe: WebView FAILED: java.lang.OutOfMemoryError: Failed to allocate a 7676737936 …
     at android.os.Parcel.createString8Array(Parcel.java:1792)
     at android.content.pm.PackageInfo.<init>(PackageInfo.java:570)
     at android.content.pm.PackageInfo$1.createFromParcel(PackageInfo.java:558)
```

## 1. Wall map (after WV1)

| # | wall | before WV1 | now |
|---|---|---|---|
| 1 | uid guard | satisfied | unchanged |
| 2 | `hasSystemFeature("android.software.webview")` | env flag (`ACOS_SPIKE_WV`) | **PASSED, and honest**: the answer is a package-table lookup (`PmsState::has_system_feature`), so the feature is true exactly when the provider package is installed (unit test `pms::tests::webview_feature_follows_the_provider_package`) |
| 3 | `webviewupdate` → `waitForAndGetProvider` | hand-written duplicate `PackageInfo` | **PASSED from the real record**: `WebViewUpdateService` reads the same `Arc<Mutex<PmsState>>` the `package` service mutates, so the peer's `PackageInfo` is the installed package's own record (signatures + `versionCode` cannot drift); no provider installed → null record + `STATUS_MISSING_ZYGOTE`, never a fabricated package |
| 4 | `PackageManager.getPackageInfo(provider, …)` → `verifyPackageInfo` → `createApplicationContext` | `PackageInfo` desync (String8 arrays) | **BLOCKED (narrowed)**: the *primary* record now decodes correctly (`sigs=1`, REL3P parcel fix), but the **provider** record still desyncs: the framework's `PackageInfo.<init>` reads a garbage count at `createString8Array()` for `splitNames`, i.e. a field **before** `splitNames` in the reply is mis-written (the nested `ApplicationInfo`/the fields ahead of it). Deterministic: the same 7 676 737 936-byte allocation every run. |
| 5 | `Class.forName("…WebViewChromiumFactoryProviderForT").create(WebViewDelegate)` | not reached, no provider APK | **not reached** — provider APK exists and is installed (see §2), the load path stops at wall 4 |

## 2. What this session built

1. `webviewupdate` peer on the real package table (`crates/acos-services/src/webview.rs`,
   replacing the spike's `spike_webview.rs`), registered unconditionally and
   answering from `PmsState`; the feature answer follows the package
   (`pms::PmsState::has_system_feature`), which removes the env-var flag from
   the design entirely.
2. `com.android.webview` provider package, built **in-repo**:
   `fixtures/apps/webview-provider/` + `tools/gen-webview-provider.py` +
   `build_provider_apk` in `tools/build-apps.sh` →
   `fixtures/build/apps/webview-provider.apk` (20 883 bytes, signed,
   zipaligned, dexed).
   The generator emits, from the API-34 image's own method list
   (`research/spike-wv/provider/METHODS.md`):
   - `hidden-stubs/`: compile-time-only `WebViewFactoryProvider`, `WebViewProvider`
     (+`ViewDelegate`/`ScrollDelegate`), `WebViewDelegate`, `WebViewFactory`,
     `WebView` (+`PrivateAccess`/`VisualStateCallback`/…), `PacProcessor`,
     `TokenBindingService` — never dexed, the image's boot classpath supplies
     the real classes at run time;
   - `src/`: `WebViewChromiumFactoryProviderForT` (the hand-looked-up entry
     point, static `create(WebViewDelegate)`), `WebViewChromiumFactoryProvider`,
     `WebViewChromium`, `WebViewChromiumViewDelegate`,
     `WebViewChromiumScrollDelegate` — **every** method of the four framework
     interfaces is implemented; the unimplemented ones throw
     `UnsupportedOperationException("ACOS WebView provider: unsupported method
     <Interface>.<method>(<dex args>)<ret>")`.
3. The session installs the provider with MPROC1's `extra_apks:` scenario key, so
   the provider is an ordinary installed package with its own uid — no special
   case in the runtime (`results/handoff/WV1/webview-probe-provider.yaml`).

## 3. What is not done (and the smallest next action for each)

| item | state | next action |
|---|---|---|
| Wall 4 parcel desync (the only blocker to `create()` running) | BLOCKED, narrowed — see §3.1 | Add `ACOS_PARCEL_DUMP=<path>` next to the existing `ACOS_PMS_TRACE` hook in `PmsService` so one `getPackageInfo(<provider>, 0)` reply is written to disk; byte-diff that reply against AOSP `PackageInfo.writeToParcel`/`ApplicationInfo.writeToParcel` (image line numbers in `docs/AOSP_REFERENCE.md`), fix the writer once for the whole class of replies, and add a regression test that decodes with the framework's reader order (the `sigs=` line and the OOM byte count are the assertions). |
| Provider methods (loadUrl/loadData, evaluateJavascript, JS interface both ways, WebViewClient/ChromeClient/settings/cookies, onDraw frames, touch/scroll/IME, history, `file:///android_asset`, `content://`) | 0 of the listed methods implemented; all throw structured unsupported | after wall 4/5: implement `WebViewChromium` + `WebViewChromiumFactoryProvider` per `research/spike-wv/provider/METHODS.md`, against the broker protocol; regenerate with `tools/gen-webview-provider.py` as methods become real |
| Broker production (Rust, host broker, one Chromium + one browser context per session, AX merge into the ACOS tree) | not started; the spike's Python broker (`research/spike-wv/broker/wv_broker.py`) is the reference implementation and was only ever driven on the host | port `cdp.py`/`wv_broker.py` to the host broker crate, keep the spike's frame/AX numbers as the acceptance targets |
| Gates (a)–(e) in a real session | 0 of 5 (no page can be shown until the provider loads) | after the provider loads: the gate scenario in `results/handoff/SPIKE-WV/REPORT.md` §1 |
| react-native-webview fixture | not run (needs the provider) | same |

### 3.1 The wall-4 desync, narrowed (measured facts + the one hypothesis left)

Facts from `evidence-1/logcat.txt`:

* `packageName` is the **second** field in `PackageInfo.writeToParcel`
  (`/home/Qui/Projects/acos/.cache/aosp/base/core/java/android/content/pm/PackageInfo.java:579-585`):
  `writeString8(packageName)` then **`writeString8Array(splitNames)`** then
  `versionCode`… So a garbage count at `createString8Array()` for `splitNames`
  means either `packageName` or the bytes before it are mis-written — the reply
  *head*, not a late array.
* The **same** `getPackageInfo(primary, 0x100024c0)` reply decodes correctly now
  (`sigs=1`), so the `String8Array` fix (REL3P) is in effect and the flags-0
  `PackageInfo` written by the `webviewupdate` peer (`WebView.getCurrentWebViewPackage()`,
  line 79) also decodes. Only the **extra package's** record desyncs — a session
  with two packages queried through `getPackageInfo` is new in this session.
* `PmsService` keeps exactly one `PmsDescription` slot
  (`crates/acos-services/src/pms.rs:54 static LAST_DESCRIPTION`, written by
  `cache()` at `pms.rs:1702` on every `getPackageInfo`/`getApplicationInfo`
  lookup) and the guest-side materialiser writes the reply from that cache. With
  two packages in one session the slot is package-agnostic.

Two more measured facts (second run, `ACOS_PMS_TRACE`):

* The trace shows **no** `getPackageInfo` for `com.android.webview` at all — only
  the primary package's (`getApplicationInfo`/`getPackageInfo`, flags
  `0x80`/`0x10000000`/`0x100024c0`). So the desynced reply is decoded **before**
  any provider `getPackageInfo` reaches PMS: it is the `webviewupdate`
  `waitForAndGetProvider` reply (`WebViewProviderResponse.writeToParcel`:
  presence int + `PackageInfo.writeToParcel` + `writeInt(status)`), which is the
  first `PackageInfo` this provider path decodes.
* In the image's reader order
  (`.cache/aosp/base/core/java/android/content/pm/PackageInfo.java:659-689`) the
  two `createString8Array` calls are `splitNames` (2nd field) and
  `requestedPermissions` (after the six typed arrays). A desync at the *second*
  field points at the reply head, a desync at `requestedPermissions` at the
  typed arrays before it (`getPackageInfo(primary, 0x100024c0)` now decodes, so
  the head is right for that record).

That is the hypothesis to test first (and it would be a root-cause fix, not a
WebView patch): dump the bytes of `getPackageInfo(<provider>, 0x100024c0)` and of
`getPackageInfo(<primary>, 0x100024c0)` with an `ACOS_PARCEL_DUMP=<path>` hook
next to `ACOS_PMS_TRACE`, and check whether the provider reply's head carries the
primary's `packageName`/flags. If it does, the fix is to key the description
cache by package (and to have the materialiser name the package it answers).

## 4. How to reproduce this session's evidence

```sh
cd <worktree>
# provider APK
ACOS_APP_ONLY=webview-provider tools/build-apps.sh
ACOS_APP_ONLY=webview-probe tools/build-apps.sh
# session (private runtime dir + socket, shared lock)
ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts \
ACOS_RUNTIME_DIR=$PWD/.cache/wv1-run ACOS_SESSION_SOCKET=$PWD/.cache/wv1-run/session.sock \
ACOS_ART_BIN=$PWD/.cache/wv1-target/release/acos-art \
tools/art-runtime/run-exclusive.sh --task WV1 -- tools/guarded.sh --mem 8G --time 600 -- \
  ./.cache/wv1-target/release/acos verify results/handoff/WV1/webview-probe-provider.yaml \
  --engine art --artifacts .cache/wv1-run/ev1
```

Host `/` was at 84 % at the end of this session (145 G free); no benchmark was
run; the ART run held the shared lock for ~1 minute.
