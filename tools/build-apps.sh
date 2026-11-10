#!/usr/bin/env bash
#
# Build the ACOS real-APK fixtures without Gradle.
#
# Toolchain: aapt2 + javac + d8 + zipalign + apksigner from the Android SDK;
# NDK clang for hello-jni. A throwaway debug keystore is generated under
# fixtures/build/apps/ (never committed).
#
# Outputs: fixtures/build/apps/hello-views.apk, fixtures/build/apps/hello-jni.apk
# Both are signed + zipaligned and verified with `apksigner verify` and
# `aapt2 dump badging`. Installation on a device is attempted only if one is
# already attached; no emulator is ever started here.
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

    rm -rf "$work"
    mkdir -p "$work/compiled" "$work/gen" "$work/classes" "$work/dex"

    echo "build-apps: [$name] compiling resources"
    "$AAPT2" compile --dir "$app_dir/res" -o "$work/compiled.zip"

    echo "build-apps: [$name] linking resources + manifest"
    "$AAPT2" link \
        -o "$work/base.apk" \
        -I "$ANDROID_JAR" \
        --manifest "$app_dir/AndroidManifest.xml" \
        -R "$work/compiled.zip" \
        --java "$work/gen" \
        --min-sdk-version 26 \
        --target-sdk-version 34 \
        --auto-add-overlay

    echo "build-apps: [$name] compiling java"
    # shellcheck disable=SC2046
    javac --release 11 -encoding UTF-8 -cp "$ANDROID_JAR" -d "$work/classes" \
        $(find "$app_dir/src" "$work/gen" -name '*.java')

    echo "build-apps: [$name] dexing"
    # shellcheck disable=SC2046
    "$D8" --lib "$ANDROID_JAR" --min-api 26 --output "$work/dex" \
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

# --- optional device smoke test ---------------------------------------------
if command -v adb >/dev/null 2>&1 && adb devices 2>/dev/null | awk 'NR>1 && $2=="device"' | grep -q .; then
    echo "build-apps: device detected, installing smoke test"
    adb install -r -g "$BUILD_DIR/hello-views.apk"
    adb install -r -g "$BUILD_DIR/hello-jni.apk"
    echo "build-apps: device-verified (installed)"
else
    echo "build-apps: not device-verified (no attached device/emulator)"
fi

echo "build-apps: done -> $BUILD_DIR/*.apk"
