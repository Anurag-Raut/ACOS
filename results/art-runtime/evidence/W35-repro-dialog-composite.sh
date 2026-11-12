#!/usr/bin/env bash
# W35: prove the C1 frame is a composite of the visible windows in z-order.
#
# Launches dialog-menu on the live ART launch path, captures before the dialog
# (one window), taps "Show dialog", captures again. With `ACOSC1_TRACE` the host
# prints the layers and placements the composite used, and `ACOS_WMS_TRACE`
# prints the dialog window's add/relayout (the frame the composite positions it
# with). The two PNGs are the evidence: the second one contains the dialog.
set -uo pipefail
cd "/home/Qui/Projects/acos" || exit 1
dir=${1:-.cache/w35/repro}
bin=${2:-target/debug/acos-art}
tapy=${3:-252}
rm -rf "$dir"; mkdir -p "$dir"
export ACOSLAUNCH_INTERACTIVE=1 ACOSLAUNCH_FRAME_WAIT_SECS=1 \
       ACOSLAUNCH_RESUME_MS=60000 ACOSLAUNCH_SESSION_SECS=300 \
       ACOS_WMS_TRACE=1 ACOSC1_TRACE=1
fifo=$dir/in.fifo; mkfifo "$fifo"
"$bin" launch --apk fixtures/build/apps/dialog-menu.apk --until activity-resumed \
  --timeout 180 --log stderr --out "$dir/out" < "$fifo" > "$dir/stdout.jsonl" 2> "$dir/stderr.txt" &
host=$!
exec 3>"$fifo"
for i in $(seq 1 480); do grep -q '"cmd":"ready"' "$dir/stdout.jsonl" 2>/dev/null && break; sleep 0.25; done
echo "READY $(date -u +%FT%TZ)"
echo '{"cmd":"screenshot"}' >&3
sleep 2
echo "TAP y=$tapy $(date -u +%FT%TZ)"
echo "{\"cmd\":\"tap\",\"x\":173,\"y\":$tapy}" >&3
sleep 3
echo '{"cmd":"screenshot"}' >&3
sleep 2
echo '{"cmd":"screenshot"}' >&3
sleep 2
echo "CLOSE $(date -u +%FT%TZ)"
echo '{"cmd":"close"}' >&3
for i in $(seq 1 40); do kill -0 "$host" 2>/dev/null || break; sleep 0.25; done
exec 3>&-
kill "$host" 2>/dev/null
wait "$host" 2>/dev/null
echo "DONE $(date -u +%FT%TZ)"
