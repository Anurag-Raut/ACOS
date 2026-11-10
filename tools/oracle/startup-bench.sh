#!/usr/bin/env bash
#
# startup-bench.sh — ACOS startup latency / memory for 01_hello.
#
# Runs `acos run-dex` N times per corelib mode and reports p50/p95 for
#   wall_ms     total process work measured by the CLI
#   vm_new_ms   boot-classpath open + VM bring-up (Vm::new)
#   run_ms      first user bytecode through exit
#   peak_rss_kb Linux VmHWM
#
# Usage: tools/oracle/startup-bench.sh [N] [--out DIR]
# Writes results/m1/startup.md (and .json alongside).
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
N="${1:-20}"
OUT="$ROOT/results/m1"
BIN="$ROOT/target/release/acos"
PROG="$ROOT/fixtures/build/dexprogs/01_hello/classes.dex"
mkdir -p "$OUT"

if [ ! -x "$BIN" ]; then
    (cd "$ROOT" && cargo build --release -p acos-cli) || exit 2
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

for mode in real bootstrap; do
    for i in $(seq 1 "$N"); do
        "$BIN" run-dex "$PROG" --main Main --corelib "$mode" --json \
            >"$TMP/$mode-$i.json" 2>/dev/null
    done
done

python3 - "$TMP" "$OUT" "$N" <<'PY'
import glob, json, os, statistics, sys

tmp, out, n = sys.argv[1], sys.argv[2], int(sys.argv[3])

def pct(xs, p):
    if not xs:
        return 0
    xs = sorted(xs)
    k = min(len(xs) - 1, int(round((p / 100.0) * (len(xs) - 1))))
    return xs[k]

report = {}
for mode in ("real", "bootstrap"):
    rows = []
    for path in glob.glob(os.path.join(tmp, f"{mode}-*.json")):
        try:
            d = json.load(open(path))
        except Exception:
            continue
        if not d.get("ok"):
            continue
        m = d.get("metrics", {})
        rows.append({k: m.get(k, 0) for k in
                     ("wall_ms", "vm_new_ms", "run_ms", "peak_rss_kb")})
    report[mode] = {}
    for key in ("wall_ms", "vm_new_ms", "run_ms", "peak_rss_kb"):
        vals = [r[key] for r in rows]
        report[mode][key] = {
            "n": len(vals),
            "p50": pct(vals, 50),
            "p95": pct(vals, 95),
            "min": min(vals) if vals else 0,
            "max": max(vals) if vals else 0,
        }

# ART measured on the reference emulator (research/dynamic-trace/REPORT.md):
ART_WALL_P50 = 71

lines = []
lines.append("# M1 startup: 01_hello on the real core library\n")
lines.append(f"N = {n} runs per mode, release build (`target/release/acos`), "
             "same host, no emulator.\n")
lines.append("| stage | real p50 | real p95 | bootstrap p50 | bootstrap p95 |")
lines.append("|---|---:|---:|---:|---:|")
labels = {
    "wall_ms": "total wall (CLI)",
    "vm_new_ms": "boot-classpath open + VM bring-up (`Vm::new`)",
    "run_ms": "first user bytecode -> exit",
    "peak_rss_kb": "peak RSS (KiB, VmHWM)",
}
for key, label in labels.items():
    r = report["real"][key]
    b = report["bootstrap"][key]
    lines.append(f"| {label} | {r['p50']} | {r['p95']} | {b['p50']} | {b['p95']} |")
lines.append("")
lines.append("| comparison | wall p50 (ms) | ratio vs ART |")
lines.append("|---|---:|---:|")
lines.append(f"| real Android 14 ART (`dalvikvm64` hello, reference emulator) | {ART_WALL_P50} | 1.00x |")
lines.append(f"| ACOS `--corelib=real` (this host) | {report['real']['wall_ms']['p50']} | "
             f"{report['real']['wall_ms']['p50'] / ART_WALL_P50:.2f}x |")
lines.append(f"| ACOS `--corelib=bootstrap` (this host) | {report['bootstrap']['wall_ms']['p50']} | "
             f"{report['bootstrap']['wall_ms']['p50'] / ART_WALL_P50:.2f}x |")
lines.append("")
lines.append("Notes: the ART number is a cold `dalvikvm64` hello on an x86_64 "
             "KVM emulator with a pre-initialised boot image. ACOS has no boot "
             "image and runs `<clinit>` from scratch, so the real-corelib "
             "number includes class loading and static initialisation that ART "
             "amortises into the image.")
lines.append("")

md = "\n".join(lines) + "\n"
open(os.path.join(out, "startup.md"), "w").write(md)
json.dump(report, open(os.path.join(out, "startup.json"), "w"), indent=2)
print(md)
PY
