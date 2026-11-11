#!/usr/bin/env bash
# fork-pss.sh -- measure whether forking a *warm* dalvikvm shares boot-image
# pages (zygote-less).  For N in 1 5 20: start one ART process, warm it up
# (JNI + class loads + JIT), fork N children from native code, sample PSS of the
# parent and every child, report per-process and marginal PSS.
#
#   scripts/fork-pss.sh [N...]
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXPDIR="$(cd "$HERE/.." && pwd)"
ACOS_ROOT="$(cd "$EXPDIR/../../.." && pwd)"
OUTDIR="$ACOS_ROOT/.cache/art-exp-c/fork"
mkdir -p "$OUTDIR"
SOCK="$ACOS_ROOT/.cache/art-exp/data/service.sock"
SVC="$ACOS_ROOT/.cache/art-exp-c/bin/acos-probe-service"

[ -x "$SVC" ] && { "$SVC" "$SOCK" >"$OUTDIR/service.log" 2>&1 & SVC_PID=$!; }
trap '[ -n "${SVC_PID:-}" ] && kill $SVC_PID 2>/dev/null; rm -f "$SOCK"' EXIT

pss_kb() { awk '/^Pss:/{print $2}' "/proc/$1/smaps_rollup" 2>/dev/null; }

N_LIST=("$@"); [ ${#N_LIST[@]} -eq 0 ] && N_LIST=(1 5 20)
for N in "${N_LIST[@]}"; do
    LOG="$OUTDIR/run-N$N.log"
    "$ACOS_ROOT/research/art-feasibility/a-dalvikvm/scripts/artroot.sh" \
        dalvikvm -cp /data/dex/artjni/classes.dex \
        -Dprobe.lib=/data/lib/libacosjniprobe.so \
        -Dprobe.mode=fork -Dfork.n="$N" -Dfork.holdMs=6000 \
        dev.acos.artprobe.Main >"$LOG" 2>&1 &
    BG=$!
    # wait for the FORKED line
    for _ in $(seq 200); do grep -q '^FORKED' "$LOG" 2>/dev/null && break; sleep 0.05; done
    line=$(grep -m1 '^FORKED' "$LOG" 2>/dev/null)
    pids=$(printf '%s' "$line" | sed -n 's/.*pids=//p')
    parent=""
    first="${pids%%,*}"
    [ -n "$first" ] && parent=$(awk '{print $4}' "/proc/$first/stat" 2>/dev/null)
    sleep 1
    sum=0; n=0; parts=""
    pids="${pids//,/ }"
    all="$parent $pids"
    for p in $all; do
        [ -n "$p" ] || continue
        v=$(pss_kb "$p")
        [ -n "$v" ] || continue
        sum=$((sum + v)); n=$((n + 1)); parts="$parts $p:${v}kB"
    done
    if [ "$n" -gt 0 ]; then
        echo "N=$N procs=$n sumPSS=$((sum / 1024))MB perProc=$((sum / n / 1024))MB ::$parts"
    else
        echo "N=$N FAILED (no pids) :: $(tail -3 "$LOG" | tr '\n' ' ')"
    fi
    wait $BG 2>/dev/null
done
