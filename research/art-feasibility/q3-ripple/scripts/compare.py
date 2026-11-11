#!/usr/bin/env python3
"""E4H comparison: our software-draw PNG/tree vs the ART oracle and ACOS-VM.

No ACOS crate, no CLI: this only reads the PNG/tree artefacts.

  scripts/compare.py            # everything, from .cache/art-exp-h/ + fixtures/
"""
import json
import sys
from pathlib import Path

import numpy as np
from PIL import Image

ROOT = Path(__file__).resolve().parents[4]
ART = ROOT / ".cache/art-exp/data/out"
ORACLE_PNG = ROOT / "fixtures/reference/counter/art-api34-x86_64/screenshot_initial.png"
ORACLE_XML = ROOT / "fixtures/reference/counter/art-api34-x86_64/ui_initial.xml"
ACOS_PNG = ROOT / "results/parity/w193a/counter.png"
ACOS_TREE = ROOT / "results/parity/w193a/counter.tree.json"
CONTENT_TOP = 275   # dumpsys window displays: app=1080x2400, mAppBounds=Rect(0,128-1080,2337)
CONTENT_BOTTOM = 2337


def ssim(a, b):
    """Global SSIM (gaussian-free, 8x8 block version), grayscale."""
    a = np.asarray(a.convert("L"), dtype=np.float64)
    b = np.asarray(b.convert("L"), dtype=np.float64)
    if a.shape != b.shape:
        return float("nan")
    c1, c2 = (0.01 * 255) ** 2, (0.03 * 255) ** 2
    out = []
    bs = 8
    h, w = a.shape
    for y in range(0, h - bs + 1, bs):
        for x in range(0, w - bs + 1, bs):
            pa = a[y:y + bs, x:x + bs]
            pb = b[y:y + bs, x:x + bs]
            ma, mb = pa.mean(), pb.mean()
            va, vb = pa.var(), pb.var()
            cov = ((pa - ma) * (pb - mb)).mean()
            out.append(((2 * ma * mb + c1) * (2 * cov + c2)) /
                       ((ma ** 2 + mb ** 2 + c1) * (va + vb + c2)))
    return float(np.mean(out))


def crop(p, already_content=False):
    """Our PNG is already the content area (1080x2062); the references are full
    1080x2400 screenshots, so crop them to the device app content band."""
    im = Image.open(p).convert("RGBA")
    if already_content:
        return im
    assert im.height >= CONTENT_BOTTOM, (p, im.size)
    return im.crop((0, CONTENT_TOP, im.width, CONTENT_BOTTOM))


def nodes_json(p):
    """Flatten our tree (list of dicts with bounds strings) or the ACOS tree."""
    d = json.loads(p.read_text())
    if "nodes" in d and isinstance(d["nodes"], list) and not d["nodes"]:
        return d
    if "result" in d:  # ACOS-VM
        out = []

        def walk(n):
            out.append({
                "class": n.get("class", ""),
                "id": n.get("resource_id", ""),
                "text": n.get("text", ""),
                "bounds": n.get("bounds"),
            })
            for c in n.get("children", []):
                walk(c)
        walk(d["result"]["tree"]["nodes"][0])
        return out
    # our own JSON: {"bounds": "...", "nodes": [ {..children..} ]}
    out = []

    def walk(n):
        b = n["bounds"]
        L = b.replace("[", "").replace("]", ",").split(",")[:4]
        out.append({
            "class": n.get("class", ""),
            "id": n.get("id", "").replace("dev.acos.fixtures.counter:", "")
            .replace("android:", "android:"),
            "text": n.get("text", ""),
            "bounds": [int(x) for x in L],
        })
        for c in n.get("children", []):
            walk(c)
    for n in d["nodes"]:
        walk(n)
    return out


def oracle_nodes():
    import xml.etree.ElementTree as ET
    t = ET.parse(ORACLE_XML)
    out = []

    def walk(n, depth=0):
        b = n.attrib.get("bounds", "")
        if not b:
            b = "[0,0][0,0]"
        L = [int(x) for x in b.replace("[", "").replace("]", ",").split(",")[:4]]
        rid = n.attrib.get("resource-id", "")
        out.append({
            "class": n.attrib.get("class", ""),
            "id": rid.replace("dev.acos.fixtures.counter:", "").replace("android:", "android:"),
            "text": n.attrib.get("text", ""),
            "bounds": L,
        })
        for c in n:
            walk(c, depth + 1)
    walk(t.getroot())
    return out


def diff_trees(ours, ref, dy=CONTENT_TOP, label=""):
    def norm(rid):
        # "dev.acos.fixtures.counter:id/count" / "id/count" / "@0xffffffff"
        if "id/" in rid:
            return "id/" + rid.split("id/")[-1]
        return rid

    def key(n):
        if n["id"] and not n["id"].startswith("@"):
            return "id:" + norm(n["id"])
        if n["text"]:
            return "text:%s|%s" % (n["class"], n["text"])
        return "class:" + n["class"]

    om = {}
    for n in ours:
        om.setdefault(key(n), []).append(n)
    rows = []
    for r in ref:
        k = key(r)
        cand = om.get(k)
        if not cand:
            # the oracle's decor/status/navigation rows are not in the content area
            if r["id"] in ("android:id/decor_content_parent", "android:id/action_bar",
                           "android:id/action_bar_container", "android:id/statusBarBackground",
                           "android:id/navigationBarBackground") or r["class"] == "android.view.View":
                continue
            rows.append({"key": k, "result": "missing_in_ours",
                         "ref": r["bounds"], "ours": None})
            continue
        o = cand.pop(0)
        rb = list(r["bounds"])
        ob = list(o["bounds"])
        dy_eff = 0 if k.startswith("class:android.widget.FrameLayout") else dy
        exp = [rb[0], rb[1] - dy_eff, rb[2], rb[3] - dy_eff]
        delta = [ob[i] - exp[i] for i in range(4)]
        rows.append({
            "key": k, "result": "match" if delta == [0, 0, 0, 0] else "bounds",
            "ref": exp, "ours": ob, "delta": delta,
        })
    return rows


def report():
    res = {}
    for variant in ("e4h-view.png", "e4h-view-nocaps.png"):
        vp = ART / variant
        if not vp.exists():
            continue
        ours = crop(vp, already_content=True)
        print("E4H png %s  content %dx%d" % (vp, ours.width, ours.height))
        for name, ref in (("art-oracle", ORACLE_PNG), ("acos-vm", ACOS_PNG)):
            if not ref.exists():
                print("%-12s no reference at %s" % (name, ref))
                continue
            o = crop(ref)
            s = ssim(ours, o)
            a = np.asarray(ours.convert("RGB"), dtype=np.int16)
            b = np.asarray(o.convert("RGB"), dtype=np.int16)
            mae = float(np.abs(a - b).mean())
            exact = float((np.all(a == b, axis=2)).mean())
            res.setdefault(variant, {})[name] = {
                "ssim": round(s, 5), "mae": round(mae, 3), "pixel_exact": round(exact, 5)}
            print("  %-11s SSIM=%.5f  MAE=%.3f/255  identical_pixels=%.2f%%  (ref %s)"
                  % (name, s, mae, 100 * exact, ref.relative_to(ROOT)))

    for variant in ("e4h-tree.txt", "e4h-tree-nocaps.txt"):
        tp = ART / variant
        if not tp.exists():
            continue
        ours_nodes = nodes_json_text(tp.read_text())
        print("\ntree %s:" % variant)
        for name, refnodes in (("art-oracle", oracle_nodes()),
                               ("acos-vm", nodes_json(ACOS_TREE))):
            rows = diff_trees(ours_nodes, refnodes)
            match = [r for r in rows if r["result"] == "match"]
            print("  -- vs %s: %d/%d nodes exact" % (name, len(match), len(rows)))
            for r in rows:
                if r["result"] != "match":
                    print("     %-48s ref=%-22s ours=%-22s delta=%s"
                          % (r["key"], r.get("ref"), r.get("ours"), r.get("delta")))
            res.setdefault("tree", {}).setdefault(variant, {})[name] = {
                "exact": len(match), "total": len(rows)}
    print("\nJSON " + json.dumps(res, indent=1))


def nodes_json_text(text):
    d = json.loads(text)
    out = []

    def walk(n):
        b = n["bounds"]
        L = [int(x) for x in b.replace("[", "").replace("]", ",").split(",")[:4]]
        out.append({
            "class": n.get("class", ""),
            "id": n.get("id", "").replace("dev.acos.fixtures.counter:", ""),
            "text": n.get("text", ""),
            "bounds": L,
        })
        for c in n.get("children", []):
            walk(c)
    for n in d["nodes"]:
        walk(n)
    return out


if __name__ == "__main__":
    report()
