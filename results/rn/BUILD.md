# `rn-hello` — React Native + Hermes fixture APK (local build note)

**Date:** 2026-11-11. **Scope:** one minimal, real React Native host APK that
ACOS can use as a Hermes + RN native-lib corpus. The JS/Metro/Hermes bundle is
real; the Android host is a stock RN 0.81 `ReactActivity`/`ReactNativeHost` app
built without Gradle by `tools/build-rn-hello.sh` (aapt2 + javac + d8 +
zipalign + apksigner), with the RN/Hermes/AndroidX classpath taken from the
prebuilt Maven AARs (react-android, hermes-android) and their closure.

This file is a working note, not a contract. Nothing was committed; the APKs and
the `.cache/rn-0.81.4/` download cache are gitignored (`/fixtures/build/`,
`/.cache/`).

---

## 1. Result at a glance

| question | answer |
|---|---|
| APK built? | **yes** — `fixtures/build/apps/rn-hello.apk` (x86_64) |
| size | **8,587,495 B** (8.19 MiB) x86_64; `rn-hello-arm64.apk` 8,452,345 B |
| signed/verified | yes, v2+v3 (`apksigner verify`), throwaway debug key |
| Hermes? | **yes** — `assets/index.android.bundle` is HBC, bytecode v96 |
| new architecture | **OFF** (`DefaultReactActivityDelegate(..., fabricEnabled=false)`, `isNewArchEnabled()==false`) |
| native libs (x86_64) | 6: libreactnative, libhermes, libjsi, libfbjni, libhermestooling, libc++_shared |
| `acos native-inventory` missing imports | **0 missing**, 4 partial (see §5) |
| phone reference captured? | **no** — `ZA222WGJPP` was claimed by another agent-device session (`/home/Qui/.paseo/worktrees/3ltis6kb/...`); the arm64 APK is built and ready but was not installed. |

Build artifacts (untracked, large):

```
fixtures/build/apps/rn-hello.apk          x86_64  (primary, ACOS host ABI)
fixtures/build/apps/rn-hello-arm64.apk    arm64-v8a (phone reference variant)
```

Sources (tracked candidates): `fixtures/apps/rn-hello/` (`index.js`,
`app.json`, `package.json`, `babel.config.js`, `metro.config.js`,
`AndroidManifest.xml`, `src/dev/acos/fixtures/rnhello/{MainActivity,
MainApplication}.java`, `res/`).

Tooling: `tools/build-rn-hello.sh` (build/fetch/bundle),
`tools/rn-hello-deps.py` (offline Gradle-cache dependency resolver).

---

## 2. Versions pinned

| component | version | note |
|---|---|---|
| React Native | **0.81.4** | last release before the legacy/Paper architecture was removed (0.82); required because this fixture must run Fabric OFF |
| Hermes | `hermes-2025-07-07-RNv0.81.0-e0fc67142ec…` | shipped in `react-native@0.81.4`; hermesc HBC version **96** |
| react-android AAR | 0.81.4-release | 133,827,108 B (`repo.reactnative.dev`) |
| hermes-android AAR | 0.81.4-release | 63,383,971 B (`repo.reactnative.dev`) |
| React (JS) | 19.1.0 | RN 0.81 peer |
| Metro | 0.83.8 | bundled by RN |
| AndroidX closure | appcompat 1.7.0, core 1.13.0, activity 1.7.0, fragment 1.5.4, lifecycle 2.6.2, swiperefreshlayout 1.1.0, emoji2 1.3.0, … | resolved from `~/.gradle/caches` |
| build tools | 35.0.0, `platforms/android-34` | same as `tools/build-apps.sh` |
| Node / JDK | v24.19.0 / OpenJDK 17.0.21 | |

### Why not Gaten's 0.86.3?

Gaten is on `react-native@0.86.3`. **New Architecture is mandatory in RN ≥ 0.82**
and the Paper `UIManagerModule` is gone from the 0.86 `react-android` AAR (only
`UIManagerModuleConstants*` remain; there is no `com/facebook/react/uimanager/
UIManagerModule.class`). A Fabric-off fixture is therefore impossible on 0.86.3,
so this fixture pins **0.81.4** (the last old-arch release) and its native libs
do **not** byte-match what Gaten ships. See §6 for the consequence. The AARs are
still fetched from the same Maven repo and the same toolchain (Hermes + JSI +
fbjni + RN C++), so the *shape* of the corpus matches.

---

## 3. What is in the APK

Manifest: `dev.acos.fixtures.rnhello`, versionCode 1, versionName 1.0,
minSdk 26, targetSdk 34, debuggable, `INTERNET`, launcher `.MainActivity`,
`android:name=".MainApplication"`, `usesCleartextTraffic=true`,
`theme=@style/AppTheme` (`Theme.AppCompat.DayNight.NoActionBar`).

Classes: `MainActivity extends ReactActivity` (component `"rnhello"`);
`MainApplication extends Application implements ReactApplication` using
`DefaultReactNativeHost` with `MainReactPackage`, `isNewArchEnabled()==false`,
`isHermesEnabled()==true`, and `SoLoader.init(this, false)`. DEX: `classes.dex`
(3,608,672 B) + `classes2.dex` (6,684,740 B) — app + AppCompat/AndroidX closure.

Assets: `assets/index.android.bundle` = 1,501,379 B Hermes bytecode (`c6 1f bc
03 …`), built from the JSX-free `index.js` that renders
`View > [Text "Hello ACOS", Text "React Native + Hermes", Button "Tap me",
Text "count: N"]` and registers `AppRegistry.registerComponent('rnhello', …)`.
The pre-Hermes JS bundle (Metro output) is 2,310,415 B and lives only in the
cache (`.cache/rn-0.81.4/jsproject/out/`).

Native libs (`lib/x86_64/`):

| library | size (B) | imports | DT_NEEDED (non-system) |
|---|---:|---:|---|
| libreactnative.so | 6,123,024 | 417 | libandroid.so, libfbjni.so, libjsi.so, liblog.so, libm.so, libc++_shared.so, libdl.so, libc.so |
| libhermes.so | 2,221,664 | 266 | libjsi.so, libfbjni.so, liblog.so, libm.so, libc++_shared.so, libdl.so, libc.so |
| libjsi.so | 398,088 | 187 | libm.so, libc++_shared.so, libdl.so, libc.so |
| libfbjni.so | 172,952 | 96 | libandroid.so, liblog.so, libm.so, libc++_shared.so, libdl.so, libc.so |
| libhermestooling.so | 134,232 | 126 | libreactnative.so, libjsi.so, libhermes.so, libandroid.so, libfbjni.so, liblog.so, libm.so, libc++_shared.so, libdl.so, libc.so |
| libc++_shared.so | 1,252,080 | 157 | libc.so, libm.so, libdl.so |

`libhermestooling.so` and `libhermes.so` are only needed if the tooling/DevSupport
path is exercised; the minimal boot path is
`libreactnative.so → libjsi.so → libfbjni.so → libhermes.so` (+ `libc++_shared.so`).

---

## 4. `acos native-inventory` output

The checked-in `target/release/acos` in the working tree is stale and has **no**
`native-inventory` subcommand; the freshly built `target/debug/acos` does. Use
that:

```
target/debug/acos native-inventory fixtures/build/apps/rn-hello.apk --abi x86_64 \
    --json results/rn/rn-hello-inventory.json
```

```
wrote results/rn/rn-hello-inventory.json (6 libraries)

6 libraries, 675 distinct imports: 671 provided, 4 partial, 0 missing (9436 defined in corpus)

rank  provision libs   symbol                                   note
1     partial   6      dl_iterate_phdr@LIBC                    guest objects only, no host objects
2     partial   1      __cxa_thread_atexit_impl                runs on host thread exit only
3     partial   1      dladdr@LIBC                             reports the object but not the nearest symbol
4     partial   1      pthread_getattr_np@LIBC                 reports the host stack, not the guest one
(sites: 1249 import references across the corpus)
```

Machine-readable: `results/rn/rn-hello-inventory.json`.
Raw text: `results/rn/rn-hello-inventory.txt`.

Compared with Gaten (`results/native-loader/inventory/gaten.json`: 23 libs,
1458 imports, **91 missing**), the fixture is deliberately small: 6 libs,
675 imports, **0 missing**, and the same 4 partial ILIBC surfaces that Gaten
also hits (`dl_iterate_phdr`, `dladdr`, `pthread_getattr_np`,
`__cxa_thread_atexit_impl`). So the *provision gap* for this corpus is exactly
the known partial shims — no new loader work is implied by the RN 0.81 libs.

---

## 5. What an ACOS runtime must supply to render the first `Text`

With Fabric off, the app is the classic RN **Paper** path:

1. `MainApplication.onCreate` → `SoLoader.init` loads the four libs above.
   The guest must satisfy the bionic imports the loader reports as partial
   (`dl_iterate_phdr`, `dladdr`, `pthread_getattr_np`, `__cxa_thread_atexit_impl`)
   and provide the exported C++/JNI surface in `libreactnative.so`.
2. `MainActivity` → `ReactActivityDelegate` → `ReactRootView` attaches to a
   `ReactInstanceManager`. The native side creates the JSI/Hermes runtime and
   evaluates `assets/index.android.bundle` (HBC v96).
3. JS registers `rnhello` via `AppRegistry`; the first render produces a
   `createView`/`updateView` batch through `UIManagerModule`.
4. **Paper view managers** materialise the tree as framework widgets:
   `RCTView` → `ReactViewGroup`/`android.view.View`,
   `RCTText` → **`com.facebook.react.views.text.ReactTextView` extends
   `androidx.appcompat.widget.AppCompatTextView` (a real `TextView`)**,
   `RCTRawText` → an `appCompatTextView`/`ReactTextView` child,
   `RCTButton` → `ReactButton`/`AppCompatButton`.
   The observable result is a `ViewGroup` containing `TextView` "Hello ACOS"
   and an `AppCompatButton` "Tap me".

   Therefore the runtime must supply (or emulate) enough of:
   - `android.os.Looper`/`Handler` + `Choreographer` for the UI batch loop,
   - `android.graphics.*` + `android.text.*` (`Layout`, `StaticLayout`,
     `TextPaint`) so `ReactTextView` can measure/layout,
   - `android.view.*` (`ViewGroup`, `MeasureSpec`, `MotionEvent`) for layout
     and the tap on the button,
   - `android.content.res.Resources`/`AssetManager` for
     `assets/index.android.bundle` and the AppCompat theme,
   - the JNI bridge (`libfbjni.so`) plus a working `JNIEnv`/`JavaVM`,
   - a Hermes VM able to load HBC **version 96**.

   The AppCompat theme is not optional: `ReactActivity` extends
   `AppCompatActivity`, and `ReactTextView` extends `AppCompatTextView`, so the
   fixture's resource table includes the AppCompat/AndroidX resources (merged
   into the APK by `aapt2 link --extra-packages`).

---

## 6. Caveats / honest limits

* **Version mismatch with Gaten.** Gaten is 0.86.3 / new arch; this fixture is
  0.81.4 / Paper. It is the closest *runnable, Fabric-off* RN, not the same
  binary ABI. To study Gaten's exact libs, use the existing
  `results/native-loader/inventory/gaten.json` corpus.
* **Manual AAR resource merge.** Library `R` ids are generated by `aapt2 link
  --extra-packages`; this matches the process, but it is not AGP. If a screen
  relies on an AppCompat resource there is a small chance of an id mismatch;
  the app was not executed on the host (per the task), so this is untested at
  runtime.
* **Stale release CLI.** `target/release/acos` predates `native-inventory`;
  the command above uses `target/debug/acos`. Rebuilding `acos-cli` is outside
  this workstream (crates/ untouched).
* **Phone reference missing.** Device `ZA222WGJPP` was held by another
  agent-device session, so no install/screenshot/uiautomator capture was made.
  The arm64 APK is built; when the device frees up, install
  `fixtures/build/apps/rn-hello-arm64.apk` (package
  `dev.acos.fixtures.rnhello`) and capture into
  `fixtures/reference-device/rn-hello/`, then uninstall.

---

## 7. Reproduce

```bash
# 1. fetch pinned AARs + npm bundler + Metro bundle + hermesc  (network, cached)
tools/build-rn-hello.sh fetch

# 2. build both APKs (offline after fetch)
tools/build-rn-hello.sh build

# 3. static evidence
target/debug/acos native-inventory fixtures/build/apps/rn-hello.apk \
    --abi x86_64 --json results/rn/rn-hello-inventory.json
SDK=/home/Qui/Android/Sdk
$SDK/build-tools/35.0.0/apksigner verify --verbose fixtures/build/apps/rn-hello.apk
$SDK/build-tools/35.0.0/aapt2 dump badging fixtures/build/apps/rn-hello.apk
```

Downloads ~430 MB (AARs 197 MB + npm closure ~230 MB), all under
`.cache/rn-0.81.4/`; host RAM stayed inside the `tools/guarded.sh --mem 6G`
bound. Scratch (`fixtures/build/apps/work-rn`, extracted AARs, template) was
deleted after the build; the AARs, `rnjs/` (hermesc), `jsproject/node_modules/`
and the npm cache are kept so the build is reproducible offline.
