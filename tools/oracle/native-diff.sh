#!/usr/bin/env bash
#
# native-diff.sh — compare the set of natives ACOS invokes on the real core
# library against the measured real-ART traces for 01/17/19.
#
# Usage:
#   tools/oracle/native-diff.sh [--corelib bootstrap|real] [--strict]
#
# Writes results/m1/native-diff/<program>.tsv (ACOS trace) and .report.txt,
# plus a combined results/m1/native-diff/summary.txt.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
CORELIB="real"
STRICT=""
while [ $# -gt 0 ]; do
    case "$1" in
        --corelib) CORELIB="$2"; shift ;;
        --strict) STRICT="--strict" ;;
        *) echo "native-diff: unknown argument: $1" >&2; exit 2 ;;
    esac
    shift
done

BIN="$ROOT/target/release/acos"
FIX="$ROOT/fixtures/build/dexprogs"
DATA="$ROOT/research/dynamic-trace/data"
OUT="$ROOT/results/m1/native-diff"
mkdir -p "$OUT"

if [ ! -x "$BIN" ]; then
    echo "native-diff: building acos (release)..." >&2
    (cd "$ROOT" && cargo build --release -p acos-cli) || exit 2
fi

declare -A ART_FOR=( [01_hello]=natives_println.tsv
                     [17_generics_collections]=natives_17_generics_collections.tsv
                     [19_threads_sync]=natives_19_threads_sync.tsv )

: >"$OUT/summary.txt"
status=0
for prog in 01_hello 17_generics_collections 19_threads_sync; do
    art="$DATA/${ART_FOR[$prog]}"
    tsv="$OUT/$prog.tsv"
    report="$OUT/$prog.report.txt"
    "$BIN" run-dex "$FIX/$prog/classes.dex" --main Main --corelib "$CORELIB" \
        --trace-natives-file "$tsv" >/dev/null 2>&1
    if [ ! -f "$tsv" ]; then
        echo "$prog: no trace produced" | tee -a "$OUT/summary.txt"
        status=1
        continue
    fi
    python3 "$ROOT/tools/oracle/native_diff.py" \
        --acos "$tsv" --art "$art" --label "$prog ($CORELIB)" $STRICT \
        | tee "$report"
    echo "" >>"$OUT/summary.txt"
    cat "$report" >>"$OUT/summary.txt"
    if [ -n "$STRICT" ] && ! grep -q "never invoked: none" "$report"; then
        status=1
    fi
done

echo "native-diff: reports under $OUT"
exit "$status"
