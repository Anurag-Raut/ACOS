#!/usr/bin/env python3
"""ZM2 part 1: per-mapping host (launcher) memory breakdown.

Reuses Z4's driver (same protocol, same checks, same runs) and replaces its
`sample_memory` with one that also dumps, for every `acos-art` launcher role:

  * full `/proc/<pid>/smaps` grouped by mapping,
  * the thread list (`/proc/<pid>/task/*/comm`, state) and
  * the fd table (`/proc/<pid>/fd`, fdinfo, socket/pipe/memfd targets)

so the report can split the launcher's Pss/Private_Dirty by component instead of
guessing. Nothing here touches product code.

    python3 research/zm2/part1.py --mode normal --n 5 --out DIR --json J
    python3 research/zm2/part1.py --mode zygote --n 5 --out DIR --json J
"""
import importlib.util
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(HERE, "..", ".."))
DRIVE = os.path.join(REPO, "research/zygote-proto/z4/drive.py")

spec = importlib.util.spec_from_file_location("z4drive", DRIVE)
z4 = importlib.util.module_from_spec(spec)
spec.loader.exec_module(z4)


def read_smaps(pid):
    rows = []
    try:
        with open(f"/proc/{pid}/smaps") as f:
            cur = None
            for line in f:
                parts = line.split()
                if not parts:
                    continue
                if "-" in parts[0] and len(parts) >= 5 and not line.startswith(" "):
                    cur = {
                        "range": parts[0],
                        "perms": parts[1],
                        "offset": parts[2],
                        "dev": parts[3],
                        "inode": parts[4],
                        "path": " ".join(parts[5:]) if len(parts) > 5 else "",
                    }
                    rows.append(cur)
                elif cur is not None and ":" in line:
                    k, v = line.split(":", 1)
                    try:
                        cur[k.strip()] = int(v.split()[0])
                    except (ValueError, IndexError):
                        pass
    except OSError:
        return []
    return rows


def threads(pid):
    out = []
    try:
        tids = os.listdir(f"/proc/{pid}/task")
    except OSError:
        return out
    for tid in tids:
        try:
            comm = open(f"/proc/{pid}/task/{tid}/comm").read().strip()
        except OSError:
            continue
        state = ""
        wchan = ""
        try:
            st = open(f"/proc/{pid}/task/{tid}/stat").read()
            state = st[st.rfind(")") + 2]
        except OSError:
            pass
        try:
            wchan = open(f"/proc/{pid}/task/{tid}/wchan").read().strip()
        except OSError:
            pass
        out.append({"tid": int(tid), "comm": comm, "state": state, "wchan": wchan})
    return out


def fds(pid):
    out = []
    try:
        names = sorted(os.listdir(f"/proc/{pid}/fd"), key=lambda x: int(x))
    except OSError:
        return out
    for name in names:
        p = f"/proc/{pid}/fd/{name}"
        try:
            target = os.readlink(p)
        except OSError:
            continue
        size = None
        try:
            size = os.stat(p).st_size
        except OSError:
            pass
        out.append({"fd": int(name), "target": target, "size": size})
    return out


def group_name(path, perms):
    """One rule per mapping path -> a stable component name."""
    p = path
    if p.startswith("/home/") or "/release/acos-art" in p:
        return "acos-art binary (r-x)" if "x" in perms else "acos-art binary (r--/rw)"
    if p.endswith("acos-art"):
        return "acos-art binary (r-x)" if "x" in perms else "acos-art binary (r--/rw)"
    if p.startswith("[stack"):
        return "thread stacks"
    if p == "[heap]":
        return "glibc heap [heap]"
    if p in ("[vdso]", "[vvar]", "[vsyscall]"):
        return "vdso/vvar"
    if "[anon:scudo" in p or "[anon:libc_malloc" in p:
        return "scudo/libc_malloc anon"
    if "/memfd:" in p or "[anon:memfd" in p:
        return "memfd (gralloc/capture)"
    if "/dev/shm/" in p or "/dev/zero" in p:
        return "dev shm/zero"
    if p.endswith(".so") or ".so." in p:
        if "libc.so" in p or "ld-linux" in p or "libm.so" in p:
            return "libc/ld.so"
        return "other .so"
    if p.startswith("/") and " " not in p:
        base = os.path.basename(p)
        return f"file: {base}"
    if p == "":
        return "anon (unnamed)"
    return p


def dump(pid):
    rows = read_smaps(pid)
    groups = {}
    for r in rows:
        g = groups.setdefault(group_name(r["path"], r["perms"]), {
            "n": 0, "size": 0, "rss": 0, "pss": 0, "private_dirty": 0,
            "private_clean": 0, "shared_clean": 0, "shared_dirty": 0, "swap": 0,
        })
        g["n"] += 1
        for k in ("Size", "Rss", "Pss", "Private_Dirty", "Private_Clean",
                  "Shared_Clean", "Shared_Dirty", "Swap"):
            g[k.lower()] += r.get(k, 0)
    status = {}
    try:
        for line in open(f"/proc/{pid}/status"):
            if ":" in line:
                k, v = line.split(":", 1)
                status[k] = v.strip()
    except OSError:
        pass
    return {
        "pid": pid,
        "groups": groups,
        "mappings": rows,
        "threads": threads(pid),
        "fds": fds(pid),
        "status": {k: status.get(k) for k in
                   ("Name", "Threads", "VmRSS", "VmSize", "VmData", "VmStk")},
    }


_orig_sample = z4.sample_memory


def sample(root, label):
    base = _orig_sample(root, label)
    detail = {}
    for pid in sorted(z4.subtree(root)):
        role, comm = z4.classify(pid)
        if role in ("launcher", "zygote"):
            detail[str(pid)] = {"role": role, "comm": comm, **dump(pid)}
    base["host_detail"] = detail
    return base


z4.sample_memory = sample

if __name__ == "__main__":
    argv = sys.argv[1:]
    if "--art" not in argv:
        pass
    z4.main()
