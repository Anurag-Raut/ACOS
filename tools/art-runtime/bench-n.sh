#!/usr/bin/env bash
#
# bench-n.sh -- Q1 baseline: N concurrent independent Android-dev task runs.
#
#   tools/art-runtime/bench-n.sh <art|vm> <N> [--reps R] [--out DIR] [--sample-ms MS]
#
# Engines
#   art  Real ART route: `acos-art render-app --apk fixtures/build/apps/counter.apk`
#        (one-shot). Each agent gets its own seeded --data dir (copied from
#        .cache/art/data, sockets excluded) so concurrent runs do not share /data.
#        Verified = render outcome "ok" AND tools/art-render-app-compare.py exit 0
#        (full-frame SSIM >= 0.99 and every structural node exact).
#   vm   Rust VM route: `acos verify --scenario scenarios/v1-counter.yaml`
#        (counter.apk, 13 scenario steps). Each agent gets its own ACOS_SESSION_ROOT.
#        Verified = verdict "pass" in the --json envelope.
#
# Every agent process is started through tools/guarded.sh --mem 4G --time 300, so a
# cohort of N is bounded at N x 4 GiB (< 50 GiB for N = 10) by cgroup MemoryMax.
#
# Concurrency model: the N agents of one cohort run concurrently (that is the
# measured condition). Cohorts run strictly one after another (never overlapping),
# and a cohort refuses to start when host used RAM >= MAX_USED% (default 80, of
# MemTotal, using MemAvailable).
#
# Memory: a Python sampler (python3 -I) walks each cohort's process trees every
# SAMPLE_MS (default 20 ms) and sums /proc/<pid>/smaps_rollup Pss and VmRSS over
# all descendants of the agents' subshells. Peak = max over samples. The
# headline integral is GiB-minutes = trapezoid integral of total Pss over the cohort
# window [first launch, last exit]. Sampling is discrete: runs shorter than one
# period are under-sampled, which biases Pss-integrals low; see the caveats in
# results/art-runtime/Q1-baselines.md.
#
# Metric: tasks per GiB-minute = verified successes / GiB-minutes, per cohort;
# the summary reports the median over repetitions.
#
# Output (default .cache/art-bench/<engine>-N<N>-<stamp>/):
#   rep-<k>/summary.json            per-cohort metrics
#   rep-<k>/agent-<i>/              per-agent stdout/stderr/rc/verified/wall
#   rep-<k>/samples.jsonl           sampler output (t, pss_kb, rss_kb, n)
#   summary.json                    all reps + medians
#
# Scratch lives under .cache/ (never /tmp).

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT" || exit 1

usage() { sed -n '2,50p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; }

ENGINE="${1:-}"
N="${2:-}"
if [ -z "$ENGINE" ] || [ -z "$N" ]; then usage >&2; exit 2; fi
case "$ENGINE" in art|vm) ;; *) echo "bench-n: engine must be art or vm" >&2; exit 2 ;; esac
case "$N" in ''|*[!0-9]*) echo "bench-n: N must be a positive integer" >&2; exit 2 ;; esac
[ "$N" -ge 1 ] || { echo "bench-n: N must be >= 1" >&2; exit 2; }
shift 2

REPS=1
OUT=""
SAMPLE_MS=20
MAX_USED=80
while [ $# -gt 0 ]; do
    case "$1" in
        --reps)      REPS="$2"; shift 2 ;;
        --out)       OUT="$2"; shift 2 ;;
        --sample-ms) SAMPLE_MS="$2"; shift 2 ;;
        --max-used)  MAX_USED="$2"; shift 2 ;;
        -h|--help)   usage; exit 0 ;;
        *) echo "bench-n: unknown option $1" >&2; exit 2 ;;
    esac
done

STAMP="$(date +%Y%m%d-%H%M%S)"
[ -n "$OUT" ] || OUT="$ROOT/.cache/art-bench/${ENGINE}-N${N}-${STAMP}"
mkdir -p "$OUT"

ART_BIN="$ROOT/target/release/acos-art"
VM_BIN="$ROOT/target/release/acos"
ART_APK="fixtures/build/apps/counter.apk"
VM_SCENARIO="scenarios/v1-counter.yaml"
ART_SEED="$ROOT/.cache/art/data"

check_ram() {
    local total avail used
    read -r total avail < <(awk '/^MemTotal:/{t=$2} /^MemAvailable:/{a=$2} END{print t, a}' /proc/meminfo)
    used=$(( (total - avail) * 100 / total ))
    if [ "$used" -ge "$MAX_USED" ]; then
        echo "bench-n: host RAM used ${used}% >= ${MAX_USED}%; refusing to start cohort" >&2
        return 1
    fi
    echo "bench-n: host RAM used ${used}% (limit ${MAX_USED}%)"
}

seed_art_data() {
    local dst="$1"
    mkdir -p "$dst"
    if [ -d "$ART_SEED" ]; then
        # Only the static render inputs; per-run session/out/socket state is not copied.
        tar -C "$ART_SEED" --exclude='./session' --exclude='./out' --exclude='./socket' \
            --exclude='*.sock' -cf - . 2>/dev/null | tar -C "$dst" -xf - 2>/dev/null
    fi
}

# Runs one agent in the foreground of its subshell. Writes rc, verified, wall.
run_agent() {
    local engine="$1" a="$2" rc=0 t0 t1 verified=0
    t0="$(date +%s.%N)"
    if [ "$engine" = art ]; then
        tools/guarded.sh --mem 4G --time 300 -- "$ART_BIN" render-app \
            --apk "$ART_APK" --data "$a/data" --out "$a/out" --json \
            >"$a/stdout.json" 2>"$a/stderr.txt"
        rc=$?
        echo "$t0 $(date +%s.%N)" >"$a/render_wall"
        local outcome
        outcome="$(python3 -I -c 'import json,sys
try:
    print(json.load(open(sys.argv[1])).get("outcome",""))
except Exception:
    print("")' "$a/stdout.json")"
        if [ "$rc" -eq 0 ] && [ "$outcome" = ok ] && [ -f "$a/out/frame.png" ]; then
            python3 "$ROOT/tools/art-render-app-compare.py" \
                --png "$a/out/frame.png" --tree "$a/out/tree.json" --app counter \
                --out "$a/compare.json" >"$a/compare.stdout" 2>"$a/compare.stderr"
            [ $? -eq 0 ] && verified=1
        fi
    else
        ACOS_SESSION_ROOT="$a/session-root" tools/guarded.sh --mem 4G --time 300 -- \
            "$VM_BIN" verify --scenario "$VM_SCENARIO" --apk "$ART_APK" \
            --artifacts "$a/artifacts" --json \
            >"$a/stdout.json" 2>"$a/stderr.txt"
        rc=$?
        local verdict
        # Envelope: {"ok": true, "result": {"verdict": {"verdict": "pass", ...}}}
        verdict="$(python3 -I -c 'import json,sys
try:
    d = json.load(open(sys.argv[1]))
    print(d["result"]["verdict"]["verdict"] if d.get("ok") is True else "")
except Exception:
    print("")' "$a/stdout.json")"
        [ "$rc" -eq 0 ] && [ "$verdict" = pass ] && verified=1
    fi
    t1="$(date +%s.%N)"
    echo "$rc" >"$a/rc"
    echo "$verified" >"$a/verified"
    echo "$t0 $t1" >"$a/wall"
}

run_cohort() {
    local dir="$1" sampler_pid i a t0 t1 pids=()
    mkdir -p "$dir"
    check_ram || return 1
    : >"$dir/roots.pids"
    : >"$dir/samples.jsonl"
    rm -f "$dir/stop"

    # Seed per-agent state BEFORE the timed window (not part of the measurement).
    for i in $(seq 1 "$N"); do
        a="$dir/agent-$i"
        mkdir -p "$a"
        if [ "$ENGINE" = art ]; then seed_art_data "$a/data"; fi
    done

    python3 -I - "$dir/samples.jsonl" "$SAMPLE_MS" "$dir/roots.pids" "$dir/stop" <<'PY' &
import json, os, sys, time
out, sample_ms, pidfile, stopfile = sys.argv[1], int(sys.argv[2]), sys.argv[3], sys.argv[4]
period = sample_ms / 1000.0

def children_map():
    m = {}
    for name in os.listdir("/proc"):
        if not name.isdigit():
            continue
        try:
            with open(f"/proc/{name}/stat") as f:
                s = f.read()
            ppid = int(s.rsplit(")", 1)[1].split()[1])
            m.setdefault(ppid, []).append(int(name))
        except (OSError, ValueError, IndexError):
            pass
    return m

def mem(pid):
    pss = rss = 0
    try:
        with open(f"/proc/{pid}/smaps_rollup") as f:
            for line in f:
                if line.startswith("Pss:"):
                    pss = int(line.split()[1])
    except OSError:
        pass
    try:
        with open(f"/proc/{pid}/status") as f:
            for line in f:
                if line.startswith("VmRSS:"):
                    rss = int(line.split()[1])
    except OSError:
        pass
    return pss, rss

with open(out, "w") as o:
    while not os.path.exists(stopfile):
        roots = []
        try:
            with open(pidfile) as f:
                roots = [int(x) for x in f.read().split() if x.isdigit()]
        except OSError:
            pass
        cm = children_map()
        seen, stack = set(), list(roots)
        while stack:
            p = stack.pop()
            if p in seen:
                continue
            seen.add(p)
            stack.extend(cm.get(p, []))
        pss_sum = rss_sum = 0
        for p in seen:
            pss, rss = mem(p)
            pss_sum += pss
            rss_sum += rss
        o.write(json.dumps({"t": time.time(), "pss_kb": pss_sum,
                            "rss_kb": rss_sum, "n": len(seen)}) + "\n")
        o.flush()
        time.sleep(period)
PY
    sampler_pid=$!

    t0="$(date +%s.%N)"
    for i in $(seq 1 "$N"); do
        a="$dir/agent-$i"
        run_agent "$ENGINE" "$a" &
        pids+=("$!")
        echo "$!" >>"$dir/roots.pids"
    done
    wait "${pids[@]}"
    t1="$(date +%s.%N)"
    touch "$dir/stop"
    wait "$sampler_pid"

    python3 -I - "$dir" "$N" "$t0" "$t1" "$ENGINE" "$ROOT" <<'PY'
import json, os, statistics, sys
sys.path.insert(0, os.path.join(sys.argv[6], "tools"))
import benchlib
# W29: every result row carries the build profile + full binary sha256.
profile = "release"
binaries = [os.path.join(sys.argv[6], "target/release/acos-art"),
            os.path.join(sys.argv[6], "target/release/acos")]
bin_stamp = [benchlib.binary_provenance(b, profile) for b in binaries]
d, n, t0, t1, engine = sys.argv[1], int(sys.argv[2]), float(sys.argv[3]), float(sys.argv[4]), sys.argv[5]
t0, t1 = float(t0), float(t1)
samples = []
with open(os.path.join(d, "samples.jsonl")) as f:
    for line in f:
        line = line.strip()
        if line:
            samples.append(json.loads(line))
win = [s for s in samples if t0 <= s["t"] <= t1]
# Trapezoid integral of total Pss (GiB) over time (minutes).
gib_min = 0.0
for a, b in zip(win, win[1:]):
    dt_min = (b["t"] - a["t"]) / 60.0
    gib_min += ((a["pss_kb"] + b["pss_kb"]) / 2.0) / (1024.0 * 1024.0) * dt_min
peak_pss_mib = max((s["pss_kb"] for s in win), default=0) / 1024.0
peak_rss_mib = max((s["rss_kb"] for s in win), default=0) / 1024.0
peak_procs = max((s["n"] for s in win), default=0)
ok = []
walls = []
render_walls = []
rcs = []
for i in range(1, n + 1):
    a = os.path.join(d, f"agent-{i}")
    def rd(name, default=""):
        try:
            with open(os.path.join(a, name)) as f:
                return f.read().strip()
        except OSError:
            return default
    ok.append(rd("verified", "0") == "1")
    rcs.append(rd("rc", "?"))
    w = rd("wall", "").split()
    if len(w) == 2:
        walls.append(float(w[1]) - float(w[0]))
    rw = rd("render_wall", "").split()
    if len(rw) == 2:
        render_walls.append(float(rw[1]) - float(rw[0]))
succ = sum(ok)
summary = {
    "engine": engine, "n": n, "cohort_wall_s": round(t1 - t0, 3),
    "profile": profile, "verdict_eligible": all(b["verdict_eligible"] for b in bin_stamp),
    "binaries": bin_stamp,
    "agent_wall_s_mean": round(statistics.mean(walls), 3) if walls else None,
    "agent_wall_s_max": round(max(walls), 3) if walls else None,
    "render_wall_s_mean": round(statistics.mean(render_walls), 3) if render_walls else None,
    "successes": succ, "rc": rcs,
    "peak_pss_mib": round(peak_pss_mib, 1), "peak_rss_mib": round(peak_rss_mib, 1),
    "peak_procs": peak_procs, "samples": len(win),
    "gib_minutes": round(gib_min, 6),
    "tasks_per_gib_min": round(succ / gib_min, 4) if gib_min > 0 else None,
}
with open(os.path.join(d, "summary.json"), "w") as f:
    json.dump(summary, f, indent=2)
print(json.dumps(summary))
PY
}

reps_json=()
for k in $(seq 1 "$REPS"); do
    echo "bench-n: engine=$ENGINE N=$N rep=$k/$REPS"
    run_cohort "$OUT/rep-$k" || { echo "bench-n: cohort rep-$k aborted" >&2; break; }
    reps_json+=("$OUT/rep-$k/summary.json")
done

python3 -I - "$OUT" "$ROOT" "${reps_json[@]}" <<'PY'
import json, os, statistics, sys
out, root, paths = sys.argv[1], sys.argv[2], sys.argv[3:]
sys.path.insert(0, os.path.join(root, "tools"))
import benchlib
reps = []
for p in paths:
    with open(p) as f:
        reps.append(json.load(f))
def med(key):
    vals = [r[key] for r in reps if r.get(key) is not None]
    return round(statistics.median(vals), 4) if vals else None
summary = {
    "engine": reps[0]["engine"] if reps else None,
    "n": reps[0]["n"] if reps else None,
    "reps": len(reps),
    "provenance": benchlib.provenance(
        root,
        [os.path.join(root, "target/release/acos"),
         os.path.join(root, "target/release/acos-art")],
        profile="release",
        gate=benchlib.host_gate(root, check_lock=False),
        engine=reps[0]["engine"] if reps else None),
    "median": {k: med(k) for k in ["cohort_wall_s", "agent_wall_s_mean", "render_wall_s_mean", "successes",
                                    "peak_pss_mib", "peak_rss_mib", "gib_minutes",
                                    "tasks_per_gib_min"]},
    "per_rep": reps,
}
with open(os.path.join(out, "summary.json"), "w") as f:
    json.dump(summary, f, indent=2)
print(json.dumps(summary["median"], indent=2))
print("summary:", os.path.join(out, "summary.json"))
PY
