#!/usr/bin/env bash
#
# Build the ACOS WebView provider APK (`com.android.webview`, ADR-0031).
#
# This is the one implementation of "how the provider APK is made". Two callers:
#
#   * `tools/build/build-apps.sh`                -> fixtures/build/apps/webview-provider.apk
#                                            (the developer/fixture build)
#   * `tools/fetch/extract-bootclasspath.sh`     -> <image>/system-apps/com.android.webview.apk
#                                            (the image build: a fresh install then needs
#                                             no separate step, and
#                                             tools/release/install.sh carries it)
#
# The provider is not a normal app: the framework's
# `WebViewFactoryProvider`/`WebViewProvider`/`WebViewDelegate`/`WebView$PrivateAccess`
# are hidden, so the sources compile against generated compile-time stubs
# (`tools/build/gen-webview-provider.py`, from the API-34 image's own dex) instead of
# against `android.jar` alone. The stubs are **not** dexed: the image's boot
# classpath supplies those classes at runtime.
#
# Usage:
#   tools/build/build-webview-provider.sh [--out PATH] [--work DIR]
#
# Toolchain: aapt2 + javac + d8 + zipalign + apksigner from the Android SDK
# (ANDROID_HOME, BUILD_TOOLS_VERSION) and the API-34 android.jar.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SDK="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-/home/Qui/Android/Sdk}}"
BUILD_TOOLS_VERSION="${BUILD_TOOLS_VERSION:-35.0.0}"
BT="$SDK/build-tools/$BUILD_TOOLS_VERSION"
ANDROID_JAR="$SDK/platforms/android-34/android.jar"
AAPT2="$BT/aapt2"
D8="$BT/d8"
ZIPALIGN="$BT/zipalign"
APKSIGNER="$BT/apksigner"

APP_DIR="$ROOT/fixtures/apps/webview-provider"
APPEND_PY="$ROOT/tools/build/apk_append.py"

OUT="$ROOT/fixtures/build/apps/webview-provider.apk"
WORK=""

log() { printf '[build-webview-provider] %s\n' "$*"; }
die() {
    printf '[build-webview-provider] error: %s\n' "$*" >&2
    exit 1
}

ORIGINAL_ARGS=("$@")
while [ $# -gt 0 ]; do
    case "$1" in
        --out) OUT=$2; shift 2 ;;
        --out=*) OUT=${1#*=}; shift ;;
        --work) WORK=$2; shift 2 ;;
        --work=*) WORK=${1#*=}; shift ;;
        -h|--help)
            sed -n '2,30p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *) die "unknown argument: $1 (try --help)" ;;
    esac
done

# Run the whole build under the host memory/time guard (ADR-0018).
if [ -z "${ACOS_GUARDED:-}" ]; then
    exec "$ROOT/tools/guard/guarded.sh" --mem 6G --time 1200 -- \
        env ACOS_GUARDED=1 "$0" "${ORIGINAL_ARGS[@]}"
fi

command -v javac >/dev/null 2>&1 || die "javac not found on PATH (need OpenJDK 17)"
command -v python3 >/dev/null 2>&1 || die "python3 not found on PATH"
for t in "$AAPT2" "$D8" "$ZIPALIGN" "$APKSIGNER"; do
    [ -x "$t" ] || die "missing tool: $t (set ANDROID_HOME / BUILD_TOOLS_VERSION)"
done
[ -f "$ANDROID_JAR" ] || die "android.jar not found: $ANDROID_JAR"
[ -f "$APPEND_PY" ] || die "helper not found: $APPEND_PY"
[ -d "$APP_DIR/hidden-stubs" ] || die "provider stubs directory not found: $APP_DIR/hidden-stubs"
[ -d "$APP_DIR/src" ] || die "provider sources not found: $APP_DIR/src"

if [ -z "$WORK" ]; then
    WORK="$(dirname -- "$OUT")/.work-webview-provider"
fi
rm -rf "$WORK"
mkdir -p "$WORK/stub-classes" "$WORK/classes" "$WORK/dex" "$(dirname -- "$OUT")"

# The keystore: one per build tree, the same debug key build-apps.sh uses, so a
# session's `PackageInfo` signature is stable across rebuilds.
KEYSTORE="${ACOS_DEBUG_KEYSTORE:-$(dirname -- "$OUT")/debug.keystore}"
KEY_ALIAS="androiddebugkey"
KEY_PASS="android"
if [ ! -f "$KEYSTORE" ]; then
    log "generating debug keystore $KEYSTORE"
    keytool -genkeypair -keystore "$KEYSTORE" -storepass "$KEY_PASS" \
        -keypass "$KEY_PASS" -alias "$KEY_ALIAS" -keyalg RSA -keysize 2048 \
        -validity 10000 -dname "CN=Android Debug,O=Android,C=US" >/dev/null 2>&1 ||
        die "keytool failed to create $KEYSTORE"
fi

log "generating hidden-API stubs + provider surface"
python3 "$ROOT/tools/build/gen-webview-provider.py"

log "compiling hidden-API stubs (compile-time only, never dexed)"
# shellcheck disable=SC2046
javac --release 11 -encoding UTF-8 -cp "$ANDROID_JAR" -d "$WORK/stub-classes" \
    $(find "$APP_DIR/hidden-stubs" -name '*.java')

log "linking manifest"
"$AAPT2" link \
    -o "$WORK/base.apk" \
    -I "$ANDROID_JAR" \
    --manifest "$APP_DIR/AndroidManifest.xml" \
    --min-sdk-version 26 \
    --target-sdk-version 34

log "compiling java (stubs first: they own the hidden names)"
# shellcheck disable=SC2046
javac --release 11 -encoding UTF-8 \
    -cp "$WORK/stub-classes:$ANDROID_JAR" -d "$WORK/classes" \
    $(find "$APP_DIR/src" -name '*.java')

log "dexing (provider classes only, never the stubs)"
# shellcheck disable=SC2046
"$D8" --lib "$ANDROID_JAR" --min-api 26 --output "$WORK/dex" \
    $(find "$WORK/classes" -name '*.class')

python3 "$APPEND_PY" "$WORK/base.apk" "$WORK/dex/classes.dex"

log "aligning"
rm -f "$OUT" "$WORK/aligned.apk"
"$ZIPALIGN" -f 4 "$WORK/base.apk" "$WORK/aligned.apk"

log "signing"
"$APKSIGNER" sign \
    --ks "$KEYSTORE" \
    --ks-pass "pass:$KEY_PASS" \
    --key-pass "pass:$KEY_PASS" \
    --ks-key-alias "$KEY_ALIAS" \
    --v1-signing-enabled true \
    --v2-signing-enabled true \
    --v3-signing-enabled true \
    --out "$OUT" \
    "$WORK/aligned.apk"

log "verifying"
"$APKSIGNER" verify --verbose "$OUT" >/dev/null
"$AAPT2" dump badging "$OUT" | sed 's/^/    /' | head -n 8
log "-> $OUT ($(stat -c '%s' "$OUT") bytes)"

rm -rf "$WORK"
