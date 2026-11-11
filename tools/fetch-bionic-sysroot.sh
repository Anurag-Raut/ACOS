#!/usr/bin/env bash
#
# fetch-bionic-sysroot.sh -- assemble an aarch64 bionic sysroot for qemu-user.
#
# The NDK ships only static bionic libc/libm/libdl archives for
# `aarch64-linux-android` (no `libc.so`, no `linker64`), so a *dynamic* aarch64
# APK `.so` needs the real Android runtime. This script reads it, **read-only**,
# from an attached arm64 device with `adb pull`:
#
#     /system/bin/linker64
#     /system/lib64/{libc,libm,libdl,liblog,libandroid,libz,libjnigraphics}.so
#     /system/lib64/libc++_shared.so        (else copied from the NDK)
#
# It makes NO device change: no `adb shell` mutation, no settings, no install,
# no root, no push. `adb pull` of these world-readable paths is enough. The
# layout is exactly what `qemu-aarch64 -L` expects:
#
#     .cache/bionic-arm64/system/bin/linker64
#     .cache/bionic-arm64/system/lib64/libc.so
#     .cache/bionic-arm64/linkerconfig/ld.config.txt
#
# Usage:
#   tools/fetch-bionic-sysroot.sh [SERIAL]
#
# Env overrides:
#   ANDROID_SERIAL     device serial (default ZA222WGJPP)
#   ACOS_BIONIC_ROOT   destination (default <repo>/.cache/bionic-arm64)
#
# Exit codes: 0 assembled; 2 usage; 3 adb missing; 4 device not usable;
#             5 a required library is missing/unusable.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SERIAL="${1:-${ANDROID_SERIAL:-ZA222WGJPP}}"
DEST="${ACOS_BIONIC_ROOT:-$ROOT/.cache/bionic-arm64}"
LIBDIR="$DEST/system/lib64"
BINDIR="$DEST/system/bin"
CONFIGDIR="$DEST/linkerconfig"

usage() {
    sed -n '2,30p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

for arg in "$@"; do
    case "$arg" in
        -h|--help) usage; exit 0 ;;
    esac
done

log() { printf '[bionic-sysroot] %s\n' "$*"; }
die() { printf '[bionic-sysroot] ERROR: %s\n' "$*" >&2; exit "${2:-1}"; }

if ! command -v adb >/dev/null 2>&1; then
    die "adb is not installed or not on PATH.

The arm64 transport needs a bionic sysroot, and this script only obtains it
by reading an attached arm64 device. Install the Android platform-tools
(adb), or supply one manually and set ACOS_BIONIC_ROOT:
    ACOS_BIONIC_ROOT=/path/to/bionic-arm64
(reproduce this layout: <root>/system/bin/linker64 + <root>/system/lib64/*.so)" 3
fi

state="$(adb -s "$SERIAL" get-state 2>/dev/null || true)"
if [ "$state" != "device" ]; then
    die "device $SERIAL is not in the 'device' state (got '${state:-absent}').

Attach the arm64 phone (USB debugging authorised) or pass another serial:
    ANDROID_SERIAL=XXXX tools/fetch-bionic-sysroot.sh
Read-only: this script never writes to the device." 4
fi

abi="$(adb -s "$SERIAL" shell getprop ro.product.cpu.abi 2>/dev/null | tr -d '\r')"
log "device $SERIAL abi=$abi"
case "$abi" in
    arm64-v8a|arm64*) ;;
    *) die "device $SERIAL reports abi '$abi', not arm64; the sysroot must be aarch64" 4 ;;
esac

mkdir -p "$LIBDIR" "$BINDIR" "$CONFIGDIR"
chmod 0755 "$DEST" "$DEST/system" "$DEST/system/bin" "$DEST/system/lib64" 2>/dev/null || true

# pull_one <guest-path> <dest-path> <required|optional>
pull_one() {
    local guest="$1" dest="$2" kind="${3:-required}"
    if adb -s "$SERIAL" pull "$guest" "$dest" >/dev/null 2>&1 && [ -s "$dest" ]; then
        if [ "$(head -c 4 "$dest" 2>/dev/null | od -An -tx1 | tr -d ' \n')" = "7f454c46" ]; then
            log "pulled $guest  ($(stat -c%s "$dest") B)"
            return 0
        fi
        log "WARNING: $guest is not an ELF, discarding $dest"
        rm -f "$dest"
    fi
    if [ "$kind" = required ]; then
        die "required library $guest could not be read from $SERIAL" 5
    fi
    log "optional $guest unavailable on $SERIAL (skipped)"
    return 1
}

# The interpreter and the core bionic runtime are mandatory.
pull_one /system/bin/linker64 "$BINDIR/linker64" required
pull_one /system/lib64/libc.so "$LIBDIR/libc.so" required
pull_one /system/lib64/libm.so "$LIBDIR/libm.so" required
pull_one /system/lib64/libdl.so "$LIBDIR/libdl.so" required

# Optional but commonly needed by native APK code.
for lib in liblog.so libandroid.so libz.so libjnigraphics.so; do
    pull_one "/system/lib64/$lib" "$LIBDIR/$lib" optional || true
done

# libc++_shared.so is often not in /system/lib64 (it ships in apps). Prefer the
# device copy; otherwise take the NDK's, which is the same ABI.
if [ ! -s "$LIBDIR/libc++_shared.so" ]; then
    pull_one /system/lib64/libc++_shared.so "$LIBDIR/libc++_shared.so" optional || true
fi
if [ ! -s "$LIBDIR/libc++_shared.so" ]; then
    ndk_cxx=""
    for base in "${ANDROID_NDK_HOME:-}" "${ANDROID_NDK_ROOT:-}" "$HOME/Android/Sdk/ndk"; do
        [ -n "$base" ] || continue
        if [ -d "$base" ]; then
            # If $base is the NDK itself it already contains the toolchain dir;
            # if it is the SDK ndk/ directory, take the newest version.
            if [ -d "$base/toolchains" ]; then
                roots="$base"
            else
                roots="$(find "$base" -maxdepth 1 -mindepth 1 -type d | sort | tail -1)"
            fi
            candidate="$roots/toolchains/llvm/prebuilt/linux-x86_64/sysroot/usr/lib/aarch64-linux-android/libc++_shared.so"
            if [ -s "$candidate" ]; then ndk_cxx="$candidate"; break; fi
        fi
    done
    if [ -n "$ndk_cxx" ]; then
        cp "$ndk_cxx" "$LIBDIR/libc++_shared.so"
        log "libc++_shared.so copied from NDK: $ndk_cxx"
    else
        log "WARNING: libc++_shared.so not found on device or in an NDK sysroot"
    fi
fi

# A minimal linker namespace config: an empty ld.config.txt keeps only the
# default namespace (enough for a single-process sysroot) and silences
# bionic's "failed to find generated linker configuration" warning. It is
# written locally, never to the device.
cat >"$CONFIGDIR/ld.config.txt" <<'EOF'
# ACOS qemu bionic sysroot: default namespace only.
# Generated by tools/fetch-bionic-sysroot.sh (no device changes).
EOF

# The interpreter symlink on device is /system/bin/linker64 -> /apex/...; adb
# pull follows it and stores the real ELF, which is what qemu's -L needs.

log "sysroot assembled at $DEST"
echo
printf '%-42s %10s  %s\n' FILE SIZE SHA256
printf '%-42s %10s  %s\n' '----' '----' '------'
for f in "$BINDIR/linker64" "$LIBDIR"/*.so; do
    [ -s "$f" ] || continue
    printf '%-42s %10s  %s\n' "${f#"$DEST"/}" "$(stat -c%s "$f")" "$(sha256sum "$f" | cut -c1-64)"
done
echo
log "qemu usage: qemu-aarch64 -L $DEST <aarch64-elf>"
log "the device was only read: no settings changed, nothing installed"
