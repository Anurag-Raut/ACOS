#!/usr/bin/env python3
"""NETSHARE prototype driver: run every arm at 1, 5 and 20 sessions.

Wraps each cell in `unshare --user --map-root-user` (the prototype needs a user
namespace and nothing else) and appends one JSON line per cell. Checks host RAM
before every cell and stops if it would cross 90 %.

  python3 run_matrix.py --out ../../results/handoff/NETSHARE/raw/measurements.jsonl [--arms b,base]
"""
import argparse
import json
import os
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))


def mem_pct():
    info = {}
    for line in open("/proc/meminfo"):
        k, v = line.split(":", 1)
        info[k] = int(v.split()[0])
    total = info["MemTotal"]
    avail = info.get("MemAvailable", info["MemFree"])
    return 100.0 * (total - avail) / total


def disk_pct(path="/"):
    st = os.statvfs(path)
    return 100.0 * (st.f_blocks - st.f_bfree) / st.f_blocks


def cell(arm, n, reps, dl_bytes, phase=""):
    if mem_pct() >= 88.0:
        raise SystemExit(f"host RAM at {mem_pct():.1f}% -- refusing to start a cell")
    if disk_pct() >= 86.0:
        raise SystemExit(f"disk at {disk_pct():.1f}% -- refusing to start a cell")
    env = dict(os.environ)
    env.update(NETSHARE_DL_REPS=str(reps), NETSHARE_DL_BYTES=str(dl_bytes),
               NETSHARE_DL_URL=f"https://speed.cloudflare.com/__down?bytes={dl_bytes}")
    if arm == "base":
        argv = ["python3", os.path.join(HERE, "arm_base.py"), "-n", str(n)]
    elif arm == "b-veth":
        argv = ["python3", os.path.join(HERE, "arm_b.py"), "--mode", "veth", "-n", str(n)]
    elif arm == "b-shared":
        argv = ["python3", os.path.join(HERE, "arm_b.py"), "--mode", "shared", "-n", str(n)]
    elif arm == "a-tap":
        argv = ["python3", os.path.join(HERE, "arm_a.py"), "-n", str(n)]
    elif arm == "c-notify":
        argv = ["python3", os.path.join(HERE, "arm_c.py"), "-n", str(n)]
    else:
        raise SystemExit(f"unknown arm {arm}")
    if phase:
        argv += ["--phase", phase]
    argv = ["unshare", "--user", "--map-root-user"] + argv
    t = time.time()
    p = subprocess.run(argv, env=env, capture_output=True, text=True, timeout=600)
    out = p.stdout.strip().splitlines()
    rec = None
    for line in out:
        if line.startswith("{"):
            rec = json.loads(line)
    if rec is None:
        rec = {"arm": arm, "sessions": n, "error": "no json", "stderr": p.stderr[-800:]}
    rec["host_mem_pct_after"] = round(mem_pct(), 1)
    rec["wall_s"] = round(time.time() - t, 1)
    return rec


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    ap.add_argument("--arms", default="base,b-veth,b-shared")
    ap.add_argument("--sizes", default="1,5,20")
    args = ap.parse_args()
    os.makedirs(os.path.dirname(os.path.abspath(args.out)), exist_ok=True)
    arms = args.arms.split(",")
    sizes = [int(x) for x in args.sizes.split(",")]
    ip = ""
    try:
        ip = subprocess.run(["getent", "ahostsv4", "speed.cloudflare.com"], capture_output=True, text=True).stdout.split()[0]
        os.environ["NETSHARE_DL_IP"] = ip
    except Exception:
        pass
    print(f"# driver: dl_ip={ip} mem={mem_pct():.1f}% disk={disk_pct():.1f}%", flush=True)
    with open(args.out, "a") as f:
        for n in sizes:
            reps = 3 if n == 1 else (2 if n == 5 else 1)
            dl = 20000000 if n <= 5 else 4000000
            for arm in arms:
                # interleave the arms per size so a slow host drifts all of them equally
                rec = cell(arm, n, reps, dl)
                f.write(json.dumps(rec) + "\n")
                f.flush()
                print(json.dumps({k: rec.get(k) for k in
                                  ("arm", "sessions", "attach_ms_mean", "throughput_mbps_mean",
                                   "net_pss_per_session_mib", "net_pss_total_mib", "net_processes",
                                   "not_ready", "error")}), flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
