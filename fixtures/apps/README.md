# ACOS real-APK fixtures

Six gradle-free Android APKs built with `aapt2 + javac + d8 + zipalign +
apksigner` (see `../../tools/build/build-apps.sh`). They are the first real,
signed APK inputs for the `acos-apk` ingest crate and, later, the Rust
runtime.

All are debug-signed with a **throwaway** keystore generated under
`fixtures/build/apps/debug.keystore` (never committed). All target
`minSdkVersion 26`, `targetSdkVersion 34`, and contain no AndroidX.

Build and verify:

    tools/build/build-apps.sh

Outputs (gitignored): `fixtures/build/apps/hello-views.apk`,
`hello-jni.apk`, `counter.apk`, `todo-list.apk`, `multi-activity.apk` and
`form-validation.apk`.

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

## counter

`dev.acos.fixtures.counter` — a `TextView` (`id/count`) plus two framework
`Button`s (`id/inc`, `id/reset`). Plain framework APIs only.

Expected observable behaviour:

1. On launch `count` shows **0**; the buttons read **Increment** and **Reset**.
2. Tapping `inc` sets `count` to `1`, then `2`, … and logs
   `increment value=N`.
3. Rotating the device recreates the activity; the count is restored from
   `onSaveInstanceState` (`onSaveInstanceState count=N` then `restored count=N`).
4. Tapping `reset` sets `count` back to **0** and logs `reset value=0`.
5. Lifecycle is logged under tag **`AcosCounter`**.

Manifest facts: package `dev.acos.fixtures.counter`, versionCode `1`,
versionName `1.0`, minSdk `26`, targetSdk `34`, one launcher activity
`.MainActivity`, permission `android.permission.INTERNET`, label `Counter`.

## todo-list

`dev.acos.fixtures.todolist` — `EditText` (`id/input`) + framework `ListView`
(`id/list`, `android.R.layout.simple_list_item_1`) + `Add`/`Clear all` buttons.
No RecyclerView, no AndroidX.

Expected observable behaviour:

1. `add` appends the input text to the list and logs
   `add item=<text> size=N`; tapping a row removes it (`remove item=… size=N`).
2. `clear` empties the list (`clear size=0`).
3. The list is persisted to `SharedPreferences` in `onPause` (`saved count=N`)
   and reloaded in `onCreate` (`loaded count=N`), so it survives rotation and
   process restart.
4. Lifecycle is logged under tag **`AcosTodo`**.

Manifest facts: package `dev.acos.fixtures.todolist`, versionCode `1`,
versionName `1.0`, minSdk `26`, targetSdk `34`, one launcher activity
`.MainActivity`, permission `android.permission.INTERNET`, label `Todo List`.

## multi-activity

`dev.acos.fixtures.multiactivity` — `MainActivity` (`EditText id/message`,
`Button id/send`) starts `SecondActivity` (`TextView id/received`,
`Button id/finish`) with a `message` extra.

Expected observable behaviour:

1. `send` starts `SecondActivity` with `putExtra("message", …)`; the second
   activity shows **`Received: <message>`** and logs `Second.received=<msg>`.
2. `finish` (or Back) returns to `MainActivity` through the back stack; the
   typed message is still there.
3. Rotating `MainActivity` saves/restores the message via
   `onSaveInstanceState` (`Main.onSaveInstanceState message=…`).
4. Lifecycle is logged under tag **`AcosMulti`**, prefixed `Main.`/`Second.`.

Manifest facts: package `dev.acos.fixtures.multiactivity`, versionCode `1`,
versionName `1.0`, minSdk `26`, targetSdk `34`, launcher `.MainActivity`,
non-exported `.SecondActivity`, permission `android.permission.INTERNET`,
label `Multi Activity`.

## form-validation

`dev.acos.fixtures.formvalidation` — `EditText`s (`id/name`, `id/email`),
`Button id/submit`, inline error `TextView id/error`, `Toast` on failure and an
`AlertDialog` on success.

Expected observable behaviour:

1. Submit with an empty name → `error` = **Name is required**
   (`validation_failed reason=name …`).
2. Name but empty email → **Email is required** (`reason=email`).
3. Malformed email → **Invalid email** (`reason=email_format`).
4. Valid name + email → `error` cleared, a Toast, and an `AlertDialog`
   titled **Success** with message **`Submitted: <name>`**
   (`validation_ok name=… email=…`).
5. Lifecycle is logged under tag **`AcosForm`**.

Manifest facts: package `dev.acos.fixtures.formvalidation`, versionCode `1`,
versionName `1.0`, minSdk `26`, targetSdk `34`, one launcher activity
`.MainActivity`, permission `android.permission.INTERNET`, label
`Form Validation`.

## Device verification status

`tools/build/build-apps.sh` runs `apksigner verify` and `aapt2 dump badging`
for every APK. Installation on an actual device/emulator is attempted
**only** when a device is already attached (`adb devices` shows one); no
emulator is started automatically. When no device is present the build
prints `not device-verified`.
