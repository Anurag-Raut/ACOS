#!/usr/bin/env bash
#
# Build the W88-A React Native legacy-bridge probe DEX:
#
#   crates/acos-jni/tests/data/rn-bridge-probe.dex
#       the Java harness entry (`BridgeProbe`) that drives the real RN
#       `CatalystInstanceImpl.initializeBridge` path inside the VM.
#
# Everything comes from the already-populated caches under `.cache/rn-0.81.4/`
# and the Gradle module cache; nothing is downloaded.
#
# Usage: tools/probes/rn-bridge-probe.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SDK="${ANDROID_HOME:-/home/Qui/Android/Sdk}"
BT="${BUILD_TOOLS_VERSION:-35.0.0}"
D8="$SDK/build-tools/$BT/d8"
ANDROID_JAR="$SDK/platforms/android-34/android.jar"
# Compile-time stubs for @hide framework APIs (`AssetManager()` /
# `addAssetPath(String)`, used by the AssetManager bundle loader). The stubs
# are a javac classpath entry only and are never packaged: d8 resolves
# `android.content.res.AssetManager` from `android.jar`.
STUBS="$ROOT/crates/acos-vm/tests/java/stubs"
CACHE_RN="$ROOT/.cache/rn-0.81.4"
WORK="$ROOT/target/tmp/rn-bridge-probe"
SRC="$ROOT/crates/acos-jni/tests/java/dev/acos/fixtures/rnhello/BridgeProbe.java"
OUT="$ROOT/crates/acos-jni/tests/data/rn-bridge-probe.dex"

die() { echo "rn-bridge-probe: error: $*" >&2; exit 1; }
log() { echo "rn-bridge-probe: $*"; }

[ -x "$D8" ] || die "d8 not found: $D8"
[ -f "$ANDROID_JAR" ] || die "android.jar not found: $ANDROID_JAR"
[ -f "$CACHE_RN/aar/react-android-0.81.4-release.aar" ] || die "react-android AAR missing"
[ -f "$CACHE_RN/aar/hermes-android-0.81.4-release.aar" ] || die "hermes-android AAR missing"
command -v javac >/dev/null 2>&1 || die "javac not found"

rm -rf "$WORK"
mkdir -p "$WORK/cp" "$WORK/classes" "$WORK/dex"

log "resolving dependency classpath"
ACOS_RN_AAR_DIR="$CACHE_RN/aar" python3 "$ROOT/tools/build/rn-hello-deps.py" \
    > "$WORK/deps.txt" 2>"$WORK/deps.log"
cat "$WORK/deps.log" >&2
[ -s "$WORK/deps.txt" ] || die "dependency resolution produced nothing"

idx=0
CP_ENTRIES=()
while IFS= read -r dep; do
    [ -f "$dep" ] || dep="$ROOT/$dep"
    [ -f "$dep" ] || die "dependency not found: $dep"
    case "$dep" in
        *.aar)
            d="$WORK/cp/$idx"
            mkdir -p "$d"
            (cd "$d" && unzip -o -q "$dep" classes.jar 2>/dev/null) || true
            if [ -f "$d/classes.jar" ]; then
                CP_ENTRIES+=("$d/classes.jar")
            fi
            ;;
        *.jar)
            CP_ENTRIES+=("$dep")
            ;;
    esac
    idx=$((idx + 1))
done < "$WORK/deps.txt"

# The fetched AARs from `.cache/rn-0.81.4/aar` may not appear in deps.txt if
# rn-hello-deps resolved them from the Gradle cache; always include them.
for aar in react-android hermes-android; do
    a="$CACHE_RN/aar/${aar}-0.81.4-release.aar"
    [ -f "$a" ] || continue
    d="$WORK/cp/fetch-$aar"
    mkdir -p "$d"
    (cd "$d" && unzip -o -q "$a" classes.jar)
    CP_ENTRIES+=("$d/classes.jar")
done

CP="$STUBS:$ANDROID_JAR:$(IFS=:; echo "${CP_ENTRIES[*]}")"
log "javac BridgeProbe (classpath: $(echo "${CP_ENTRIES[*]}" | wc -w) entries)"
javac --release 11 -encoding UTF-8 -sourcepath "$STUBS" -cp "$CP" -d "$WORK/classes" "$SRC"

log "d8 BridgeProbe"
mapfile -t CLASSES < <(find "$WORK/classes" -name '*.class' | sort)
"$D8" --lib "$ANDROID_JAR" --min-api 26 --output "$WORK/dex" "${CLASSES[@]}"
cp "$WORK/dex/classes.dex" "$OUT"
log "  -> $OUT ($(stat -c '%s' "$OUT") bytes)"
log "done"
