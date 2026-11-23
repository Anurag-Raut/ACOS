#!/usr/bin/env bash
#
# Build the V1 negative-fixture APKs from the existing fixture sources.
#
# These are *deliberately broken* variants of the checked-in fixtures, used by
# `docs/V1.md` §4 to prove the verifier cannot false-pass:
#
#   broken-wrong-text.apk      counter with `count` initialised to 5
#                              (the scenario expects "0" -> ASSERTION_FAILED)
#   broken-crash.apk           hello-views that throws in onCreate
#                              (launch surfaces the exception + stack)
#   broken-missing-symbol.apk  hello-jni that calls an unexported native method
#                              (the Rust loader reports the missing symbol)
#
# Sources are derived into fixtures/build/negative-src/<name> at build time;
# nothing is committed but this script. Same toolchain as tools/build/build-apps.sh.
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

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

if [ -z "${ACOS_GUARDED:-}" ]; then
    exec "$ROOT/tools/guard/guarded.sh" --mem 6G --time 1200 -- \
        env ACOS_GUARDED=1 "$0" "$@"
fi

APPS_DIR="$ROOT/fixtures/apps"
BUILD_DIR="$ROOT/fixtures/build/apps"
SRC_DIR="$ROOT/fixtures/build/negative-src"
APPEND_PY="$ROOT/tools/build/apk_append.py"
KEYSTORE="$BUILD_DIR/debug.keystore"
KEY_ALIAS="androiddebugkey"
KEY_PASS="android"

die() { echo "build-negative-apps: error: $*" >&2; exit 1; }

command -v javac >/dev/null 2>&1 || die "javac not found on PATH"
command -v python3 >/dev/null 2>&1 || die "python3 not found on PATH"
for t in "$AAPT2" "$D8" "$ZIPALIGN" "$APKSIGNER"; do
    [ -x "$t" ] || die "missing tool: $t (set ANDROID_HOME / BUILD_TOOLS_VERSION)"
done
[ -f "$ANDROID_JAR" ] || die "android.jar not found: $ANDROID_JAR"
[ -f "$APPEND_PY" ] || die "helper not found: $APPEND_PY"
[ -f "$KEYSTORE" ] || die "missing debug keystore; run tools/build/build-apps.sh first"

mkdir -p "$BUILD_DIR" "$SRC_DIR"

# build_apk <name> <app_dir> [<extra_dir>:<arcname> ...]
build_apk() {
    local name="$1"; shift
    local app_dir="$1"; shift
    local work="$BUILD_DIR/$name"
    local out="$BUILD_DIR/$name.apk"

    rm -rf "$work"
    mkdir -p "$work/compiled" "$work/gen" "$work/classes" "$work/dex"

    echo "build-negative-apps: [$name] compiling resources"
    "$AAPT2" compile --dir "$app_dir/res" -o "$work/compiled.zip"
    "$AAPT2" link \
        -o "$work/base.apk" \
        -I "$ANDROID_JAR" \
        --manifest "$app_dir/AndroidManifest.xml" \
        -R "$work/compiled.zip" \
        --java "$work/gen" \
        --min-sdk-version 26 \
        --target-sdk-version 34 \
        --auto-add-overlay

    echo "build-negative-apps: [$name] compiling java"
    # shellcheck disable=SC2046
    javac --release 11 -encoding UTF-8 -cp "$ANDROID_JAR" -d "$work/classes" \
        $(find "$app_dir/src" "$work/gen" -name '*.java')

    echo "build-negative-apps: [$name] dexing"
    # shellcheck disable=SC2046
    "$D8" --lib "$ANDROID_JAR" --min-api 26 --output "$work/dex" \
        $(find "$work/classes" -name '*.class')

    python3 "$APPEND_PY" "$work/base.apk" "$work/dex/classes.dex" "$@"

    echo "build-negative-apps: [$name] aligning + signing"
    rm -f "$out" "$work/aligned.apk"
    "$ZIPALIGN" -f 4 "$work/base.apk" "$work/aligned.apk"
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
    "$APKSIGNER" verify "$out" >/dev/null
    echo "build-negative-apps: [$name] -> $out ($(stat -c '%s' "$out") bytes)"
}

# --- wrong text: initial count is 5, scenario expects 0 ----------------------
rm -rf "$SRC_DIR/broken-wrong-text"
cp -a "$APPS_DIR/counter" "$SRC_DIR/broken-wrong-text"
python3 - "$SRC_DIR/broken-wrong-text/src/dev/acos/fixtures/counter/MainActivity.java" <<'PY'
import sys
path = sys.argv[1]
src = open(path).read()
assert "private int count = 0;" in src
src = src.replace("private int count = 0;", "private int count = 5; // BROKEN: scenario expects 0")
open(path, "w").write(src)
PY
build_apk broken-wrong-text "$SRC_DIR/broken-wrong-text"

# --- crash: throw in onCreate ----------------------------------------------
rm -rf "$SRC_DIR/broken-crash"
cp -a "$APPS_DIR/hello-views" "$SRC_DIR/broken-crash"
python3 - "$SRC_DIR/broken-crash/src/dev/acos/fixtures/helloviews/MainActivity.java" <<'PY'
import sys
path = sys.argv[1]
src = open(path).read()
needle = 'super.onCreate(savedInstanceState);\n        Log.i(TAG, "onCreate");'
assert needle in src
src = src.replace(
    needle,
    needle + '\n        if (true) { throw new IllegalStateException("crash in onCreate"); }',
)
open(path, "w").write(src)
PY
build_apk broken-crash "$SRC_DIR/broken-crash"

# --- missing symbol: call a native method the .so does not export -----------
rm -rf "$SRC_DIR/broken-missing-symbol"
cp -a "$APPS_DIR/hello-jni" "$SRC_DIR/broken-missing-symbol"
python3 - "$SRC_DIR/broken-missing-symbol/src/dev/acos/fixtures/hellojni/MainActivity.java" <<'PY'
import sys
path = sys.argv[1]
src = open(path).read()
needle = "    private native void throwFromJNI();"
assert needle in src
src = src.replace(
    needle,
    needle + "\n\n    /** BROKEN: no translation unit in libhellojni.so exports this. */\n"
    "    private native String missingSymbol();",
)
needle2 = 'Log.i(TAG, "onCreate");'
assert needle2 in src
src = src.replace(
    needle2,
    needle2 + '\n        sbMissing = missingSymbol(); // must fail with the exact symbol name',
    1,
)
# declare the field used above
src = src.replace(
    "    private String voidResult = \"\";",
    "    private String voidResult = \"\";\n    private String sbMissing = \"\";",
)
open(path, "w").write(src)
PY
JNI_WORK="$BUILD_DIR/broken-missing-symbol-jni"
rm -rf "$JNI_WORK"
mkdir -p "$JNI_WORK/x86_64" "$JNI_WORK/arm64-v8a"
"$NDK_BIN/x86_64-linux-android26-clang" -shared -fPIC -O2 \
    -o "$JNI_WORK/x86_64/libhellojni.so" "$APPS_DIR/hello-jni/jni/hello-jni.c"
"$NDK_BIN/aarch64-linux-android26-clang" -shared -fPIC -O2 \
    -o "$JNI_WORK/arm64-v8a/libhellojni.so" "$APPS_DIR/hello-jni/jni/hello-jni.c"
build_apk broken-missing-symbol "$SRC_DIR/broken-missing-symbol" "$JNI_WORK:lib"

echo "build-negative-apps: done -> $BUILD_DIR/broken-*.apk"
