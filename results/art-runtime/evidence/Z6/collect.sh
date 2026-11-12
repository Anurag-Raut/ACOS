#!/usr/bin/env bash
# Z6: freeze the evidence — gzip the driver reports, rebuild the tables.
#   raw JSONs stay in $CACHE/z6-run (never /tmp); the committed copies are the
#   gzipped reports + summary.json + table.txt (see Z6-gate.md §9).
set -uo pipefail
D=$(cd "$(dirname "$0")" && pwd)
CACHE=/home/Qui/Projects/acos/.cache
OUT=$CACHE/z6-run
mkdir -p "$D/raw"
rm -f "$D"/raw/*.json.gz
for f in "$OUT"/*.json; do
  b=$(basename "$f")
  case "$b" in
    *smoke*|*probe-smoke*) continue ;;
  esac
  gzip -9 -c "$f" > "$D/raw/$b.gz"
done
python3 "$D/tables.py" > "$D/table.txt" 2>&1
gzip -9 -f "$D/table.txt"
ls -la "$D" "$D/raw" | head -40
du -sh "$D"
