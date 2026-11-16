#!/usr/bin/env python3
"""NETSHARE prototype, option B: one shared relay netns served by ONE pasta.

Run as:  unshare --user --map-root-user python3 arm_b.py --mode veth|shared -n N

Layout (unprivileged, no sudo):

  supervisor (this process)          userns root, HOST netns
    |- fabric  (bash fabric.sh)      userns root, relay netns  (owns veth ends, rules)
    |- pasta --netns <fabric netns>  userns root, HOST netns   (the only NAT process)
    |- session i                     userns root, own netns    (mode veth: + veth to fabric)
                                                                  (mode shared: inside fabric)

mode veth   = B2: per-session netns joined by a veth pair -> per-session controls possible
mode shared = B1: every session joins the relay netns      -> no per-session controls

Prints one JSON object: setup ms, PSS of the network machinery, guest throughput.
Not product code.
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
DL_URL = os.environ.get("NETSHARE_DL_URL", "https://speed.cloudflare.com/__down?bytes=20000000")
DL_REPS = int(os.environ.get("NETSHARE_DL_REPS", "3"))
HOST_IP = os.environ.get("NETSHARE_DL_IP", "")  # set by the driver (getent ahostsv4)


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


def ns_of(pid):
    try:
        return os.readlink(f"/proc/{pid}/ns/net")
    except OSError:
        return None


class Fabric:
    """The relay netns: one long-lived bash that executes `ip`/`iptables` for us."""

    def __init__(self, state):
        self.state = state
        self.fifo = os.path.join(state, "fabric.fifo")
        if not os.path.exists(self.fifo):
            os.mkfifo(self.fifo)
        self.w = None
        self.n = 0
        self.proc = subprocess.Popen(
            ["unshare", "-n", "bash", os.path.join(HERE, "fabric.sh"), state],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )
        self.pid = None
        for _ in range(3000):
            p = os.path.join(state, "fabric.pid")
            if os.path.exists(p) and open(p).read().strip():
                self.pid = int(open(p).read().strip())
                break
            time.sleep(0.002)
        if self.pid is None:
            raise SystemExit("fabric did not start")
        self.w = open(self.fifo, "w", buffering=1)

    def run(self, cmd, timeout=30.0):
        self.n += 1
        i = self.n
        ack = os.path.join(self.state, f"ack.{i}")
        try:
            os.unlink(ack)
        except OSError:
            pass
        self.w.write(f"{i}|{cmd}\n")
        end = time.time() + timeout
        while time.time() < end:
            if os.path.exists(ack):
                rc = int(open(ack).read().strip() or 1)
                os.unlink(ack)
                return rc
            time.sleep(0.001)
        raise SystemExit(f"fabric command timed out: {cmd}")

    def close(self):
        try:
            self.w.write("0|quit\n")
            self.w.close()
        except Exception:
            pass
        try:
            self.proc.terminate()
        except Exception:
            pass


def build_guest_script(state, tag):
    path = os.path.join(state, f"guest-{tag}.sh")
    with open(path, "w") as f:
        f.write(
            f"""#!/bin/bash
set -u
res=/state/sess-{tag}
t0=$(date +%s%N)
# Proves the UDP datapath (DNS) through the relay, then the TCP/TLS path.
ip4=$(dig +short +time=3 +tries=1 @1.1.1.1 speed.cloudflare.com 2>/dev/null | grep -E '^[0-9.]+$' | head -1)
[ -n "$ip4" ] || ip4="${{NETSHARE_DL_IP:-}}"
t_dns=$(date +%s%N)
: >$res.reps
for k in $(seq 1 {DL_REPS}); do
  s=$(date +%s%N)
  out=$(curl -4 -sS -m 120 -o /dev/null -w '%{{size_download}} %{{speed_download}}' \\
      --resolve "speed.cloudflare.com:443:$ip4" "{DL_URL}" 2>>$res.err)
  rc=$?
  e=$(date +%s%N)
  echo "rep=$k rc=$rc bytes=$(echo "$out" | cut -d' ' -f1) rate=$(echo "$out" | cut -d' ' -f2) ms=$(( (e-s)/1000000 ))" >>$res.reps
done
echo "t0=$t0 t_dns=$t_dns ip4=$ip4 t_done=$(date +%s%N)" >$res.times
echo ok >$res.ready
exec sleep 300
"""
        )
    os.chmod(path, 0o755)
    return path


def bwrap_argv(state, script):
    return [
        "bwrap",
        "--ro-bind", "/usr", "/usr",
        "--ro-bind", "/etc", "/etc",
        "--ro-bind", "/run", "/run",
        "--symlink", "usr/lib", "/lib",
        "--symlink", "usr/lib64", "/lib64",
        "--symlink", "usr/bin", "/bin",
        "--proc", "/proc",
        "--dev", "/dev",
        "--tmpfs", "/tmp",
        "--ro-bind", script, "/tmp/guest.sh",
        "--bind", state, "/state",
        "--unshare-pid",
        "--unshare-ipc",
        "--unshare-uts",
        "--die-with-parent",
        "--cap-drop", "ALL",
        "--clearenv",
        "--setenv", "PATH", "/usr/bin:/bin",
        "--setenv", "HOME", "/tmp",
        "--setenv", "NETSHARE_DL_IP", HOST_IP,
        "--", "/bin/bash", "/tmp/guest.sh",
    ]


def install_rules(fabric, result):
    rules = [
        "sysctl -qw net.ipv4.ip_forward=1",
        "iptables -w -t nat -F POSTROUTING",
        "iptables -w -t nat -A POSTROUTING -o tap0 -j MASQUERADE",
        "iptables -w -F FORWARD",
        "(iptables -w -N ACOS-SESS 2>/dev/null; iptables -w -F ACOS-SESS)",
        "iptables -w -A FORWARD -j ACOS-SESS",
        "iptables -w -A FORWARD -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT",
    ]
    for cidr in ("192.168.0.0/16", "10.0.0.0/8", "172.16.0.0/12", "169.254.0.0/16", "100.64.0.0/10"):
        rules.append(f"iptables -w -A FORWARD -i v+ -d {cidr} -j DROP")
    rules += [
        "iptables -w -A FORWARD -o tap0 -j ACCEPT",
        "iptables -w -A FORWARD -i v+ -j DROP",
    ]
    for r in rules:
        rc = fabric.run(r)
        if rc != 0:
            result.setdefault("rule_errors", []).append(f"{r} -> {rc}")
    result["rules_installed"] = "rule_errors" not in result


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--mode", choices=["veth", "shared"], default="veth")
    ap.add_argument("-n", "--sessions", type=int, default=1)
    ap.add_argument("--state", default=None)
    ap.add_argument("--keep", action="store_true")
    ap.add_argument("--ready-timeout", type=float, default=180.0)
    ap.add_argument("--phase", default="", help="label written into the result")
    args = ap.parse_args()

    state = args.state or os.path.join(HERE, "state-b", f"{args.mode}-{args.sessions}-{os.getpid()}")
    shutil.rmtree(state, ignore_errors=True)
    os.makedirs(state, exist_ok=True)

    result = {"arm": f"B-{args.mode}", "sessions": args.sessions, "phase": args.phase}
    t_start = time.time()
    fabric = Fabric(state)

    pasta = subprocess.Popen(
        [
            PASTA, "--netns", f"/proc/{fabric.pid}/ns/net",
            "--config-net", "-I", "tap0", "-a", f"{GUEST_IP}/24", "-g", GUEST_GW,
            "-t", "none", "-u", "none", "-T", "none", "-U", "none",
            "--no-splice", "--map-host-loopback", "none", "--no-map-gw",
            "-D", "1.1.1.1", "-f", "-q",
        ],
        stdout=subprocess.DEVNULL,
        stderr=open(os.path.join(state, "pasta.err"), "w"),
    )
    result["pasta_pid"] = pasta.pid

    # tap0 appears in the relay netns once pasta configured it (nsenter is much
    # faster than round-tripping through the fabric).
    t_tap0 = None
    for _ in range(2000):
        r = subprocess.run(["nsenter", "-t", str(fabric.pid), "-n", "ip", "link", "show", "tap0"],
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        if r.returncode == 0:
            t_tap0 = time.time()
            break
        if pasta.poll() is not None:
            result["error"] = "pasta exited: " + open(os.path.join(state, "pasta.err")).read()[-400:]
            print(json.dumps(result))
            return 1
        time.sleep(0.005)
    if t_tap0 is None:
        result["error"] = "no tap0 in relay netns; pasta.err=" + open(os.path.join(state, "pasta.err")).read()[-400:]
        print(json.dumps(result))
        pasta.kill()
        return 1
    result["relay_start_ms"] = round((t_tap0 - t_start) * 1000, 1)

    install_rules(fabric, result)

    sessions = []
    my_netns = ns_of(fabric.pid)
    for i in range(args.sessions):
        idx = i + 1
        tag = str(idx)
        script = build_guest_script(state, tag)
        sess = {"idx": idx, "iface": f"v{idx}", "peer": f"s{idx}",
                "addr": f"10.163.{idx}.2/30", "gw": f"10.163.{idx}.1"}
        t_req = time.time()
        if args.mode == "veth":
            p = subprocess.Popen(
                ["unshare", "-n", "bash", os.path.join(HERE, "session.sh"),
                 state, sess["peer"], sess["addr"], sess["gw"]] + bwrap_argv(state, script),
                stdout=subprocess.DEVNULL,
                stderr=open(os.path.join(state, f"sess-{tag}.stderr"), "w"),
            )
            # wait until the session netns exists and differs from the relay's
            for _ in range(4000):
                n = ns_of(p.pid)
                if n and n != my_netns:
                    break
                time.sleep(0.002)
            rc = fabric.run(
                f"ip link add {sess['iface']} type veth peer name {sess['peer']} && "
                f"ip link set {sess['peer']} netns /proc/{p.pid}/ns/net && "
                f"ip addr add {sess['gw']}/30 dev {sess['iface']} && "
                f"ip link set {sess['iface']} up"
            )
            sess["veth_rc"] = rc
        else:
            argv = bwrap_argv(state, script)
            p = subprocess.Popen(["bash", "-c", f"exec {subprocess.list2cmdline(argv)}"],
                                 stdout=subprocess.DEVNULL,
                                 stderr=open(os.path.join(state, f"sess-{tag}.stderr"), "w"))
        sess.update(pid=p.pid, proc=p, t_req=t_req)
        sessions.append(sess)

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

    pids = {"pasta": pasta.pid, "fabric": fabric.pid}
    result["net_pss_kb"] = {k: pss_kb(v) for k, v in pids.items()}
    result["net_rss_kb"] = {k: rss_kb(v) for k, v in pids.items()}
    total = sum(result["net_pss_kb"].values())
    result["net_pss_total_mib"] = round(total / 1024, 2)
    result["net_pss_per_session_mib"] = round(total / 1024 / args.sessions, 3)
    result["net_processes"] = len([v for v in pids.values() if os.path.exists(f"/proc/{v}")])
    result["guest_pss_kb"] = {s["idx"]: pss_kb(s["pid"]) for s in sessions}

    if args.keep:
        print(json.dumps(result))
        return 0
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
    pasta.terminate()
    fabric.close()
    try:
        pasta.wait(timeout=5)
    except Exception:
        pasta.kill()
    print(json.dumps(result))
    return 0


if __name__ == "__main__":
    sys.exit(main())
