#!/usr/bin/env bash
#
# Honest micro-benchmarks for the ACOS Rust DEX interpreter.
#
#   tools/bench-vm.sh
#
# Compares ACOS against the HOST JVM (OpenJDK, default JIT), `java -Xint` and
# `java -Xshare:auto`. These are host-JVM numbers, NOT Android/ART numbers.
#
# Env:
#   ACOS_BENCH_REPS   repetitions per measurement (default 20)
#
# Writes results/vm/bench-<timestamp>.json.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPS="${ACOS_BENCH_REPS:-20}"
export ACOS_BENCH_REPS="$REPS"

python3 - "$ROOT" "$REPS" <<'PY'
import json, os, resource, statistics, subprocess, sys, time, datetime, shutil

root, reps = sys.argv[1], int(sys.argv[2])
reps = max(reps, 1)
outdir = os.path.join(root, "results/vm")
os.makedirs(outdir, exist_ok=True)
tmp = os.path.join(root, "target/tmp/acos-bench")
os.makedirs(tmp, exist_ok=True)

sdk = os.environ.get("ANDROID_HOME", "/home/Qui/Android/Sdk")
build_tools = os.environ.get("BUILD_TOOLS_VERSION", "35.0.0")
d8 = os.path.join(sdk, "build-tools", build_tools, "d8")
android_jar = os.path.join(sdk, "platforms", "android-34", "android.jar")

def run(cmd, stdin=None):
    return subprocess.run(cmd, check=False, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

# Build release ACOS.
print("bench-vm: building acos (release)...", flush=True)
run(["cargo", "build", "--release", "-p", "acos-cli"], )
acos = os.path.join(root, "target/release/acos")
if not os.path.exists(acos):
    print("bench-vm: error: acos not built", file=sys.stderr); sys.exit(2)

# Compile the Java benchmarks (host javac = build tool) and (for ACOS) d8.
bench_srcs = {
    "BenchFib": os.path.join(root, "crates/acos-vm/benches/java/BenchFib.java"),
    "BenchLoop": os.path.join(root, "crates/acos-vm/benches/java/BenchLoop.java"),
    "BenchString": os.path.join(root, "crates/acos-vm/benches/java/BenchString.java"),
    "BenchAlloc": os.path.join(root, "crates/acos-vm/benches/java/BenchAlloc.java"),
    # Framework-shaped programs (ADR-0013 step 1): polymorphic interface
    # dispatch, field-heavy graph walk, and ArrayList/HashMap/StringBuilder.
    "BenchVirtual": os.path.join(root, "crates/acos-vm/benches/java/BenchVirtual.java"),
    "BenchFields": os.path.join(root, "crates/acos-vm/benches/java/BenchFields.java"),
    "BenchCollections": os.path.join(root, "crates/acos-vm/benches/java/BenchCollections.java"),
}
bench = {}
for name, src in bench_srcs.items():
    out = os.path.join(tmp, name)
    classes = os.path.join(out, "classes")
    dex = os.path.join(out, "classes.dex")
    if not os.path.exists(dex):
        shutil.rmtree(out, ignore_errors=True)
        os.makedirs(classes, exist_ok=True)
        if run(["javac", "--release", "11", "-d", classes, src]).returncode != 0:
            print(f"bench-vm: javac failed for {name}", file=sys.stderr); sys.exit(2)
        class_files = sorted(
            os.path.join(classes, f) for f in os.listdir(classes) if f.endswith(".class")
        )
        if run([d8, "--lib", android_jar, "--min-api", "26", "--output", out] + class_files).returncode != 0:
            print(f"bench-vm: d8 failed for {name}", file=sys.stderr); sys.exit(2)
    bench[name] = {"classes": classes, "dex": dex}

# Startup program: 01_hello (classes already built by tools/build-fixtures.sh).
hello_classes = os.path.join(root, "fixtures/build/dexprogs/01_hello/classes")
hello_dex = os.path.join(root, "fixtures/build/dexprogs/01_hello/classes.dex")
if not os.path.exists(hello_dex):
    print("bench-vm: error: fixtures/build/dexprogs/01_hello absent", file=sys.stderr); sys.exit(2)

def measure(cmd, n, warmup=2):
    """Run `cmd` n+warmup times, return (p50_ms, p95_ms, peak_rss_kb)."""
    samples = []
    rss = 0
    total = n + warmup
    for i in range(total):
        t0 = time.monotonic()
        p = subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        _pid, status, ru = os.wait4(p.pid, 0)
        p.returncode = os.waitstatus_to_exitcode(status)
        dt = (time.monotonic() - t0) * 1000.0
        rss = max(rss, getattr(ru, "ru_maxrss", 0))
        if i >= warmup:
            samples.append(dt)
    samples.sort()
    p50 = statistics.median(samples)
    idx = min(len(samples) - 1, int(0.95 * (len(samples) - 1)))
    p95 = samples[idx]
    return p50, p95, rss

def pct_unused(samples):
    samples = sorted(samples)
    return samples[min(len(samples) - 1, int(0.95 * (len(samples) - 1)))]

java = shutil.which("java") or "java"

# ---- (a) process start -> exit for 01_hello --------------------------------
startup = []
variants = [
    ("acos", [acos, "run-dex", hello_dex, "--main", "Main"]),
    ("java", [java, "-cp", hello_classes, "Main"]),
    ("java -Xint", [java, "-Xint", "-cp", hello_classes, "Main"]),
    ("java -Xshare:auto", [java, "-Xshare:auto", "-cp", hello_classes, "Main"]),
]
for label, cmd in variants:
    p50, p95, rss = measure(cmd, reps)
    startup.append({"runtime": label, "p50_ms": round(p50, 3), "p95_ms": round(p95, 3), "peak_rss_kb": rss})

# ---- (b) CPU-bound benchmarks ----------------------------------------------
def acos_instructions(dex, main):
    p = subprocess.run([acos, "run-dex", dex, "--main", main, "--json"],
                       stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, check=False)
    try:
        env = json.loads(p.stdout)
        return int((env.get("metrics") or {}).get("instructions", 0))
    except Exception:
        return 0

cpu = []
for name in [
    "BenchFib", "BenchString", "BenchLoop", "BenchAlloc",
    "BenchVirtual", "BenchFields", "BenchCollections",
]:
    dex = bench[name]["dex"]
    classes = bench[name]["classes"]
    insns = acos_instructions(dex, name)
    row = {"program": name, "instructions": insns, "runtimes": []}
    for label, cmd in [
        ("acos", [acos, "run-dex", dex, "--main", name]),
        ("java -Xint", [java, "-Xint", "-cp", classes, name]),
        ("java", [java, "-cp", classes, name]),
    ]:
        p50, p95, rss = measure(cmd, reps)
        entry = {"runtime": label, "p50_ms": round(p50, 3), "p95_ms": round(p95, 3),
                 "peak_rss_kb": rss}
        if label == "acos" and p50 > 0:
            entry["instructions_per_second"] = int(insns / (p50 / 1000.0))
        row["runtimes"].append(entry)
    cpu.append(row)

doc = {
    "timestamp": datetime.datetime.now().isoformat(timespec="seconds"),
    "repetitions": reps,
    "comparison_target": "HOST JVM (OpenJDK 17, default JIT); NOT Android/ART",
    "fidelity": "host-jvm-baseline",
    "methodology": {
        "wall_clock": "monotonic clock around process start->exit",
        "peak_rss": "os.wait4 rusage.ru_maxrss (Linux, KiB)",
        "warmup_runs": 2,
        "percentiles": "p50 = median, p95 = nearest-rank on sorted samples",
    },
    "startup_01_hello": startup,
    "cpu_bound": cpu,
}

stamp = datetime.datetime.now().strftime("%Y-%m-%d-%H%M%S")
path = os.path.join(outdir, f"bench-{stamp}.json")
with open(path, "w") as f:
    json.dump(doc, f, indent=2)

print()
print("=== (a) process start -> exit: 01_hello ===")
print(f"{'runtime':<18} {'p50 ms':>10} {'p95 ms':>10} {'peak RSS KiB':>13}")
for r in startup:
    print(f"{r['runtime']:<18} {r['p50_ms']:>10.2f} {r['p95_ms']:>10.2f} {r['peak_rss_kb']:>13}")
print()
print("=== (b) CPU-bound (ACOS vs host JVM) ===")
print(f"{'program':<14} {'runtime':<12} {'p50 ms':>12} {'p95 ms':>12} {'peak RSS KiB':>13} {'M insns/s':>10}")
for row in cpu:
    for entry in row["runtimes"]:
        mips = entry.get("instructions_per_second", 0) / 1e6
        print(f"{row['program']:<14} {entry['runtime']:<12} {entry['p50_ms']:>12.1f} "
              f"{entry['p95_ms']:>12.1f} {entry['peak_rss_kb']:>13} {mips:>10.2f}")
print()
print(f"bench-vm: wrote {path}")
print("bench-vm: NOTE: comparison target is the HOST JVM (OpenJDK 17), not Android ART;")
print("bench-vm:       these numbers are not Android-verified.")
PY
