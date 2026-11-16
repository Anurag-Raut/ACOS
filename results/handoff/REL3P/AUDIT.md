# REL3P — parcelable wire-format audit (ACOS v0.3.0)

Status: **pm family fixed and verified; two `android.net` codecs are wrong and are
not fixed here** (see §5). Branch `rel3p`.

## 0. Method, and why it caught the defect

`crates/acos-services/src/parcelable.rs` describes every parcelable ACOS writes as a
`ParcelableSpec`: the `FieldOp` list is the `writeToParcel` field order, and
`tests/parcelable.rs::codec_field_orders_match_the_framework_dex` checks that list
against the **image's own bytecode** (`Scanner::write_to_parcel`, the collapsed
`Parcel.write*` sequence of `Landroid/…;->writeToParcel` in
`.cache/boot/android-34/dex/framework/classes*.dex`).

Two holes in that oracle let this class of defect through:

1. `FieldOp::StringArray` matched **either** `writeStringArray` **or**
   `writeString8Array`. Those are different encodings (`String16` vs `String8`), so
   the `android.content.pm` specs "verified" while writing the wrong bytes.
2. The oracle only scanned `dex/framework`. Specs whose class lives in another boot
   classpath dex (`framework-connectivity`: `android.net.*`) were skipped as
   "class not found" and stayed `Verification::Full` without ever being checked.

Both are fixed: the kinds are distinct ops now (`StringArray` = `writeStringArray`,
new `String8Array` = `writeString8Array`, `StringList` = `writeStringList` only), and
`results/handoff/REL3P/parcelable-audit.txt` records the per-class result for both
dex directories.

Reproduce the table in this document:

```
cargo run -p acos-services --example parcelable_audit -- .cache/boot/android-34/dex/framework
cargo run -p acos-services --example parcelable_audit -- .cache/boot/android-34/dex/framework-connectivity
```

The `AOSP` column is the image's `writeToParcel` call list (android-14, tag
`android-14.0.0_r75` in `.cache/aosp` for the Java source; the image's DEX wins
where the two disagree, e.g. `Display$Mode`).

## 1. The defect

`PackageInfo.writeToParcel` writes `splitNames` (`0x000b`) and
`requestedPermissions` (`0x0072`) with **`writeString8Array`**
(`PackageInfo.java:585,608`). ACOS wrote `writeString16Array` for both. For a
**non-empty** array the two encodings differ (length word counts UTF-8 bytes vs
UTF-16 units, one byte per ASCII character vs two), so every field after
`requestedPermissions` was read out of the middle of the string payload:

```
07 000000  ...        # PackageInfo.signatures? no: the reader is inside the UTF-16 payload
OutOfMemoryError: Failed to allocate a 7676737936 byte allocation
  at Parcel.createString8Array <- PackageInfo.<init> <- PackageInfo$1.createFromParcel
```

`getPackageInfo(pkg, 0x100024c0)` = `GET_SIGNATURES|GET_META_DATA|GET_PERMISSIONS`
is what `WebViewFactory` calls, so any app using those flags got a corrupted
`PackageInfo` (SPIKE-WV wall 4, `results/handoff/SPIKE-WV/REPORT.md` §2.1).

## 2. The fix

| change | file |
|---|---|
| `Parcel::write_string8_array` / `read_string8_array` (and `write_string16_array_nullable`, so a `null` **element** is `-1` rather than an empty string, which is a different byte count) | `crates/acos-parcel/src/parcel.rs` |
| new `FieldOp::String8Array`; `StringArray`/`StringList`/`String8Array` each match exactly one `Parcel.write*` name | `crates/acos-services/src/parcelable.rs` |
| every `android.content.pm` array switched to `String8Array` (16 fields, §3) | same |
| `ProviderInfoList` spec was `[StringArray]`, the class actually writes `writeTypedList(ProviderInfo)` | same |
| `FieldOp::TypedArray` **reader** did not consume the per-element `writeTypedObject` presence marker the writer emits (invisible while every typed array was empty; the first non-empty `Signature[]` desynchronised the rest of the reply) | same |
| `GET_SIGNATURES` replies carried **zero** certificates: the APK signing-block parser read the signers *sequence* length as the first signer's length, so `signed_data` swallowed the signatures and v2/v3 parsing failed with `truncated signatures` | `crates/acos-apk/src/signing.rs` |

## 3. `android.content.pm` family — fields in AOSP order, ACOS op, match

`S8[]` = `writeString8Array`, `S16[]` = `writeStringArray` (String16),
`TL` = `writeTypedList`, `TA` = `writeTypedArray`, `Obj` = `writeTypedObject`.

### 3.1 `android.content.pm.PackageInfo` — `Verification::Full`, oracle 43/43

| # | AOSP field | AOSP call | ACOS op | match |
|---|---|---|---|---|
| 0 | packageName | writeString8 | Str8 | yes |
| 1 | splitNames | **writeString8Array** | StringArray → **String8Array** | **fixed** |
| 2-3 | versionCode, versionCodeMajor | writeInt ×2 | Int ×2 | yes |
| 4 | versionName | writeString8 | Str8 | yes |
| 5-6 | baseRevisionCode, splitRevisionCodes | writeInt, writeIntArray | Int, IntArray | yes |
| 7-8 | sharedUserId, sharedUserLabel | writeString8, writeInt | Str8, Int | yes |
| 9 | applicationInfo | writeInt marker + writeToParcel | Cond(Nested) | yes |
| 10-11 | firstInstallTime, lastUpdateTime | writeLong ×2 | Long ×2 | yes |
| 12 | gids | writeIntArray | IntArray | yes |
| 13-18 | activities, receivers, services, providers, instrumentation, permissions | writeTypedArray ×6 | TypedArray ×6 | yes |
| 19 | requestedPermissions | **writeString8Array** | StringArray → **String8Array** | **fixed** |
| 20 | requestedPermissionsFlags | writeIntArray | IntArray | yes |
| 21 | signatures | writeTypedArray | TypedArray (reader marker) | **fixed** |
| 22-25 | configPreferences, reqFeatures, featureGroups, attributions | writeTypedArray ×4 | TypedArray ×4 | yes |
| 26-29 | installLocation, isStub, coreApp, requiredForAllUsers | writeInt ×4 | Int ×4 | yes |
| 30-33 | restrictedAccountType, requiredAccountType, overlayTarget, overlayCategory | writeString8 ×4 | Str8 ×4 | yes |
| 34 | overlayPriority | writeInt | Int | yes |
| 35 | mOverlayIsStatic | writeBoolean | Bool | yes |
| 36-37 | compileSdkVersion, compileSdkVersionCodename | writeInt, writeString8 | Int, Str8 | yes |
| 38 | signingInfo | writeInt marker + writeToParcel | Cond(Nested) | yes |
| 39-40 | isApex, isActiveApex | writeBoolean ×2 | Bool ×2 | yes |

(The image's `writeToParcel` ends at `isActiveApex`; `mArchiveTimeMillis` /
`mApexPackageName` in the `r75` tag are **not** in the image, so they are not
written — the DEX oracle is what ACOS follows.)

### 3.2 `android.content.pm.ApplicationInfo` — `Verification::Full`, oracle 84/84

| AOSP field (in order) | AOSP call | ACOS op | match |
|---|---|---|---|
| maybeWriteSquashed marker | writeInt | Int | yes |
| PackageItemInfo: name, packageName, banner, label, icon, labelRes, metaData, showUser, showBadge | writeString8 ×2, writeInt, TextUtils, writeInt ×2, writeBundle, writeInt ×2 | Str8 ×2, Int, CharSequence, Int ×2, Bundle, Int ×2 | yes |
| taskAffinity, permission, processName, className | writeString8 ×4 | Str8 ×4 | yes |
| theme, flags, privateFlags, privateFlagsExt, requiresSmallestWidthDp, compatibleWidthLimitDp, largestWidthLimitDp | writeInt ×7 | Int ×7 | yes |
| storageUuid | writeInt marker + writeLong ×2 | Cond(Long,Long) | yes |
| scanSourceDir, scanPublicSourceDir, sourceDir, publicSourceDir | writeString8 ×4 | Str8 ×4 | yes |
| splitNames, splitSourceDirs, splitPublicSourceDirs | **writeString8Array ×3** | **String8Array ×3** | **fixed** |
| splitDependencies | writeSparseArray | SparseArray (null) | yes |
| nativeLibraryDir, secondaryNativeLibraryDir, nativeLibraryRootDir, nativeLibraryRootRequiresIsa, primaryCpuAbi, secondaryCpuAbi | writeString8 ×3, writeInt, writeString8 ×2 | Str8 ×3, Int, Str8 ×2 | yes |
| resourceDirs, overlayPaths | **writeString8Array ×2** | **String8Array ×2** | **fixed** |
| seInfo, seInfoUser | writeString8 ×2 | Str8 ×2 | yes |
| sharedLibraryFiles | **writeString8Array** | **String8Array** | **fixed** |
| sharedLibraryInfos | writeTypedList | TypedList | yes |
| dataDir, deviceProtectedDataDir, credentialProtectedDataDir | writeString8 ×3 | Str8 ×3 | yes |
| uid, minSdkVersion, targetSdkVersion, longVersionCode, enabled, enabledSetting, installLocation | writeInt ×5, writeLong, writeInt | Int ×5, Long, Int | yes |
| manageSpaceActivityName, backupAgentName | writeString8 ×2 | Str8 ×2 | yes |
| descriptionRes, uiOptions, fullBackupContent, dataExtractionRulesRes, crossProfile, networkSecurityConfigRes, category, targetSandboxVersion | writeInt ×7, writeBoolean | Int ×7, Bool | yes |
| classLoaderName | writeString8 | Str8 | yes |
| splitClassLoaderNames | **writeString8Array** | **String8Array** | **fixed** |
| compileSdkVersion, compileSdkVersionCodename, appComponentFactory, iconRes, roundIconRes, mHiddenApiPolicy, hiddenUntilInstalled, zygotePreloadName, gwpAsanMode, memtagMode, nativeHeapZeroInitialized | writeInt ×6, writeString8 ×3, writeInt ×3 | Int ×9, Str8 ×3 | yes |
| requestRawExternalStorageAccess (`ForBoolean`) | writeInt (helper) | Int | yes |
| createTimestamp | writeLong | Long | yes |
| mAppClassNamesByProcess | writeInt count + writeString ×2 per entry | Cond(Str16, Str16) | yes |
| localeConfigRes | writeInt | Int | yes |
| mKnownActivityEmbeddingCerts (`ForStringSet`) | writeInt / set | Int | yes |

### 3.3 `ComponentInfo`, `ActivityInfo`, `ServiceInfo`, `ProviderInfo`

All four are `Verification::Full` and match the image call-for-call (17/17, 45/45, 20/20, 28/28); the only
mismatch was `attributionTags` (1 per class), now `String8Array`:

| class | AOSP | ACOS op | match |
|---|---|---|---|
| `ComponentInfo` | PackageItemInfo prefix, **ApplicationInfo.writeToParcel**, processName, splitName, **writeString8Array(attributionTags)**, descriptionRes, enabled, exported, directBootAware | …, Nested(ApplicationInfo), Str8, Str8, **String8Array**, Int ×4 | yes (attributionTags **fixed**) |
| `ActivityInfo` | ComponentInfo prefix, theme, launchMode, documentLaunchMode, permission, taskAffinity, targetActivity, launchToken, flags, privateFlags, screenOrientation, configChanges, softInputMode, uiOptions, parentActivityName, persistableMode, maxRecents, lockTaskLaunchMode, WindowLayout, resizeMode, requestedVrComponent, rotationAnimation, colorMode, mMaxAspectRatio, mMinAspectRatio, supportsSizeChanges, mKnownActivityEmbeddingCerts, requiredDisplayCategory | same order, Cond(Nested(WindowLayout)), Float ×2, Bool, Int, Str8 | yes |
| `ServiceInfo` | ComponentInfo prefix, permission, flags, mForegroundServiceType | same | yes |
| `ProviderInfo` | ComponentInfo prefix, authority, readPermission, writePermission, grantUriPermissions, forceUriPermissions, PathPermission ×2 (typed arrays), multiprocess, initOrder, flags, isSyncable | same | yes |

### 3.4 The rest of the pm family

| class | verification | AOSP vs ACOS | match | fixed |
|---|---|---|---|---|
| `PackageItemInfo` | Full (9/9) | name, packageName, banner, label (TextUtils), icon, labelRes, metaData, showUser, showBadge | yes | — |
| `InstrumentationInfo` | Full (26/26) | splitNames, splitSourceDirs, splitPublicSourceDirs are **writeString8Array** | **was String16** | **fixed** |
| `PermissionInfo` | Full (17/17) | Group/backgroundPermission are String8; `knownCerts` models the null `ForStringSet` | yes | — |
| `Signature` | Full (1/1) | one `writeByteArray` | yes | — |
| `SigningInfo` → `SigningDetails` | Full (`writeBoolean`, `writeTypedArray`, `writeInt`, `writeArraySet`, `writeTypedArray`) | ACOS writes a `Cond(Nested(SigningInfo))` and never a non-null `SigningInfo`; a present one is a Curated gap | n/a (never written non-null) | — |
| `FeatureInfo` | Full (4/4) | name (String8), version, reqGlEsVersion, flags | yes | — |
| `ResolveInfo` | Full (20/20) | activity branch + null IntentFilter; `Cond` marker 1 | yes | — |
| `ProviderInfoList` | Full (1/1) | **writeTypedList(ProviderInfo)**, not a String array | **was `[StringArray]`** | **fixed** |
| `ParceledListSlice` / `BaseParceledListSlice` | handled by `write_parceled_list_slice` | count, per-item creator + length + payload, `writeStrongBinder` | yes (byte-identical replay T-36) | — |
| `ContentProviderHolder` | Full (5/5) | Nested(ProviderInfo), 2 binders, 2 ints | yes | — |
| `Annotation`/`Attribution`/`ConfigurationInfo`/`FeatureGroupInfo`/`PathPermission`/`SharedLibraryInfo` | no spec (never written by ACOS; typed-array slots are always empty) | — | — | — |

## 4. Non-pm parcelables ACOS writes

| class | verification | oracle result | note |
|---|---|---|---|
| `android.content.res.Configuration` | Full | 27/27 | also proved byte-for-byte against the real Java `writeToParcel` (`acos-runtime` `parcelable_java`) |
| `android.app.WindowConfiguration`, `android.graphics.Rect/Point/Insets`, `android.view.Display$Mode`, `DisplayShape`, `SurfaceControl$RefreshRateRange`, `android.window.SizeConfigurationBuckets` | Full | match | — |
| `android.view.DisplayInfo` | Prefix | 24/52 | documented gaps (HdrCapabilities etc.); the fields ACOS writes are in order and the full order is pinned by `tests/displayinfo_api34.rs` |
| `android.view.InputDevice`, `android.media.AudioAttributes` | Prefix | 21/28, 5/10 | image-specific trailing fields, documented |
| `android.app.ActivityManager$TaskDescription` | Curated | 3/31 | conditional layout pinned by its own test; icon is a structured `Unsupported` |
| `android.content.AttributionSourceState` | Curated | 8/9 | `renouncedPermissions` is `writeStringArray` (**String16**) — correctly `StringArray`, not `String8Array` |
| `android.net.Uri`, `ComponentName`, `Bundle`, `ParcelFileDescriptor`, `PersistableBundle`, `MemoryIntArray`, `ProfilerInfo`, `CompatibilityInfo`, `ClientTransaction`, `AutofillId/Value`, `OverlayProperties`, `HostUsiVersion`, `KeyCharacterMap`, `SpellChecker*`, `RunningAppProcessInfo`, `AndroidFuture`, `SharedMemory`, `AssistStructure/Content`, `ContentValues`, `ImeTracker$Token`, `UserHandle`, `AutofillOptions`, `ContentCaptureOptions` | Curated | see `parcelable-audit.txt` | small ACOS-owned values; several have no Java `writeToParcel` at all |

## 5. Mismatches found and **not** fixed (honest gap)

| class | ACOS spec | image `writeToParcel` | consequence |
|---|---|---|---|
| `android.net.NetworkCapabilities` | 19 ops, matched 5/18 (`Verification::Full` claim is false) | `writeLong ×3, writeInt ×2, writeParcelable ×2, writeInt, writeIntArray, writeString, writeBoolean, writeIntArray, writeInt ×2, writeString, writeIntArray, writeTypedList, writeInt` | a `getNetworkCapabilities()` reply written by ACOS is read with a different field layout (the transport/enterprise ids and the nested transport info/specifier). **Same defect class, different codec.** |
| `android.net.LinkProperties` | 18 ops, matched 2/17 | `writeString, writeInt, writeParcelable, writeBoolean, writeString ×2, writeInt, writeString, writeInt, writeParcelable, writeByte, writeParcelable ×2, writeList, writeBoolean, writeParcelable ×2` | same: `getLinkProperties()` replies desync. |

Neither is touched by the `getPackageInfo` defect and both need their own
AOSP-order rewrite plus the `Named`/`List` value plumbing in `connectivity.rs`;
that is the smallest next action (§5 of the report) and they must not be reported
as fixed. Both are declared `Verification::Full` while failing the oracle only
because the oracle's `dex_dir` is `dex/framework`; the fix is to scan every boot
classpath dex directory (evidence: `parcelable-audit.txt`, second section).

A third, narrower gap: `SigningInfo` is only ever written as an absent conditional
marker (`Cond(Nested)`, present=false); a present `SigningInfo` would need
`SigningDetails`' `writeArraySet` and two typed arrays.

## 6. Regression tests added

| test | fails before | passes after |
|---|---|---|
| `acos-parcel` `tests/string8_array.rs` (8 tests) — exact bytes of `writeString8Array` vs `writeString16Array`, null element, UTF-8 length, null/empty array, allocation bound | the op did not exist | yes |
| `acos-services` `tests/parcelable.rs::codec_field_orders_match_the_framework_dex` (strict op kinds) | yes — `PackageInfo` claimed `StringArray` for `writeString8Array` | yes |
| `parcelable.rs::package_info_reply_reads_back_in_the_framework_order` — walks a real `0x100024c0` reply in `PackageInfo.readFromParcel` order with `readString8`/`createString8Array` | yes (desync at `requestedPermissions`) | yes |
| `parcelable.rs::application_info_split_arrays_are_string8` — `splitNames`/`resourceDirs`/`sharedLibraryFiles` before `dataDir` | yes | yes |
| `parcelable.rs::typed_array_elements_round_trip_with_their_presence_marker` — non-empty `Signature[]` | yes (reader skipped the marker) | yes |
| `acos-runtime` `tests/parcelable_java.rs` — the **real framework** `PackageInfo.writeToParcel` bytes vs the ACOS codec, then `PackageInfo.CREATOR.createFromParcel` on the ACOS bytes, field checks, and a byte-identical re-emission | yes (bytes differ) | yes |
| `acos-apk` `tests/fixture_apk.rs::signed_fixture_reports_its_signer_certificate` — a real v2+v3 APK yields its 791-byte DER leaf, cross-checked with `apksigner verify --print-certs` (`fc297f51…`) | yes (`APK_SIGNING_MALFORMED: … truncated signatures`) | yes |
| `acos-apk` `signing::tests::v3_signer_reads_the_certificate_after_the_sdk_bounds` | yes | yes |
| ART fixture `fixtures/apps/pms-parcel` + `scenarios/rel3p-pms-parcel.yaml` | yes (`OutOfMemoryError`, then `sigs=0`) | yes (`report sigs=1 perms=2 …`, `AcosPmsParcel OK`) |
