#!/usr/bin/env bash
#
# Build a *real-widget* Flutter framework app as a release/AOT x86_64 APK,
# without Gradle, using a locally downloaded Flutter SDK.
#
# This is the W189-A fixture builder. Unlike tools/build/build-flutter-hello.sh (a
# raw `dart:ui` probe with no `package:flutter`), this compiles a full
# Material app against the SDK's `packages/flutter` and the pub dependency
# closure, AOT-compiles it with the pinned engine's `gen_snapshot`, and packages
# the release embedding + assets with aapt2/d8/apksigner.
#
# The Flutter SDK location is supplied via the environment (never committed):
#
#   FLUTTER_SDK=/path/to/flutter tools/build/build-flutter-app.sh \
#       fixtures/apps/flutter-counter fixtures/build/apps/acos-counter.apk
#
# Optional:
#   ACOS_ANDROIDX_JAR   path to a prebuilt AndroidX closure (defaults to
#                       .cache/flutter-1/androidx.jar; generated on demand by
#                       tools/build/flutter_androidx.py)
#   ANDROID_HOME        Android SDK root (default /home/Qui/Android/Sdk)
#   BUILD_TOOLS_VERSION (default 35.0.0)
#   APP_ABI             x86_64 (default) | arm64
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

APP_DIR="${1:-$ROOT/fixtures/apps/flutter-counter}"
OUT="${2:-$ROOT/fixtures/build/apps/acos-counter.apk}"
APP_ABI="${APP_ABI:-x86_64}"

die() { echo "build-flutter-app: error: $*" >&2; exit 1; }
log() { echo "build-flutter-app: $*"; }

[ -n "${FLUTTER_SDK:-}" ] || die "FLUTTER_SDK is not set (path to a Flutter SDK checkout)"
[ -x "$FLUTTER_SDK/bin/flutter" ] || die "no flutter tool at $FLUTTER_SDK/bin/flutter"
[ -f "$APP_DIR/pubspec.yaml" ] || die "no pubspec.yaml under $APP_DIR"
[ -f "$APP_DIR/lib/main.dart" ] || die "no lib/main.dart under $APP_DIR"

case "$APP_ABI" in
    x86_64) ARCH="x64"; JNI_ABI="x86_64" ;;
    arm64)  ARCH="arm64"; JNI_ABI="arm64-v8a" ;;
    *) die "unknown APP_ABI: $APP_ABI" ;;
esac

SDK_HOME="${ANDROID_HOME:-/home/Qui/Android/Sdk}"
BT="$SDK_HOME/build-tools/${BUILD_TOOLS_VERSION:-35.0.0}"
ANDROID_JAR="$SDK_HOME/platforms/android-34/android.jar"
AAPT2="$BT/aapt2"; D8="$BT/d8"; ZIPALIGN="$BT/zipalign"; APKSIGNER="$BT/apksigner"

ENGINE_DIR="$FLUTTER_SDK/bin/cache/artifacts/engine"
PATCHED="$ENGINE_DIR/common/flutter_patched_sdk_product"
GEN="$ENGINE_DIR/android-$ARCH-release/linux-x64/gen_snapshot"
FLUTTER_JAR="$ENGINE_DIR/android-$ARCH-release/flutter.jar"
FRONTEND="$ENGINE_DIR/linux-x64/frontend_server_aot.dart.snapshot"
DART_AOT="$FLUTTER_SDK/bin/cache/dart-sdk/bin/dartaotruntime"
MATERIAL_FONTS="$FLUTTER_SDK/bin/cache/artifacts/material_fonts"

for f in "$PATCHED/platform_strong.dill" "$GEN" "$FLUTTER_JAR" "$FRONTEND" "$DART_AOT"; do
    [ -e "$f" ] || die "missing SDK artifact: $f"
done
for t in "$AAPT2" "$D8" "$ZIPALIGN" "$APKSIGNER"; do
    [ -x "$t" ] || die "missing Android build tool: $t (set ANDROID_HOME)"
done
[ -f "$ANDROID_JAR" ] || die "android.jar not found: $ANDROID_JAR"

OUT_DIR="$(mkdir -p "$(dirname "$OUT")" && cd "$(dirname "$OUT")" && pwd)"
OUT="$OUT_DIR/$(basename "$OUT")"
KEYSTORE="$OUT_DIR/debug.keystore"
KEY_ALIAS="androiddebugkey"; KEY_PASS="android"
WORK="$OUT_DIR/work-$(basename "$APP_DIR")-$APP_ABI"
rm -rf "$WORK"
mkdir -p "$WORK"/{compiled,gen,classes,dex,lib/"$JNI_ABI",assets/flutter_assets/fonts}
export JAVA_OPTS="${JAVA_OPTS:--Xmx2g}"

command -v javac >/dev/null || die "javac not found"
command -v keytool >/dev/null || die "keytool not found"

# --- 1. pub closure ----------------------------------------------------------
PKG_CONFIG="$APP_DIR/.dart_tool/package_config.json"
if [ ! -f "$PKG_CONFIG" ]; then
    log "resolving pub dependencies (flutter pub get)"
    (cd "$APP_DIR" && "$FLUTTER_SDK/bin/flutter" pub get) || die "flutter pub get failed"
fi
[ -f "$PKG_CONFIG" ] || die "package_config.json not produced"

# --- 2. Dart kernel (package:flutter framework + app) ------------------------
log "[$APP_ABI] compiling Dart kernel (package:flutter)"
"$DART_AOT" "$FRONTEND" \
    --sdk-root "$PATCHED" \
    --platform "$PATCHED/platform_strong.dill" \
    --target=flutter --aot --tfa --link-platform \
    --packages "$PKG_CONFIG" \
    --output-dill "$WORK/app.dill" \
    "$APP_DIR/lib/main.dart" 2>&1 | tee "$WORK/kernel.log"
[ -f "$WORK/app.dill" ] || die "Dart kernel not produced"
log "[$APP_ABI] app.dill: $(stat -c '%s' "$WORK/app.dill") bytes"

# --- 3. AOT snapshot -> libapp.so -------------------------------------------
log "[$APP_ABI] AOT compiling libapp.so (release)"
"$GEN" --snapshot_kind=app-aot-elf --elf="$WORK/lib/$JNI_ABI/libapp.so" --strip \
    "$WORK/app.dill" 2>&1 | tee "$WORK/gen_snapshot.log"
[ -f "$WORK/lib/$JNI_ABI/libapp.so" ] || die "libapp.so not produced"

# --- 4. Engine libflutter.so from flutter.jar --------------------------------
log "[$APP_ABI] extracting libflutter.so from flutter.jar"
unzip -o -j -q "$FLUTTER_JAR" "lib/$JNI_ABI/libflutter.so" -d "$WORK/lib/$JNI_ABI" \
    || die "libflutter.so not found in $FLUTTER_JAR"

# --- 5. flutter_assets (Material icons, shaders, manifests) ------------------
# Use the SDK's own bundle builder: it emits the compiled GLSL shaders
# (`shaders/ink_sparkle.frag`, `stretch_effect.frag`), `NativeAssetsManifest.json`,
# the real `AssetManifest.bin` and `NOTICES.Z`. Hand-crafting these (W189-A's
# first cut) left the Material ripple shader out, so the first tap raised
# `Asset 'shaders/ink_sparkle.frag' not found` in the framework.
log "[$APP_ABI] building flutter_assets (flutter build bundle)"
(cd "$APP_DIR" && "$FLUTTER_SDK/bin/flutter" build bundle --release \
    --target-platform "android-$ARCH") || die "flutter build bundle failed"
BUNDLE="$APP_DIR/build/flutter_assets"
[ -d "$BUNDLE" ] || die "flutter build bundle produced no $BUNDLE"
cp -a "$BUNDLE"/. "$WORK/assets/flutter_assets"/
rm -f "$WORK/assets/flutter_assets/.last_build_id"
if [ -f "$MATERIAL_FONTS/MaterialIcons-Regular.otf" ] && \
   [ ! -f "$WORK/assets/flutter_assets/fonts/MaterialIcons-Regular.otf" ]; then
    cp "$MATERIAL_FONTS/MaterialIcons-Regular.otf" \
        "$WORK/assets/flutter_assets/fonts/MaterialIcons-Regular.otf"
fi

# --- 6. AndroidX closure -----------------------------------------------------
ANDROIDX="${ACOS_ANDROIDX_JAR:-$ROOT/.cache/flutter-1/androidx.jar}"
if [ ! -f "$ANDROIDX" ]; then
    log "generating AndroidX closure"
    python3 "$ROOT/tools/build/flutter_androidx.py" "$ANDROIDX" \
        --gradle-cache "${ANDROIDX_GRADLE_CACHE:-$HOME/.gradle/caches/modules-2/files-2.1}" \
        || die "AndroidX resolution failed"
fi

# --- 7. Resources + Java -----------------------------------------------------
log "[$APP_ABI] compiling resources"
"$AAPT2" compile --dir "$APP_DIR/res" -o "$WORK/compiled.zip"
"$AAPT2" link \
    -o "$WORK/base.apk" -I "$ANDROID_JAR" \
    --manifest "$APP_DIR/AndroidManifest.xml" \
    -R "$WORK/compiled.zip" --java "$WORK/gen" \
    --min-sdk-version 26 --target-sdk-version 34 --auto-add-overlay

log "[$APP_ABI] compiling java"
# shellcheck disable=SC2046
javac --release 11 -encoding UTF-8 \
    -cp "$ANDROID_JAR:$FLUTTER_JAR:$ANDROIDX" \
    -d "$WORK/classes" \
    $(find "$APP_DIR/src" "$WORK/gen" -name '*.java')
(cd "$WORK/classes" && jar cf "$WORK/app-classes.jar" .)

# --- 8. DEX ------------------------------------------------------------------
log "[$APP_ABI] dexing (app + flutter embedding + androidx)"
"$D8" --lib "$ANDROID_JAR" --min-api 26 --output "$WORK/dex" \
    "$WORK/app-classes.jar" "$FLUTTER_JAR" "$ANDROIDX" 2>"$WORK/d8.log" || {
        tail -60 "$WORK/d8.log" >&2; die "d8 failed"; }

# --- 9. Assemble + sign ------------------------------------------------------
log "[$APP_ABI] assembling APK"
python3 "$ROOT/tools/build/apk_append.py" "$WORK/base.apk" \
    "$WORK/dex:." "$WORK/lib:lib" "$WORK/assets:assets"

if [ ! -f "$KEYSTORE" ]; then
    log "generating throwaway debug keystore"
    keytool -genkeypair -keystore "$KEYSTORE" \
        -storepass "$KEY_PASS" -keypass "$KEY_PASS" -alias "$KEY_ALIAS" \
        -dname "CN=Android Debug,O=Android,C=US" \
        -keyalg RSA -keysize 2048 -validity 10000 \
        -deststoretype pkcs12 >/dev/null 2>&1 || die "keytool failed"
fi

rm -f "$OUT" "$WORK/aligned.apk"
"$ZIPALIGN" -f 4 "$WORK/base.apk" "$WORK/aligned.apk"
"$APKSIGNER" sign --ks "$KEYSTORE" \
    --ks-pass "pass:$KEY_PASS" --key-pass "pass:$KEY_PASS" \
    --ks-key-alias "$KEY_ALIAS" \
    --v1-signing-enabled true --v2-signing-enabled true --v3-signing-enabled true \
    --out "$OUT" "$WORK/aligned.apk"
"$APKSIGNER" verify --verbose "$OUT" >/dev/null

log "[$APP_ABI] native libs + assets in APK:"
unzip -l "$OUT" | grep -E "lib/.*\.so|assets/flutter_assets|classes.*\.dex" | awk '{print "    "$1" "$4}'
log "[$APP_ABI] -> $OUT ($(stat -c '%s' "$OUT") bytes)"
