#!/usr/bin/env bash
# Z3: build the research zygote guest dex + the raw-syscall fork shim.
#
# The dex is the *production* launch guest (LaunchMain/RenderNat/Nat) compiled
# together with ZygoteApp and Z1's dev.acos.z1.Nat, so the child runs the
# unmodified guest entry point. Nothing under crates/ is built here.
#
#   research/zygote-proto/z3/build.sh
#
# Scratch: .cache/z3-build/ (never /tmp).
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROTO="$(cd "$HERE/.." && pwd)"
CACHE="${ACOS_CACHE:-/home/Qui/Projects/acos/.cache}"
SDK="${ANDROID_HOME:-$HOME/Android/Sdk}"
AJAR="$SDK/platforms/android-34/android.jar"
D8="$(ls -d "$SDK"/build-tools/*/d8 2>/dev/null | sort | tail -1)"
GUEST="$HERE/../../.."
OUT="$CACHE/${ZBUILD:-z3-build}"
rm -rf "$OUT/classes" "$OUT/dex"
mkdir -p "$OUT/classes" "$OUT/dex" "$OUT/lib"

[ -f "$AJAR" ] || { echo "missing $AJAR" >&2; exit 1; }
[ -n "$D8" ] || { echo "missing d8 in $SDK/build-tools" >&2; exit 1; }

mapfile -t SRC < <(
    printf '%s\n' \
        "$GUEST/crates/acos-art/guest/java/dev/acos/art/LaunchMain.java" \
        "$GUEST/crates/acos-art/guest/java/dev/acos/art/RenderNat.java" \
        "$GUEST/crates/acos-art/guest/java/dev/acos/art/Nat.java" \
        "$HERE/java/dev/acos/art/ZygoteApp.java" \
        "$PROTO/java/dev/acos/z1/Nat.java" \
        "$PROTO/java/dalvik/system/ZygoteHooks.java"
)
javac -nowarn -source 11 -target 11 -cp "$AJAR" -d "$OUT/classes" "${SRC[@]}"
"$D8" --min-api 34 --lib "$AJAR" --output "$OUT/dex" \
    $(find "$OUT/classes" -name '*.class') >/dev/null

clang -nostdlib -ffreestanding -fno-builtin -fno-stack-protector -fPIC -shared -O2 -Wall \
    -o "$OUT/lib/libz2fork.so" "$PROTO/shim/z2fork.c"

# Z4: the production guest guard, rebuilt from the same source with
# -fno-stack-protector because a dlopen from a classloader namespace cannot
# resolve bionic's hidden `__stack_chk_fail` (the blob and the Landlock plan it
# reads are still the production artefacts). Only the Z4 path binds this copy.
clang -nostdlib -ffreestanding -fno-builtin -fno-stack-protector -fPIC -shared -O2 -Wall \
    -o "$OUT/lib/libacos_seccomp_guest.so" "$GUEST/crates/acos-art/src/seccomp_guest.c"

echo "built $OUT/dex/classes.dex, $OUT/lib/libz2fork.so, $OUT/lib/libacos_seccomp_guest.so"
