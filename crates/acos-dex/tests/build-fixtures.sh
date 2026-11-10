#!/usr/bin/env bash
# Compile the Java fixtures in tests/java/ and produce checked-in DEX files.
#
# Requirements: `javac` (JDK 11+) and an Android SDK build-tools `d8` plus a
# platform android.jar. Override with env vars if your SDK lives elsewhere:
#
#   ANDROID_SDK=/path/to/sdk BUILD_TOOLS=35.0.0 ./build-fixtures.sh
#
# Outputs (checked into tests/data/):
#   fixtures.dex        — all classes, --min-api 26 (invoke-custom lambdas)
#   fixtures_api19.dex  — all classes, --min-api 19 (desugared lambdas)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
JAVA_DIR="$SCRIPT_DIR/java"
DATA_DIR="$SCRIPT_DIR/data"

ANDROID_SDK="${ANDROID_SDK:-$HOME/Android/Sdk}"
BUILD_TOOLS="${BUILD_TOOLS:-35.0.0}"
D8="${D8:-$ANDROID_SDK/build-tools/$BUILD_TOOLS/d8}"
ANDROID_JAR="${ANDROID_JAR:-$ANDROID_SDK/platforms/android-34/android.jar}"

if ! command -v javac >/dev/null 2>&1; then
    echo "error: javac not found on PATH" >&2
    exit 1
fi
if [ ! -x "$D8" ]; then
    echo "error: d8 not found/executable at $D8" >&2
    exit 1
fi
if [ ! -f "$ANDROID_JAR" ]; then
    echo "error: android.jar not found at $ANDROID_JAR" >&2
    exit 1
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

mkdir -p "$DATA_DIR"

echo "== javac =="
javac --release 11 -encoding UTF-8 -d "$TMP" "$JAVA_DIR"/*.java

# Collect class files (there are nested/auxiliary classes).
mapfile -t CLASSES < <(find "$TMP" -name '*.class' | sort)
echo "compiled ${#CLASSES[@]} class files"

build_dex() {
    local min_api="$1"
    local out_name="$2"
    local extra="${3:-}"
    local out_dir="$TMP/out-$min_api${extra:+-nodsg}"
    mkdir -p "$out_dir"
    # shell expand the extra flag if present
    # shellcheck disable=SC2086
    echo "== d8 --min-api $min_api $extra =="
    "$D8" --lib "$ANDROID_JAR" --min-api "$min_api" $extra --output "$out_dir" "${CLASSES[@]}"
    cp "$out_dir/classes.dex" "$DATA_DIR/$out_name"
    echo "wrote $DATA_DIR/$out_name ($(stat -c%s "$DATA_DIR/$out_name") bytes)"
}

build_dex 26 fixtures.dex
build_dex 19 fixtures_api19.dex
# --no-desugaring keeps lambdas as invoke-custom call sites (d8 otherwise
# desugars them even at --min-api 26).
build_dex 26 fixtures_invokecustom.dex --no-desugaring

echo "done"
