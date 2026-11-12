#!/usr/bin/env bash
# Z1: run the prototype inside the same bwrap-style mini-Android root ACOS uses.
# The argv/env/mount list below is copied from the product launcher
# (crates/acos-art/src/lib.rs:883-908 SANDBOX_FLAGS, :1018-1240 mounts + env,
#  crates/acos-art/src/launch.rs:644 argv), not called through it.
#
#   run.sh fork <children> <holdMs> [preloadLimit]   zygote parent + N children
#   run.sh cold [tag]                                plain dalvikvm64 control
set -uo pipefail
ROOT=/home/Qui/Projects/acos
HERE="$ROOT/research/zygote-proto"
S="$ROOT/.cache/art/sysroot"
D="$ROOT/.cache/art/data"
B="$ROOT/.cache/z1/build"
R="$ROOT/.cache/z1/run"
MODE="${1:-fork}"

[ -f "$B/dex/classes.dex" ] || { echo "run build.sh first" >&2; exit 1; }

# --- the guest /data root (fresh per invocation) ---------------------------
rm -rf "$R"; mkdir -p "$R/data/app" "$R/data/shim" "$R/data/dalvik-cache/x86_64" \
    "$R/data/misc/apexdata/com.android.art/dalvik-cache/x86_64" "$R/data/tmp"
cp "$B/dex/classes.dex" "$R/data/app/classes.dex"
cp "$B/libz1fork.so" "$R/data/shim/libz1fork.so"
# The product's own LD_PRELOAD shim (props + liblog sink), reused verbatim from
# the content-addressed build cache -- no new shim code is written for Z1.
SHIM="$(ls -t "$ROOT"/.cache/art/build-cache/*/libartshim.so 2>/dev/null | head -1)"
cp "$SHIM" "$R/data/shim/libartshim.so"
cp "$D/acos.properties" "$R/data/acos.properties"
# Boot image. `-Xzygote` sets Runtime::only_use_system_oat_files_ (runtime.cc:1626),
# and OatFileManager::RegisterOatFile then refuses any oat file outside
# /system, /system_ext or $ANDROID_ART_ROOT (file_utils.cc:899 LocationIsTrusted).
# /data is not trusted, so the image is hardlink-copied *into* a hardlink copy of
# the ART APEX directory, which is still a trusted location. Nothing is copied
# byte-wise and nothing under .cache/art/sysroot is written.
mkdir -p "$R/apex-art"
cp -al "$S/system/apex/com.android.art/." "$R/apex-art/" 2>/dev/null || cp -a "$S/system/apex/com.android.art/." "$R/apex-art/"
cp -al "$S/bootimg" "$R/apex-art/bootimg" 2>/dev/null || cp -a "$S/bootimg" "$R/apex-art/bootimg"

BC="$(tr '\n' ':' < "$S/BOOTCLASSPATH" | sed 's/:$//')"

BWRAP=(bwrap
    --unshare-pid --unshare-net --unshare-ipc --unshare-uts --unshare-cgroup-try
    --die-with-parent --clearenv --cap-drop ALL
    --tmpfs /
    --ro-bind "$S/system" /system
    --dir /apex
    --ro-bind "$D/apex-info-list.xml" /apex/apex-info-list.xml)
while IFS=$'\t' read -r mod _file; do
    [ -d "$S/system/apex/$mod" ] || continue
    # The APEX mount name is the `name` field of apex_manifest.pb, not the
    # directory name (crates/acos-art/src/lib.rs:1379 apex_mount_name). The only
    # one that differs in this sysroot is the shared-libs apex, which apexd
    # mounts at /apex/sharedlibs; /apex/com.android.art/lib64/libc++.so is a
    # symlink into it, so without this bind the guest linker cannot start.
    guest="/apex/$mod"
    src="$S/system/apex/$mod"
    case "$mod" in
        com.google.mainline.primary.libs) guest=/apex/sharedlibs ;;
        # the copy that carries the boot image (see above)
        com.android.art) src="$R/apex-art" ;;
    esac
    BWRAP+=(--ro-bind "$src" "$guest")
done < "$S/APEX_MODULES.txt"
BWRAP+=(--ro-bind "$D/linkerconfig" /linkerconfig)
BWRAP+=(--bind "$R/data" /data)
BWRAP+=(--dev /dev --proc /proc)
ENVS=(
    --setenv ANDROID_ROOT /system
    --setenv ANDROID_RUNTIME_ROOT /apex/com.android.runtime
    --setenv ANDROID_ART_ROOT /apex/com.android.art
    --setenv ANDROID_I18N_ROOT /apex/com.android.i18n
    --setenv ANDROID_TZDATA_ROOT /apex/com.google.android.tzdata5
    --setenv ANDROID_DATA /data
    --setenv ANDROID_STORAGE /data
    --setenv ANDROID_TMP /data/tmp
    --setenv ANDROID_ART_PROPERTIES /data/acos.properties
    --setenv BOOTCLASSPATH "$BC"
    --setenv DEX2OATBOOTCLASSPATH "$BC"
    --setenv LD_PRELOAD /data/shim/libartshim.so
)

# Must be the /system/apex/** spelling: bionic keys the per-APEX namespace off it.
DALVIK=/system/apex/com.android.art/bin/dalvikvm64
ARGS=(-Ximage:/apex/com.android.art/bootimg/boot.art -Xmx512m -Xms8m)
if [ "$MODE" = cold ]; then
    TAG="${2:-7}"; WMODE="${3:-cold}"
    ARGS+=(-cp /data/app/classes.dex dev.acos.z1.Work "$TAG" "$WMODE")
else
    N="${2:-2}"; HOLD="${3:-8000}"; PRE="${4:-4000}"
    ARGS+=(-Xzygote -Xhidden-api-policy:disabled
           -cp /data/app/classes.dex dev.acos.z1.ZygoteMain "$N" "$HOLD" "$PRE")
fi

exec "${BWRAP[@]}" "${ENVS[@]}" "$DALVIK" "${ARGS[@]}"
