#!/usr/bin/env bash
#
# Build the four W46-A dogfood fixture apps with the same Gradle-free toolchain
# as tools/build-apps.sh (aapt2 + javac + d8 + zipalign + apksigner).
#
# This is deliberately a *separate* script rather than an edit to
# tools/build-apps.sh so the shared fixture list stays owned by its own worker.
#
# Outputs (gitignored):
#   fixtures/build/apps/sqlite-crud.apk
#   fixtures/build/apps/canvas-shapes.apk
#   fixtures/build/apps/service-provider.apk
#   fixtures/build/apps/two-activity.apk
#
# Usage:
#   tools/build-dogfood-apps.sh [name ...]     # default: all four
set -euo pipefail

SDK="${ANDROID_HOME:-/home/Qui/Android/Sdk}"
BUILD_TOOLS_VERSION="${BUILD_TOOLS_VERSION:-35.0.0}"
BT="$SDK/build-tools/$BUILD_TOOLS_VERSION"
ANDROID_JAR="$SDK/platforms/android-34/android.jar"
AAPT2="$BT/aapt2"
D8="$BT/d8"
ZIPALIGN="$BT/zipalign"
APKSIGNER="$BT/apksigner"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

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

ALL_APPS=(sqlite-crud canvas-shapes service-provider two-activity)

die() { echo "build-dogfood-apps: error: $*" >&2; exit 1; }

command -v javac >/dev/null 2>&1 || die "javac not found on PATH"
command -v python3 >/dev/null 2>&1 || die "python3 not found on PATH"
for t in "$AAPT2" "$D8" "$ZIPALIGN" "$APKSIGNER"; do
    [ -x "$t" ] || die "missing tool: $t (set ANDROID_HOME / BUILD_TOOLS_VERSION)"
done
[ -f "$ANDROID_JAR" ] || die "android.jar not found: $ANDROID_JAR"
[ -f "$APPEND_PY" ] || die "helper not found: $APPEND_PY"

mkdir -p "$BUILD_DIR"

if [ ! -f "$KEYSTORE" ]; then
    echo "build-dogfood-apps: generating throwaway debug keystore"
    keytool -genkeypair \
        -keystore "$KEYSTORE" \
        -storepass "$KEY_PASS" -keypass "$KEY_PASS" \
        -alias "$KEY_ALIAS" \
        -dname "CN=Android Debug,O=Android,C=US" \
        -keyalg RSA -keysize 2048 -validity 10000 \
        -deststoretype pkcs12 >/dev/null 2>&1 \
        || die "keytool failed to create $KEYSTORE"
fi

build_apk() {
    local name="$1"
    local app_dir="$APPS_DIR/$name"
    local work="$BUILD_DIR/$name"
    local out="$BUILD_DIR/$name.apk"

    [ -d "$app_dir" ] || die "no source at $app_dir"
    rm -rf "$work"
    mkdir -p "$work/compiled" "$work/gen" "$work/classes" "$work/dex"

    echo "build-dogfood-apps: [$name] compiling resources"
    "$AAPT2" compile --dir "$app_dir/res" -o "$work/compiled.zip"

    echo "build-dogfood-apps: [$name] linking resources + manifest"
    "$AAPT2" link \
        -o "$work/base.apk" \
        -I "$ANDROID_JAR" \
        --manifest "$app_dir/AndroidManifest.xml" \
        -R "$work/compiled.zip" \
        --java "$work/gen" \
        --min-sdk-version 26 \
        --target-sdk-version 34 \
        --auto-add-overlay

    echo "build-dogfood-apps: [$name] compiling java"
    # shellcheck disable=SC2046
    javac --release 11 -encoding UTF-8 -cp "$ANDROID_JAR" -d "$work/classes" \
        $(find "$app_dir/src" "$work/gen" -name '*.java')

    echo "build-dogfood-apps: [$name] dexing"
    # shellcheck disable=SC2046
    "$D8" --lib "$ANDROID_JAR" --min-api 26 --output "$work/dex" \
        $(find "$work/classes" -name '*.class')

    python3 "$APPEND_PY" "$work/base.apk" "$work/dex/classes.dex"

    echo "build-dogfood-apps: [$name] aligning"
    rm -f "$out" "$work/aligned.apk"
    "$ZIPALIGN" -f 4 "$work/base.apk" "$work/aligned.apk"

    echo "build-dogfood-apps: [$name] signing"
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

    echo "build-dogfood-apps: [$name] verifying"
    "$APKSIGNER" verify --verbose "$out" >/dev/null
    "$AAPT2" dump badging "$out" | sed 's/^/    /' | head -n 6
    echo "build-dogfood-apps: [$name] -> $out ($(stat -c '%s' "$out") bytes)"
}

if [ "$#" -gt 0 ]; then
    for app in "$@"; do build_apk "$app"; done
else
    for app in "${ALL_APPS[@]}"; do build_apk "$app"; done
fi

echo "build-dogfood-apps: done -> $BUILD_DIR/{sqlite-crud,canvas-shapes,service-provider,two-activity}.apk"
