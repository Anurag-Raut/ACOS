# PMS `ApplicationInfo.flags` — W151-A provenance and decision

**Question:** `crates/acos-runtime/tests/pms.rs::pms_java_app_info_matches_the_reference_capture`
reported `app.flags=536870912` (0x2000_0000, `ApplicationInfo.FLAG_HARDWARE_ACCELERATED`)
while the committed golden `crates/acos-runtime/tests/data/pms_hello_views.golden`
expected `0`. W133-A (`21a633e`) set the flag unconditionally; the golden predates it.
Which side is the real-device truth?

**Verdict: the flag is correct; the golden was the wrong side.** The code now derives the
flag from the manifest exactly like AOSP, and the golden is corrected to `536870912`.

## 1. What the reference capture actually contains

`fixtures/reference/hello-views/art-api34-x86_64/` is a genuine device capture
(`adb`/`am`/`dumpsys`/`logcat`, fingerprint
`google/sdk_gphone64_x86_64/emu64xa:14/UE1A.230829.050/12077443:userdebug/dev-keys`).
It contains **no** `dumpsys package` output and **no** dump of `ApplicationInfo`:
`commands.txt` only captures `am start -W`, screenshots, `uiautomator` dumps,
`dumpsys window displays`, `dumpsys meminfo`, and `logcat`. The test comment's
"derived from `manifest.json` and `display.txt`" is therefore accurate for package
name / version / `minSdk` / `targetSdk` / `uid`, but `app.flags` has **no measured
source**. The golden's `0` was an ACOS-authored placeholder, never a device value.

## 2. The fixture manifest

`fixtures/apps/hello-views/AndroidManifest.xml`:

```xml
<uses-sdk android:minSdkVersion="26" android:targetSdkVersion="34" />
<application android:label="@string/app_name" android:allowBackup="false"
             android:debuggable="true"> ... </application>
```

`targetSdkVersion=34`, no `android:hardwareAccelerated` attribute.

## 3. AOSP logic (checked against the real Android 14 `framework.jar`)

Source: `.cache/boot/android-34/jars/framework.jar` → `classes.dex`, disassembled with
`~/Android/Sdk/build-tools/34.0.0/dexdump -d`.

`PackageParser.parseBaseApplication` (`[37ade4]`):

```
37b0c2: iget v1, ApplicationInfo.targetSdkVersion
37b0c6: const/16 v2, #int 14                    ; ICE_CREAM_SANDWICH
37b0ca: if-lt v1, v2, 016f                      ; targetSdk >= 14 ? true : false
37b0d4: const/16 v11, #int 23                   ; AndroidManifestApplication_hardwareAccelerated
37b0d8: invoke-virtual TypedArray.getBoolean(23, default)
37b0e0: iput-boolean baseHardwareAccelerated
37b0e8: const/high16 v11, 0x20000000            ; ApplicationInfo.FLAG_HARDWARE_ACCELERATED
37b0f0: ApplicationInfo.flags |= 0x20000000
```

i.e. `sa.getBoolean(hardwareAccelerated, targetSdkVersion >= 14)` → flag. This matches
the task's quoted `ParsingPackageUtils.parseBaseApplication` (the Android 14 tree still
carries the legacy `PackageParser`; the naming differs, the logic does not).

`PackageParser.parseActivity` (`[3756d8]`) sets `ActivityInfo.FLAG_HARDWARE_ACCELERATED`
(0x200) when not a receiver, with the **application default** as the attribute default
(the call site at `[37b6d6]`–`[37b712]` passes `pkg.baseHardwareAccelerated` as the last
argument):

```
375ae0: if-nez isReceiver, skip
375ae8: getBoolean(AndroidManifestActivity_hardwareAccelerated, appBaseHardwareAccelerated)
375b00: ActivityInfo.flags |= 0x200
```

For this fixture the app default is `true` (targetSdk 34), so the existing hardcoded
activity `0x200` happens to be correct; the app-level unconditional flag is what was
wrong (it would also have been set for an app with `targetSdk < 14`).

## 4. Fix

* `crates/acos-apk/src/manifest.rs`: parse `android:hardwareAccelerated` into
  `ApplicationInfo.hardware_accelerated: Option<bool>`.
* `crates/acos-services/src/pms.rs` `AppInfo::from_manifest`: derive

  ```rust
  let hardware_accelerated = manifest.application.hardware_accelerated
      .unwrap_or(target_sdk >= 14);          // ICE_CREAM_SANDWICH
  let flags = if hardware_accelerated { 0x2000_0000 } else { 0 };
  ```

  instead of the unconditional `0x2000_0000`.
* `crates/acos-runtime/tests/data/pms_hello_views.golden`: `app.flags=0` →
  `app.flags=536870912`, with the test header recording that this field is
  AOSP-derived rather than captured.

The golden is corrected because the evidence shows the reference capture never recorded
`app.flags` and AOSP would set it on the device for targetSdk 34.

## 5. Not changed (scope)

The XML comment lives in `crates/acos-runtime/tests/pms.rs`; code is unchanged.
`PmsState::from_apk`'s activity `flags: 0x200`
(`crates/acos-services/src/pms.rs`) and the runtime's synthetic `ActivityInfo`
(`client_transaction.rs`, `ams_java.rs`) are left as-is: AOSP's activity default is the
app value, which is true for every fixture here. Full per-component attribute
derivation is out of scope for this wave and is not exercised by the golden.
