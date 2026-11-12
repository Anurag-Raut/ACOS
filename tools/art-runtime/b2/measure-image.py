#!/usr/bin/env python3
"""B2: N concurrent `acos-art run` sessions of the heap/maps probe, with the
boot image either ART's default (LZ4-compressed, decompressed into private anon
memory) or the uncompressed image built by `build-uncompressed.sh` and passed
through the product's own `-Ximage` seam (`acos-art run --image DIR`).

Measures, per session, from /proc/PID/smaps:
  * the boot image mappings (file-backed `.art` for an uncompressed image, or
    the `[anon:dalvik-...boot...art]` decompression of a compressed one):
    Size/Rss/Pss/Private_Dirty/Private_Clean/Shared_Clean;
  * region space (the CC heap reservation);
and records each guest's reported image base addresses (relocation/ASLR) and
the spawn->READY wall time.

No product code is changed: `acos-art run --image` is the documented seam.
Every run goes through run-exclusive.sh + guarded.sh (the caller wraps this).
"""
import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path("/home/Qui/Projects/acos")
ART = ROOT / "target/release/acos-art"
OUT = ROOT / ".cache/b2/probe"
SDK = Path(os.environ.get("ANDROID_SDK_ROOT", "/home/Qui/Android/Sdk"))
ANDROID_JAR = SDK / "platforms/android-34/android.jar"
D8 = SDK / "build-tools/34.0.0/d8"
SRC = ROOT / "research/b2/HeapProbe.java"
MiB = 1024.0 * 1024.0
UNCOMPRESSED = ROOT / ".cache/b2/image"


def build_probe():
    OUT.mkdir(parents=True, exist_ok=True)
    classes = OUT / "classes"
    if classes.exists():
        shutil.rmtree(classes)
    classes.mkdir(parents=True)
    subprocess.run(["javac", "-nowarn", "-source", "11", "-target", "11",
                    "-cp", str(ANDROID_JAR), "-d", str(classes), str(SRC)],
                   check=True, capture_output=True)
    subprocess.run([str(D8), "--lib", str(ANDROID_JAR), "--min-api", "34",
                    "--output", str(OUT), str(classes / "b2/HeapProbe.class")],
                   check=True, capture_output=True)


def children(pid):
    out = set()
    try:
        for d in os.listdir("/proc"):
            if not d.isdigit():
                continue
            try:
                st = Path("/proc/%s/stat" % d).read_text()
            except OSError:
                continue
            # comm may contain spaces; ppid is field 4 after the last ')'
            rest = st[st.rfind(")") + 2:].split()
            if len(rest) > 1 and int(rest[1]) == pid:
                out.add(int(d))
    except OSError:
        pass
    return out


def descendants(pid):
    seen, todo = set(), [pid]
    while todo:
        p = todo.pop()
        for c in children(p):
            if c not in seen:
                seen.add(c)
                todo.append(c)
    return seen


IMG_KEYS = ("Size", "Rss", "Pss", "Private_Clean", "Private_Dirty",
            "Shared_Clean", "Shared_Dirty")


def smaps_groups(pid):
    groups = {"image_file": dict.fromkeys(IMG_KEYS, 0),
              "image_anon": dict.fromkeys(IMG_KEYS, 0),
              "region_space": dict.fromkeys(IMG_KEYS, 0)}
    cur = None
    try:
        f = open("/proc/%d/smaps" % pid)
    except OSError:
        return groups
    with f:
        for line in f:
            m = re.match(r"^([0-9a-f]+)-([0-9a-f]+) (\S+) \S+ \S+ \S+\s*(.*)$", line)
            if m:
                path = m.group(4).strip()
                if path.endswith(".art"):
                    cur = "image_file"          # file-mapped (uncompressed)
                elif path.startswith("[anon:dalvik-") and path.endswith(".art]"):
                    cur = "image_anon"          # decompressed (LZ4) image
                elif path == "[anon:dalvik-main space (region space)]":
                    cur = "region_space"
                else:
                    cur = None
                continue
            if cur is None or ":" not in line:
                continue
            k, _, v = line.partition(":")
            if k in IMG_KEYS:
                try:
                    groups[cur][k] += int(v.strip().split()[0]) * 1024
                except (ValueError, IndexError):
                    pass
    return groups


def guest_pids(root):
    # The guest's comm is "main" (dalvikvm renames its main thread), so the
    # process is identified by the ART address-space layout, not by comm.
    out = []
    for p in descendants(root):
        try:
            maps = Path("/proc/%d/maps" % p).read_text()
        except OSError:
            continue
        if "dalvik-main space" in maps:
            out.append(p)
    return out


def launch(index, n, image, hold_ms, pids):
    argv = [str(ART), "run", "--dex", str(OUT / "classes.dex"),
            "--class", "b2.HeapProbe", "--timeout", "600", "--json"]
    if image == "uncompressed":
        argv += ["--image", str(UNCOMPRESSED)]
    else:
        argv += ["--image", "none"]
    argv += ["--", "--hold-ms", str(hold_ms), "--maps"]
    env = dict(os.environ)
    env["ACOS_ART_DATA"] = str(ROOT / (".cache/b2/run-%d-%d" % (os.getpid(), index)))
    t0 = time.time()
    p = subprocess.Popen(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                         env=env, text=True, bufsize=1)
    pids.append(p)
    return p, t0


def maps_bases(pid):
    bases = {}
    try:
        for line in Path("/proc/%d/maps" % pid).read_text().splitlines():
            parts = line.split(None, 5)
            if len(parts) < 6:
                continue
            start, path = parts[0].split("-")[0], parts[5].strip()
            if path.endswith(".art"):
                key = path
            elif path == "[anon:dalvik-main space (region space)]":
                key = "region-space"
            else:
                continue
            bases.setdefault(key, start)
    except OSError:
        pass
    return {"bases": bases}


def guest_state(pid):
    groups = smaps_groups(pid)
    return groups


def run(args):
    build_probe()
    pids, procs = [], []
    for i in range(args.n):
        p, t0 = launch(i, args.n, args.image, args.hold_ms, pids)
        procs.append({"p": p, "t0": t0, "i": i, "ready_s": None, "gpid": None})
    # Readiness is a host-side fact, not a stdout line: `acos-art run` buffers
    # guest stdout until exit, so waiting for a printed marker would deadlock.
    # Ready = the guest process exists, the boot image is mapped, and the CC
    # region space has been touched.
    deadline = time.time() + args.timeout
    for s in procs:
        while time.time() < deadline:
            gp = guest_pids(s["p"].pid)
            if gp:
                s["gpid"] = gp[0]
                g = smaps_groups(gp[0])
                img = g["image_file"]["Rss"] + g["image_anon"]["Rss"]
                if img > 0 and g["region_space"]["Rss"] > 0.25 * MiB:
                    s["ready_s"] = time.time() - s["t0"]
                    break
            if s["p"].poll() is not None:
                break
            time.sleep(0.02)
        if s["ready_s"] is None:
            raise SystemExit("session %d never became ready (rc=%r)"
                             % (s["i"], s["p"].poll()))
    time.sleep(args.settle)
    samples = []
    for s in procs:
        gp = s["gpid"]
        groups = smaps_groups(gp)
        roll = {}
        try:
            for line in Path("/proc/%d/smaps_rollup" % gp).read_text().splitlines():
                k, _, v = line.partition(":")
                if k in ("Rss", "Pss", "Private_Dirty", "Shared_Clean"):
                    roll[k] = int(v.strip().split()[0]) * 1024
        except OSError:
            pass
        samples.append({"i": s["i"], "pid": gp, "ready_s": s["ready_s"],
                        "maps": maps_bases(gp), "groups": groups, "rollup": roll})
    for p in pids:
        try:
            p.terminate()
        except OSError:
            pass
    survivors = []
    for p in pids:
        try:
            p.wait(timeout=20)
        except subprocess.TimeoutExpired:
            p.kill()
            p.wait()
    time.sleep(1.0)
    for s in samples:
        if Path("/proc/%d" % s["pid"]).exists():
            survivors.append(s["pid"])

    def agg(group, key):
        return sum(x["groups"][group][key] for x in samples)

    doc = {
        "schema": "acos-b2-image-measure/1",
        "utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "image": args.image,
        "n": args.n,
        "hold_ms": args.hold_ms,
        "sessions": samples,
        "sum": {
            g: {k: agg(g, k) for k in IMG_KEYS}
            for g in ("image_file", "image_anon", "region_space")
        },
        "ready_s": [x["ready_s"] for x in samples],
        "survivors": survivors,
    }
    return doc


def render(doc):
    s = doc["sum"]
    lines = []
    lines.append("### %s image, N=%d" % (doc["image"], doc["n"]))
    lines.append("")
    lines.append("| group | Size MiB | Rss MiB | Pss MiB | Private_Clean MiB | "
                 "Private_Dirty MiB | Shared_Clean MiB |")
    lines.append("|---|---:|---:|---:|---:|---:|---:|")
    for g in ("image_file", "image_anon", "region_space"):
        v = s[g]
        lines.append("| %s | %.2f | %.2f | %.2f | %.2f | %.2f | %.2f |" % (
            g, v["Size"] / MiB, v["Rss"] / MiB, v["Pss"] / MiB,
            v["Private_Clean"] / MiB, v["Private_Dirty"] / MiB,
            v["Shared_Clean"] / MiB))
    per = [x["groups"] for x in doc["sessions"]]
    lines.append("")
    lines.append("Per session (Rss / Private_Dirty / Shared_Clean, MiB): " + "; ".join(
        "#%d %s %.2f/%.2f/%.2f  region %.2f/%.2f/%.2f" % (
            x["i"],
            "file" if x["groups"]["image_file"]["Rss"] else "anon",
            x["groups"]["image_file"]["Rss"] / MiB,
            x["groups"]["image_file"]["Private_Dirty"] / MiB,
            x["groups"]["image_file"]["Shared_Clean"] / MiB,
            x["groups"]["region_space"]["Rss"] / MiB,
            x["groups"]["region_space"]["Private_Dirty"] / MiB,
            x["groups"]["region_space"]["Shared_Clean"] / MiB)
        for x in doc["sessions"]))
    lines.append("")
    lines.append("Launch (spawn->READY, s): " + ", ".join(
        "%.3f" % r for r in doc["ready_s"]))
    lines.append("")
    lines.append("Image bases: " + json.dumps(
        [x["maps"] for x in doc["sessions"]], separators=(",", ":")))
    lines.append("Survivors: %s" % (doc["survivors"] if doc["survivors"] else "none"))
    return "\n".join(lines)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--n", type=int, default=1)
    ap.add_argument("--image", choices=("default", "uncompressed"),
                    default="uncompressed")
    ap.add_argument("--hold-ms", type=int, default=30000)
    ap.add_argument("--settle", type=float, default=1.0)
    ap.add_argument("--timeout", type=float, default=180.0)
    ap.add_argument("--out", default=str(ROOT / ".cache/b2/measure"))
    a = ap.parse_args()
    doc = run(a)
    out = Path(a.out)
    out.mkdir(parents=True, exist_ok=True)
    name = "%s-n%d-%d.json" % (a.image, a.n, int(time.time()))
    (out / name).write_text(json.dumps(doc, indent=1))
    print(render(doc))
    print("\nwrote %s" % (out / name))
    return 0 if not doc["survivors"] else 1


if __name__ == "__main__":
    sys.exit(main())
