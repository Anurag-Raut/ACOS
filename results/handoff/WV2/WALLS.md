# WV2 — wall map, walked by measurement (WebView track, session 2)

Branch `wv2` (from `wv1` @ 70923102). Every row is a run whose logcat line is
quoted; evidence is under `results/handoff/WV2/evidence-1/`. Host `/` was 85 %
throughout; the ART run held the shared lock once (~1 min) and was not a
benchmark.

| # | wall | before WV2 | now |
|---|---|---|---|
| 1 | uid guard | satisfied | unchanged |
| 2 | `hasSystemFeature("android.software.webview")` | package-table lookup | unchanged (`true`) |
| 3 | `webviewupdate` → `waitForAndGetProvider` | peer answers from the installed record | unchanged |
| 4 | the `waitForAndGetProvider` **reply encoding** (`WebViewProviderResponse` → `PackageInfo`) | **CLOSED in WV2** — the reply was 4 bytes short (see §1) | decodes; `WebViewFactory` proceeds past `verifyPackageInfo` |
| 5 | `WebViewFactory.getWebViewContextAndSetProvider` → `ActivityManager.addPackageDependency` | not reached | **BLOCKED (new)**: `UnsupportedOperationException: unsupported Android API: android.app.IActivityManager code 96 (addPackageDependency)` (`evidence-1/wall5-addPackageDependency.txt`) |
| 6 | `createApplicationContext` for a package the process did not launch (`LoadedApk` + `PathClassLoader` over `/data/app/com.android.webview.apk`) | not reached | not reached |
| 7 | `Class.forName("…WebViewChromiumFactoryProviderForT").create(WebViewDelegate)` + `new WebView(...)` | not reached | not reached |

`evidence-1/logcat-after-parcel-fix.txt` lines 78–83: the OOM is gone, the
provider package is found, and the framework fails on the **next** missing
foundation API:

```
82 E WebViewFactory: error instantiating provider
   java.lang.UnsupportedOperationException: unsupported Android API: android.app.IActivityManager code 96 (addPackageDependency)
     at android.app.IActivityManager$Stub$Proxy.addPackageDependency(IActivityManager.java:7159)
     at android.webkit.WebViewFactory.getWebViewContextAndSetProvider(WebViewFactory.java:434)
```

## 1. Wall 4: the cause, found by a byte dump (not by guessing)

`ACOS_PARCEL_DUMP=<path>` (new, beside `ACOS_PMS_TRACE`) writes each reply's
written bytes to `<path>.<seq>-<tag>.bin` plus a `<path>.index` line. One
session recorded four replies (`evidence-1/index.txt`): the probe's own
`getPackageInfo`, `webviewupdate.7` (`getCurrentWebViewPackage`),
`webviewupdate.2` (`waitForAndGetProvider`).

The pre-fix reply, first 32 bytes (`evidence-1/reconstructed-before-fix-head.hex`
— the ev1 dump file itself was overwritten by the post-fix run, so it is
reproduced by dropping the one `write_int32(1)` that the fix added, which is the
only difference between the two replies; the ev1 log with the OOM is
`evidence-1/logcat-before-parcel-fix.txt`):

```
00000000: 0000 0000 0100 0000 1300 0000 636f 6d2e   ............com.
00000010: 616e 6472 6f69 642e                        android.
```

= `[0 exception][1 presence][19 packageName length]["com.android.webview"]`.

The framework's reader — both halves read out of the API-34 image's own dex,
not from the AOSP tag — is:

* `classes4.dex:Landroid/webkit/IWebViewUpdateService$Stub$Proxy;->waitForAndGetProvider`
  = `readException()` then `readTypedObject(WebViewProviderResponse.CREATOR)`
  (**one presence word for the return value**, because the method returns a
  parcelable);
* `classes4.dex:Landroid/webkit/WebViewProviderResponse;-><init>(Landroid/os/Parcel;)`
  = `readTypedObject(PackageInfo.CREATOR)` (**a second presence word**) then
  `readInt(status)`; its `writeToParcel` is `writeTypedObject(packageInfo)` +
  `writeInt(status)`.

The post-fix head is `evidence-1/after-fix-head.hex`
(`[0][1][1][19]`, `evidence-1/webviewupdate.2.reconstructed-before-fix.bin` for
the pre-fix bytes).

So the correct head is `[0][1][1][19]`. With `[0][1][19]` the reader took
`19` for the `packageInfo` presence, started `PackageInfo.<init>` at the string
payload, read `packageName` from the first four characters (`"com."`), then read
`splitNames`'s count out of the next characters: the deterministic
`7 676 737 936`-byte allocation. The image's own debug line table confirms the
crash site (`PackageInfo.<init>` `pc 0x000c line 570` is the **first**
`createString8Array`, i.e. `splitNames`; `readString8` is line 569).

Fix (root cause, one place, for the class): a method that returns a parcelable
writes the return value's presence word **and** the parcelable's own nested
writes — `WebViewUpdateService` now writes `[exception][presence(response)]
[presence(packageInfo)]payload[status]`, and the honest no-provider answer is a
present response with a null `packageInfo` and `STATUS_MISSING_ZYGOTE`.

Regression tests (fail before, pass after):

* `crates/acos-services/src/webview.rs`
  `wait_for_and_get_provider_writes_the_frameworks_expected_shape` — asserts the
  two presence words and `status` (the old test asserted the buggy single-marker
  shape, which is why the defect survived WV1).
  Measured both ways: without the outer word it fails with
  `left: 19, right: 1` (the `packageName` length read as the presence word —
  exactly what the framework did), with the fix it passes.

* `crates/acos-runtime/tests/parcelable_java.rs`
  `webview_provider_package_info_reads_back_in_the_framework_order` — the nested
  `PackageInfo` of the installed provider record, through the real
  `PackageInfo.CREATOR` (`packageName`, `applicationInfo.sourceDir`/`uid`,
  `requestedPermissions`, `splitNames`). This is what proved the
  `PackageInfo`/`ApplicationInfo` codecs were **not** at fault, which saved a
  wrong "fix".

The end-to-end oracle is the session table above: the same scenario went from
`OutOfMemoryError` (ev1) to wall 5 (ev2) with only the presence word changed.

### 1.1 An in-process replay discrepancy, found and left open (not hidden)

Replaying the **whole** reply in-process (`ParcelableCompareMain`:
`p.unmarshall(bytes)` → `p.readException()` → `WebViewProviderResponse.CREATOR`)
still OOMs at `PackageInfo.<init>` line 570 with the count
`778923875` = the first four bytes of `"com.android.webview"` read as an `i32`.
That means the `readString8` before it consumed only its length word. The same
bytes decode correctly in the session, so this path differs from the framework's
own (the session's reply is built natively by the transport, not by
`unmarshall(byte[])`); it is a VM `Parcel` question, not a WebView one, and this
session did not hide it behind a green test: the whole-reply case is **not** in
the tree, the nested-`PackageInfo` case is, and this paragraph names the exact
symptom and the byte offset for the next session.

## 2. Tag vs image (a trap that cost WV1 its hypothesis)

The AOSP source cache is newer than the pinned image, so both of these are
**wrong for this image** and were rejected after checking the image's dex:

| field | AOSP tag | image `classes.dex` |
|---|---|---|
| `ApplicationInfo.optionalSharedLibraryInfos` | written | **absent** |
| `ApplicationInfo.allowCrossUidActivitySwitchFromBelow` | written | **absent** |
| `PackageInfo.mArchiveTimeMillis`, `mApexPackageName` | written | **absent** (writer and reader both end at `isActiveApex`) |

Ground truth used: `dexdump` (built from `crates/acos-dex`) over
`.cache/boot/android-34/dex/framework/classes{,4}.dex`, plus the `line_table`
of `PackageInfo.<init>` for the crash line.

## 3. What wall 5 needs (smallest next action)

`IActivityManager.addPackageDependency(String, int)` — AOSP records that the
caller's process depends on the package so it is not killed, and returns
nothing. It is called by
`WebViewFactory.getWebViewContextAndSetProvider` before
`createPackageContext`. Implementing it as "record the dependency in the AMS
state, return no exception" is faithful and is the smallest next action; it is
AMS surface (`crates/acos-services/src/ams.rs` + the `acos-aidl` method table),
not WebView work, so this session did not invent it.

After that the expected next walls are `createApplicationContext` for a package
the process did not launch (§3 items 3–4 of SPIKE-WV/REPORT.md) — foundation
work, and per the brief an architectural wall to report, not to work around.
