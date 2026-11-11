#!/usr/bin/env python3
"""measure.py -- Step 3 measurements for the real-ART feasibility experiment.

Everything runs through scripts/artroot.sh (bwrap mini-Android root, real ART
dalvikvm64 from the com.android.art APEX, real 34-jar API-34 boot classpath).

  (a) startup:  wall time process-start -> exit for hello / an empty main,
                in three ART modes: no boot image (interp+JIT), -Xint,
                and with an AOT boot image built by dex2oat64.
  (b) memory:   VmRSS / Pss / anon / file-backed from smaps_rollup while the
                process sits in main() (HelloSleep).
  (c) speed:    the ACOS benchmark programs, same three modes, median of N.
  (d) sharing:  1/5/20 concurrent HelloSleep processes: sum PSS vs sum RSS.

Results -> results/art-feasibility/measurements-<stamp>.json and markdown on stdout.
"""
import json
import os
import re
import shutil
import statistics
import subprocess
import sys
import time

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "../../../.."))
SCRIPTS = os.path.join(ROOT, "research/art-feasibility/a-dalvikvm/scripts")
ARTROOT = os.path.join(SCRIPTS, "artroot.sh")
DATA = os.path.join(ROOT, ".cache/art-exp/data")
BENCH = os.path.join(ROOT, ".cache/art-exp/bench/dex")
HELLO = os.path.join(ROOT, "fixtures/build/dexprogs/01_hello/classes.dex")
IMG = "/data/img/boot.art"

REPS = int(os.environ.get("ART_REPS", "5"))
MODES = {
    "noimage": [],                       # interpreter + JIT (no boot image)
    "int": ["-Xint"],                    # interpreter only
    "aot": [f"-Ximage:{IMG}"],           # AOT boot image, JIT for app code
}


def stage(dex, name):
    """Copy a dex into the mini-root's /data/dex/<name>.dex, return device path."""
    d = os.path.join(DATA, "dex")
    os.makedirs(d, exist_ok=True)
    dst = os.path.join(d, name + ".dex")
    if not os.path.exists(dst) or os.path.getmtime(dst) < os.path.getmtime(dex):
        shutil.copy2(dex, dst)
    return "/data/dex/" + name + ".dex"


def run_once(args, timeout=600):
    t0 = time.monotonic()
    p = subprocess.run([ARTROOT] + args, stdout=subprocess.PIPE,
                       stderr=subprocess.PIPE, timeout=timeout)
    return time.monotonic() - t0, p


def med(vals):
    return statistics.median(vals)


def startup_table():
    stage(HELLO, "01_hello")
    stage(os.path.join(BENCH, "Noop.dex"), "Noop")
    rows = []
    for prog, name in (("01_hello", "hello (prints 16 lines)"), ("Noop", "empty main()")):
        dp = "/data/dex/" + prog + ".dex"
        for mode, extra in MODES.items():
            ts = []
            rc = None
            for _ in range(REPS):
                dt, p = run_once(["dalvikvm"] + extra + ["-cp", dp,
                                 (prog if prog == "Noop" else "Main")])
                ts.append(dt * 1000)
                rc = p.returncode
            rows.append({"program": name, "mode": mode, "n": len(ts),
                         "p50_ms": round(med(ts), 1),
                         "min_ms": round(min(ts), 1),
                         "max_ms": round(max(ts), 1), "exit": rc})
    return rows


SMAPS = ("Rss", "Pss", "Pss_Anon", "Pss_File", "Pss_Shmem",
         "Private_Dirty", "Private_Clean", "Shared_Clean", "Anonymous")


def smaps(pid):
    out = {}
    try:
        with open(f"/proc/{pid}/smaps_rollup") as f:
            for line in f:
                k, _, v = line.partition(":")
                if k in SMAPS:
                    out[k] = int(v.split()[0])
    except OSError:
        return None
    return out


def art_pids():
    """PIDs of dalvikvm64 processes (the comm is renamed to `main`, so match
    the real cmdline, not comm)."""
    out = []
    for e in os.listdir("/proc"):
        if not e.isdigit():
            continue
        try:
            with open(f"/proc/{e}/cmdline", "rb") as f:
                cmd = f.read().split(b"\0")[0].decode("utf-8", "replace")
        except OSError:
            continue
        if cmd.endswith("/dalvikvm64"):
            out.append(int(e))
    return sorted(out)


def find_pid(deadline=5.0):
    end = time.monotonic() + deadline
    while time.monotonic() < end:
        pids = art_pids()
        if pids:
            return max(pids)
        time.sleep(0.01)
    return None


def memory_table():
    stage(os.path.join(BENCH, "HelloSleep.dex"), "HelloSleep")
    rows = []
    for mode, extra in MODES.items():
        for _ in range(3):
            p = subprocess.Popen([ARTROOT, "dalvikvm"] + extra +
                                 ["-cp", "/data/dex/HelloSleep.dex", "HelloSleep", "3000"],
                                 stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            pid = find_pid()
            time.sleep(1.5)
            m = smaps(pid) if pid else None
            p.wait()
            if m:
                rows.append({"mode": mode, **m})
    out = {}
    for mode in MODES:
        agg = {}
        for r in rows:
            if r["mode"] != mode:
                continue
            for k, v in r.items():
                if k != "mode":
                    agg.setdefault(k, []).append(v)
        out[mode] = {k: round(med(v) / 1024, 1) for k, v in agg.items()}
        out[mode]["samples"] = len(agg.get("Pss", []))
    return out, len(rows)


BENCHES = ["BenchFib", "BenchLoop", "BenchString", "BenchAlloc", "BenchVirtual", "BenchGc"]


def bench_table():
    rows = []
    for b in BENCHES:
        dp = stage(os.path.join(BENCH, b + ".dex"), b)
        for mode, extra in MODES.items():
            ts = []
            for i in range(REPS):
                dt, p = run_once(["dalvikvm"] + extra + ["-cp", dp, b], timeout=900)
                if p.returncode != 0:
                    print(f"  !! {b} {mode} rc={p.returncode}: {p.stderr[-300:]!r}", file=sys.stderr)
                ts.append(dt * 1000)
            rows.append({"program": b, "mode": mode, "n": len(ts),
                         "p50_ms": round(med(ts), 1), "min_ms": round(min(ts), 1),
                         "max_ms": round(max(ts), 1)})
    return rows


def sharing_table():
    stage(os.path.join(BENCH, "HelloSleep.dex"), "HelloSleep")
    out = []
    for n in (1, 5, 20):
        for mode in ("noimage", "aot"):
            procs = [subprocess.Popen([ARTROOT, "dalvikvm"] + MODES[mode] +
                                      ["-cp", "/data/dex/HelloSleep.dex", "HelloSleep", "4000"],
                                      stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                     for _ in range(n)]
            time.sleep(2.0)
            pids = art_pids()
            rss = pss = pss_file = pss_anon = 0
            got = 0
            for pid in pids:
                m = smaps(pid)
                if not m:
                    continue
                got += 1
                rss += m["Rss"]
                pss += m["Pss"]
                pss_file += m["Pss_File"]
                pss_anon += m["Pss_Anon"]
            for q in procs:
                q.wait()
            out.append({"n": n, "mode": mode, "sampled": got,
                        "sum_rss_mb": round(rss / 1024, 1),
                        "sum_pss_mb": round(pss / 1024, 1),
                        "sum_pss_file_mb": round(pss_file / 1024, 1),
                        "sum_pss_anon_mb": round(pss_anon / 1024, 1),
                        "pss_per_proc_mb": round(pss / 1024 / max(got, 1), 1)})
    return out


def shim_floor():
    ts = []
    for _ in range(5):
        dt, _p = run_once(["/apex/com.android.runtime/bin/linkerconfig", "--target", "/linkerconfig"])
        ts.append(dt * 1000)
    return round(med(ts), 1)


def main():
    what = sys.argv[1:] or ["startup", "memory", "bench", "sharing"]
    res = {"engine": "real Google ART 14 (com.android.art APEX, x86_64)",
           "host": "Linux 7.2.11 x86_64, no KVM, bwrap user+mount namespace",
           "reps": REPS, "stamp": time.strftime("%Y-%m-%dT%H:%M:%S")}
    if "startup" in what:
        res["shim_floor_ms"] = shim_floor()
        res["startup"] = startup_table()
    if "memory" in what:
        res["memory"], res["memory_samples"] = memory_table()
    if "bench" in what:
        res["bench"] = bench_table()
    if "sharing" in what:
        res["sharing"] = sharing_table()
    out = os.path.join(ROOT, "results/art-feasibility",
                       "measurements-%s.json" % time.strftime("%Y%m%d-%H%M%S"))
    os.makedirs(os.path.dirname(out), exist_ok=True)
    with open(out, "w") as f:
        json.dump(res, f, indent=1)
    print(json.dumps(res, indent=1))
    print("\nwrote", out, file=sys.stderr)


if __name__ == "__main__":
    main()
