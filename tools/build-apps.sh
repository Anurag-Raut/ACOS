#!/usr/bin/env bash
#
# Build the ACOS real-APK fixtures without Gradle.
#
# Toolchain: aapt2 + javac + d8 + zipalign + apksigner from the Android SDK;
# NDK clang for hello-jni. A throwaway debug keystore is generated under
# fixtures/build/apps/ (never committed).
#
# Outputs (all signed + zipaligned, verified with `apksigner verify` and
# `aapt2 dump badging`):
#   fixtures/build/apps/hello-views.apk
#   fixtures/build/apps/hello-jni.apk
#   fixtures/build/apps/counter.apk
#   fixtures/build/apps/todo-list.apk
#   fixtures/build/apps/multi-activity.apk
#   fixtures/build/apps/form-validation.apk
#   fixtures/build/apps/svg-viewbox.apk
#   fixtures/build/apps/offscreen-pager.apk
#   fixtures/build/apps/insets-probe.apk
#   fixtures/build/apps/multi-package-probe.apk
# Installation on a device is attempted only if one is already attached; no
# emulator is ever started here.
set -euo pipefail

SDK="${ANDROID_HOME:-/home/Qui/Android/Sdk}"
BUILD_TOOLS_VERSION="${BUILD_TOOLS_VERSION:-35.0.0}"
BT="$SDK/build-tools/$BUILD_TOOLS_VERSION"
ANDROID_JAR="$SDK/platforms/android-34/android.jar"
AAPT2="$BT/aapt2"
D8="$BT/d8"
ZIPALIGN="$BT/zipalign"
APKSIGNER="$BT/apksigner"
NDK="${ANDROID_NDK_HOME:-$SDK/ndk/27.1.12297006}"
NDK_BIN="$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin"
# Public NDK seams used by the native fixtures. Stock bionic resolves an app
# `.so`'s undefined symbols only from the global group plus its own DT_NEEDED
# local group (bionic/linker/linker.cpp: `SymbolLookupList(global_group,
# local_group)`), so a library that calls `AAssetManager_*`/`AndroidBitmap_*`/
# `ANativeWindow_*` without the matching `-l` flag fails on a real device with
# `cannot locate symbol` even though the app namespace is allowed to load the
# library. The NDK sysroot's `<api>/lib{android,jnigraphics,nativewindow}.so`
# are ABI stubs: they contribute the correct DT_SONAME/DT_NEEDED entry to the
# APK's library and the API-34 image supplies the real implementations at
# runtime (docs/IMAGE_ABI_NOTEBOOK.md, W20/W21).
NATIVE_PUBLIC_LIBS="-landroid -ljnigraphics -lnativewindow"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Run the whole build under the host memory/time guard (ADR-0018).
if [ -z "${ACOS_GUARDED:-}" ]; then
    exec "$ROOT/tools/guarded.sh" --mem 6G --time 1800 -- \
        env ACOS_GUARDED=1 "$0" "$@"
fi

APPS_DIR="$ROOT/fixtures/apps"
BUILD_DIR="$ROOT/fixtures/build/apps"
APPEND_PY="$ROOT/tools/apk_append.py"
KEYSTORE="$BUILD_DIR/debug.keystore"
KEY_ALIAS="androiddebugkey"
KEY_PASS="android"

die() { echo "build-apps: error: $*" >&2; exit 1; }

command -v javac >/dev/null 2>&1 || die "javac not found on PATH"
command -v python3 >/dev/null 2>&1 || die "python3 not found on PATH"
for t in "$AAPT2" "$D8" "$ZIPALIGN" "$APKSIGNER"; do
    [ -x "$t" ] || die "missing tool: $t (set ANDROID_HOME / BUILD_TOOLS_VERSION)"
done
[ -f "$ANDROID_JAR" ] || die "android.jar not found: $ANDROID_JAR"
[ -f "$APPEND_PY" ] || die "helper not found: $APPEND_PY"

mkdir -p "$BUILD_DIR"

# --- throwaway debug keystore ------------------------------------------------
if [ ! -f "$KEYSTORE" ]; then
    echo "build-apps: generating throwaway debug keystore"
    keytool -genkeypair \
        -keystore "$KEYSTORE" \
        -storepass "$KEY_PASS" -keypass "$KEY_PASS" \
        -alias "$KEY_ALIAS" \
        -dname "CN=Android Debug,O=Android,C=US" \
        -keyalg RSA -keysize 2048 -validity 10000 \
        -deststoretype pkcs12 >/dev/null 2>&1 \
        || die "keytool failed to create $KEYSTORE"
fi

# build_apk <name> <src_java_root> [<extra_dir>:<arcname> ...]
#   Compiles resources + java + dex, appends extra payload, aligns and signs.
build_apk() {
    local name="$1"; shift
    local app_dir="$APPS_DIR/$name"
    local work="$BUILD_DIR/$name"
    local out="$BUILD_DIR/$name.apk"
    # W7: a fixture may need a different SDK. `insets-probe` is targetSdk 35
    # (Android 15 edge-to-edge default); everything else stays on the frozen
    # API-34 profile its baselines were captured on.
    local target_sdk="${APP_TARGET_SDK:-34}"
    local android_jar="$SDK/platforms/android-$target_sdk/android.jar"
    [ -f "$android_jar" ] || die "android.jar not found: $android_jar"

    rm -rf "$work"
    mkdir -p "$work/compiled" "$work/gen" "$work/classes" "$work/dex"

    echo "build-apps: [$name] compiling resources"
    "$AAPT2" compile --dir "$app_dir/res" -o "$work/compiled.zip"

    echo "build-apps: [$name] linking resources + manifest"
    "$AAPT2" link \
        -o "$work/base.apk" \
        -I "$android_jar" \
        --manifest "$app_dir/AndroidManifest.xml" \
        -R "$work/compiled.zip" \
        --java "$work/gen" \
        --min-sdk-version 26 \
        --target-sdk-version "$target_sdk" \
        --auto-add-overlay

    echo "build-apps: [$name] compiling java"
    # shellcheck disable=SC2046
    javac --release 11 -encoding UTF-8 -cp "$android_jar" -d "$work/classes" \
        $(find "$app_dir/src" "$work/gen" -name '*.java')

    echo "build-apps: [$name] dexing"
    # shellcheck disable=SC2046
    "$D8" --lib "$android_jar" --min-api 26 --output "$work/dex" \
        $(find "$work/classes" -name '*.class')

    python3 "$APPEND_PY" "$work/base.apk" "$work/dex/classes.dex" "$@"

    echo "build-apps: [$name] aligning"
    rm -f "$out" "$work/aligned.apk"
    "$ZIPALIGN" -f 4 "$work/base.apk" "$work/aligned.apk"

    echo "build-apps: [$name] signing"
    "$APKSIGNER" sign \
        --ks "$KEYSTORE" \
        --ks-pass "pass:$KEY_PASS" \
        --key-pass "pass:$KEY_PASS" \
        --ks-key-alias "$KEY_ALIAS" \
        --v1-signing-enabled true \
        --v2-signing-enabled true \
        --v3-signing-enabled true \
        --out "$out" \
        "$work/aligned.apk"

    echo "build-apps: [$name] verifying"
    "$APKSIGNER" verify --verbose "$out" >/dev/null
    "$APKSIGNER" verify --print-certs "$out" | sed 's/^/    /'
    echo "build-apps: [$name] badging:"
    "$AAPT2" dump badging "$out" | sed 's/^/    /' | head -n 20
    echo "build-apps: [$name] -> $out ($(stat -c '%s' "$out") bytes)"
}

# K5b: `ACOS_APP_ONLY="a b"` builds just the named fixtures, so one app can be
# iterated without rebuilding the whole corpus.
if [ -n "${ACOS_APP_ONLY:-}" ]; then
    for app in $ACOS_APP_ONLY; do
        build_apk "$app"
    done
    echo "build-apps: done (ACOS_APP_ONLY=$ACOS_APP_ONLY) -> $BUILD_DIR"
    exit 0
fi

# --- hello-views -------------------------------------------------------------
build_apk hello-views

# --- hello-jni ---------------------------------------------------------------
JNI_WORK="$BUILD_DIR/hello-jni-jni"
rm -rf "$JNI_WORK"
mkdir -p "$JNI_WORK/x86_64" "$JNI_WORK/arm64-v8a"
echo "build-apps: [hello-jni] compiling native libs"
"$NDK_BIN/x86_64-linux-android26-clang" -shared -fPIC -O2 \
    -o "$JNI_WORK/x86_64/libhellojni.so" "$APPS_DIR/hello-jni/jni/hello-jni.c"
"$NDK_BIN/aarch64-linux-android26-clang" -shared -fPIC -O2 \
    -o "$JNI_WORK/arm64-v8a/libhellojni.so" "$APPS_DIR/hello-jni/jni/hello-jni.c"
build_apk hello-jni "$JNI_WORK:lib"

# --- native-hello ------------------------------------------------------------
# Java Application calls a bundled libnativehello.so from onCreate (String,
# APK asset via AAssetManager, Bitmap via AndroidBitmap_lockPixels).
NATIVE_JNI_WORK="$BUILD_DIR/native-hello-jni"
rm -rf "$NATIVE_JNI_WORK"
mkdir -p "$NATIVE_JNI_WORK/x86_64" "$NATIVE_JNI_WORK/arm64-v8a"
echo "build-apps: [native-hello] compiling native libs"
# shellcheck disable=SC2086
"$NDK_BIN/x86_64-linux-android26-clang" -shared -fPIC -O2 $NATIVE_PUBLIC_LIBS \
    -o "$NATIVE_JNI_WORK/x86_64/libnativehello.so" "$APPS_DIR/native-hello/jni/nativehello.c"
# shellcheck disable=SC2086
"$NDK_BIN/aarch64-linux-android26-clang" -shared -fPIC -O2 $NATIVE_PUBLIC_LIBS \
    -o "$NATIVE_JNI_WORK/arm64-v8a/libnativehello.so" "$APPS_DIR/native-hello/jni/nativehello.c"
build_apk native-hello "$NATIVE_JNI_WORK:lib" "$APPS_DIR/native-hello/assets:assets"

# --- native-hostile ----------------------------------------------------------
# Bundled library that attempts denied syscalls; used to assert the ADR-0022
# confinement layers (execve, /etc/passwd, AF_INET, out-of-dir write, ptrace).
HOSTILE_JNI_WORK="$BUILD_DIR/native-hostile-jni"
rm -rf "$HOSTILE_JNI_WORK"
mkdir -p "$HOSTILE_JNI_WORK/x86_64" "$HOSTILE_JNI_WORK/arm64-v8a"
echo "build-apps: [native-hostile] compiling native libs"
"$NDK_BIN/x86_64-linux-android26-clang" -shared -fPIC -O2 \
    -o "$HOSTILE_JNI_WORK/x86_64/libhostile.so" "$APPS_DIR/native-hostile/jni/hostile.c"
"$NDK_BIN/aarch64-linux-android26-clang" -shared -fPIC -O2 \
    -o "$HOSTILE_JNI_WORK/arm64-v8a/libhostile.so" "$APPS_DIR/native-hostile/jni/hostile.c"
build_apk native-hostile "$HOSTILE_JNI_WORK:lib"

# --- framework-only fixture apps (no AndroidX, no native libs) ---------------
# Each is a plain android.app.Activity app exercising one Android subsystem:
# counter (view state + onSaveInstanceState), todo-list (ListView + input +
# SharedPreferences), multi-activity (Intent extras + back stack +
# onSaveInstanceState), form-validation (EditText validation + Toast +
# AlertDialog) and settle-lab (P11: animations, delayed/chained updates,
# dialogs, Activity transition, ListView scroll, infinite progress,
# LayoutTransition, rapid taps).
# offscreen-pager is the tree-visibility fixture (GR2 W4): a two-page
# HorizontalScrollView whose second page is warm-mounted one display width off
# screen, with text that must be reported `visible:false` until you scroll.
for app in counter todo-list multi-activity form-validation settle-lab thread-heavy svg-viewbox offscreen-pager; do
    build_apk "$app"
done

# multi-package-probe (MPROC1) is the multi-package session fixture: it asks
# `PackageManager` about a second installed package (`dev.acos.fixtures.counter`)
# and builds that package's Context/resources. It needs no second APK at build
# time; the session installs `counter.apk` with `--extra-apk`.
build_apk multi-package-probe

# intent-probe (MPROC1 step 2) drives the two intents a device routes to another
# app: an implicit ACTION_SEND text share (`startActivity`) and an
# ACTION_GET_CONTENT image pick (`startActivityForResult`, requestCode 42).
build_apk intent-probe

# pms-parcel is the REL3P fixture: it calls
# getPackageInfo(own package, GET_SIGNATURES|GET_META_DATA|GET_PERMISSIONS =
# 0x100024c0) and shows the signature / requested-permission counts the
# framework decoded from the ACOS PMS reply. It is the app-visible check that
# `PackageInfo.writeToParcel`'s String8 arrays are encoded as String8.
build_apk pms-parcel

# keystore-probe is the K5b AndroidKeyStore fixture: fidelity assertions plus
# the cross-session reach probe. One launcher activity; the mode comes from
# ACOS_ART_KEYPROBE_MODE (see results/compat/K5.md).
build_apk keystore-probe

# SPIKE-WV (ADR-0031 gate, research/spike-wv/): webview-probe creates a real
# android.webkit.WebView and reports the exact outcome (uid, hasSystemFeature,
# provider discovery, load, JS) into its UI tree and logcat tag WVProbe.
build_apk webview-probe

# insets-probe is the W7 window-model fixture: an edge-to-edge Activity that
# prints the frame and the reported system-bar insets. It is the only fixture
# built at targetSdk 35, the Android 15 edge-to-edge default.
APP_TARGET_SDK=35 build_apk insets-probe

# --- optional device smoke test ---------------------------------------------
# Opt-in only (2026-11-12): a physical phone may be attached (the owner's own
# device). Never install anything unless the caller names the target explicitly:
#   ACOS_DEVICE_SMOKE=1 ANDROID_SERIAL=<emulator-serial> tools/build-apps.sh
if [[ "${ACOS_DEVICE_SMOKE:-0}" == "1" && -n "${ANDROID_SERIAL:-}" ]] \
    && command -v adb >/dev/null 2>&1 \
    && adb -s "$ANDROID_SERIAL" get-state 2>/dev/null | grep -q device; then
    echo "build-apps: ACOS_DEVICE_SMOKE=1, installing smoke test on $ANDROID_SERIAL"
    for apk in hello-views hello-jni counter todo-list multi-activity form-validation; do
        adb -s "$ANDROID_SERIAL" install -r -g "$BUILD_DIR/$apk.apk"
    done
    echo "build-apps: device-verified (installed)"
else
    echo "build-apps: not device-verified (device smoke is opt-in: ACOS_DEVICE_SMOKE=1 ANDROID_SERIAL=...)"
fi

echo "build-apps: done -> $BUILD_DIR/*.apk"
