#!/usr/bin/env bash
#
# art-perf.sh — record ART-side wall-time reference for later ACOS benchmarks.
#
# Runs the `01_hello` console DEX 20 times on the API-34 emulator and measures
# each `dalvikvm64` invocation ON DEVICE (nanosecond clock before/after, so adb
# overhead is excluded).  If a benchmark program exists under
# crates/acos-vm/benches/java/ (another agent is creating it) it is measured the
# same way; otherwise it is recorded as absent.
#
# Writes results/oracle/art-perf.json.  Labels: emulator under KVM, x86_64,
# interpreter/JIT mode as-is (default ART).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SDK="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-/home/Qui/Android/Sdk}}"
ADB="$SDK/platform-tools/adb"
SERIAL="${ACOS_ORACLE_SERIAL:-emulator-5554}"
RESULT_DIR="$ROOT/results/oracle"
OUT="$RESULT_DIR/art-perf.json"
DEVICE_DIR="/data/local/tmp/acos-perf"
RUNS="${ACOS_PERF_RUNS:-20}"

log() { printf '[art-perf] %s\n' "$*"; }
die() { printf '[art-perf] error: %s\n' "$*" >&2; exit 1; }

"$ADB" -s "$SERIAL" get-state >/dev/null 2>&1 || die "no device at $SERIAL (start it: tools/oracle/emulator.sh start)"
mkdir -p "$RESULT_DIR"

HELLO_DEX="$ROOT/fixtures/build/dexprogs/01_hello/classes.dex"
[ -f "$HELLO_DEX" ] || die "missing $HELLO_DEX (run tools/build-fixtures.sh)"

BENCH_DEX="$(find "$ROOT/crates/acos-vm/benches/java" -name '*.dex' -print -quit 2>/dev/null || true)"
BENCH_CLASS=""
if [ -n "$BENCH_DEX" ]; then
    # Prefer a class literally named Main; else the first class-ish dir name.
    if unzip -l "$BENCH_DEX" >/dev/null 2>&1; then :; fi
    BENCH_CLASS="Main"
fi

push_and_run() { # <name> <dex> <runs>  -> prints "<exit> <ms>" per run
    local name="$1" dex="$2" runs="$3"
    "$ADB" -s "$SERIAL" shell "rm -rf $DEVICE_DIR/$name && mkdir -p $DEVICE_DIR/$name" >/dev/null
    "$ADB" -s "$SERIAL" push "$dex" "$DEVICE_DIR/$name/classes.dex" >/dev/null
    local i
    for i in $(seq 1 "$runs"); do
        "$ADB" -s "$SERIAL" shell \
            "cd $DEVICE_DIR/$name && s=\$(date +%s%N); dalvikvm64 -cp classes.dex ${BENCH_CLASS:-Main} >/dev/null 2>&1; rc=\$?; e=\$(date +%s%N); echo \"\$rc \$(( (e-s)/1000000 ))\"" \
            2>/dev/null | tr -d '\r'
    done
}

log "measuring 01_hello: $RUNS runs on $SERIAL"
hello_raw="$(push_and_run hello "$HELLO_DEX" "$RUNS")"

bench_raw=""
if [ -n "$BENCH_DEX" ]; then
    log "measuring benchmark: $BENCH_DEX ($RUNS runs)"
    bench_raw="$(push_and_run bench "$BENCH_DEX" "$RUNS")"
else
    log "no benchmark dex under crates/acos-vm/benches/java/ — recording as absent"
fi

# Device/environment facts.
props="$("$ADB" -s "$SERIAL" shell 'getprop ro.build.type; getprop ro.product.cpu.abi; getprop ro.build.version.sdk; getprop dalvik.vm.usejit; getprop dalvik.vm.jitthreshold; getprop dalvik.vm.isa.x86_64.variant; getprop dalvik.vm.dex2oat-filter' 2>/dev/null | tr -d '\r' || true)"

HELLO_RAW="$hello_raw" BENCH_RAW="$bench_raw" BENCH_DEX="$BENCH_DEX" PROPS="$props" ACOS_ROOT="$ROOT" \
    "$(command -v python3)" - "$OUT" <<'PYEOF'
import json, os, sys

out = sys.argv[1]
props = os.environ.get("PROPS", "").splitlines()
props = props + [""] * (7 - len(props))

def parse(raw):
    runs = []
    for line in raw.splitlines():
        line = line.strip()
        if not line:
            continue
        parts = line.split()
        if len(parts) != 2:
            continue
        try:
            rc = int(parts[0])
            ms = int(parts[1])
        except ValueError:
            continue
        runs.append({"exit": rc, "wall_ms": ms})
    return runs

def pct(values, p):
    if not values:
        return None
    vals = sorted(values)
    import math
    idx = max(0, min(len(vals) - 1, math.ceil(p * len(vals)) - 1))
    return vals[idx]

hello = parse(os.environ.get("HELLO_RAW", ""))
bench = parse(os.environ.get("BENCH_RAW", ""))
bench_dex = os.environ.get("BENCH_DEX", "")

def summarize(runs):
    ms = [r["wall_ms"] for r in runs]
    return {
        "runs": len(ms),
        "wall_ms": ms,
        "p50_ms": pct(ms, 0.50),
        "p95_ms": pct(ms, 0.95),
        "min_ms": min(ms) if ms else None,
        "max_ms": max(ms) if ms else None,
        "exit_codes": sorted(set(r["exit"] for r in runs)),
    }

doc = {
    "fidelity": "art-reference",
    "label": "emulator under KVM, x86_64, interpreter/JIT mode as-is (default ART)",
    "note": "Wall time measured ON DEVICE with `date +%s%N` around the dalvikvm64 process; adb overhead excluded.",
    "device": {
        "serial": "emulator-5554",
        "build_type": props[0],
        "abi": props[1],
        "sdk": props[2],
        "dalvik.vm.usejit": props[3],
        "dalvik.vm.jitthreshold": props[4],
        "dalvik.vm.isa.x86_64.variant": props[5],
        "dalvik.vm.dex2oat-filter": props[6],
    },
    "programs": {
        "01_hello": dict({"invocation": "dalvikvm64 -cp classes.dex Main"}, **summarize(hello)),
    },
    "benchmark": (
        dict({"source_dex": os.path.relpath(bench_dex, os.environ.get("ACOS_ROOT", ".")) if bench_dex else None,
              "invocation": "dalvikvm64 -cp classes.dex " + ("Main" if bench_dex else "")},
             **summarize(bench))
        if bench_dex else
        {"present": False, "note": "crates/acos-vm/benches/java/ had no built .dex at measurement time"}
    ),
    "generated_by": "tools/oracle/art-perf.sh",
}
with open(out, "w") as f:
    json.dump(doc, f, indent=2)
    f.write("\n")
print(json.dumps({"hello": summarize(hello), "benchmark_present": bool(bench_dex)}, indent=2))
PYEOF

log "wrote $OUT"
