#!/usr/bin/env bash
#
# Build the ACOS JNI native test fixture.
#
# The fixture is a normal NDK-compiled x86_64 Android shared object, loaded by
# `acos-native-loader` exactly like `fixtures/apps/hello-jni/jni/hello-jni.c`.
# It contains only test code owned by this repository; no third-party APK
# natives are executed here.
#
# Output: fixtures/build/jni/x86_64/libacosjni_slots.so
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
NDK="${ANDROID_NDK_HOME:-${ANDROID_HOME:-/home/Qui/Android/Sdk}/ndk/27.1.12297006}"
CLANG="$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin/x86_64-linux-android26-clang"
SRC="$ROOT/fixtures/jni/acos_jni_slots.c"
OUT="$ROOT/fixtures/build/jni/x86_64"

if [ ! -x "$CLANG" ]; then
    echo "build-jni-fixture: NDK clang not found: $CLANG" >&2
    exit 1
fi

mkdir -p "$OUT"
"$CLANG" -shared -fPIC -O2 -Wall -o "$OUT/libacosjni_slots.so" "$SRC"
echo "build-jni-fixture: wrote $OUT/libacosjni_slots.so"

# NDK platform surface (libandroid / libjnigraphics / EGL) fixture. Loaded by
# `acos-native-loader`'s `tests/ndk.rs`; checked in under that crate's tests/data.
CLANG29="$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin/x86_64-linux-android29-clang"
"$CLANG29" -shared -fPIC -O2 -Wall -o \
    "$ROOT/crates/acos-native-loader/tests/data/libacosndk.so" \
    "$ROOT/fixtures/jni/acos_ndk_surface.c" -llog -landroid -ljnigraphics -lEGL -lGLESv2
echo "build-jni-fixture: wrote crates/acos-native-loader/tests/data/libacosndk.so"
