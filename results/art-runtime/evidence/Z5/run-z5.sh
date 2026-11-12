#!/usr/bin/env bash
# Z5 gate measurement: three arms x N, Z4's harness and method.
#   usage: run-z5.sh a b c ...          (arm letters)
#   Z5_NS="1 5 10 20"                   (default)
#   arm a = optimised current (main default, no env knobs)
#   arm b = shared boot image (B3 ACOS_SHARED_BOOT_IMAGE), no zygote
#   arm c = zygote (Z4 seam) with the Z5 no-tombstone holder
set -uo pipefail
cd /home/Qui/.paseo/worktrees/3oxa6xao/zygote-z5
CACHE=/home/Qui/Projects/acos/.cache
export ACOS_CACHE=$CACHE
export ACOS_ART_BIN=$CACHE/z5-target/release/acos-art
export ACOS_FONT_DIR=$CACHE/boot/android-34/fonts
NS=${Z5_NS:-1 5 10 20}
SFX=${Z5_SUFFIX:-}
for arm in "$@"; do
  for n in $NS; do
    case "$arm" in
      a) mode=normal;  unset ACOS_SHARED_BOOT_IMAGE; unset ACOS_ZYGOTE_NO_STUB ;;
      b) mode=normal;  export ACOS_SHARED_BOOT_IMAGE=$CACHE/b2/image; unset ACOS_ZYGOTE_NO_STUB ;;
      c) mode=zygote;  unset ACOS_SHARED_BOOT_IMAGE; export ACOS_ZYGOTE_NO_STUB=1 ;;
      *) echo "unknown arm $arm" >&2; exit 2 ;;
    esac
    echo "=== arm=$arm mode=$mode n=$n $(date -u +%FT%TZ) ==="
    python3 research/zygote-proto/z4/drive.py --mode "$mode" --n "$n" --label "$arm" \
      --out "$CACHE/z5-run/$arm-$n$SFX" --json "$CACHE/z5-run/$arm-$n$SFX.json"
    echo "=== arm=$arm n=$n rc=$? ==="
  done
done
