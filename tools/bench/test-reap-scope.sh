#!/bin/sh
# SERVELEAK2/F2 dry test: two bench "runs" share one SCRATCH_ROOT and only the
# caller's daemons are reaped.
#
# No `acos` binary and no ART guest are involved: each "daemon" is a fake
# executable named `acos` whose command line reads `... acos serve --socket
# <scratch>/r1-a/serve.sock ...`, i.e. exactly the shape the old `pgrep -f
# "acos serve .*$SCRATCH_ROOT/r[0-9]"` matched — which is why the first run to
# finish SIGKILLed the other run's daemons.
#
#   run-1  ACOS_BENCH_RUN_ID=run-1  pid file 1
#   run-2  ACOS_BENCH_RUN_ID=run-2  pid file 2   (same scratch root)
#
# Then run-1's reaper runs. It must reap run-1's daemon only.
#
#   sh tools/bench/test-reap-scope.sh      # exit 0 = pass
set -u

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
SCRATCH_ROOT="${ACOS_BENCH_SCRATCH:-$ROOT/.cache/serveleak2/f2-scratch}"
WORK="$ROOT/.cache/serveleak2/f2-dry"
rm -rf "$WORK"
mkdir -p "$WORK" "$SCRATCH_ROOT/r1-a" "$SCRATCH_ROOT/r1-b"

# A fake `acos` that just lives: `acos serve --socket ...` on its command line.
cat > "$WORK/acos" <<'EOF'
#!/usr/bin/env python3
import sys, time
if len(sys.argv) > 1 and sys.argv[1] == "serve":
    while True:
        time.sleep(0.2)
sys.exit(2)
EOF
chmod +x "$WORK/acos"

start_daemon() { # run-id pid-file socket
    ACOS_BENCH_RUN_ID="$1" ACOS_BENCH_PIDFILE="$2" \
        "$WORK/acos" serve --socket "$3" --engine art --idle-timeout 900 &
}

PID1FILE="$WORK/run1.pids"
PID2FILE="$WORK/run2.pids"
: > "$PID1FILE"
: > "$PID2FILE"
start_daemon run-1 "$PID1FILE" "$SCRATCH_ROOT/r1-a/serve.sock"
PID1=$!
start_daemon run-2 "$PID2FILE" "$SCRATCH_ROOT/r1-b/serve.sock"
PID2=$!
# The shell records `pid starttime` at spawn, like d2_warm.Serve does.
for spec in "$PID1:$PID1FILE" "$PID2:$PID2FILE"; do
    pid=${spec%%:*}
    file=${spec#*:}
    start=$(sed 's/.*) //' "/proc/$pid/stat" | awk '{print $20}')
    echo "$pid $start" >> "$file"
done
sleep 0.3

fail=0
check() { # label, condition
    if [ "$2" = "1" ]; then
        echo "PASS $1"
    else
        echo "FAIL $1"
        fail=1
    fi
}

alive() { [ -d "/proc/$1" ] && [ "$(awk '{print $3}' "/proc/$1/stat" 2>/dev/null)" != "Z" ] && echo 1 || echo 0; }

# --- 1. run-1's reaper must reap run-1's daemon only ------------------------
ACOS_BENCH_RUN_ID=run-1 ACOS_BENCH_PIDFILE="$PID1FILE" \
    sh -c ". '$ROOT/tools/bench/reap-daemons.sh'; bench_reap_daemons" 2>"$WORK/reap1.err"
alive1=$(alive "$PID1")
alive2=$(alive "$PID2")
check "run-1's own daemon is reaped" "$([ "$alive1" = 0 ] && echo 1 || echo 0)"
check "run-2's daemon in the same scratch root survives" "$alive2"

# --- 2. a recycled pid cannot be reaped through a stale record --------------
# run-2's pid recorded with a *wrong* start time (its id was recycled by another
# process): the reaper must refuse to signal it.
echo "$PID2 1" > "$PID2FILE"
ACOS_BENCH_RUN_ID=run-2 ACOS_BENCH_PIDFILE="$PID2FILE" \
    sh -c ". '$ROOT/tools/bench/reap-daemons.sh'; bench_reap_daemons" 2>"$WORK/reap2.err"
check "a recorded pid whose start time no longer matches is not signalled" "$(alive "$PID2")"

# --- 3. a run id that is not this process's daemon is never reaped ----------
# run-2's reaper, pointed at run-1's (dead) pid, must not touch run-2 either.
echo "$PID2 $(sed 's/.*) //' "/proc/$PID2/stat" | awk '{print $20}')" > "$PID2FILE"
ACOS_BENCH_RUN_ID=run-3 ACOS_BENCH_PIDFILE="$PID2FILE" \
    sh -c ". '$ROOT/tools/bench/reap-daemons.sh'; bench_reap_daemons" 2>"$WORK/reap3.err"
check "a daemon carrying another run's id is not reaped" "$(alive "$PID2")"

# --- clean up only what this test started -----------------------------------
if [ "$(alive "$PID2")" = "1" ]; then
    kill -TERM "$PID2" 2>/dev/null
    for _ in $(seq 1 20); do [ -d "/proc/$PID2" ] || break; sleep 0.1; done
    [ -d "/proc/$PID2" ] && kill -KILL "$PID2" 2>/dev/null
fi
[ -d "/proc/$PID1" ] && kill -KILL "$PID1" 2>/dev/null
rm -rf "$WORK"
[ "$fail" = 0 ] && echo "f2 dry test: PASS" || echo "f2 dry test: FAIL"
exit "$fail"
