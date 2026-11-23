#!/usr/bin/env bash
#
# Build the W93-A Flutter harness dex:
#
#   crates/acos-jni/tests/data/flutter-probe.dex
#       the Java entry (`FlutterProbe`) that loads `libflutter.so` through
#       `System.loadLibrary` and drives `FlutterJNI.init`/`attachToNative`/
#       `runBundleAndSnapshotFromLibrary` from real Java inside the ACOS VM.
#
# Everything comes from the populated `.cache/flutter-1` artifacts and the
# local Android SDK; nothing is downloaded.
#
# Usage: tools/probes/flutter-hello-probe.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SDK="${ANDROID_HOME:-/home/Qui/Android/Sdk}"
BT="${BUILD_TOOLS_VERSION:-35.0.0}"
D8="$SDK/build-tools/$BT/d8"
ANDROID_JAR="$SDK/platforms/android-34/android.jar"
FLUTTER_JAR="$ROOT/.cache/flutter-1/engine/artifacts/flutter.jar"
ANDROIDX_JAR="$ROOT/.cache/flutter-1/androidx.jar"
# Compile-time stubs for @hide framework APIs (`AssetManager()` /
# `addAssetPath(String)`). The stub classes are a javac classpath entry only and
# are never packaged: d8 resolves `android.content.res.AssetManager` from
# `android.jar`, so the DEX keeps the real framework reference.
STUBS="$ROOT/crates/acos-vm/tests/java/stubs"
WORK="$ROOT/target/tmp/flutter-hello-probe"

die() { echo "flutter-hello-probe: error: $*" >&2; exit 1; }

[ -x "$D8" ] || die "d8 not found: $D8"
[ -f "$ANDROID_JAR" ] || die "android.jar not found: $ANDROID_JAR"
[ -f "$FLUTTER_JAR" ] || die "flutter.jar not found: $FLUTTER_JAR (run tools/build/build-flutter-hello.sh)"
[ -f "$ANDROIDX_JAR" ] || die "androidx.jar not found: $ANDROIDX_JAR"

rm -rf "$WORK"
mkdir -p "$WORK/classes" "$WORK/dex"

CP="$STUBS:$FLUTTER_JAR:$ANDROID_JAR:$ANDROIDX_JAR"
echo "flutter-hello-probe: javac"
javac --release 11 -encoding UTF-8 -sourcepath "$STUBS" -cp "$CP" -d "$WORK/classes" \
    "$ROOT/crates/acos-jni/tests/java/dev/acos/fixtures/flutterhello/FlutterProbe.java"

echo "flutter-hello-probe: d8"
# Every class the probe compiled (anonymous delegates become nest mates).
mapfile -t PROBE_CLASSES < <(find "$WORK/classes/dev" -name '*.class' | sort)
"$D8" --min-api 26 --lib "$ANDROID_JAR" --output "$WORK/dex" \
    "${PROBE_CLASSES[@]}"
cp "$WORK/dex/classes.dex" "$ROOT/crates/acos-jni/tests/data/flutter-probe.dex"
echo "flutter-hello-probe: -> crates/acos-jni/tests/data/flutter-probe.dex"
