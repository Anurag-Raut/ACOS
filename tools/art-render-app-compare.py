#!/usr/bin/env python3
"""Compare one `acos-art render-app` frame + tree against the ART oracle.

Reuses E4H's comparator (`research/art-feasibility/h-view/scripts/compare.py`:
`ssim`, `crop`, `nodes_json_text`, `oracle_nodes`, `diff_trees`) instead of
re-implementing SSIM / bounds matching, so `render-app` numbers are directly
comparable with E4H / P2a / P2c.

    tools/art-render-app-compare.py --png OUT/frame.png --tree OUT/tree.json \
        --app counter

Defaults point at the `counter` oracle and the device content-band origin
(`dy = 275`: the oracle's `mAppBounds` is 0,128-1080,2337 and the ActionBar ends
at 275). Prints one JSON line and exits non-zero when the frame is missing.
"""
import argparse
import importlib.util
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CMP = ROOT / "research/art-feasibility/h-view/scripts/compare.py"


def load_cmp():
    # Do not leave __pycache__ in the research tree we import from.
    sys.dont_write_bytecode = True
    spec = importlib.util.spec_from_file_location("e4h_compare", CMP)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--png", required=True)
    ap.add_argument("--tree", required=True)
    ap.add_argument("--app", default="counter")
    ap.add_argument("--oracle-png", default=None)
    ap.add_argument("--oracle-xml", default=None)
    ap.add_argument("--dy", type=int, default=275)
    ap.add_argument("--out", default=None, help="write the JSON report here")
    args = ap.parse_args()

    ref_dir = ROOT / f"fixtures/reference/{args.app}/art-api34-x86_64"
    oracle_png = Path(args.oracle_png or ref_dir / "screenshot_initial.png")
    oracle_xml = Path(args.oracle_xml or ref_dir / "ui_initial.xml")
    png, tree = Path(args.png), Path(args.tree)
    for p in (png, tree, oracle_png, oracle_xml):
        if not p.is_file():
            print(f"missing: {p}", file=sys.stderr)
            return 2

    cmp = load_cmp()
    import numpy as np  # noqa: E402  (same dependency the comparator uses)

    ours = cmp.crop(png, already_content=True)
    ref = cmp.crop(oracle_png)
    s = cmp.ssim(ours, ref)
    a = np.asarray(ours.convert("RGB"), dtype=np.int16)
    b = np.asarray(ref.convert("RGB"), dtype=np.int16)
    mae = float(np.abs(a - b).mean())
    exact = float((np.all(a == b, axis=2)).mean())

    ours_nodes = cmp.nodes_json_text(tree.read_text())
    rows = cmp.diff_trees(ours_nodes, cmp.oracle_nodes(), dy=args.dy)
    matched = [r for r in rows if r["result"] == "match"]

    # The app's own content nodes: what the oracle reports inside id/content.
    # The app's own content nodes: the root LinearLayout + the three views.
    # `android:id/content` (the activity's FrameLayout) and the decor rows are
    # structural nodes render-app deliberately does not build (P2a §0).
    content = [
        r for r in rows
        if r["key"] == "class:android.widget.LinearLayout"
        or (r["key"].startswith("id:") and r["key"] != "id:id/content")
    ]
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
        "tree_exact": len(matched),
        "tree_total": len(rows),
        "content_exact": sum(1 for r in content if r["result"] == "match"),
        "content_total": len(content),
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
    return 0 if s >= 0.99 else 1


if __name__ == "__main__":
    raise SystemExit(main())
