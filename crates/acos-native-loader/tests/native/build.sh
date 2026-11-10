#!/usr/bin/env bash
#
# Build the native loader test libraries with the NDK, checking the resulting
# .so files into tests/data/ so `cargo test` needs no NDK.
#
# Run from anywhere:  crates/acos-native-loader/tests/native/build.sh
set -euo pipefail

NDK="${ANDROID_NDK_HOME:-/home/Qui/Android/Sdk/ndk/27.1.12297006}"
BIN="$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin"
CC="$BIN/x86_64-linux-android26-clang"
CXX="$BIN/x86_64-linux-android26-clang++"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT="$HERE/../data"
mkdir -p "$OUT"

echo "build: libtestc.so"
"$CC" -shared -fPIC -O2 -o "$OUT/libtestc.so" "$HERE/testc.c" -llog

echo "build: libtestdep.so"
"$CC" -shared -fPIC -fsemantic-interposition -O2 -o "$OUT/libtestdep.so" "$HERE/testdep.c" -llog

echo "build: libtestuser.so (DT_NEEDED libtestdep.so)"
"$CC" -shared -fPIC -O2 -o "$OUT/libtestuser.so" "$HERE/testuser.c" \
    -L"$OUT" -ltestdep -Wl,-rpath,'$ORIGIN' -llog

echo "build: libtestcpp.so (static libc++/libunwind)"
"$CXX" -shared -fPIC -O2 -static-libstdc++ -o "$OUT/libtestcpp.so" \
    "$HERE/testcpp.cpp" -llog

echo "build: libtestc-android.so (APS2 packed relocations)"
"$CC" -shared -fPIC -O2 -Wl,--pack-dyn-relocs=android -o "$OUT/libtestc-android.so" \
    "$HERE/testc.c" -llog

echo "build: libtestc-relr.so (RELR packed relocations)"
"$CC" -shared -fPIC -O2 -Wl,--pack-dyn-relocs=relr -o "$OUT/libtestc-relr.so" \
    "$HERE/testc.c" -llog

echo "build: libtestmissing.so (imports a symbol the bionic table lacks)"
"$CC" -shared -fPIC -O2 -o "$OUT/libtestmissing.so" "$HERE/testmissing.c" -llog

echo "build: libhostref.so (host glibc reference, NOT an Android library)"
cc -shared -fPIC -O2 -o "$OUT/libhostref.so" "$HERE/host_ref.c"

echo "--- DT_NEEDED ---"
for f in "$OUT"/libtest*.so; do
    printf '%s: ' "$(basename "$f")"
    readelf -dW "$f" | grep NEEDED | sed 's/.*\[//;s/\]//' | tr '\n' ' '
    echo
done

echo "--- TLS relocations (libtestc) ---"
readelf -rW "$OUT/libtestc.so" | grep -iE 'TLS|TPOFF|DTP' || echo "(none)"
