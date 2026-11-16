#!/usr/bin/env python3
"""NETSHARE prototype: prove the per-session controls still work in option B2.

Two sessions share the ONE relay (one pasta, one relay netns). Session 1 is taken
offline for a window, session 2 gets 400 ms of latency for a window, and each
session's own request log shows only its own change.

  unshare --user --map-root-user python3 controls_b.py
"""
import json
import os
import shutil
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import arm_b  # noqa: E402

STATE = os.path.join(HERE, "state-controls")
IP = os.environ.get("NETSHARE_DL_IP", "")


def guest_script(tag):
    path = os.path.join(STATE, f"probe-{tag}.sh")
    with open(path, "w") as f:
        f.write(f"""#!/bin/bash
set -u
res=/state/probe-{tag}
: >$res.log
for k in $(seq 1 32); do
  s=$(date +%s%N)
  out=$(curl -4 -sS -o /dev/null -w '%{{http_code}} %{{time_total}}' --max-time 3 \\
      --resolve "speed.cloudflare.com:443:{IP}" \\
      "https://speed.cloudflare.com/__down?bytes=2000" 2>/dev/null)
  rc=$?
  e=$(date +%s%N)
  echo "$(( s / 1000000 )) rc=$rc out=${{out:-none}} ms=$(( (e-s)/1000000 ))" >>$res.log
  sleep 0.6
done
echo done >$res.done
exec sleep 60
""")
    os.chmod(path, 0o755)
    return path


def main():
    shutil.rmtree(STATE, ignore_errors=True)
    os.makedirs(STATE, exist_ok=True)
    fab = arm_b.Fabric(STATE)
    pasta = subprocess.Popen(
        [arm_b.PASTA, "--netns", f"/proc/{fab.pid}/ns/net", "--config-net", "-I", "tap0",
         "-a", "169.254.2.15/24", "-g", "169.254.2.1",
         "-t", "none", "-u", "none", "-T", "none", "-U", "none",
         "--no-splice", "--map-host-loopback", "none", "--no-map-gw", "-D", "1.1.1.1", "-f", "-q"],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    for _ in range(2000):
        if subprocess.run(["nsenter", "-t", str(fab.pid), "-n", "ip", "link", "show", "tap0"],
                          stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0:
            break
        time.sleep(0.005)
    res = {}
    arm_b.install_rules(fab, res)

    procs = []
    relay_netns = arm_b.ns_of(fab.pid)
    for idx in (1, 2):
        script = guest_script(str(idx))
        p = subprocess.Popen(
            ["unshare", "-n", "bash", os.path.join(HERE, "session.sh"),
             STATE, f"s{idx}", f"10.163.{idx}.2/30", f"10.163.{idx}.1"] + arm_b.bwrap_argv(STATE, script),
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        for _ in range(4000):
            n = arm_b.ns_of(p.pid)
            if n and n != relay_netns:
                break
            time.sleep(0.002)
        fab.run(f"ip link add v{idx} type veth peer name s{idx} && "
                f"ip link set s{idx} netns /proc/{p.pid}/ns/net && "
                f"ip addr add 10.163.{idx}.1/30 dev v{idx} && ip link set v{idx} up")
        procs.append(p)

    time.sleep(4)  # baseline window
    t_off = time.time()
    fab.run("iptables -w -I ACOS-SESS -i v1 -j DROP")
    time.sleep(8)
    t_on = time.time()
    fab.run("iptables -w -D ACOS-SESS -i v1 -j DROP")
    time.sleep(4)
    t_lat = time.time()
    fab.run("tc qdisc add dev v2 root netem delay 400ms")
    time.sleep(8)
    t_latend = time.time()
    fab.run("tc qdisc del dev v2 root")
    time.sleep(4)

    out = {"offline_window": [t_off, t_on], "latency_window": [t_lat, t_latend], "sessions": {}}
    for idx in (1, 2):
        log = os.path.join(STATE, f"probe-{idx}.log")
        rows = []
        if os.path.exists(log):
            for line in open(log):
                f = dict(x.split("=", 1) for x in line.split()[1:] if "=" in x)
                ts = int(line.split()[0]) / 1000.0
                rows.append({"t": round(ts, 2), "rc": f.get("rc"), "code": f.get("out", "").split()[0] if f.get("out") else "",
                             "ms": f.get("ms")})
        out["sessions"][idx] = rows

    def summarize(rows, w0, w1):
        inside = [r for r in rows if w0 <= r["t"] <= w1]
        outside = [r for r in rows if not (w0 <= r["t"] <= w1)]
        ok_in = [r for r in inside if r["rc"] == "0"]
        ms_out = [int(r["ms"]) for r in outside if r["rc"] == "0"]
        ms_in = [int(r["ms"]) for r in inside if r["rc"] == "0"]
        return {"n_in": len(inside), "ok_in": len(ok_in),
                "ms_in_median": sorted(ms_in)[len(ms_in) // 2] if ms_in else None,
                "ms_out_median": sorted(ms_out)[len(ms_out) // 2] if ms_out else None}

    out["summary"] = {
        "s1_offline_window": summarize(out["sessions"][1], t_off, t_on),
        "s1_latency_window": summarize(out["sessions"][1], t_lat, t_latend),
        "s2_offline_window": summarize(out["sessions"][2], t_off, t_on),
        "s2_latency_window": summarize(out["sessions"][2], t_lat, t_latend),
    }
    for p in procs:
        p.terminate()
    pasta.terminate()
    fab.close()
    print(json.dumps(out))
    return 0


if __name__ == "__main__":
    sys.exit(main())
