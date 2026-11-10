# ACOS real-APK fixtures

Two gradle-free Android APKs built with `aapt2 + javac + d8 + zipalign +
apksigner` (see `../../tools/build-apps.sh`). They are the first real,
signed APK inputs for the `acos-apk` ingest crate and, later, the Rust
runtime.

Both are debug-signed with a **throwaway** keystore generated under
`fixtures/build/apps/debug.keystore` (never committed). Both target
`minSdkVersion 26`, `targetSdkVersion 34`, and contain no AndroidX.

Build and verify:

    tools/build-apps.sh

Outputs (gitignored): `fixtures/build/apps/hello-views.apk` and
`fixtures/build/apps/hello-jni.apk`.

## hello-views

`dev.acos.fixtures.helloviews` — a plain `android.app.Activity` with a
`LinearLayout` created from `res/layout/main.xml`.

| id | widget | initial text |
|---|---|---|
| `title` | `TextView` | `Hello ACOS` (`@string/title`) |
| `input` | `EditText` | hint `Enter your name` |
| `btn` | `Button` | `Greet` |
| `result` | `TextView` | `clicks: 0` (`@string/initial_result`) |

Expected observable behaviour:

1. On launch, `title` shows **Hello ACOS** and `result` shows **clicks: 0**.
2. Tapping `btn` with `input` = `Ada` sets `result` to
   **`Hello, Ada! clicks: 1`**; a second tap with `input` = `Bob` sets it to
   **`Hello, Bob! clicks: 2`**.
3. Lifecycle callbacks are logged under tag **`AcosFixture`**, in order:
   `onCreate`, `onStart`, `onResume` (and on the way out `onPause`,
   `onStop`, `onDestroy`).
4. The click counter is stored in `onSaveInstanceState` and restored after
   recreation; `result` is repopulated as `clicks: N`.

Manifest facts to assert: package `dev.acos.fixtures.helloviews`,
versionCode `1`, versionName `1.0`, minSdk `26`, targetSdk `34`, one
launcher activity `.MainActivity`, permission
`android.permission.INTERNET`, app label `Hello Views`.

## hello-jni

`dev.acos.fixtures.hellojni` — loads `libhellojni.so` (built for
`x86_64` and `arm64-v8a` with NDK clang, API 26) and shows a `TextView`
whose text is a newline-separated report.

Expected text lines:

    stringFromJNI=Hello from native C
    add(20,22)=42
    sumArray([1,2,3,4,5])=15
    javaCallbackResult=42
    voidCallback=from-native
    registeredEcho=echo!
    throwFromJNI caught=thrown from native

Native behaviours exercised:

- `stringFromJNI()` — return a `jstring` (`NewStringUTF`).
- `add(int,int)` — primitive arguments and return.
- `sumArray(int[])` — `GetArrayLength` + `GetIntArrayElements` /
  `ReleaseIntArrayElements`.
- `callBack()` — native → Java: `CallStaticIntMethod` into
  `javaCallback(int)` and `CallVoidMethod` into `voidCallback(String)`.
- `registeredEcho(String)` — dynamically registered with
  `RegisterNatives` from `JNI_OnLoad`.
- `throwFromJNI()` — `ThrowNew(IllegalStateException, "thrown from
  native")`, caught in Java.

Log lines use tag **`AcosJniFixture`**: `onCreate`,
`javaCallback x=21`, `voidCallback s=from-native`.

Manifest facts to assert: package `dev.acos.fixtures.hellojni`,
versionCode `2`, versionName `2.0`, minSdk `26`, targetSdk `34`, one
launcher activity `.MainActivity`, permission
`android.permission.INTERNET`, app label `Hello JNI`, and native libs
under `lib/x86_64/` and `lib/arm64-v8a/`.

## Device verification status

`tools/build-apps.sh` runs `apksigner verify` and `aapt2 dump badging`
for every APK. Installation on an actual device/emulator is attempted
**only** when a device is already attached (`adb devices` shows one); no
emulator is started automatically. When no device is present the build
prints `not device-verified`.
