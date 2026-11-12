#!/usr/bin/env bash
# Run the K1 real-world corpus through ACOS's real-ART engine.
#
# One app at a time, each in its own guarded scope under the host-wide ART lock
# (tools/art-runtime/run-exclusive.sh), so other agents (Z7, GR1, GL1, P12) get
# the lock between apps. The lock is released after every app because each app
# is one `run-exclusive.sh` invocation.
#
# Usage:
#   tools/compat/run-k1.sh                       # whole corpus from the manifest
#   tools/compat/run-k1.sh --only org.fdroid.fdroid,com.termux
#   tools/compat/run-k1.sh --limit 5             # first 5 (smoke)
#   tools/compat/run-k1.sh --time 120            # per-app wall limit (default 180)
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

ONLY=""
LIMIT=0
TIME=180
FIRST_FRAME_TIMEOUT=20
while [ $# -gt 0 ]; do
    case "$1" in
        --only) ONLY="$2"; shift 2 ;;
        --limit) LIMIT="$2"; shift 2 ;;
        --time) TIME="$2"; shift 2 ;;
        --first-frame-timeout) FIRST_FRAME_TIMEOUT="$2"; shift 2 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done

[ -x "$ACOS_BIN" ] || { echo "missing $ACOS_BIN (build: cargo build --release -p acos-cli -p acos-art)" >&2; exit 2; }
[ -x "$ACOS_ART_BIN" ] || { echo "missing $ACOS_ART_BIN" >&2; exit 2; }
[ -f "$CORPUS/manifest.json" ] || { echo "missing $CORPUS/manifest.json (run tools/compat/fetch-corpus.py)" >&2; exit 2; }

mkdir -p "$RUN"

mapfile -t APKS < <(python3 - "$CORPUS/manifest.json" "$ONLY" <<'PY'
import json, sys
m = json.load(open(sys.argv[1]))
only = set(sys.argv[2].split(",")) if sys.argv[2] else None
for app in sorted(m["apps"], key=lambda a: a["package"]):
    if only and app["package"] not in only:
        continue
    print(app["package"] + "\t" + app["name"])
PY
)

if [ "$LIMIT" -gt 0 ]; then
    APKS=("${APKS[@]:0:$LIMIT}")
fi
echo "K1: ${#APKS[@]} apps, per-app wall ${TIME}s, run dir $RUN"

i=0
for row in "${APKS[@]}"; do
    i=$((i + 1))
    pkg="${row%%$'\t'*}"
    apk="$CORPUS/apks/${row#*$'\t'}"
    out="$RUN/$pkg"
    mkdir -p "$out"
    if [ ! -f "$apk" ]; then
        echo "K1 [$i/${#APKS[@]}] $pkg: APK missing: $apk" | tee "$out/report.txt"
        continue
    fi
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
    echo "=== K1 [$i/${#APKS[@]}] $pkg (ram ${mem_pct}%, disk ${disk_pct}%) $(date -u +%FT%TZ)"
    tools/art-runtime/run-exclusive.sh --task K1 -- \
        tools/guarded.sh --mem 8G --time "$TIME" -- \
        python3 tools/compat/k1-run.py \
            --apk "$apk" --name "$pkg" --out "$out" \
            --first-frame-timeout "$FIRST_FRAME_TIMEOUT" 2>&1 | tail -40
    rc=${PIPESTATUS[0]}
    echo "=== K1 [$i/${#APKS[@]}] $pkg rc=$rc $(date -u +%FT%TZ)"
done

echo "K1: done. Aggregate with tools/compat/k1-analyze.py"
