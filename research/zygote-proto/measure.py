#!/usr/bin/env python3
"""Z2: measure the sandboxed Zygote prototype.

Starts research/zygote-proto/run.sh, waits until the parent reports HOLD_START,
samples /proc/<pid>/smaps_rollup and the grouped /proc/<pid>/smaps of the parent
and every process of the child tree, then reports Rss/Pss/Private_Dirty of the
zygote, the ART apps (comm z2app) and the two sandbox helper processes
(comm z2ns, z2init).

    tools: python3 measure.py --n 2 --hold 6000 --preload 800 --out ...
Everything runs under the ART lock (see results/art-runtime/Z2.md).
"""
import argparse
import json
import os
import re
import subprocess
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
RUN = HERE / "run.sh"
CACHE = Path(os.environ.get("ACOS_CACHE", "/home/Qui/Projects/acos/.cache"))

ROLLUP = ["Rss", "Pss", "Private_Clean", "Private_Dirty", "Shared_Clean",
          "Shared_Dirty", "Swap"]
GROUPS = {
    "boot_image": ("boot.art", "boot.oat", "boot-framework", "boot-core"),
    "dalvik_zygote_space": ("dalvik-zygote",),
    "dalvik_region_space": ("dalvik-region",),
    "dalvik_main_space": ("dalvik-main space", "dalvik-non moving", "dalvik-LOS",
                          "dalvik-rosalloc"),
    "dalvik_all_anon": ("dalvik-",),
}


def read_text(path):
    try:
        with open(path, "r", errors="replace") as fh:
            return fh.read()
    except OSError:
        return ""


def rollup(pid):
    out = {}
    for line in read_text("/proc/%d/smaps_rollup" % pid).splitlines():
        k, _, v = line.partition(":")
        if k in ROLLUP:
            out[k] = int(v.split()[0])
    return out


def smaps_grouped(pid):
    tot = {g: dict((f, 0) for f in ("Rss", "Pss", "Private_Dirty", "Shared_Clean",
                                    "Shared_Dirty")) for g in GROUPS}
    seen = {g: 0 for g in GROUPS}
    cur = None
    for line in read_text("/proc/%d/smaps" % pid).splitlines():
        if re.match(r"^[0-9a-f]+-[0-9a-f]+ ", line):
            parts = line.split(" ", 5)
            cur = parts[5].strip() if len(parts) > 5 else "[anon]"
            continue
        if not line or line[0] not in "RPSs" or cur is None:
            continue
        k, _, v = line.partition(":")
        k = k.strip()
        if k not in ("Rss", "Pss", "Private_Dirty", "Shared_Clean", "Shared_Dirty"):
            continue
        n = int(v.split()[0])
        for g, needles in GROUPS.items():
            if any(nd in cur for nd in needles):
                tot[g][k] += n
                seen[g] += 1
    return tot, seen


def read_int_file(path, default=-1):
    try:
        return int(read_text(path).strip())
    except ValueError:
        return default


def find_candidates():
    out = []
    for d in os.listdir("/proc"):
        if not d.isdigit():
            continue
        cl = read_text("/proc/%s/cmdline" % d).replace("\0", " ")
        if "dev.acos.z1.ZygoteMain" in cl:
            out.append(int(d))
    return out


def ppid_map():
    m = {}
    for d in os.listdir("/proc"):
        if not d.isdigit():
            continue
        st = read_text("/proc/%s/stat" % d)
        if not st:
            continue
        try:
            m[int(d)] = int(st[st.rindex(")") + 2:].split()[1])
        except (ValueError, IndexError):
            pass
    return m


def subtree(roots):
    m = ppid_map()
    kids = {}
    for pid, p in m.items():
        kids.setdefault(p, []).append(pid)
    seen, stack = set(), list(roots)
    while stack:
        p = stack.pop()
        if p in seen:
            continue
        seen.add(p)
        stack.extend(kids.get(p, []))
    return seen


def comm(pid):
    return read_text("/proc/%d/comm" % pid).strip()


def sample(pids):
    rec = {}
    for pid in pids:
        r = rollup(pid)
        if not r:
            continue
        g, seen = smaps_grouped(pid)
        rec[pid] = {"rollup": r, "groups": g, "mappings": seen,
                    "comm": comm(pid),
                    "threads": read_int_file("/proc/%d/status" % pid),
                    "cmdline": read_text("/proc/%d/cmdline" % pid).replace("\0", " ")[:120]}
    return rec


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--n", type=int, default=2)
    ap.add_argument("--hold", type=int, default=6000)
    ap.add_argument("--preload", type=int, default=800)
    ap.add_argument("--settle", type=float, default=2.0)
    ap.add_argument("--out", default="")
    ap.add_argument("--log", default="")
    args = ap.parse_args()

    log = Path(args.log) if args.log else CACHE / ("z2-logs/run-n%d.log" % args.n)
    log.parent.mkdir(parents=True, exist_ok=True)
    cmd = [str(RUN), "fork", str(args.n), str(args.hold), str(args.preload)]
    with open(log, "w") as fh:
        proc = subprocess.Popen(cmd, stdout=fh, stderr=subprocess.STDOUT)
        t0 = time.time()
        while time.time() - t0 < 300:
            txt = read_text(log)
            if "HOLD_START" in txt or "PARENT_DONE" in txt or proc.poll() is not None:
                break
            time.sleep(0.05)
        time.sleep(args.settle)
        cands = find_candidates()
        allpids = subtree(cands) if cands else set()
        snap = sample(sorted(allpids))
        # The ART zygote is the largest process of the tree (the app children
        # are ~80 MB Rss, the zygote ~127 MB).
        parent = max(snap, key=lambda p: snap[p]["rollup"].get("Rss", 0)) if snap else None
        roles = {}
        for pid in snap:
            if pid == parent:
                roles[pid] = "zygote"
            elif snap[pid]["comm"] == "z2app":
                roles[pid] = "app"
            elif snap[pid]["comm"] in ("z2ns", "z2init"):
                roles[pid] = "sandbox-helper"
            elif snap[pid]["rollup"].get("Rss", 0) > 10240:
                roles[pid] = "other-large"
            else:
                roles[pid] = "other"
        proc.wait()
    text = read_text(log)

    def numbers(key):
        return [int(x) for x in re.findall(r"%s=(\d+)" % key, text)]

    def lines(prefix):
        return re.findall(r"^%s.*$" % re.escape(prefix), text, re.M)

    want = [p for p in snap if roles[p] in ("zygote", "app", "sandbox-helper", "other-large")]
    rec = {
        "n": args.n,
        "hold_ms": args.hold,
        "preload_limit": args.preload,
        "parent_pid": parent,
        "roles": {str(p): roles[p] for p in snap},
        "snapshot": {str(p): {"rollup": snap[p]["rollup"],
                              "comm": snap[p]["comm"],
                              "groups": snap[p]["groups"],
                              "mappings": snap[p]["mappings"]} for p in want},
        "fork_to_post_fork_ms": numbers("fork_to_post_fork_ms"),
        "sandbox_ms": numbers("sandbox_ms"),
        "fork_to_ready_ms": numbers("fork_to_ready_ms"),
        "cold": {"exec_to_main_ms": numbers("exec_to_main_ms"),
                 "exec_to_ready_ms": numbers("exec_to_ready_ms"),
                 "work_ms": numbers("work_ms")},
        "static_check": {
            "inherited": sorted(set(re.findall(r"inherited=(-?\d+)", text))),
            "private": sorted(set(re.findall(r"private=(-?\d+)", text))),
            "ihash_String": sorted(set(re.findall(r"ihash_String=(-?\d+)", text))),
        },
        "sandbox_rc": lines("sandbox_rc"),
        "sandbox_ids": lines("sandbox_ids"),
        "sandbox_ns": lines("sandbox_ns"),
        "sandbox_seccomp": lines("sandbox_seccomp"),
        "isolation": lines("iso_"),
        "parent_private": lines("parent_private"),
        "parent_ns": lines("parent_ns "),
        "gc_lines": re.findall(r"^gc pid=\d+.*$", text, re.M),
        "thread_lines": re.findall(r"^threads pid=\d+.*$", text, re.M),
        "reap_lines": re.findall(r"^(?:late_fork_)?reap\w+ .*$", text, re.M),
        "late_fork": re.findall(r"^late_fork .*$", text, re.M),
        "child_exit": lines("child_exit"),
        "log": str(log),
    }

    print("\n== Z2 memory snapshot (N=%d, %s) ==" % (args.n, time.strftime("%H:%M:%S")))
    hdr = "%-7s %-15s %-9s %9s %9s %9s %9s %9s" % (
        "pid", "role", "comm", "Rss", "Pss", "PrivDirty", "SharedCln", "SharedDirty")
    print(hdr)
    totals = dict((k, 0) for k in ("Rss", "Pss", "Private_Dirty", "Shared_Clean", "Shared_Dirty"))
    app_totals = dict((k, 0) for k in totals)
    for pid in sorted(want, key=lambda p: (roles[p] != "zygote", p)):
        u = snap[pid]["rollup"]
        print("%-7d %-15s %-9s %9d %9d %9d %9d %9d" % (
            pid, roles[pid], snap[pid]["comm"], u.get("Rss", 0), u.get("Pss", 0),
            u.get("Private_Dirty", 0), u.get("Shared_Clean", 0), u.get("Shared_Dirty", 0)))
        if roles[pid] in ("zygote", "app"):
            for k in totals:
                totals[k] += u.get(k, 0)
        if roles[pid] == "app":
            for k in app_totals:
                app_totals[k] += u.get(k, 0)
    print("%-7s %-15s %-9s %9d %9d %9d %9d %9d" % (
        "TOTAL", "zygote+apps", "", totals["Rss"], totals["Pss"],
        totals["Private_Dirty"], totals["Shared_Clean"], totals["Shared_Dirty"]))
    print("%-7s %-15s %-9s %9d %9d %9d %9d %9d" % (
        "TOTAL", "apps-only", "", app_totals["Rss"], app_totals["Pss"],
        app_totals["Private_Dirty"], app_totals["Shared_Clean"], app_totals["Shared_Dirty"]))
    print("\n-- timing --")
    print("fork->post_fork ms:", rec["fork_to_post_fork_ms"])
    print("sandbox setup ms  :", rec["sandbox_ms"])
    print("fork->ready ms    :", rec["fork_to_ready_ms"])
    print("statics           :", rec["static_check"])
    for line in rec["reap_lines"] + rec["child_exit"] + rec["late_fork"]:
        print("   ", line)
    print("\n-- sandbox / isolation lines --")
    for key in ("sandbox_rc", "sandbox_ids", "sandbox_ns", "sandbox_seccomp",
                "parent_private", "parent_ns", "isolation"):
        for line in rec[key]:
            print("   ", line)

    if args.out:
        out = Path(args.out)
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_text(json.dumps(rec, indent=1, sort_keys=True) + "\n")
        print("\nwrote", out)


if __name__ == "__main__":
    main()
