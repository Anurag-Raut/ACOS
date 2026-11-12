set -uo pipefail
cd /home/Qui/.paseo/worktrees/3oxa6xao/zygote-z5
CACHE=/home/Qui/Projects/acos/.cache
export ACOS_CACHE=$CACHE ACOS_ART_BIN=$CACHE/z5-target/release/acos-art ACOS_FONT_DIR=$CACHE/boot/android-34/fonts
for n in 5 10; do
  for arm in a c; do
    case $arm in
      a) mode=normal; unset ACOS_SHARED_BOOT_IMAGE; unset ACOS_ZYGOTE_NO_STUB ;;
      c) mode=zygote; unset ACOS_SHARED_BOOT_IMAGE; export ACOS_ZYGOTE_NO_STUB=1 ;;
    esac
    echo "=== settle60 arm=$arm n=$n $(date -u +%FT%TZ) ==="
    python3 research/zygote-proto/z4/drive.py --mode "$mode" --n "$n" --label "$arm" \
      --settle 60 --out "$CACHE/z5-run/$arm-$n-s60" --json "$CACHE/z5-run/$arm-$n-s60.json"
  done
done
