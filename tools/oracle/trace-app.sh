#!/usr/bin/env bash
#
# trace-app.sh — measure what a real Activity launch and interaction execute on
# ART (API 34, x86_64, KVM emulator), for the ACOS fixture APKs.
#
# hello-views:
#   * launch trace:  am start -S -W -P <file>   (full instrumentation, stops on idle)
#   * interaction:   am profile start (full) -> tap input, type World, tap Greet, Back
#                    -> am profile stop
# hello-jni:
#   * launch trace:  am start -S -W -P <file>
#
# For each trace it writes methods/natives/classes TSVs + JSON, a binder work-list
# TSV, a timeline JSON, and channel hits.  It also captures:
#   * untraced cold/warm am start -W timings (x10 each, p50/p95)
#   * dumpsys meminfo -d x3 at idle after first frame
#   * /proc/<pid>/maps and /proc/<pid>/fd snapshots
#
# Raw traces and raw dumps go to .cache/trace/app/ (gitignored); parsed data to
# research/dynamic-trace/data/.
set -euo pipefail
export PYTHONDONTWRITEBYTECODE=1

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SDK="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-/home/Qui/Android/Sdk}}"
ADB="$SDK/platform-tools/adb"
SERIAL="${ACOS_ORACLE_SERIAL:-emulator-5554}"
DATA_DIR="$ROOT/research/dynamic-trace/data"
RAW_DIR="$ROOT/.cache/trace/app"
DEV="/data/local/tmp/acos-trace"
NATIVE_SET="$DATA_DIR/native_methods_bootclasspath.tsv"
APP_NATIVE_SET="$DATA_DIR/native_methods_app_fixtures.tsv"
ALL_NATIVE_SET="$DATA_DIR/native_methods_all.tsv"
HV_PKG="dev.acos.fixtures.helloviews"
HJ_PKG="dev.acos.fixtures.hellojni"
APPS_DIR="$ROOT/fixtures/build/apps"
TIMINGS="${ACOS_TIMING_RUNS:-10}"

log() { printf '[trace-app] %s\n' "$*"; }
die() { printf '[trace-app] error: %s\n' "$*" >&2; exit 1; }

"$ADB" -s "$SERIAL" get-state >/dev/null 2>&1 || die "no device at $SERIAL"
mkdir -p "$DATA_DIR" "$RAW_DIR"
"$ADB" -s "$SERIAL" shell "mkdir -p $DEV && chmod 777 $DEV" >/dev/null

[ -s "$NATIVE_SET" ] || die "missing $NATIVE_SET (run tools/oracle/trace-console.sh first)"

# hello-jni's own native methods (declared in the app DEX).
if [ ! -s "$APP_NATIVE_SET" ]; then
    cat >"$APP_NATIVE_SET" <<'EOF'
dev.acos.fixtures.hellojni.MainActivity	stringFromJNI	()Ljava/lang/String;
dev.acos.fixtures.hellojni.MainActivity	add	(II)I
dev.acos.fixtures.hellojni.MainActivity	sumArray	([I)I
dev.acos.fixtures.hellojni.MainActivity	callBack	()V
dev.acos.fixtures.hellojni.MainActivity	registeredEcho	(Ljava/lang/String;)Ljava/lang/String;
dev.acos.fixtures.hellojni.MainActivity	throwFromJNI	()V
EOF
fi
cat "$NATIVE_SET" "$APP_NATIVE_SET" | sort -u >"$ALL_NATIVE_SET"

# --- build / install apps ----------------------------------------------------
for apk in hello-views hello-jni; do
    [ -f "$APPS_DIR/$apk.apk" ] || { log "building apps"; bash "$ROOT/tools/build/build-apps.sh" >/dev/null; break; }
done
"$ADB" -s "$SERIAL" install -r -g "$APPS_DIR/hello-views.apk" >/dev/null
"$ADB" -s "$SERIAL" install -r -g "$APPS_DIR/hello-jni.apk" >/dev/null

wait_idle() { # <pkg>
    local pkg="$1" i=0
    while :; do
        if "$ADB" -s "$SERIAL" shell dumpsys window 2>/dev/null | grep -q "mCurrentFocus=.*$pkg"; then
            return 0
        fi
        i=$((i + 1)); [ "$i" -ge 40 ] && return 1; sleep 0.5
    done
}

parse_trace() { # <trace> <label> <app-pkg> <main-class>
    local trace="$1" label="$2" pkg="$3" maincls="$4"
    python3 "$ROOT/tools/oracle/art_trace.py" parse "$trace" \
        --native-set "$ALL_NATIVE_SET" --main-class "$maincls" \
        --label "$label" --json "$DATA_DIR/trace_$label.json" \
        --data-dir "$DATA_DIR" --top 15
    python3 "$ROOT/tools/oracle/binder_from_trace.py" "$trace" \
        --label "$label" \
        --out "$DATA_DIR/binder_calls_$label.tsv" \
        --channels-out "$DATA_DIR/channels_$label.tsv"
    python3 "$ROOT/tools/oracle/timeline_from_trace.py" "$trace" \
        --app-pkg "$pkg" --json "$DATA_DIR/timeline_$label.json"
}

# --- hello-views launch trace ------------------------------------------------
launch_trace() { # <pkg> <label>
    local pkg="$1" label="$2"
    log "=== launch trace $label ($pkg) ==="
    "$ADB" -s "$SERIAL" shell "am force-stop $pkg" >/dev/null 2>&1 || true
    "$ADB" -s "$SERIAL" shell "rm -f $DEV/$label.trace" >/dev/null 2>&1 || true
    "$ADB" -s "$SERIAL" shell \
        "am start -S -W -P $DEV/$label.trace -n $pkg/.MainActivity" \
        >"$RAW_DIR/${label}_amstart.txt" 2>&1 || true
    wait_idle "$pkg" || log "warning: $pkg not focused"
    sleep 1
    "$ADB" -s "$SERIAL" pull "$DEV/$label.trace" "$RAW_DIR/$label.trace" >/dev/null
    parse_trace "$RAW_DIR/$label.trace" "$label" "$pkg" "android.app.ActivityThread"
    # maps/fd snapshot of the live process
    local pid
    pid="$("$ADB" -s "$SERIAL" shell pidof "$pkg" 2>/dev/null | tr -d '\r')"
    if [ -n "$pid" ]; then
        "$ADB" -s "$SERIAL" shell "cat /proc/$pid/maps" >"$RAW_DIR/${label}_maps.txt" 2>/dev/null || true
        "$ADB" -s "$SERIAL" shell "ls -l /proc/$pid/fd" >"$RAW_DIR/${label}_fd.txt" 2>/dev/null || true
        "$ADB" -s "$SERIAL" shell "cat /proc/$pid/status" >"$RAW_DIR/${label}_status.txt" 2>/dev/null || true
        log "  pid=$pid maps=$(wc -l <"$RAW_DIR/${label}_maps.txt") fd=$(wc -l <"$RAW_DIR/${label}_fd.txt")"
    fi
}

# --- interaction trace -------------------------------------------------------
interaction_trace() { # <pkg> <label>
    local pkg="$1" label="$2"
    log "=== interaction trace $label ($pkg) ==="
    "$ADB" -s "$SERIAL" shell "am force-stop $pkg" >/dev/null 2>&1 || true
    "$ADB" -s "$SERIAL" shell "am start -W -n $pkg/.MainActivity" >/dev/null 2>&1 || true
    wait_idle "$pkg" || true
    sleep 1
    # coordinates from a UI dump taken before profiling starts
    "$ADB" -s "$SERIAL" shell "uiautomator dump /sdcard/window_dump.xml" >/dev/null 2>&1 || true
    "$ADB" -s "$SERIAL" exec-out cat /sdcard/window_dump.xml >"$RAW_DIR/${label}_ui.xml" 2>/dev/null || true
    local input_xy btn_xy coords
    mapfile -t coords < <(python3 - "$RAW_DIR/${label}_ui.xml" <<'PYEOF'
import re, sys
d = open(sys.argv[1]).read()
want = {":id/input": None, ":id/btn": None}
for m in re.finditer(r'<node[^>]*>', d):
    t = m.group(0)
    rid = re.search(r'resource-id="([^"]*)"', t)
    if not rid:
        continue
    for k in want:
        if want[k] is None and rid.group(1).endswith(k):
            b = re.search(r'bounds="\[(\d+),(\d+)\]\[(\d+),(\d+)\]"', t)
            if b:
                want[k] = "%d %d" % ((int(b.group(1)) + int(b.group(3))) // 2,
                                      (int(b.group(2)) + int(b.group(4))) // 2)
print(want[":id/input"] or "540 427")
print(want[":id/btn"] or "157 549")
PYEOF
)
    input_xy="${coords[0]:-540 427}"
    btn_xy="${coords[1]:-157 549}"
    log "  input_xy=$input_xy btn_xy=$btn_xy"
    "$ADB" -s "$SERIAL" shell "rm -f $DEV/$label.trace" >/dev/null 2>&1 || true
    "$ADB" -s "$SERIAL" shell "am profile start $pkg $DEV/$label.trace" >/dev/null 2>&1
    sleep 0.5
    "$ADB" -s "$SERIAL" shell "input tap $input_xy" >/dev/null 2>&1
    sleep 0.5
    "$ADB" -s "$SERIAL" shell "input text World" >/dev/null 2>&1
    sleep 0.5
    "$ADB" -s "$SERIAL" shell "input tap $btn_xy" >/dev/null 2>&1
    sleep 0.7
    "$ADB" -s "$SERIAL" shell "input keyevent KEYCODE_BACK" >/dev/null 2>&1
    sleep 0.7
    "$ADB" -s "$SERIAL" shell "am profile stop $pkg" >/dev/null 2>&1 || true
    sleep 1
    "$ADB" -s "$SERIAL" pull "$DEV/$label.trace" "$RAW_DIR/$label.trace" >/dev/null 2>&1 || \
        die "interaction trace not written"
    parse_trace "$RAW_DIR/$label.trace" "$label" "$pkg" "android.app.ActivityThread"
}

# --- memory + untraced timings ----------------------------------------------
memory_and_timings() { # <pkg> <label>
    local pkg="$1" label="$2" i pid
    log "=== memory + timings $label ==="
    : >"$RAW_DIR/${label}_meminfo.txt"
    local pss_lines=""
    for i in 1 2 3; do
        "$ADB" -s "$SERIAL" shell "am force-stop $pkg" >/dev/null 2>&1 || true
        "$ADB" -s "$SERIAL" shell "am start -W -n $pkg/.MainActivity" >/dev/null 2>&1 || true
        wait_idle "$pkg" || true
        sleep 2
        {
            echo "===== run $i ====="
            "$ADB" -s "$SERIAL" shell "dumpsys meminfo -d $pkg"
        } >>"$RAW_DIR/${label}_meminfo.txt" 2>&1 || true
        pid="$("$ADB" -s "$SERIAL" shell pidof "$pkg" 2>/dev/null | tr -d '\r')"
        pss_lines+="$pid "
    done
    # untraced cold timings
    : >"$RAW_DIR/${label}_timings.txt"
    for i in $(seq 1 "$TIMINGS"); do
        "$ADB" -s "$SERIAL" shell "am force-stop $pkg" >/dev/null 2>&1 || true
        "$ADB" -s "$SERIAL" shell "am start -W -n $pkg/.MainActivity" 2>/dev/null \
            | tr -d '\r' | grep -E 'TotalTime|WaitTime' | tr '\n' ' ' >>"$RAW_DIR/${label}_timings.txt"
        echo " cold" >>"$RAW_DIR/${label}_timings.txt"
        sleep 0.3
    done
    for i in $(seq 1 "$TIMINGS"); do
        "$ADB" -s "$SERIAL" shell "input keyevent KEYCODE_HOME" >/dev/null 2>&1 || true
        sleep 0.5
        "$ADB" -s "$SERIAL" shell "am start -W -n $pkg/.MainActivity" 2>/dev/null \
            | tr -d '\r' | grep -E 'TotalTime|WaitTime' | tr '\n' ' ' >>"$RAW_DIR/${label}_timings.txt"
        echo " warm" >>"$RAW_DIR/${label}_timings.txt"
        sleep 0.3
    done
    python3 - "$RAW_DIR/${label}_timings.txt" "$DATA_DIR/memory.json" "$label" "${label}_meminfo.txt" <<'PYEOF'
import json, math, os, re, sys
timings_path, out_path, label, meminfo_name = sys.argv[1:5]
cold, warm = [], []
for line in open(timings_path):
    m = re.search(r"TotalTime:\s*(\d+).*WaitTime:\s*(\d+)", line)
    if not m:
        continue
    vals = {"total_ms": int(m.group(1)), "wait_ms": int(m.group(2))}
    (cold if line.strip().endswith("cold") else warm).append(vals)
def pct(vals, p):
    if not vals: return None
    v = sorted(vals); i = max(0, min(len(v) - 1, math.ceil(p * len(v)) - 1)); return v[i]
doc = {}
if os.path.exists(out_path):
    try: doc = json.load(open(out_path))
    except Exception: doc = {}
entry = {
    "cold": {"runs": len(cold),
             "total_p50": pct([c["total_ms"] for c in cold], .5),
             "total_p95": pct([c["total_ms"] for c in cold], .95),
             "wait_p50": pct([c["wait_ms"] for c in cold], .5),
             "wait_p95": pct([c["wait_ms"] for c in cold], .95),
             "raw": cold},
    "warm": {"runs": len(warm),
             "total_p50": pct([c["total_ms"] for c in warm], .5),
             "total_p95": pct([c["total_ms"] for c in warm], .95),
             "wait_p50": pct([c["wait_ms"] for c in warm], .5),
             "wait_p95": pct([c["wait_ms"] for c in warm], .95),
             "raw": warm},
}
doc.setdefault("timings", {})[label] = entry
json.dump(doc, open(out_path, "w"), indent=2)
print("%s cold total p50/p95 = %s/%s ms; warm = %s/%s ms" % (
    label, entry["cold"]["total_p50"], entry["cold"]["total_p95"],
    entry["warm"]["total_p50"], entry["warm"]["total_p95"]))
PYEOF
}

# --- run ---------------------------------------------------------------------
launch_trace "$HV_PKG" hello_views_launch
interaction_trace "$HV_PKG" hello_views_interaction
launch_trace "$HJ_PKG" hello_jni_launch
memory_and_timings "$HV_PKG" hello_views

# final meminfo-derived memory.json summary for hello-views (3 runs)
python3 - "$RAW_DIR/hello_views_meminfo.txt" "$DATA_DIR/memory.json" <<'PYEOF'
import json, re, sys
raw, out = sys.argv[1:3]
doc = json.load(open(out)) if __import__("os").path.exists(out) else {}
runs = []
cur = None
for line in open(raw):
    if line.startswith("===== run"):
        cur = {"objects": {}, "categories": {}}
        runs.append(cur)
        continue
    m = re.match(r"\s*TOTAL PSS:\s*(\d+).*TOTAL RSS:\s*(\d+)", line)
    if m and cur is not None:
        cur["total_pss_kb"] = int(m.group(1)); cur["total_rss_kb"] = int(m.group(2))
        continue
    m = re.match(r"\s*(Java Heap|Native Heap|Code|Stack|Graphics|Private Other|System):\s+(\d+)\s+(\d+)\s+(\d+)\s+(\d+)\s+(\d+)\s+(\d+)\s+(\d+)\s+(\d+)\s*$", line)
    if m and cur is not None:
        cur["categories"][m.group(1)] = {"pss_kb": int(m.group(2)), "rss_kb": int(m.group(3))}
        continue
    m = re.match(r"\s*(\S+):\s+(\d+)\s+(\d+)\s+(\d+)\s*$", line)
    if m and cur is not None and m.group(1) not in ("TOTAL",):
        cur["objects"][m.group(1)] = int(m.group(2))
doc.setdefault("meminfo", {})["hello_views_idle"] = {"runs": runs}
json.dump(doc, open(out, "w"), indent=2)
print("meminfo runs:", len(runs))
for i, r in enumerate(runs):
    print("  run %d TOTAL PSS=%s KB RSS=%s KB" % (i+1, r.get("total_pss_kb"), r.get("total_rss_kb")))
PYEOF

# maps summary
python3 "$ROOT/tools/oracle/summarize_maps.py" \
    "$RAW_DIR/hello_views_launch_maps.txt" "$DATA_DIR/memory.json" \
    hello_views_after_first_frame

# --- task-named aliases + combined timeline ---------------------------------
cp -f "$DATA_DIR/natives_hello_views_launch.tsv" "$DATA_DIR/natives_activity_launch.tsv" 2>/dev/null || true
cp -f "$DATA_DIR/natives_hello_views_interaction.tsv" "$DATA_DIR/natives_activity_interaction.tsv" 2>/dev/null || true
cp -f "$DATA_DIR/binder_calls_hello_views_launch.tsv" "$DATA_DIR/binder_calls_launch.tsv" 2>/dev/null || true
cp -f "$DATA_DIR/binder_calls_hello_views_interaction.tsv" "$DATA_DIR/binder_calls_interaction.tsv" 2>/dev/null || true

python3 - "$DATA_DIR/timeline_hello_views_launch.json" "$DATA_DIR/timeline_hello_views_interaction.json" \
    "$DATA_DIR/memory.json" "$DATA_DIR/timeline.json" <<'PYEOF'
import json, os, sys
launch, interaction, mem, out = sys.argv[1:5]
tl = {}
if os.path.exists(launch):
    tl["activity_launch"] = json.load(open(launch))
if os.path.exists(interaction):
    tl["activity_interaction"] = json.load(open(interaction))
if os.path.exists(mem):
    m = json.load(open(mem))
    tl["untraced_start_timings"] = m.get("timings", {})
    tl["meminfo_idle"] = m.get("meminfo", {})
json.dump(tl, open(out, "w"), indent=2)
print("timeline.json written")
PYEOF

log "done. data in $DATA_DIR; raw in $RAW_DIR"
