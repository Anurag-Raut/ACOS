#!/usr/bin/env bash
#
# Build the S8 hostile-probe fixture APK with the same gradle-free toolchain as
# tools/build-apps.sh (aapt2 + javac + d8 + zipalign + apksigner). This fixture
# is our own code: it forges ACOSPROBE/ACOSLAUNCH lines and draws a red surface
# that does not match its claim. It is used only by the S8 hostile tests.
#
# Output: fixtures/build/apps/hostile-probe.apk (signed, zipaligned).
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
APP_DIR="$ROOT/fixtures/apps/hostile-probe"
BUILD_DIR="$ROOT/fixtures/build/apps"
WORK="$BUILD_DIR/hostile-probe"
OUT="$BUILD_DIR/hostile-probe.apk"
KEYSTORE="$BUILD_DIR/debug.keystore"
KEY_ALIAS="androiddebugkey"
KEY_PASS="android"

die() { echo "build-hostile-probe: error: $*" >&2; exit 1; }

command -v javac >/dev/null 2>&1 || die "javac not found on PATH"
for t in "$AAPT2" "$D8" "$ZIPALIGN" "$APKSIGNER"; do
    [ -x "$t" ] || die "missing tool: $t (set ANDROID_HOME / BUILD_TOOLS_VERSION)"
done
[ -f "$ANDROID_JAR" ] || die "android.jar not found: $ANDROID_JAR"
[ -f "$ROOT/tools/apk_append.py" ] || die "missing tools/apk_append.py"

mkdir -p "$BUILD_DIR"
if [ ! -f "$KEYSTORE" ]; then
    echo "build-hostile-probe: generating throwaway debug keystore"
    keytool -genkeypair \
        -keystore "$KEYSTORE" \
        -storepass "$KEY_PASS" -keypass "$KEY_PASS" \
        -alias "$KEY_ALIAS" \
        -dname "CN=Android Debug,O=Android,C=US" \
        -keyalg RSA -keysize 2048 -validity 10000 \
        -deststoretype pkcs12 >/dev/null 2>&1 \
        || die "keytool failed to create $KEYSTORE"
fi

rm -rf "$WORK"
mkdir -p "$WORK/compiled" "$WORK/gen" "$WORK/classes" "$WORK/dex"

echo "build-hostile-probe: compiling resources"
"$AAPT2" compile --dir "$APP_DIR/res" -o "$WORK/compiled.zip"
echo "build-hostile-probe: linking resources + manifest"
"$AAPT2" link \
    -o "$WORK/base.apk" \
    -I "$ANDROID_JAR" \
    --manifest "$APP_DIR/AndroidManifest.xml" \
    -R "$WORK/compiled.zip" \
    --java "$WORK/gen" \
    --min-sdk-version 26 \
    --target-sdk-version 34 \
    --auto-add-overlay
echo "build-hostile-probe: compiling java"
javac --release 11 -encoding UTF-8 -cp "$ANDROID_JAR" -d "$WORK/classes" \
    $(find "$APP_DIR/src" "$WORK/gen" -name '*.java')
echo "build-hostile-probe: dexing"
"$D8" --lib "$ANDROID_JAR" --min-api 26 --output "$WORK/dex" \
    $(find "$WORK/classes" -name '*.class')
python3 "$ROOT/tools/apk_append.py" "$WORK/base.apk" "$WORK/dex/classes.dex"

echo "build-hostile-probe: aligning"
rm -f "$OUT" "$WORK/aligned.apk"
"$ZIPALIGN" -f 4 "$WORK/base.apk" "$WORK/aligned.apk"
echo "build-hostile-probe: signing"
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
"$APKSIGNER" verify "$OUT" >/dev/null
echo "build-hostile-probe: -> $OUT ($(stat -c '%s' "$OUT") bytes)"
