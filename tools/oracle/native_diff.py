#!/usr/bin/env python3
"""Compare the natives ACOS invokes against the measured real-ART traces.

Input: the `natives.tsv` emitted by `acos run-dex --corelib real
--trace-natives-file` and one of the `research/dynamic-trace/data/natives_*.tsv`
files. Both use the columns

    enters  exits  first_order  call_site  class  method  signature

ACOS records class *descriptors* (`Ljava/lang/String;`); the ART traces use
dotted names (`java.lang.String`). `normalise_class` bridges the two.

Reported:
  * both      - native called by ACOS and ART
  * acos_only - ACOS calls it, ART does not (usually a lower layer ACOS
                short-circuits, or a VM-internal native)
  * art_only  - ART calls it, ACOS does not (a fidelity gap, or a native ACOS
                replaced with an override)
  * count     - invocation-count drift for natives in `both`

With `--strict`, a non-empty `art_only` set exits nonzero.
"""

import argparse
import csv
import sys


def normalise_class(name: str) -> str:
    name = name.strip()
    if name.startswith("L") and name.endswith(";"):
        name = name[1:-1].replace("/", ".")
    while name.startswith("["):
        name = name[1:]
        if name.startswith("L") and name.endswith(";"):
            name = name[1:-1].replace("/", ".")
    return name


def load(path: str):
    rows = {}
    with open(path, newline="") as f:
        reader = csv.DictReader(f, delimiter="\t")
        for row in reader:
            if not row.get("class") or not row.get("method"):
                continue
            key = (
                normalise_class(row["class"]),
                row["method"].strip(),
                row.get("signature", "").strip(),
            )
            enters = int(row.get("enters") or 0)
            order = int(row.get("first_order") or 0)
            if key in rows:
                rows[key] = (rows[key][0] + enters, min(rows[key][1], order))
            else:
                rows[key] = (enters, order)
    return rows


def fmt(key) -> str:
    cls, method, sig = key
    return f"{cls}->{method}{sig}"


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--acos", required=True)
    ap.add_argument("--art", required=True)
    ap.add_argument("--label", default="")
    ap.add_argument("--strict", action="store_true")
    ap.add_argument("--limit", type=int, default=40)
    args = ap.parse_args()

    acos = load(args.acos)
    art = load(args.art)

    both = sorted(set(acos) & set(art))
    acos_only = sorted(set(acos) - set(art))
    art_only = sorted(set(art) - set(acos))

    print(f"== native-diff {args.label} ==")
    print(f"ACOS natives: {len(acos)}   ART natives: {len(art)}   "
          f"both: {len(both)}   acos-only: {len(acos_only)}   art-only: {len(art_only)}")

    if art_only:
        print(f"\nART natives ACOS never invoked ({len(art_only)}):")
        for k in art_only[: args.limit]:
            print(f"  - {fmt(k)}  art_enters={art[k][0]}")
    else:
        print("\nART natives ACOS never invoked: none")

    if acos_only:
        print(f"\nACOS natives ART never invoked ({len(acos_only)}):")
        for k in acos_only[: args.limit]:
            print(f"  + {fmt(k)}  acos_enters={acos[k][0]}")

    drifted = [
        (k, acos[k][0], art[k][0])
        for k in both
        if acos[k][0] != art[k][0]
    ]
    if drifted:
        print(f"\nInvocation-count drift ({len(drifted)}):")
        for k, a, b in drifted[: args.limit]:
            print(f"  ~ {fmt(k)}  acos={a} art={b}")

    if args.strict and art_only:
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
