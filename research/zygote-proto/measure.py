#!/usr/bin/env python3
"""Z1: measure the Zygote prototype.

Starts research/zygote-proto/run.sh, waits until the parent reports HOLD_START
(children idle after their work), samples /proc/<pid>/smaps_rollup and the
grouped /proc/<pid>/smaps of the parent and every child, then reports
Rss/Pss/Private_Dirty/Shared_Clean/Shared_Dirty plus the dalvik-specific
mappings (boot image, zygote space, region/main/non-moving spaces).

    tools: python3 measure.py --n 2 --hold 6000 --preload 800 --out results/art-runtime/Z1-N2.json

Everything runs under the ART lock: the caller wraps this script in
tools/art-runtime/run-exclusive.sh + tools/guarded.sh (see results/art-runtime/Z1.md).
"""
import argparse
import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path("/home/Qui/Projects/acos")
RUN = ROOT / "research/zygote-proto/run.sh"

ROLLUP = ["Rss", "Pss", "Private_Clean", "Private_Dirty", "Shared_Clean",
          "Shared_Dirty", "Swap"]
# which smaps groups we care about, by name substring
GROUPS = {
    "boot_image": ("boot.art", "boot.oat", "boot-framework", "boot-core"),
    "dalvik_zygote_space": ("dalvik-zygote",),
    "dalvik_region_space": ("dalvik-region",),
    "dalvik_main_space": ("dalvik-main space", "dalvik-non moving", "dalvik-LOS",
                          "dalvik-rosalloc"),
    "dalvik_all_anon": ("dalvik-",),
    "zygote_flag": ("dalvik-zygote flag",),
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
    """{group: {Rss,Pss,Private_Dirty,...}} summed over matching mappings."""
    tot = {g: dict((f, 0) for f in ("Rss", "Pss", "Private_Dirty", "Shared_Clean",
                                    "Shared_Dirty", "Shared_Hugetlb")) for g in GROUPS}
    seen = {g: 0 for g in GROUPS}
    cur = None
    for line in read_text("/proc/%d/smaps" % pid).splitlines():
        if re.match(r"^[0-9a-f]+-[0-9a-f]+ ", line):
            cur = line.split(" ", 5)[5].strip() if len(line.split(" ", 5)) > 5 else "[anon]"
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


def children_of(pid):
    kids = []
    for d in os.listdir("/proc"):
        if not d.isdigit():
            continue
        st = read_text("/proc/%s/stat" % d)
        if not st:
            continue
        try:
            fields = st[st.rindex(")") + 2:].split()
            ppid = int(fields[1])
        except (ValueError, IndexError):
            continue
        if ppid == pid:
            kids.append(int(d))
    return kids


def find_candidates():
    """Every pid whose cmdline names our ZygoteMain (bwrap's supervisor carries a
    copy of the target argv, so there can be more than one)."""
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


def mapping_hist(pid):
    """{name: {Rss,Pss,Private_Dirty}} for every mapping of one process."""
    out = {}
    cur = None
    for line in read_text("/proc/%d/smaps" % pid).splitlines():
        if re.match(r"^[0-9a-f]+-[0-9a-f]+ ", line):
            parts = line.split(" ", 5)
            cur = parts[5].strip() if len(parts) > 5 else "[anon]"
            out.setdefault(cur, {"Rss": 0, "Pss": 0, "Private_Dirty": 0, "Shared_Clean": 0})
            continue
        if not line or cur is None or line[0] not in "RPSs":
            continue
        k, _, v = line.partition(":")
        k = k.strip()
        if k in ("Rss", "Pss", "Private_Dirty", "Shared_Clean"):
            out[cur][k] += int(v.split()[0])
    return out


def sample(pids):
    rec = {}
    for pid in pids:
        r = rollup(pid)
        if not r:
            continue
        g, seen = smaps_grouped(pid)
        rec[pid] = {"rollup": r, "groups": g, "mappings": seen,
                    "names": mapping_hist(pid),
                    "raw_smaps": read_text("/proc/%d/smaps" % pid),
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

    log = Path(args.log) if args.log else Path("/home/Qui/Projects/acos/.cache/z1/logs/run-n%d.log" % args.n)
    log.parent.mkdir(parents=True, exist_ok=True)
    cmd = [str(RUN), "fork", str(args.n), str(args.hold), str(args.preload)]
    with open(log, "w") as fh:
        proc = subprocess.Popen(cmd, stdout=fh, stderr=subprocess.STDOUT)
        t0 = time.time()
        start = None
        while time.time() - t0 < 240:
            txt = read_text(log)
            if "HOLD_START" in txt or "PARENT_DONE" in txt or proc.poll() is not None:
                start = time.time()
                break
            time.sleep(0.05)
        time.sleep(args.settle)
        cands = find_candidates()
        allpids = subtree(cands) if cands else set()
        snap = sample(sorted(allpids))
        # decide roles *while* the tree is alive: the ART parent is the largest
        # process of the tree, its children are every other process with a
        # non-trivial footprint (the rest is bwrap/supervisor glue)
        parent = max(snap, key=lambda p: snap[p]["rollup"].get("Rss", 0)) if snap else None
        pm = ppid_map()
        kids = sorted(p for p in snap
                      if parent and p != parent
                      and snap[p]["rollup"].get("Rss", 0) > 10240)
        proc.wait()
    text = read_text(log)

    def field(name):
        m = re.search(r"^%s(.*)$" % name, text, re.M)
        return m.group(0).strip() if m else ""

    def numbers(key):
        return [int(x) for x in re.findall(r"%s=(\d+)" % key, text)]

    rec = {
        "n": args.n,
        "hold_ms": args.hold,
        "preload_limit": args.preload,
        "parent_pid": parent,
        "child_pids": kids,
        "snapshot": snap,
        "fork_to_post_fork_ms": numbers("fork_to_post_fork_ms"),
        "fork_to_ready_ms": numbers("fork_to_ready_ms"),
        "cold": {"exec_to_main_ms": numbers("exec_to_main_ms"),
                 "exec_to_ready_ms": numbers("exec_to_ready_ms"),
                 "work_ms": numbers("work_ms")},
        "static_check": {
            "inherited": sorted(set(re.findall(r"inherited=(-?\d+)", text))),
            "private": sorted(set(re.findall(r"private=(-?\d+)", text))),
            "ihash_String": sorted(set(re.findall(r"ihash_String=(-?\d+)", text))),
            "threads": sorted(set(re.findall(r"threads=(\d+)", text))),
        },
        "gc_lines": re.findall(r"^gc pid=\d+.*$", text, re.M),
        "thread_lines": re.findall(r"^threads pid=\d+.*$", text, re.M),
        "reap_lines": re.findall(r"^(?:late_fork_)?reap\w+ .*$", text, re.M),
        "late_fork": re.findall(r"^late_fork .*$", text, re.M),
        "log": str(log),
    }

    # table
    print("\n== Z1 memory snapshot (N=%d children + parent, %s) ==" % (args.n, time.strftime("%H:%M:%S")))
    hdr = "%-7s %-10s %9s %9s %9s %9s %9s" % ("pid", "role", "Rss", "Pss", "PrivDirty", "SharedCln", "SharedDirty")
    print(hdr)
    totals = dict((k, 0) for k in ("Rss", "Pss", "Private_Dirty", "Shared_Clean", "Shared_Dirty"))
    for pid, r in sorted(snap.items(), key=lambda kv: kv[0]):
        role = "parent" if pid == parent else "child"
        u = r["rollup"]
        print("%-7d %-10s %9d %9d %9d %9d %9d" % (pid, role, u.get("Rss", 0), u.get("Pss", 0),
                                                  u.get("Private_Dirty", 0), u.get("Shared_Clean", 0),
                                                  u.get("Shared_Dirty", 0)))
        for k in totals:
            totals[k] += u.get(k, 0)
    print("%-7s %-10s %9d %9d %9d %9d %9d" % ("TOTAL", "", totals["Rss"], totals["Pss"],
                                              totals["Private_Dirty"], totals["Shared_Clean"],
                                              totals["Shared_Dirty"]))
    print("\n-- grouped mappings (KiB, summed over the processes above) --")
    for g in GROUPS:
        vals = dict((k, sum(r["groups"][g][k] for r in snap.values())) for k in
                    ("Rss", "Pss", "Private_Dirty", "Shared_Clean"))
        nmap = sum(r["mappings"][g] for r in snap.values())
        if nmap:
            print("%-22s maps=%-3d Rss=%-8d Pss=%-8d PrivDirty=%-8d SharedClean=%d"
                  % (g, nmap, vals["Rss"], vals["Pss"], vals["Private_Dirty"], vals["Shared_Clean"]))
    print("\n-- mapping names, per process (KiB; the dalvik/image ones only) --")
    for pid, r in sorted(snap.items(), key=lambda kv: kv[0]):
        role = "parent" if pid == parent else "child"
        print(" %s %d:" % (role, pid))
        for name, v in sorted(r["names"].items(), key=lambda kv: -kv[1]["Rss"]):
            if v["Rss"] < 256:
                continue
            if not any(t in name for t in ("dalvik", "boot", "[heap]", "scudo", "libart")):
                continue
            print("    %-46s Rss=%-8d Pss=%-8d PrivDirty=%-6d SharedClean=%d"
                  % (name, v["Rss"], v["Pss"], v["Private_Dirty"], v["Shared_Clean"]))
    print("\n-- timing --")
    print("fork->post_fork ms:", rec["fork_to_post_fork_ms"])
    print("fork->ready ms    :", rec["fork_to_ready_ms"])
    print("cold exec->main ms:", rec["cold"]["exec_to_main_ms"],
          " cold exec->ready ms:", rec["cold"]["exec_to_ready_ms"])
    print("statics           :", rec["static_check"])
    print("gc lines          :", len(rec["gc_lines"]))
    for line in rec["gc_lines"]:
        print("   ", line)
    for line in rec["thread_lines"]:
        print("   ", line)
    for line in rec["reap_lines"] + rec["late_fork"]:
        print("   ", line)

    if args.out:
        out = Path(args.out)
        out.parent.mkdir(parents=True, exist_ok=True)
        raw = Path(str(out) + ".smaps")
        with open(raw, "w") as fh:
            for pid, r in rec["snapshot"].items():
                fh.write("### pid %d %s\n%s\n" % (pid, r.get("cmdline", ""), r.pop("raw_smaps", "")))
        rec["raw_smaps_file"] = str(raw)
        out.write_text(json.dumps(rec, indent=1, sort_keys=True) + "\n")
        print("\nwrote", out)


if __name__ == "__main__":
    main()
