#!/usr/bin/env bash
#
# Build the S8/W44 hostile-probe fixture APKs with the same gradle-free
# toolchain as tools/build/build-apps.sh (aapt2 + javac + d8 + zipalign + apksigner).
# This fixture is our own code: it forges ACOSPROBE/ACOSLAUNCH lines, forged
# window forests, hostile binder parcels and draws a red surface that does not
# match its claims. It is used only by the hostile tests.
#
# W44: the ART engine always launches the manifest's MAIN/LAUNCHER activity, so
# each hostile activity gets its own signed variant (same code, one launcher):
#   hostile-probe.apk   .MainActivity    (forged verdict + two-window forest)
#   hostile-forest.apk  .ForestActivity  (truncated:false with a missing node)
#   hostile-spam.apk    .SpamActivity    (>8 MiB probe line + forged spam)
#   hostile-parcel.apk  .ParcelActivity  (hostile setTransactionState/sizeCompat)
#
# Output: fixtures/build/apps/hostile-*.apk (signed, zipaligned).
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
KEYSTORE="$BUILD_DIR/debug.keystore"
KEY_ALIAS="androiddebugkey"
KEY_PASS="android"
VARIANTS="main:MainActivity:hostile-probe forest:ForestActivity:hostile-forest spam:SpamActivity:hostile-spam parcel:ParcelActivity:hostile-parcel"

die() { echo "build-hostile-probe: error: $*" >&2; exit 1; }

command -v javac >/dev/null 2>&1 || die "javac not found on PATH"
for t in "$AAPT2" "$D8" "$ZIPALIGN" "$APKSIGNER"; do
    [ -x "$t" ] || die "missing tool: $t (set ANDROID_HOME / BUILD_TOOLS_VERSION)"
done
[ -f "$ANDROID_JAR" ] || die "android.jar not found: $ANDROID_JAR"
[ -f "$ROOT/tools/build/apk_append.py" ] || die "missing tools/build/apk_append.py"
[ -f "$APP_DIR/AndroidManifest.template.xml" ] || die "missing manifest template"

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
mkdir -p "$WORK/compiled" "$WORK/gen" "$WORK/classes" "$WORK/dex" \
    "$WORK/manifests" "$WORK/linked"

echo "build-hostile-probe: compiling resources"
"$AAPT2" compile --dir "$APP_DIR/res" -o "$WORK/compiled.zip"

echo "build-hostile-probe: linking resources + manifests"
for variant in $VARIANTS; do
    short="${variant%%:*}"
    rest="${variant#*:}"
    launcher="${rest%%:*}"
    python3 - "$APP_DIR/AndroidManifest.template.xml" \
        "$WORK/manifests/$short.xml" "$launcher" <<'PY'
import sys

tpl, out, launcher = sys.argv[1:4]
activities = ["MainActivity", "ForestActivity", "SpamActivity", "ParcelActivity"]
block = []
for name in activities:
    if name == launcher:
        block.append(
            f'    <activity android:name=".{name}" android:exported="true">\n'
            '            <intent-filter>\n'
            '                <action android:name="android.intent.action.MAIN" />\n'
            '                <category android:name="android.intent.category.LAUNCHER" />\n'
            '            </intent-filter>\n'
            '        </activity>')
    else:
        block.append(f'    <activity android:name=".{name}" android:exported="true" />')
with open(tpl) as handle:
    text = handle.read()
with open(out, "w") as handle:
    handle.write(text.replace("@@ACTIVITIES@@", "\n".join(block)))
PY
    "$AAPT2" link \
        -o "$WORK/linked/$short.apk" \
        -I "$ANDROID_JAR" \
        --manifest "$WORK/manifests/$short.xml" \
        -R "$WORK/compiled.zip" \
        --java "$WORK/gen" \
        --min-sdk-version 26 \
        --target-sdk-version 34 \
        --auto-add-overlay
done

echo "build-hostile-probe: compiling java"
javac --release 11 -encoding UTF-8 -cp "$ANDROID_JAR" -d "$WORK/classes" \
    $(find "$APP_DIR/src" "$WORK/gen" -name '*.java')

echo "build-hostile-probe: dexing"
"$D8" --lib "$ANDROID_JAR" --min-api 26 --output "$WORK/dex" \
    $(find "$WORK/classes" -name '*.class')

for variant in $VARIANTS; do
    short="${variant%%:*}"
    rest="${variant#*:}"
    out_name="${rest##*:}"
    out="$BUILD_DIR/$out_name.apk"
    python3 "$ROOT/tools/build/apk_append.py" "$WORK/linked/$short.apk" "$WORK/dex/classes.dex"
    echo "build-hostile-probe: aligning $out_name"
    rm -f "$out" "$WORK/aligned-$short.apk"
    "$ZIPALIGN" -f 4 "$WORK/linked/$short.apk" "$WORK/aligned-$short.apk"
    echo "build-hostile-probe: signing $out_name"
    "$APKSIGNER" sign \
        --ks "$KEYSTORE" \
        --ks-pass "pass:$KEY_PASS" \
        --key-pass "pass:$KEY_PASS" \
        --ks-key-alias "$KEY_ALIAS" \
        --v1-signing-enabled true \
        --v2-signing-enabled true \
        --v3-signing-enabled true \
        --out "$out" \
        "$WORK/aligned-$short.apk"
    "$APKSIGNER" verify "$out" >/dev/null
    echo "build-hostile-probe: -> $out ($(stat -c '%s' "$out") bytes)"
done
