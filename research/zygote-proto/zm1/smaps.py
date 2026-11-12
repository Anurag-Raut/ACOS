#!/usr/bin/env python3
"""ZM1: /proc/PID/smaps parser + per-category aggregation.

The categories are the ones the ZM1 brief names. A mapping is classified from
its name/path (the first token after the inode column), with the *content*
rules first (boot image, ART spaces) and the file-extension rules last, so
`boot.art` is never counted as a generic dex/apk mapping.

`Shared_Clean`/`Shared_Dirty` are per-mapping *full* sizes (smaps semantics:
the page is counted in every process that maps it), `Pss` is the apportioned
share. That distinction is the whole point of the CoW-erosion table: a child
that still maps an inherited page shows it in Shared_*, a child that wrote to
it moves the page into Private_Dirty (and its Shared_* entry shrinks).
"""
import re
import sys

# (category, predicate on the lowercased mapping name). Order matters.
_RULES = [
    ("boot_image", lambda n: re.search(r"boot[^/]*\.(art|oat|vdex|odex)$", n) is not None
                              or "/bootimg/" in n
                              or ("dalvik-" in n and re.search(r"boot[^/]*\.(art|oat|vdex)", n))),
    ("zygote_space", lambda n: "zygote space" in n),
    ("nonmoving_space", lambda n: "non moving" in n or "non-moving" in n),
    ("region_space", lambda n: any(s in n for s in (
        "region space", "main space", "alloc space", "allocspace", "rosalloc",
        "card table", "mod union", "mark stack", "object stack", "read barrier",
        "live bitmap", "mark bitmap", "gc "))),
    ("large_object", lambda n: "large object" in n),
    ("class_meta", lambda n: any(s in n for s in (
        "class table", "class-table", "intern table", "intern-table",
        "class linker", "classlinker", "linear alloc", "linearalloc",
        "dex cache", "dex-cache", "art method", "artmethod", "method table",
        "dalvik-classes", "dalvik-irtoc", "dalvik-image"))),
    ("jit", lambda n: any(s in n for s in (
        "jit code cache", "jit-code-cache", "jit cache", "compilermetadata",
        "compiler metadata", "profile saver", "profilesaver", "jit-code"))),
    ("art_runtime", lambda n: any(s in n for s in (
        "libart.so", "libartbase", "libdexfile", "libprofile", "libc++_shared",
        "dalvikvm", "libsigchain"))),
    ("gralloc", lambda n: any(s in n for s in (
        "gralloc", "ashmem", "memfd:", "/dev/kgsl", "dmabuf", "dma-buf"))),
    ("malloc", lambda n: any(s in n for s in (
        "[heap]", "libc_malloc", "scudo", "[anon:malloc", "malloc"))),
    ("stacks", lambda n: any(s in n for s in (
        "[stack]", "[stack:", "stack_and_tls", "thread stack"))),
    ("fonts", lambda n: any(s in n for s in (".ttf", ".otf", ".ttc", "/system/fonts"))),
    ("icu", lambda n: any(s in n for s in ("icu", ".dat"))),
    ("dex_apk", lambda n: any(n.endswith(s) or (s + " ") in n
                              for s in (".apk", ".dex", ".jar", ".vdex", ".oat", ".odex"))),
    ("other_so", lambda n: n.endswith(".so") or ".so " in n),
]


def classify(name):
    n = name.lower()
    for cat, pred in _RULES:
        try:
            if pred(n):
                return cat
        except Exception:
            pass
    if n.startswith("dalvik-") or "dalvik" in n:
        return "dalvik_other"
    return "other"


def parse(path):
    """Parse one smaps file. Returns (mappings, rollup)."""
    maps = []
    roll = {}
    cur = None
    with open(path, "r", errors="replace") as f:
        for line in f:
            line = line.rstrip("\n")
            if not line:
                continue
            if re.match(r"^[0-9a-f]+-[0-9a-f]+ ", line):
                parts = line.split()
                name = " ".join(parts[5:]) if len(parts) > 5 else "[anon]"
                cur = {"name": name, "counts": {}}
                maps.append(cur)
                continue
            if ":" not in line or cur is None:
                continue
            k, v = line.split(":", 1)
            k = k.strip()
            if k in ("Size", "Rss", "Pss", "Shared_Clean", "Shared_Dirty",
                     "Private_Clean", "Private_Dirty", "Swap", "Pss_Anon",
                     "Pss_File", "Pss_Shmem", "Anonymous", "Locked"):
                try:
                    cur["counts"][k] = int(v.split()[0])
                except (ValueError, IndexError):
                    cur["counts"][k] = 0
                if k in ("Rss", "Pss", "Shared_Clean", "Shared_Dirty",
                         "Private_Clean", "Private_Dirty", "Pss_Anon",
                         "Pss_File", "Pss_Shmem"):
                    roll[k] = roll.get(k, 0) + cur["counts"][k]
    return maps, roll


_FIELDS = ("rss", "pss", "shared_clean", "shared_dirty",
           "private_clean", "private_dirty", "size", "n")


def aggregate(maps):
    out = {}
    for m in maps:
        cat = classify(m["name"])
        c = m["counts"]
        a = out.setdefault(cat, {k: 0 for k in _FIELDS})
        a["rss"] += c.get("Rss", 0)
        a["pss"] += c.get("Pss", 0)
        a["shared_clean"] += c.get("Shared_Clean", 0)
        a["shared_dirty"] += c.get("Shared_Dirty", 0)
        a["private_clean"] += c.get("Private_Clean", 0)
        a["private_dirty"] += c.get("Private_Dirty", 0)
        a["size"] += c.get("Size", 0)
        a["n"] += 1
    return out


def top_names(maps, limit=8):
    """Biggest mappings by Pss, with their category (evidence for the classifier)."""
    rows = sorted(maps, key=lambda m: m["counts"].get("Pss", 0), reverse=True)
    return [{"name": m["name"][:160], "cat": classify(m["name"]),
             "pss": m["counts"].get("Pss", 0),
             "shared_dirty": m["counts"].get("Shared_Dirty", 0),
             "private_dirty": m["counts"].get("Private_Dirty", 0)}
            for m in rows[:limit]]


def to_mib(cats):
    return {cat: {k: round(v / 1024.0, 2) for k, v in vals.items()}
            for cat, vals in cats.items()}


def pss_mib(cats):
    return round(sum(v["pss"] for v in cats.values()) / 1024.0, 2)


def main():
    for path in sys.argv[1:]:
        maps, roll = parse(path)
        cats = aggregate(maps)
        print(f"== {path}  Rss={roll.get('Rss',0)/1024:.1f} "
              f"Pss={roll.get('Pss',0)/1024:.1f} "
              f"PD={roll.get('Private_Dirty',0)/1024:.1f} "
              f"SD={roll.get('Shared_Dirty',0)/1024:.1f} "
              f"SC={roll.get('Shared_Clean',0)/1024:.1f} MiB")
        for cat, v in sorted(cats.items(), key=lambda kv: -kv[1]["pss"]):
            print(f"  {cat:15s} pss={v['pss']/1024:8.2f} rss={v['rss']/1024:8.2f} "
                  f"sc={v['shared_clean']/1024:8.2f} sd={v['shared_dirty']/1024:8.2f} "
                  f"pc={v['private_clean']/1024:7.2f} pd={v['private_dirty']/1024:8.2f} "
                  f"n={v['n']}")
        print("  top:", ", ".join(f"{r['name'].split('/')[-1]}[{r['cat']}]={r['pss']/1024:.1f}"
                                  for r in top_names(maps, 6)))


if __name__ == "__main__":
    main()
