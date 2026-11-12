#!/usr/bin/env bash
# Z6 gate measurement: the three arms on ONE binary, three time points per run.
#   usage: run-z6.sh a b c            (arm letters; see the case block)
#   Z6_NS="1 5 10 20"                 (default)
#   Z6_SAMPLES="0.5,15,60"            (hold times after the last tap)
#   Z6_OUT=$CACHE/z6-run              (scratch; JSONs are copied into evidence)
#   arm a = optimised current (main default, no ACOS_* knob)
#   arm b = shared boot image (B3 ACOS_SHARED_BOOT_IMAGE), no zygote
#   arm c = zygote (Z4 seam) with the Z5 no-tombstone holder
#   arm d = (a) + ACOS_ART_HEAPGROWTHLIMIT (an existing W48 knob; the
#           "would the exec path release the same memory if ART ran a trim"
#           control — not part of the three-arm gate)
set -uo pipefail
D=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$D/../../../.." && pwd)
cd "$REPO" || exit 2
CACHE=/home/Qui/Projects/acos/.cache
export ACOS_CACHE=$CACHE
export ACOS_ART_BIN=$CACHE/z6-target/release/acos-art
export ACOS_FONT_DIR=$CACHE/boot/android-34/fonts
NS=${Z6_NS:-1 5 10 20}
SFX=${Z6_SUFFIX:-}
OUT=${Z6_OUT:-$CACHE/z6-run}
SAMPLES=${Z6_SAMPLES:-0.5,15,60}
mkdir -p "$OUT"
[ -x "$ACOS_ART_BIN" ] || { echo "missing $ACOS_ART_BIN" >&2; exit 2; }

for arm in "$@"; do
  for n in $NS; do
    extra=()
    case "$arm" in
      a) mode=normal
         unset ACOS_SHARED_BOOT_IMAGE ACOS_ZYGOTE_NO_STUB ACOS_ART_HEAPGROWTHLIMIT ;;
      b) mode=normal
         export ACOS_SHARED_BOOT_IMAGE=$CACHE/b2/image
         unset ACOS_ZYGOTE_NO_STUB ACOS_ART_HEAPGROWTHLIMIT ;;
      c) mode=zygote
         unset ACOS_SHARED_BOOT_IMAGE ACOS_ART_HEAPGROWTHLIMIT
         export ACOS_ZYGOTE_NO_STUB=1 ;;
      d) mode=normal
         unset ACOS_SHARED_BOOT_IMAGE ACOS_ZYGOTE_NO_STUB
         export ACOS_ART_HEAPGROWTHLIMIT=${Z6_HEAPGROWTHLIMIT:-32m} ;;
      *) echo "unknown arm $arm" >&2; exit 2 ;;
    esac
    if [ -n "${Z6_PROBE:-}" ]; then extra=(--release-probe "$Z6_PROBE"); fi
    used=$(free -m | awk '/Mem:/{print int(100*$3/$2)}')
    free -m | awk 'NR==1 || /Mem:/'
    if [ "$used" -ge 90 ]; then
      echo "Z6: host RAM ${used}% >= 90%, refusing to run N=$n" >&2
      exit 3
    fi
    echo "=== arm=$arm mode=$mode n=$n host=${used}% samples=$SAMPLES probe=${Z6_PROBE:-0} $(date -u +%FT%TZ) ==="
    rm -rf "$OUT/$arm-$n$SFX"
    python3 research/zygote-proto/z4/drive.py --mode "$mode" --n "$n" --label "$arm" \
      --samples "$SAMPLES" "${extra[@]}" \
      --out "$OUT/$arm-$n$SFX" --json "$OUT/$arm-$n$SFX.json"
    echo "=== arm=$arm n=$n rc=$? $(date -u +%FT%TZ) ==="
  done
done
