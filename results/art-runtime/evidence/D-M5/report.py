#!/usr/bin/env python3
"""D-M5: aggregate one or more probe.json files into the per-phase table."""

import json
import sys
from pathlib import Path

PHASES = [
    # (key, label, kind)  -- `top` rows partition the wall clock, `of` rows nest
    ("client_write", "1. client request write (sendall)", "top"),
    ("serve_read", "   socket ready -> request line parsed", "top"),
    ("serve_dispatch", "2. serve dispatch = handle_request", "top"),
    ("art_roundtrip", "3. of which: request to the guest/launcher (acos-art)", "of"),
    ("art_composite", "4. of which: C1 composite/capture", "of2"),
    ("art_encode", "6. of which: PNG encode", "of2"),
    ("art_png_write", "7a. of which: launcher writes the frame file", "of2"),
    ("art_md5", "8a. of which: md5sum subprocess over the frame", "of2"),
    ("art_ipc_remainder", "   of which: pipe + launcher loop remainder", "of2"),
    ("png_read", "5/8b. of which: serve reads the frame file back", "of"),
    ("shot_write", "7b. of which: serve writes the caller's PNG", "of"),
    ("serve_dispatch_json", "   of which: envelope build + JSON", "of"),
    ("serve_reply", "9. reply write", "top"),
    ("client_read", "   client read of the reply", "top"),
]

SUB_C1 = [
    ("c1_kernel", "   kernel_rows + sfgeom compute"),
    ("c1_memfd_read", "   memfd read (guest buffer -> host Vec)"),
    ("c1_blend", "   blend_layer (blit into the canvas)"),
    ("c1_canvas_alloc", "   canvas allocation"),
    ("c1_fnv_hash", "   hash_pixels (FNV-1a + non-zero scan)"),
]

SUB_ENCODE = [
    ("encode_filter", "   filter-0 row copy"),
    ("encode_stored", "   zlib stored-block framing"),
    ("encode_adler32", "   adler32 (scalar)"),
    ("encode_idat_chunk", "   IDAT chunk (copy + CRC32 + write)"),
    ("encode_crc32_idat", "     of which crc32 (bit-serial)"),
]


def stats(vals):
    vals = sorted(v for v in vals if isinstance(v, (int, float)))
    if not vals:
        return None
    n = len(vals)

    def pct(p):
        if n == 1:
            return vals[0]
        k = (n - 1) * p
        lo = int(k)
        hi = min(lo + 1, n - 1)
        return vals[lo] + (vals[hi] - vals[lo]) * (k - lo)

    return {"n": n, "p50": pct(0.50), "p95": pct(0.95),
            "min": vals[0], "max": vals[-1]}


def table(docs):
    rows = []
    for d in docs:
        for r in d["rows"]:
            if r.get("warmup"):
                continue
            rows.append(r)
    ok = [r for r in rows if r["ok"] and r["fresh"]]
    out = {"samples": len(rows), "fresh_ok": len(ok),
           "wall": stats([r["wall_ms"] for r in ok])}
    for key, label, kind in PHASES + [(k, l, "sub") for k, l in SUB_C1 + SUB_ENCODE]:
        vals = [r["phases_us"].get(key) for r in ok]
        st = stats(vals)
        if st:
            out[key] = {"label": label, "kind": kind, "us": st}
    # pad: the remainder of the wall time not attributed to a measured phase
    for r in ok:
        p = r["phases_us"]
        acc = sum(p.get(k) or 0 for k in
                  ("client_write", "serve_read", "serve_dispatch",
                   "serve_reply", "client_read"))
        r["unattributed_us"] = r["wall_ms"] * 1000.0 - acc
        inner = sum(p.get(k) or 0 for k in
                    ("art_roundtrip", "png_read", "shot_write"))
        p["serve_dispatch_json"] = (p["serve_dispatch"] - inner
                                   if p.get("serve_dispatch") is not None else None)
    out["unattributed"] = stats([r["unattributed_us"] for r in ok])
    return out


def render(res, title):
    lines = ["### %s" % title, "",
             "n=%d (fresh %d), one `acos_observe {png:true, screenshot:path}` per sample,"
             % (res["samples"], res["fresh_ok"]),
             "microseconds unless stated, p50 / p95 over the non-warm-up samples.", "",
             "| phase | p50 | p95 | min | max | n |",
             "|---|---:|---:|---:|---:|---:|"]
    w = res["wall"]
    lines.append("| **wall (request write -> reply read), ms** | **%.2f** | **%.2f** | %.2f | %.2f | %d |"
                 % (w["p50"], w["p95"], w["min"], w["max"], w["n"]))
    for key, label, kind in PHASES:
        st = res.get(key, {}).get("us")
        if not st:
            continue
        lines.append("| %s | %d | %d | %d | %d | %d |"
                     % (label, st["p50"], st["p95"], st["min"], st["max"], st["n"]))
    st = res.get("unattributed")
    if st:
        lines.append("| (unattributed: json/parse/probe) | %d | %d | %d | %d | %d |"
                     % (st["p50"], st["p95"], st["min"], st["max"], st["n"]))
    for group, name in (([("c1_total", "C1 total")] + SUB_C1, "C1 composite internals"),
                        (SUB_ENCODE, "PNG encode internals")):
        lines += ["", "**%s**" % name, "",
                  "| sub-phase | p50 | p95 | min | max | n |", "|---|---:|---:|---:|---:|---:|"]
        for key, label in group:
            st = res.get(key, {}).get("us")
            if not st:
                continue
            lines.append("| %s | %d | %d | %d | %d | %d |"
                         % (label, st["p50"], st["p95"], st["min"], st["max"], st["n"]))
    return "\n".join(lines)


def main(argv):
    docs = [json.load(open(p)) for p in argv[1:]]
    res = table(docs)
    print(json.dumps(res, indent=2))
    print()
    print(render(res, "D-M5 phase table"))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
