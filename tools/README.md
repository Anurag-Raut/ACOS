# ACOS fixture tooling

Build scripts for the executable fixture corpus. They need the Android SDK
(`ANDROID_HOME`, default `/home/Qui/Android/Sdk`) and OpenJDK 17. Both are
idempotent and write only under `fixtures/build/` (gitignored) plus the
intentionally checked-in `fixtures/expected/`.

| Script | What it does |
|---|---|
| `build-fixtures.sh` | Compiles every `fixtures/dexprogs/<name>/` with `javac --release 11`, runs it on the host JVM to capture `fixtures/expected/dexprogs/<name>.{stdout,exit[,stderr]}`, then converts it to DEX with `d8` into `fixtures/build/dexprogs/<name>/classes.dex`. |
| `build-apps.sh` | Builds `fixtures/apps/hello-views` and `fixtures/apps/hello-jni` into signed, zipaligned APKs under `fixtures/build/apps/` using `aapt2 + javac + d8 + zipalign + apksigner` (and NDK clang for the JNI libs). Verifies each with `apksigner verify` and `aapt2 dump badging`. |
| `apk_append.py` | Helper used by `build-apps.sh` to add `classes.dex` / `lib/**` to an `aapt2`-linked APK without a synthetic `META-INF/MANIFEST.MF`. |

Expectations under `fixtures/expected/` are labelled **jvm-reference** (host
OpenJDK 17), not Android/ART-verified. See `fixtures/README.md`.

Device install smoke-testing only happens if `adb devices` already lists a
device; no emulator is ever started by these scripts.
