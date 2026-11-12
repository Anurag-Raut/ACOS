#!/usr/bin/env python3
"""Z6 release probe: what exactly does a session give back, when, and why.

Z5 (§3) left one question open: a zygote child releases ~45 MiB of private memory
~10 s after its first frame while an exec'd guest releases nothing even after
60 s, and the mechanism was not established.  This probe pins it:

* once per second, `smaps_rollup` for **every** managed process of the driver's
  tree (Swap/SwapPss included) and the full `smaps` **attribution** (region
  space / main space / zygote space / malloc / other, see `smaps.py`) for every
  app child and the zygote parent;
* the guest log sink (`{"cmd":"logs"}`) of every session, so an ART `GC freed` /
  `heap trim` / `Using … GC` line can be placed in the same second as the drop.

The caller (`drive.py`) owns the sessions and the process classification, so it
passes both in as callables; this module owns only the sampling loop and the
shape of the series.
"""
import os
import time

import smaps


def subtree(root):
    """Pids below `root` (children map rebuilt per call: cheap, no /proc races)."""
    children = {}
    for name in os.listdir("/proc"):
        if not name.isdigit():
            continue
        try:
            with open(f"/proc/{name}/stat", "rb") as f:
                line = f.read().decode("utf-8", "replace")
        except OSError:
            continue
        rest = line[line.rfind(")") + 2:].split()
        try:
            children.setdefault(int(rest[1]), []).append(int(name))
        except (IndexError, ValueError):
            continue
    out, todo = [], [root]
    while todo:
        pid = todo.pop()
        out.append(pid)
        todo.extend(children.get(pid, []))
    return out


def app_child(comm):
    # /proc/<pid>/comm is 16 bytes including the NUL, and ART writes the name
    # without a terminator, so "fixtures.counter" reads back as
    # "ixtures.counter" (the leading `f` is dropped — ZM1 §1.4 saw the same).
    return comm.startswith(("fixtures", "ixtures", "dev.acos"))


def probe_series(root, identify, seconds, interval=1.0, log_fn=None,
                 attribute_roles=("guest", "zygote"), max_attributed=24,
                 max_launchers=1):
    """Sample the tree for `seconds` at `interval`; return the series.

    Entries: {"t_s": float, "proc": [{pid, role, comm, app_child, rollup, attr?}],
              "sum": {pss, private_dirty, swap_pss}, "logs": [...]}
    """
    series = []
    t0 = time.time()
    next_t = 0.0
    while True:
        t = time.time() - t0
        if t + 1e-6 < next_t:
            time.sleep(min(0.02, max(0.0, next_t - t)))
            continue
        if t > seconds + 1e-6 and series:
            break
        rows = []
        attributed = 0
        launched = 0
        sum_roll = {"rss": 0, "pss": 0, "pss_dirty": 0, "private_dirty": 0,
                    "shared_dirty": 0, "swap": 0, "swap_pss": 0}
        fields = {"Rss": "rss", "Pss": "pss", "Pss_Dirty": "pss_dirty",
                  "Private_Dirty": "private_dirty", "Shared_Dirty": "shared_dirty",
                  "Swap": "swap", "SwapPss": "swap_pss"}
        for pid in sorted(subtree(root)):
            role, comm = identify(pid)
            if role in ("driver", "gone"):
                continue
            roll = smaps.read_rollup(pid)
            if roll is None:
                continue
            row = {"pid": pid, "role": role, "comm": comm,
                   "app_child": app_child(comm),
                   "rollup": {k: roll.get(k, 0) for k in fields}}
            for k, dst in fields.items():
                sum_roll[dst] += row["rollup"][k]
            want = (row["app_child"]
                    or (role in attribute_roles and comm == "main")
                    or (role == "launcher" and launched < max_launchers))
            if want and attributed < max_attributed:
                row["attr"] = smaps.attribute(pid)
                attributed += 1
                if role == "launcher":
                    launched += 1
            rows.append(row)
        entry = {"t_s": round(t, 2), "proc": rows, "sum": sum_roll}
        if log_fn is not None:
            entry["logs"] = log_fn()
        series.append(entry)
        next_t = (len(series)) * interval
    return series
