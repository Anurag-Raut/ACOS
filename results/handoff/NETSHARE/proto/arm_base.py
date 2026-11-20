#!/usr/bin/env python3
"""NETSHARE prototype, BASELINE arm: one pasta per session (the spike branch).

Run as:  unshare --user --map-root-user python3 arm_base.py -n N

Each session is exactly the SPIKE-NET shape: `pasta <hardened args> -- /bin/sh -c
'<boundary shim>; exec bwrap ...'`. That means one pasta process per session plus
the shim's `iptables` execs. This arm exists only to give the shared designs
something measured to be compared against.

Prints one JSON object. Not product code.
"""
import argparse
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
PASTA = os.environ.get("NETSHARE_PASTA", os.path.join(HERE, "..", "passt-src", "pasta"))
GUEST_IP = "169.254.2.15"
GUEST_GW = "169.254.2.1"
DL_REPS = int(os.environ.get("NETSHARE_DL_REPS", "3"))
DL_BYTES = os.environ.get("NETSHARE_DL_BYTES", "20000000")
HOST_IP = os.environ.get("NETSHARE_DL_IP", "")

# The boundary chain of the spike branch (crates/acos-art/src/spike_net.rs).
LAN_CIDRS = ["192.168.0.0/16", "10.0.0.0/8", "172.16.0.0/12", "169.254.0.0/16", "100.64.0.0/10"]


def boundary_script():
    chain = "acos-net-drop"
    out = ["set -u", "command -v iptables >/dev/null 2>&1 || exit 111",
           f"(iptables -w -N {chain} 2>/dev/null || iptables -w -F {chain}) || exit 111"]
    for c in LAN_CIDRS:
        out.append(f"iptables -w -A {chain} -d {c} -j DROP || exit 111")
    out.append(f"iptables -w -C OUTPUT -j {chain} 2>/dev/null || iptables -w -A OUTPUT -j {chain} || exit 111")
    for c in LAN_CIDRS:
        out.append(f"iptables -w -C {chain} -d {c} -j DROP || exit 111")
    return "\n".join(out)


def pasta_argv(script_cmd):
    ip = GUEST_IP
    return [
        PASTA, "-f", "-q", "--config-net",
        "-a", f"{GUEST_IP}/24", "-g", GUEST_GW,
        "--no-map-gw", "--map-host-loopback", "none",
        "-t", "none", "-u", "none", "-T", "none", "-U", "none", "--no-splice",
        "-D", "1.1.1.1",
        "--", "/bin/sh", "-c", boundary_script() + "\n" + script_cmd,
    ]


def pss_kb(pid):
    try:
        with open(f"/proc/{pid}/smaps_rollup") as f:
            for line in f:
                if line.startswith("Pss:"):
                    return int(line.split()[1])
    except OSError:
        return 0
    return 0


def rss_kb(pid):
    try:
        with open(f"/proc/{pid}/status") as f:
            for line in f:
                if line.startswith("VmRSS:"):
                    return int(line.split()[1])
    except OSError:
        return 0
    return 0


def build_guest_script(state, tag):
    path = os.path.join(state, f"guest-{tag}.sh")
    with open(path, "w") as f:
        f.write(f"""#!/bin/bash
set -u
res=/state/sess-{tag}
t0=$(date +%s%N)
ip4=$(dig +short +time=3 +tries=1 @1.1.1.1 speed.cloudflare.com 2>/dev/null | grep -E '^[0-9.]+$' | head -1)
[ -n "$ip4" ] || ip4="${{NETSHARE_DL_IP:-}}"
t_dns=$(date +%s%N)
: >$res.reps
for k in $(seq 1 {DL_REPS}); do
  s=$(date +%s%N)
  out=$(curl -4 -sS -m 120 -o /dev/null -w '%{{size_download}} %{{speed_download}}' \\
      --resolve "speed.cloudflare.com:443:$ip4" "{os.environ.get('NETSHARE_DL_URL','https://speed.cloudflare.com/__down?bytes=' + DL_BYTES)}" 2>>$res.err)
  rc=$?
  e=$(date +%s%N)
  echo "rep=$k rc=$rc bytes=$(echo "$out" | cut -d' ' -f1) rate=$(echo "$out" | cut -d' ' -f2) ms=$(( (e-s)/1000000 ))" >>$res.reps
done
echo "t0=$t0 t_dns=$t_dns ip4=$ip4 t_done=$(date +%s%N)" >$res.times
echo ok >$res.ready
exec sleep 300
""")
    os.chmod(path, 0o755)
    return path


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("-n", "--sessions", type=int, default=1)
    ap.add_argument("--state", default=None)
    ap.add_argument("--keep", action="store_true")
    ap.add_argument("--ready-timeout", type=float, default=180.0)
    ap.add_argument("--phase", default="")
    args = ap.parse_args()

    state = args.state or os.path.join(HERE, "state-base", f"{args.sessions}-{os.getpid()}")
    shutil.rmtree(state, ignore_errors=True)
    os.makedirs(state, exist_ok=True)
    result = {"arm": "BASE-pasta-per-session", "sessions": args.sessions, "phase": args.phase}
    t_start = time.time()

    sessions = []
    for i in range(args.sessions):
        idx = i + 1
        script = build_guest_script(state, str(idx))
        bwrap = [
            "bwrap", "--ro-bind", "/usr", "/usr", "--ro-bind", "/etc", "/etc", "--ro-bind", "/run", "/run",
            "--symlink", "usr/lib", "/lib", "--symlink", "usr/lib64", "/lib64", "--symlink", "usr/bin", "/bin",
            "--proc", "/proc", "--dev", "/dev", "--tmpfs", "/tmp",
            "--ro-bind", script, "/tmp/guest.sh", "--bind", state, "/state",
            "--unshare-pid", "--unshare-ipc", "--unshare-uts",
            "--die-with-parent", "--cap-drop", "ALL", "--clearenv",
            "--setenv", "PATH", "/usr/bin:/bin", "--setenv", "HOME", "/tmp",
            "--setenv", "NETSHARE_DL_IP", HOST_IP,
            "--", "/bin/bash", "/tmp/guest.sh",
        ]
        cmd = f"exec {subprocess.list2cmdline(bwrap)}"
        t_req = time.time()
        p = subprocess.Popen(pasta_argv(cmd), stdout=subprocess.DEVNULL,
                             stderr=open(os.path.join(state, f"sess-{idx}.stderr"), "w"))
        sessions.append({"idx": idx, "pid": p.pid, "proc": p, "t_req": t_req})

    deadline = time.time() + args.ready_timeout
    pending = {s["idx"] for s in sessions}
    while pending and time.time() < deadline:
        for s in sessions:
            if s["idx"] in pending and os.path.exists(os.path.join(state, f"sess-{s['idx']}.ready")):
                s["t_ready"] = time.time()
                pending.discard(s["idx"])
        time.sleep(0.01)
    result["not_ready"] = sorted(pending)
    result["wall_to_all_ready_s"] = round(max([s.get("t_ready", time.time()) for s in sessions] or [0]) - t_start, 2)

    time.sleep(0.5)
    lats = []
    for s in sessions:
        p = os.path.join(state, f"sess-{s['idx']}.times")
        if os.path.exists(p):
            m = re.search(r"t0=(\d+)", open(p).read())
            if m:
                lats.append(int(m.group(1)) / 1e6 - s["t_req"] * 1000.0)
    result["attach_ms_mean"] = round(sum(lats) / len(lats), 1) if lats else None
    result["attach_ms_max"] = round(max(lats), 1) if lats else None

    rates, sizes, reps = [], [], []
    for s in sessions:
        p = os.path.join(state, f"sess-{s['idx']}.reps")
        if not os.path.exists(p):
            continue
        for line in open(p):
            m = dict(re.findall(r"(\w+)=(\S+)", line))
            try:
                if m.get("rate"):
                    rates.append(float(m["rate"]) / 1e6)
                if m.get("bytes"):
                    sizes.append(int(m["bytes"]))
            except ValueError:
                pass
            reps.append({"idx": s["idx"], **m})
    result["throughput_mbps_mean"] = round(sum(rates) / len(rates), 2) if rates else None
    result["throughput_mbps_max"] = round(max(rates), 2) if rates else None
    result["bytes_mean"] = int(sum(sizes) / len(sizes)) if sizes else None
    result["guest_reps_sample"] = reps[:4]

    pss = {s["idx"]: pss_kb(s["pid"]) for s in sessions}
    result["net_pss_kb"] = pss
    result["net_rss_kb"] = {s["idx"]: rss_kb(s["pid"]) for s in sessions}
    total = sum(pss.values())
    result["net_pss_total_mib"] = round(total / 1024, 2)
    result["net_pss_per_session_mib"] = round(total / 1024 / args.sessions, 3)
    result["net_processes"] = len([s for s in sessions if os.path.exists(f"/proc/{s['pid']}")])

    if not args.keep:
        for s in sessions:
            for sig in (signal.SIGTERM, signal.SIGKILL):
                try:
                    os.kill(s["pid"], sig)
                except OSError:
                    break
                try:
                    s["proc"].wait(timeout=5)
                    break
                except Exception:
                    pass
    print(json.dumps(result))
    return 0


if __name__ == "__main__":
    sys.exit(main())
