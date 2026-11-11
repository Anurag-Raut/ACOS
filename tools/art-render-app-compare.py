#!/usr/bin/env python3
"""Compare one `acos-art render-app` window against the ART oracle.

`render-app` now builds the APK's real window chrome (`PhoneWindow` ->
DecorView -> ActionBarOverlayLayout -> id/content) and draws the **full window
frame** at `width x panelHeight` with the device's real insets, so this
comparator no longer crops our frame to the content band: it compares the
full 1080x2400 frame and, separately, the decor band (the area the app window
owns, derived from the oracle's `statusBarBackground` / `navigationBarBackground`
nodes — never hardcoded here).

Tree comparison is full-hierarchy from the DecorView down, in device
coordinates (`--dy 0` by default). The ART oracle tree is the emulator's
`ui_initial.xml` (`oracle_nodes()` in E4H's comparator, imported unchanged);
`--oracle-tree` accepts an existing JSON tree (e.g.
`results/parity/w193a/counter.tree.json`) for the same diff.

Reuses E4H's comparator (`research/art-feasibility/h-view/scripts/compare.py`:
`ssim`, `nodes_json_text`, `oracle_nodes`, `diff_trees`) instead of
re-implementing SSIM / bounds matching.

    tools/art-render-app-compare.py --png OUT/frame.png --tree OUT/tree.json \
        --app counter [--oracle-tree results/parity/w193a/counter.tree.json]

Exit 0 when the full-frame SSIM is >= 0.99 and every *structural* node
(DecorView, decor_content_parent, action bar container/toolbar, id/content,
statusBarBackground, navigationBarBackground, the activity's content root) is
present with exact bounds. Non-structural text rows are reported with their px
delta (the ActionBar title is font-file dependent).
"""
import argparse
import importlib.util
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CMP = ROOT / "research/art-feasibility/h-view/scripts/compare.py"

# Full-hierarchy nodes every window must expose (matched by resource id).
STRUCTURAL_IDS = (
    "id:id/decor_content_parent",
    "id:id/action_bar_container",
    "id:id/action_bar",
    "id:id/content",
    "id:id/statusBarBackground",
    "id:id/navigationBarBackground",
)


def load_cmp():
    # Do not leave __pycache__ in the research tree we import from.
    sys.dont_write_bytecode = True
    spec = importlib.util.spec_from_file_location("e4h_compare", CMP)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def xml_nodes(cmp_mod, xml_path):
    """uiautomator XML -> flat node dicts, minus the <hierarchy> pseudo-node."""
    cmp_mod.ORACLE_XML = Path(xml_path)
    nodes = cmp_mod.oracle_nodes()
    return [n for n in nodes if n["class"] or n["id"] or n["text"]]


def json_nodes(doc):
    """An ACOS/agent-device tree JSON -> flat node dicts (same field names)."""
    tree = doc.get("result", {}).get("tree", doc)
    roots = tree.get("nodes", []) if isinstance(tree, dict) else []
    out = []

    def walk(n):
        b = n.get("bounds", "[0,0][0,0]")
        if isinstance(b, str):
            L = [int(x) for x in b.replace("[", "").replace("]", ",").split(",")[:4]]
        else:
            L = list(b)
        out.append({
            "class": n.get("class", ""),
            "id": (n.get("resource_id") or n.get("id") or "")
            .replace("android:", "android:"),
            "text": n.get("text", ""),
            "bounds": L,
        })
        for c in n.get("children", []):
            walk(c)

    for r in roots:
        walk(r)
    return out


def normalize(nodes):
    """uiautomator reports DecorView as FrameLayout; match on the real class."""
    for n in nodes:
        if n["class"] == "com.android.internal.policy.DecorView":
            n["class"] = "android.widget.FrameLayout"
        n["id"] = n["id"].replace("dev.acos.fixtures.counter:", "")
    return nodes


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--png", required=True)
    ap.add_argument("--tree", required=True)
    ap.add_argument("--app", default="counter")
    ap.add_argument("--oracle-png", default=None)
    ap.add_argument("--oracle-xml", default=None)
    ap.add_argument("--oracle-tree", default=None, help="JSON tree instead of the XML")
    ap.add_argument("--dy", type=int, default=0, help="oracle-y offset (0: device coords)")
    ap.add_argument("--out", default=None, help="write the JSON report here")
    args = ap.parse_args()

    ref_dir = ROOT / f"fixtures/reference/{args.app}/art-api34-x86_64"
    oracle_png = Path(args.oracle_png or ref_dir / "screenshot_initial.png")
    oracle_xml = Path(args.oracle_xml or ref_dir / "ui_initial.xml")
    png, tree = Path(args.png), Path(args.tree)
    for p in (png, tree, oracle_png):
        if not p.is_file():
            print(f"missing: {p}", file=sys.stderr)
            return 2

    cmp = load_cmp()
    import numpy as np  # noqa: E402  (same dependency the comparator uses)

    ours = cmp.crop(png, already_content=True)
    ref = cmp.crop(oracle_png, already_content=True)
    if ours.size != ref.size:
        print(f"frame size {ours.size} != oracle {ref.size}", file=sys.stderr)
        return 2
    s = cmp.ssim(ours, ref)
    a = np.asarray(ours.convert("RGB"), dtype=np.int16)
    b = np.asarray(ref.convert("RGB"), dtype=np.int16)
    mae = float(np.abs(a - b).mean())
    exact = float((np.all(a == b, axis=2)).mean())

    # The decor band: everything between the oracle's own status/navigation
    # background frames. Derived from the oracle, not from a constant.
    if oracle_xml.is_file():
        ref_nodes = xml_nodes(cmp, oracle_xml)
    else:
        ref_nodes = normalize(json_nodes(json.loads(Path(args.oracle_tree).read_text())))
    by_id = {}
    for n in ref_nodes:
        by_id.setdefault(n["id"], n)
    top = by_id.get("android:id/statusBarBackground", {}).get("bounds", [0, 0, 0, 0])[3]
    bottom = by_id.get("android:id/navigationBarBackground", {}).get("bounds", [0, 0, 0, 0])[1]
    band = None
    if top or bottom:
        band = (0, top, ours.width, bottom or ours.height)
        oc, rc = ours.crop(band), ref.crop(band)
        band = {
            "rect": list(band),
            "ssim": round(cmp.ssim(oc, rc), 5),
            "mae": round(float(np.abs(
                np.asarray(oc.convert("RGB"), dtype=np.int16)
                - np.asarray(rc.convert("RGB"), dtype=np.int16)).mean()), 3),
        }

    ours_nodes = normalize(cmp.nodes_json_text(tree.read_text()))
    rows = cmp.diff_trees(ours_nodes, ref_nodes, dy=args.dy)
    structural = [
        r for r in rows
        if r["key"] in STRUCTURAL_IDS or r["key"] == "class:android.widget.LinearLayout"
        or r["key"] == "class:android.widget.FrameLayout"
    ]
    structural_ok = bool(structural) and all(r["result"] == "match" for r in structural)
    matched = [r for r in rows if r["result"] == "match"]
    title = next((r for r in rows if r["key"].startswith("text:") and "Counter" in r["key"]), None)
    widths = {}
    for n in ours_nodes:
        if n.get("id", "").endswith("id/inc") or n.get("id", "").endswith("id/reset"):
            widths[n["id"].rsplit("/", 1)[-1]] = n["bounds"][2] - n["bounds"][0]

    out = {
        "app": args.app,
        "ours": list(ours.size),
        "ref": list(ref.size),
        "ssim": round(s, 5),
        "mae": round(mae, 3),
        "pixel_exact": round(exact, 5),
        "decor_band": band,
        "tree_exact": len(matched),
        "tree_total": len(rows),
        "structural_exact": sum(1 for r in structural if r["result"] == "match"),
        "structural_total": len(structural),
        "structural_ok": structural_ok,
        "action_bar_title": title,
        "button_width": widths,
        "nodes": [
            {"key": r["key"], "result": r["result"], "ref": r.get("ref"),
             "ours": r.get("ours"), "delta": r.get("delta")}
            for r in rows
        ],
    }
    text = json.dumps(out, indent=1)
    print(text)
    if args.out:
        Path(args.out).write_text(text + "\n")
    return 0 if (s >= 0.99 and structural_ok) else 1


if __name__ == "__main__":
    raise SystemExit(main())
