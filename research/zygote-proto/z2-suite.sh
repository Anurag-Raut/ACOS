#!/usr/bin/env bash
# Z2 suite: ECHILD diagnostic + three memory snapshots, inside one ART lock hold.
# Called by tools/art-runtime/run-exclusive.sh + tools/guard/guarded.sh.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CACHE="${ACOS_CACHE:-/home/Qui/Projects/acos/.cache}"
LOG="$CACHE/z2-logs"
mkdir -p "$LOG"

echo "=== diag $(date -u +%FT%TZ) ==="
"$HERE/run.sh" diag >"$LOG/diag.log" 2>&1
echo "diag rc=$?"

for n in 1 2 5; do
    echo "=== N=$n $(date -u +%FT%TZ) ==="
    python3 "$HERE/measure.py" --n "$n" --hold 6000 --preload 800 \
        --out "$LOG/Z2-N$n.json" --log "$LOG/run-n$n.log"
    echo "measure N=$n rc=$?"
done
echo "=== done $(date -u +%FT%TZ) ==="
