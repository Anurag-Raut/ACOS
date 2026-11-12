#!/usr/bin/env bash
# P11 measurement batch: settle-lab (both modes) + counter M8 (both modes).
# The caller holds the host-wide ART lock via tools/art-runtime/run-exclusive.sh.
set -euo pipefail
ACOS="$1"
OUT="$2"
REPS="${3:-20}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
mkdir -p "$OUT"
for mode in quiet event; do
  echo "=== settle-lab mode=$mode reps=$REPS $(date -u +%FT%TZ) ==="
  ACOS_STRICT_SETTLE="$mode" python3 "$HERE/p11_measure.py" \
    --acos "$ACOS" --out "$OUT/settle-$mode.json" \
    --scratch "/home/Qui/Projects/acos/.cache/p11-scratch-$mode" --reps "$REPS"
  echo "=== counter M8 mode=$mode $(date -u +%FT%TZ) ==="
  ACOS_STRICT_SETTLE="$mode" python3 "$HERE/../P10/p10_m8.py" \
    --acos "$ACOS" --out "$OUT/counter-$mode.json" \
    --scratch "/home/Qui/Projects/acos/.cache/p11-m8-$mode" --reps "$REPS"
done
