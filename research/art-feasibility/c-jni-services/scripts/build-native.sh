#!/usr/bin/env bash
# build-native.sh -- build libacosjniprobe.so for x86_64 Android (API 34) with
# the NDK clang, i.e. against real bionic, and stage it where artroot.sh can
# see it (/data/lib inside the mini-root).
#
# Rust target x86_64-linux-android (rust-std installed) + the JNI shim compiled
# by the NDK clang and passed to the linker.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXPDIR="$(cd "$HERE/.." && pwd)"
ACOS_ROOT="$(cd "$EXPDIR/../../.." && pwd)"

NDK="${ANDROID_NDK:-$HOME/Android/Sdk/ndk/27.1.12297006}"
API="${API_LEVEL:-34}"
TC="$NDK/toolchains/llvm/prebuilt/linux-x86_64"
CC="$TC/bin/x86_64-linux-android${API}-clang"
[ -x "$CC" ] || { echo "no NDK clang at $CC" >&2; exit 1; }

SCRATCH="$ACOS_ROOT/.cache/art-exp-c"
OUT="$SCRATCH/build"; mkdir -p "$OUT"

# 1. JNI shim -> object file
"$CC" -c -O2 -fPIC -Wall -o "$OUT/shim.o" "$EXPDIR/native/shim.c"

# 2. Rust cdylib, linked with the shim object
export CARGO_TARGET_DIR="$SCRATCH/target-native"
export CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER="$CC"
# Rust links cdylibs with `--gc-sections`, which drops the (unreferenced) JNI
# entry points in shim.o.  `-u <symbol>` keeps them, and an explicit version
# script re-exports them as *global* (Rust's cdylib link otherwise localises
# every symbol that is not part of the Rust crate) -- without that ART's dlsym()
# lookup for Java_<class>_<method> fails with UnsatisfiedLinkError.
NM="$TC/bin/llvm-nm"
KEEP=()
for sym in $("$NM" --defined-only "$OUT/shim.o" | awk '$2=="T"{print $3}'); do
    KEEP+=("-C" "link-arg=-Wl,-u,$sym")
done
printf '{ global: Java_*; probe_*; local: *; };\n' >"$OUT/exports.map"
export RUSTFLAGS="-C link-arg=-Wl,--whole-archive -C link-arg=$OUT/shim.o -C link-arg=-Wl,--no-whole-archive ${KEEP[*]} -C link-arg=-Wl,--version-script=$OUT/exports.map -C link-arg=-Wl,--build-id=sha1"
( cd "$EXPDIR/native" && cargo build --release --target x86_64-linux-android )

SO="$CARGO_TARGET_DIR/x86_64-linux-android/release/libacosjniprobe.so"
mkdir -p "$SCRATCH/lib" "$ACOS_ROOT/.cache/art-exp/data/lib"
cp -f "$SO" "$SCRATCH/lib/libacosjniprobe.so"
cp -f "$SO" "$ACOS_ROOT/.cache/art-exp/data/lib/libacosjniprobe.so"
echo "built $SCRATCH/lib/libacosjniprobe.so ($(stat -c%s "$SO") bytes)"
file "$SO" | sed 's/^/  /'
