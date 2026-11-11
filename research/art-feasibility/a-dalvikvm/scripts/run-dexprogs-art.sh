#!/usr/bin/env bash
# run-dexprogs-art.sh -- Step 2 of the ART feasibility experiment: run the 27
# ACOS DEX correctness programs under *real Google ART* (dalvikvm64 from the
# Android 14 APEX, inside the bwrap mini-Android root) and diff stdout + exit
# code against fixtures/expected-art/dexprogs (recorded from real ART on a
# device) and fixtures/expected/dexprogs (JVM reference).
#
# Usage: scripts/run-dexprogs-art.sh [--mode default|int] [--json OUT]
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HERE/env.sh"
ROOT="$ACOS_ROOT"
DATA="$ANDROID_DATA"
MODE=default
JSON="$ROOT/results/art-feasibility/dexprogs-art-$MODE.json"
while [ $# -gt 0 ]; do
    case "$1" in
        --mode) MODE="$2"; JSON="$ROOT/results/art-feasibility/dexprogs-art-$MODE.json"; shift ;;
        --json) JSON="$2"; shift ;;
        *) echo "unknown arg $1" >&2; exit 2 ;;
    esac
    shift
done

EXTRA=()
[ "$MODE" = int ] && EXTRA+=(-Xint)

mkdir -p "$DATA/dex" "$(dirname "$JSON")"
pass=0; fail=0; rows=""
for d in "$ROOT"/fixtures/build/dexprogs/*/; do
    name="$(basename "$d")"
    cp -r "$d" "$DATA/dex/$name" 2>/dev/null
    out="$DATA/out-$name.txt"; err="$DATA/err-$name.txt"
    start=$(date +%s%N)
    "$HERE/artroot.sh" dalvikvm "${EXTRA[@]}" -cp "/data/dex/$name/classes.dex" Main \
        >"$out" 2>"$err"
    rc=$?
    ms=$(( ($(date +%s%N) - start) / 1000000 ))
    exp_out="$ROOT/fixtures/expected-art/dexprogs/$name.stdout"
    exp_exit="$(cat "$ROOT/fixtures/expected-art/dexprogs/$name.exit" 2>/dev/null | tr -d '[:space:]')"
    [ -n "$exp_exit" ] || exp_exit=0
    stdout_ok=no; diff -q "$out" "$exp_out" >/dev/null 2>&1 && stdout_ok=yes
    exit_ok=no; [ "$rc" = "$exp_exit" ] && exit_ok=yes
    if [ "$stdout_ok" = yes ] && [ "$exit_ok" = yes ]; then
        pass=$((pass+1)); status=PASS
    else
        fail=$((fail+1)); status=FAIL
    fi
    printf '%-28s %-4s stdout=%s exit=%s/%s %6sms\n' "$name" "$status" "$stdout_ok" "$rc" "$exp_exit" "$ms"
    rows="$rows${rows:+,}{\"prog\":\"$name\",\"pass\":$([ $status = PASS ] && echo true || echo false),\"stdout_ok\":$([ $stdout_ok = yes ] && echo true || echo false),\"exit\":$rc,\"expected_exit\":$exp_exit,\"ms\":$ms}"
done
printf '\n%d pass, %d fail of 27 (ART mode=%s)\n' "$pass" "$fail" "$MODE"
printf '{"engine":"art-dalvikvm64","mode":"%s","pass":%d,"fail":%d,"programs":[%s]}\n' \
    "$MODE" "$pass" "$fail" "$rows" >"$JSON"
echo "wrote $JSON"
[ "$fail" -eq 0 ]
