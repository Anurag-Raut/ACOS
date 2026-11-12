#!/usr/bin/env python3
"""Z6: /proc/PID/smaps parser with the five mapping buckets the Z6 brief names.

The brief asks to "attribute the drop by mapping (region space / main space /
zygote space / malloc / other)".  Two ART facts make the bucket rules simple and
exact rather than heuristic:

* the child's own Java heap lives in one anonymous mapping whose name ART sets to
  `[anon:dalvik-main space (region space)]` (`RegionSpace`; `main space` and
  `region space` are the same space), so `region_space` and `main_space` are one
  bucket here;
* the pre-fork shared heap is `[anon:dalvik-zygote space]`, the boot image is
  `[anon:dalvik-image ...]` / `boot*.art`, and the native allocator is `[heap]`
  (scudo/glibc) plus `[anon:malloc-*]`.

Everything that is not one of those is `other`; the largest `other` mappings are
kept by name in the sample so a wrongly bucketed drop cannot hide.

`Shared_*` fields are per-mapping *full* sizes (smaps semantics: a shared page is
counted in every mapper); `Pss` is the apportioned share.  `SwapPss` is kept for
every row: the guarded scope sets `memory.swap.max=0`, so it must be 0.
"""
import os
import re

# (bucket, predicate on the lowercased mapping name). Order matters.
_RULES = [
    ("region_space", lambda n: "region space" in n or "main space" in n),
    ("zygote_space", lambda n: "zygote space" in n),
    ("image", lambda n: ("dalvik-image" in n
                         or re.search(r"boot[^/]*\.(art|oat|vdex|odex)$", n) is not None
                         or "/bootimg/" in n)),
    ("malloc", lambda n: any(s in n for s in (
        "[heap]", "scudo", "anon:malloc", "libc_malloc", "gwp-asan"))),
    ("dalvik_other", lambda n: "dalvik-" in n or "allocspace" in n),
    ("gralloc", lambda n: any(s in n for s in (
        "gralloc", "ashmem", "memfd:", "/dev/kgsl", "dmabuf", "dma-buf"))),
    ("stacks", lambda n: n.startswith("[stack") or "stack_and_tls" in n),
]

BUCKETS = ("region_space", "zygote_space", "image", "malloc", "dalvik_other",
           "gralloc", "stacks", "other")

_FIELDS = {
    "Size": "size", "Rss": "rss", "Pss": "pss", "Pss_Dirty": "pss_dirty",
    "Shared_Clean": "shared_clean", "Shared_Dirty": "shared_dirty",
    "Private_Clean": "private_clean", "Private_Dirty": "private_dirty",
    "Swap": "swap", "SwapPss": "swap_pss", "Anonymous": "anonymous",
}

_HDR = re.compile(r"^[0-9a-f]+-[0-9a-f]+ ")


def bucket(name):
    n = (name or "").lower()
    for cat, pred in _RULES:
        if pred(n):
            return cat
    return "other"


def mapping_name(header):
    """The mapping's name/path: everything after the 5 fixed columns + inode."""
    parts = header.split()
    # addr perms offset dev inode [name...]
    if len(parts) > 5:
        return " ".join(parts[5:])
    return "[anon]" if len(parts) == 5 else ""


def parse_smaps(pid):
    """Full /proc/<pid>/smaps as a list of mapping dicts (KiB)."""
    out = []
    cur = None
    path = f"/proc/{pid}/smaps"
    try:
        f = open(path, "r", errors="replace")
    except OSError:
        return out
    with f:
        for line in f:
            if _HDR.match(line):
                if cur is not None:
                    out.append(cur)
                cur = {"header": line.rstrip("\n"), "name": mapping_name(line.rstrip("\n"))}
                cur.update({v: 0 for v in _FIELDS.values()})
                continue
            if cur is None or ":" not in line:
                continue
            key, _, rest = line.partition(":")
            field = _FIELDS.get(key.strip())
            if field is None:
                continue
            tok = rest.split()
            if tok:
                try:
                    cur[field] = int(tok[0])
                except ValueError:
                    cur[field] = 0
    if cur is not None:
        out.append(cur)
    return out


def attribute(pid, top_n=8):
    """One process: per-bucket KiB totals + the largest mappings by Pss."""
    maps = parse_smaps(pid)
    buckets = {b: {"pss": 0, "private_dirty": 0, "swap_pss": 0, "size": 0, "n": 0}
               for b in BUCKETS}
    for m in maps:
        b = bucket(m["name"])
        for k in ("pss", "private_dirty", "swap_pss", "size"):
            buckets[b][k] += m[k]
        buckets[b]["n"] += 1
    total = {k: sum(m[k] for m in maps)
             for k in ("size", "rss", "pss", "pss_dirty", "shared_clean",
                       "shared_dirty", "private_clean", "private_dirty",
                       "swap", "swap_pss", "anonymous")}
    top = sorted(maps, key=lambda m: m["pss"], reverse=True)[:top_n]
    return {
        "buckets": buckets,
        "total": total,
        "top": [{"name": (m["name"] or "[anon]")[:72], "pss": m["pss"],
                 "private_dirty": m["private_dirty"],
                 "bucket": bucket(m["name"])} for m in top],
        "mappings": len(maps),
    }


def read_rollup(pid):
    """smaps_rollup (KiB), including Swap/SwapPss, or None."""
    roll = {}
    try:
        with open(f"/proc/{pid}/smaps_rollup") as f:
            for line in f:
                if ":" in line:
                    k, v = line.split(":", 1)
                    parts = v.split()
                    roll[k.strip()] = int(parts[0]) if parts else 0
    except OSError:
        return None
    return roll


if __name__ == "__main__":
    import json
    import sys
    print(json.dumps(attribute(int(sys.argv[1])), indent=1))
