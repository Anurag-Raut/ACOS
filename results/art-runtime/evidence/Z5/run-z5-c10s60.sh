set -uo pipefail
cd /home/Qui/.paseo/worktrees/3oxa6xao/zygote-z5
CACHE=/home/Qui/Projects/acos/.cache
export ACOS_CACHE=$CACHE ACOS_ART_BIN=$CACHE/z5-target/release/acos-art ACOS_FONT_DIR=$CACHE/boot/android-34/fonts
export ACOS_ZYGOTE_NO_STUB=1
unset ACOS_SHARED_BOOT_IMAGE
echo "=== c n=10 settle60 retry $(date -u +%FT%TZ) ==="
python3 research/zygote-proto/z4/drive.py --mode zygote --n 10 --label c --settle 60 \
  --out "$CACHE/z5-run/c-10-s60" --json "$CACHE/z5-run/c-10-s60.json"
