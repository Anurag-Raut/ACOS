#!/usr/bin/env bash
# repro.sh -- reproduce Q3's three outcomes and capture the evidence.
#
#   scripts/repro.sh demo    # pressed + focused Button, recommended policy  -> PNG
#   scripts/repro.sh segv    # stock ripple on a Looper thread, SM present   -> gdb backtrace
#   scripts/repro.sh hang    # stock ripple on a Looper thread, no SM        -> gdb backtrace
#   scripts/repro.sh all
#
# Every run goes through tools/guard/guarded.sh (--mem 4G --time 300) and keeps all
# scratch under .cache/ (see Q3.md).
set -uo pipefail
Q3="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ACOS_ROOT="$(cd "$Q3/../../.." && pwd)"
ART="$(dirname "$Q3")/a-dalvikvm/scripts/env.sh"
# shellcheck disable=SC1090
. "$ART"
OUT="$ACOS_ROOT/.cache/art-exp/data/out"
A="$Q3/artifacts"

gdb_script() { # $1 = out file, $2 = extra gdb command(s)
    cat >"$1" <<EOF
set pagination off
set confirm off
set sysroot $ACOS_ROOT/.cache/art-exp/sysroot
sharedlibrary
$2
EOF
}

# The dalvikvm64 process renames itself to "main" (ART's main thread), so the
# only reliable handle is /proc/<pid>/cmdline.
find_guest_pid() {
    local pid
    pid=$(for d in /proc/[0-9]*; do
              [ -r "$d/cmdline" ] || continue
              tr '\0' ' ' <"$d/cmdline" 2>/dev/null | grep -q "dev.acos.e4h.Main" || continue
              grep -q "^main$" "$d/comm" 2>/dev/null && echo "${d#/proc/}"
          done | sort -n | tail -1)
    [ -n "$pid" ] && echo "$pid"
}

wait_for() { # $1 = log, $2 = grep pattern, $3 = timeout s
    for _ in $(seq 1 $((3 * $3))); do
        grep -q -- "$2" "$1" && return 0
        sleep 0.33
    done
    return 1
}

run_demo() {
    echo "== demo: press + focus, content-layer policy, Looper-less draw thread =="
    Q3_MODE=nolooper+demo Q3_GUARD_TIME=120 "$Q3/scripts/run.sh" draw \
        >"$A/q3-demo-pressed-focused.log" 2>&1
    echo "exit=$?"
    cp -f "$OUT/q3-demo-pressed-focused.png" "$OUT/q3-demo-tree.txt" "$A/" 2>/dev/null
    grep -E "q3/demo|DOWNGRADE|PASS \[draw\]|PASS \[png\]|PASS \[pixels\]|failures=" \
        "$A/q3-demo-pressed-focused.log"
}

# The stock code path: a RippleDrawable drawn on a Canvas that is NOT hardware
# accelerated, on a thread that HAS a Looper (what ActivityThread.systemMain()
# leaves behind). Two native outcomes, both from Choreographer/DisplayEventReceiver:
#   * no servicemanager  -> WaitForProperty("servicemanager.ready") never returns
#   * servicemanager stub -> DisplayEventReceiver init fails -> ~NativeDisplayEventReceiver
#                            -> AndroidRuntime::getJNIEnv() -> gCurRuntime == NULL -> SIGSEGV
run_segv() {
    echo "== segv: mode=plain, servicemanager.ready served by libq3prop =="
    gdb_script "$ACOS_ROOT/.cache/art-exp-q3/segv.gdb" \
        'handle SIGSEGV stop print nopass
continue
printf "\n==== SIGSEGV CAUGHT ====\n"
info registers rip rsp rdi rsi rdx
x/i $pc
bt 25
printf "==== END ====\n"'
    (
        E2D_PRELOAD="/data/lib/libq3prop.so:/data/lib/libbindershim.so:/data/lib/libcrashtrace.so" \
        Q3_MODE=plain Q3_WAIT=20000 Q3_GUARD_TIME=180 "$Q3/scripts/run.sh" draw \
            >"$A/q3-segv-crashtrace.log" 2>&1
    ) &
    local run=$!
    if wait_for "$A/q3-segv-crashtrace.log" "WAIT pid=" 40; then
        local pid
        pid=$(sed -n 's/.*WAIT pid=\([0-9]*\).*/\1/p' "$A/q3-segv-crashtrace.log")
        echo "  attaching gdb to $pid (comm=$(cat /proc/$pid/comm 2>/dev/null))"
        timeout 120 gdb -p "$pid" -x "$ACOS_ROOT/.cache/art-exp-q3/segv.gdb" -batch \
            2>&1 | grep -v "^warning: section" >"$A/q3-segv-gdb.txt"
        # the inferior is left stopped at the fault; let the run finish normally
        kill -CONT "$pid" 2>/dev/null
    fi
    wait $run
    sed -n '/Q3-CRASHTRACE/,/end Q3-CRASHTRACE/p' "$A/q3-segv-crashtrace.log" >>"$A/q3-segv-gdb.txt"
    sed -n '/SIGSEGV CAUGHT/,/END/p' "$A/q3-segv-gdb.txt" | head -25
}

run_hang() {
    echo "== hang: mode=pressed, plain servicemanager (= E4H's environment) =="
    gdb_script "$ACOS_ROOT/.cache/art-exp-q3/hang.gdb" 'thread 1
bt 40'
    (
        Q3_MODE=pressed Q3_GUARD_TIME=90 "$Q3/scripts/run.sh" draw \
            >"$A/q3-hang.log" 2>&1
    ) &
    local run=$!
    if wait_for "$A/q3-hang.log" "inc pressed=true" 60; then
        sleep 2
        local pid
        pid=$(find_guest_pid)
        echo "  attaching gdb to $pid"
        timeout 120 gdb -p "$pid" -x "$ACOS_ROOT/.cache/art-exp-q3/hang.gdb" -batch \
            2>&1 | grep -v "^warning: section" >"$A/q3-hang-gdb.txt"
        kill -TERM "$pid" 2>/dev/null
    fi
    wait $run
    sed -n '/^#0 /,$p' "$A/q3-hang-gdb.txt" | head -22
}

case "${1:-all}" in
    demo) run_demo ;;
    segv) run_segv ;;
    hang) run_hang ;;
    all) run_demo; run_segv; run_hang ;;
    *) echo "usage: $0 demo|segv|hang|all" >&2; exit 2 ;;
esac
