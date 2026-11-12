#!/usr/bin/env bash
# W32: reproduce the dialog window attach stall on the launch (LaunchMain probe) path
# and capture why the guest main thread stops.
set -uo pipefail
cd /home/Qui/Projects/acos
dir=${1:-.cache/w32/repro}
bin=${2:-.cache/w32-target/debug/acos-art}
tapy=${3:-252}
rm -rf "$dir"; mkdir -p "$dir"
export ACOSLAUNCH_INTERACTIVE=1 ACOSLAUNCH_FRAME_WAIT_SECS=1 \
       ACOSLAUNCH_RESUME_MS=60000 ACOSLAUNCH_SESSION_SECS=300 ACOS_WMS_TRACE=1
fifo=$dir/in.fifo; mkfifo "$fifo"
"$bin" launch --apk fixtures/build/apps/dialog-menu.apk --until activity-resumed \
  --timeout 180 --log stderr --out "$dir/out" < "$fifo" > "$dir/stdout.jsonl" 2> "$dir/stderr.txt" &
host=$!
exec 3>"$fifo"
for i in $(seq 1 480); do grep -q '"cmd":"ready"' "$dir/stdout.jsonl" 2>/dev/null && break; sleep 0.25; done
echo "READY $(date -u +%FT%TZ)"
echo '{"cmd":"tree"}' >&3
sleep 4
echo "TAP y=$tapy $(date -u +%FT%TZ)"
echo "{\"cmd\":\"tap\",\"x\":173,\"y\":$tapy}" >&3
sleep 2
ps -eo pid,ppid,stat,comm,args > "$dir/ps.txt" 2>&1
mapfile -t gpids < <(pgrep -x main; pgrep -f dalvikvm64; pgrep -f 'com.android.runtime/bin')
echo "GUEST_PIDS=${gpids[*]} $(date -u +%FT%TZ)"
for g in "${gpids[@]}"; do
  [ -d "/proc/$g" ] || continue
  kill -QUIT "$g" 2>/dev/null
  {
    echo "### pid $g"
    cat "/proc/$g/comm" 2>/dev/null
    ps -L -o pid,tid,stat,pcpu,comm -p "$g" 2>&1
    echo "--- wchan per thread"
    for t in /proc/$g/task/*; do
      tid=$(basename "$t")
      printf '%s %s %s\n' "$tid" "$(cat "$t/wchan" 2>/dev/null)" "$(cut -d' ' -f3 "$t/stat" 2>/dev/null)"
    done
    echo "--- fds"
    ls -l "/proc/$g/fd" 2>&1 | head -40
  } >> "$dir/blocked.txt" 2>&1
done
sleep 3
echo "TREE2 $(date -u +%FT%TZ)"
echo '{"cmd":"tree"}' >&3
sleep 3
echo "CLOSE $(date -u +%FT%TZ)"
echo '{"cmd":"close"}' >&3
for i in $(seq 1 40); do kill -0 "$host" 2>/dev/null || break; sleep 0.25; done
exec 3>&-
kill "$host" 2>/dev/null
wait "$host" 2>/dev/null
echo "DONE $(date -u +%FT%TZ)"
