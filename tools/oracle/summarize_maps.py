#!/usr/bin/env python3
"""Summarise a /proc/<pid>/maps snapshot into memory.json.

Estimates are of *virtual* mapping size (end-start), which for anonymous
reservations (cfi shadow, scudo reserves, dalvik region space) is much larger
than RSS.  File-backed categories (apk/jar, dex/oat/art, .so, fonts,
ashmem/memfd) are the ones relevant to the ACOS memory comparison.

CLI: summarize_maps.py <maps-file> <memory.json> <key>
"""
import json
import os
import sys
from collections import defaultdict


def kind_of(p):
    if p.endswith((".dex", ".vdex", ".oat", ".art", ".odex")) or "/dalvik-cache/" in p:
        return "dex/oat/art"
    if p.endswith(".so"):
        return "native .so"
    if p.endswith((".ttf", ".otf", ".ttc")):
        return "fonts"
    if p.endswith((".apk", ".jar")):
        return "apk/jar"
    if "/dev/ashmem" in p or "memfd" in p:
        return "ashmem/memfd"
    if p == "" or "(deleted)" in p:
        return "anon/deleted"
    if "dalvik" in p:
        return "dalvik heap (virtual)"
    if p.startswith("["):
        return "anon/reserved (virtual)"
    return "other file"


def main():
    maps_path, out_path, key = sys.argv[1], sys.argv[2], sys.argv[3]
    doc = json.load(open(out_path)) if os.path.exists(out_path) else {}
    by_kind = defaultdict(lambda: {"count": 0, "bytes": 0})
    entries = []
    for line in open(maps_path, errors="replace"):
        parts = line.split(None, 5)
        if len(parts) < 5:
            continue
        ab = parts[0].split("-")
        if len(ab) != 2:
            continue
        try:
            size = int(ab[1], 16) - int(ab[0], 16)
        except ValueError:
            continue
        perms = parts[1]
        p = parts[5].strip() if len(parts) > 5 else ""
        k = kind_of(p)
        by_kind[k]["count"] += 1
        by_kind[k]["bytes"] += size
        entries.append({"path": p, "bytes": size, "perms": perms})
    entries.sort(key=lambda e: -e["bytes"])
    doc.setdefault("memory_map", {})[key] = {
        "by_kind": {k: {"count": v["count"], "mib": round(v["bytes"] / 1048576, 2)}
                    for k, v in sorted(by_kind.items())},
        "top": entries[:30],
        "total_mappings": len(entries),
        "total_virtual_mib": round(sum(e["bytes"] for e in entries) / 1048576, 1),
        "note": "sizes are virtual mapping sizes (end-start); anon reservations are not RSS",
    }
    json.dump(doc, open(out_path, "w"), indent=2)
    for k, v in sorted(by_kind.items()):
        print("  %-26s %4d mappings  %9.2f MiB" % (k, v["count"], v["bytes"] / 1048576))


if __name__ == "__main__":
    main()
