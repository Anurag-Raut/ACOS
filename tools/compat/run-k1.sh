#!/usr/bin/env bash
# Run the K1 real-world corpus through ACOS's real-ART engine.
#
# Every guest run happens inside `tools/art-runtime/run-exclusive.sh` (the
# host-wide ART lock) and `tools/guard/guarded.sh` (hard memory + wall bounds). Each
# app gets its own `acos serve --engine art` daemon, its own session, and its
# own evidence directory; the guest is destroyed after every app.
#
# The host lock is released after every *batch* (`--batch`, default 4). A batch
# is bounded by `--time` and normally finishes in well under a minute, so other
# agents get the lock between batches. (One-app-per-acquisition is the ideal;
# with a 20-minute holder in the queue it starves every other agent, so the
# batch is the practical unit. `--batch 1` restores one app per acquisition.)
#
# Usage:
#   tools/compat/run-k1.sh                       # whole corpus from the manifest
#   tools/compat/run-k1.sh --only org.fdroid.fdroid,com.termux
#   tools/compat/run-k1.sh --limit 5 --batch 1   # smoke
#
# Env:
#   ACOS_K1_CORPUS  corpus dir (default /home/Qui/Projects/acos/.cache/corpus/k1)
#   ACOS_K1_RUN     run dir    (default /home/Qui/Projects/acos/.cache/k1/runs)
#   ACOS_BIN        acos binary  (default /home/Qui/Projects/acos/.cache/k1/bin/acos)
#   ACOS_ART_BIN    acos-art    (default /home/Qui/Projects/acos/.cache/k1/bin/acos-art)
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
CORPUS="${ACOS_K1_CORPUS:-/home/Qui/Projects/acos/.cache/corpus/k1}"
RUN="${ACOS_K1_RUN:-/home/Qui/Projects/acos/.cache/k1/runs}"
export ACOS_BIN="${ACOS_BIN:-/home/Qui/Projects/acos/.cache/k1/bin/acos}"
export ACOS_ART_BIN="${ACOS_ART_BIN:-/home/Qui/Projects/acos/.cache/k1/bin/acos-art}"
export ACOS_FONT_DIR="${ACOS_FONT_DIR:-/home/Qui/Projects/acos/.cache/boot/android-34/fonts}"
unset ACOS_BOOT_DIR

FIRST_FRAME_TIMEOUT=20
STEP_TIMEOUT=15
APP_TIMEOUT=90
BATCH=4

# ---- inner mode: one batch, already inside the lock ----------------------
if [ "${1:-}" = "--inner" ]; then
    shift
    for item in "$@"; do
        pkg="${item%%=*}"
        apk="$CORPUS/apks/${item#*=}"
        out="$RUN/$pkg"
        mkdir -p "$out"
        if [ ! -f "$apk" ]; then
            echo "--- K1 $pkg missing APK $apk"
            continue
        fi
        echo "--- K1 $pkg $(date -u +%FT%TZ)"
        timeout -k 5 "$APP_TIMEOUT" python3 tools/compat/k1-run.py \
            --apk "$apk" --name "$pkg" --out "$out" \
            --first-frame-timeout "$FIRST_FRAME_TIMEOUT" --step-timeout "$STEP_TIMEOUT"
        rc=$?
        echo "--- K1 $pkg rc=$rc $(date -u +%FT%TZ)"
    done
    exit 0
fi

# ---- outer mode -----------------------------------------------------------
ONLY=""
LIMIT=0
TIME=0
while [ $# -gt 0 ]; do
    case "$1" in
        --only) ONLY="$2"; shift 2 ;;
        --limit) LIMIT="$2"; shift 2 ;;
        --batch) BATCH="$2"; shift 2 ;;
        --time) TIME="$2"; shift 2 ;;
        --first-frame-timeout) FIRST_FRAME_TIMEOUT="$2"; shift 2 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done

[ -x "$ACOS_BIN" ] || { echo "missing $ACOS_BIN (build: cargo build --release -p acos-cli -p acos-art)" >&2; exit 2; }
[ -x "$ACOS_ART_BIN" ] || { echo "missing $ACOS_ART_BIN" >&2; exit 2; }
[ -f "$CORPUS/manifest.json" ] || { echo "missing $CORPUS/manifest.json (run tools/compat/fetch-corpus.py)" >&2; exit 2; }
[ "$BATCH" -ge 1 ] || BATCH=1
# One app's worst case is APP_TIMEOUT; leave room for daemon start/teardown.
[ "$TIME" -gt 0 ] || TIME=$((APP_TIMEOUT * BATCH + 60))

mkdir -p "$RUN"

mapfile -t APKS < <(python3 - "$CORPUS/manifest.json" "$ONLY" <<'PY'
import json, sys
m = json.load(open(sys.argv[1]))
only = set(sys.argv[2].split(",")) if sys.argv[2] else None
for app in sorted(m["apps"], key=lambda a: a["package"]):
    if only and app["package"] not in only:
        continue
    print(app["package"] + "=" + app["name"])
PY
)
if [ "$LIMIT" -gt 0 ]; then
    APKS=("${APKS[@]:0:$LIMIT}")
fi

total=${#APKS[@]}
echo "K1: $total apps, batch=$BATCH, batch wall ${TIME}s, run dir $RUN"

i=0
while [ "$i" -lt "$total" ]; do
    # Host resource gates: never start a guest when the host is already hot.
    disk_pct=$(df --output=pcent / | tail -1 | tr -dc '0-9')
    if [ "${disk_pct:-0}" -gt 85 ]; then
        echo "K1: / is ${disk_pct}% full (>85%), stopping" | tee -a "$RUN/STOPPED"
        break
    fi
    mem_pct=$(awk '/MemTotal/{t=$2} /MemAvailable/{a=$2} END{printf "%d", (t-a)*100/t}' /proc/meminfo)
    if [ "${mem_pct:-0}" -ge 90 ]; then
        echo "K1: host RAM ${mem_pct}% used (>=90%), stopping" | tee -a "$RUN/STOPPED"
        break
    fi

    chunk=()
    for ((j = i; j < i + BATCH && j < total; j++)); do
        chunk+=("${APKS[$j]}")
    done
    first_pkg="${chunk[0]%%=*}"
    echo "=== K1 batch [$((i + 1))-$((i + ${#chunk[@]}))/$total] first=$first_pkg ram=${mem_pct}% disk=${disk_pct}% $(date -u +%FT%TZ)"
    tools/art-runtime/run-exclusive.sh --task K1 -- \
        tools/guard/guarded.sh --mem 8G --time "$TIME" -- \
        bash "$0" --inner "${chunk[@]}" 2>&1 | tail -60 || true
    echo "=== K1 batch done [$((i + 1))-$((i + ${#chunk[@]}))/$total] $(date -u +%FT%TZ)"
    i=$((i + ${#chunk[@]}))
done

echo "K1: done. Aggregate with tools/compat/k1-analyze.py"
