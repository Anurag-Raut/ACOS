#!/usr/bin/env python3
"""Generate docs/WINDOW_LAYERS.md from the AOSP source cache.

The window layer table is *transcribed*, never invented: this tool reads the
android-14.0.0_r75 sources under `.cache/aosp` and prints every row with the
`file:line` it comes from, so a reviewer can diff the document against the
constant table in `crates/acos-wm/src/layers.rs`.

Usage:
  tools/gen-window-layer-table.py [--aosp-root DIR] [--out FILE] [--tag TAG]
"""
from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def read(path: Path) -> list[str]:
    try:
        return path.read_text(encoding="utf-8", errors="replace").splitlines()
    except OSError as exc:
        sys.exit(f"gen-window-layer-table: cannot read {path}: {exc}")


def type_values(window_manager: Path) -> dict[str, str]:
    """`TYPE_* = FIRST_SYSTEM_WINDOW + n` / `= n`, from WindowManager.java."""
    out: dict[str, str] = {}
    pat = re.compile(r"\b(TYPE_[A-Z0-9_]+)\s*=\s*([^;]+);")
    for line in read(window_manager):
        m = pat.search(line)
        if not m:
            continue
        name, expr = m.group(1), m.group(2)
        expr = re.sub(r"\s+", " ", expr).strip()
        expr = expr.replace("FIRST_APPLICATION_WINDOW", "1")
        expr = expr.replace("LAST_APPLICATION_WINDOW", "99")
        expr = expr.replace("FIRST_SUB_WINDOW", "1000")
        expr = expr.replace("LAST_SUB_WINDOW", "1999")
        expr = expr.replace("FIRST_SYSTEM_WINDOW", "2000")
        expr = expr.replace("LAST_SYSTEM_WINDOW", "2999")
        try:
            value = eval(expr, {"__builtins__": {}}, {})  # noqa: S307 - literals only
        except Exception:
            continue
        out[name] = str(value)
    return out


def policy_rows(policy: Path) -> tuple[list[tuple[int, list[str], str]], int, int]:
    """Rows of `getWindowLayerFromTypeLw`: (line, [case names], return expr)."""
    lines = read(policy)
    start = next(
        i
        for i, line in enumerate(lines)
        if "default int getWindowLayerFromTypeLw(int type, boolean canAddInternalSystemWindow," in line
    )
    end = next(
        i for i, line in enumerate(lines[start:], start) if "default int getMaxWindowLayer()" in line
    )
    rows: list[tuple[int, list[str], str]] = []
    cases: list[str] = []
    for i in range(start, end):
        line = lines[i]
        stripped = line.strip()
        m = re.match(r"case (TYPE_[A-Z0-9_]+):", stripped)
        if m:
            cases.append(m.group(1))
            continue
        if stripped.startswith("return ") and stripped.endswith(";"):
            expr = stripped[len("return "):-1].strip()
            rows.append((i + 1, cases, expr))
            cases = []
    application = next(
        (i + 1 for i in range(start, end) if "return APPLICATION_LAYER;" in lines[i]), None
    )
    return rows, start + 1, application or 0


def sublayer_rows(policy: Path) -> tuple[list[tuple[int, list[str], str]], int]:
    lines = read(policy)
    start = next(
        i
        for i, line in enumerate(lines)
        if "default int getSubWindowLayerFromTypeLw(int type)" in line
    )
    end = next(
        i for i, line in enumerate(lines[start:], start) if line.strip() == "}" and i > start + 10
    )
    rows: list[tuple[int, list[str], str]] = []
    cases: list[str] = []
    for i in range(start, end):
        stripped = lines[i].strip()
        m = re.match(r"case (TYPE_[A-Z0-9_]+):", stripped)
        if m:
            cases.append(m.group(1))
            continue
        if stripped.startswith("return ") and stripped.endswith(";"):
            rows.append((i + 1, cases, stripped[len("return "):-1].strip()))
            cases = []
    return rows, start + 1


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--aosp-root", default="/home/Qui/Projects/acos/.cache/aosp")
    ap.add_argument("--out", default=str(ROOT / "docs/WINDOW_LAYERS.md"))
    ap.add_argument("--tag", default="android-14.0.0_r75")
    args = ap.parse_args()
    root = Path(args.aosp_root)
    policy = root / "base/services/core/java/com/android/server/policy/WindowManagerPolicy.java"
    wm = root / "base/core/java/android/view/WindowManager.java"
    constants = root / "base/core/java/android/view/WindowManagerPolicyConstants.java"
    if not policy.is_file() or not wm.is_file():
        sys.exit(
            "gen-window-layer-table: source cache missing; run tools/fetch-aosp-reference.sh "
            f"(looked for {policy} and {wm})"
        )
    values = type_values(wm)
    rows, fn_line, application_line = policy_rows(policy)
    subs, sub_fn_line = sublayer_rows(policy)
    const_lines = read(constants) if constants.is_file() else []
    const_rows = []
    for i, line in enumerate(const_lines, 1):
        m = re.match(r"\s*int (APPLICATION_[A-Z_]*LAYER)\s*=\s*(-?\d+);", line)
        if m:
            const_rows.append((i, m.group(1), m.group(2)))

    policy_rel = policy.relative_to(root)
    wm_rel = wm.relative_to(root)
    const_rel = constants.relative_to(root) if constants.is_file() else Path("(missing)")
    out: list[str] = []
    add = out.append
    add("# AOSP window layers (generated)")
    add("")
    add(f"Generated by `tools/gen-window-layer-table.py` from the `{args.tag}` source cache.")
    add("Do not edit by hand; re-run the generator and diff.")
    add("")
    add("`crates/acos-wm/src/layers.rs` transcribes this table into `policy_layer()` and")
    add("`sub_window_layer()`; the test `policy_layers_match_the_aosp_table` pins every row.")
    add("")
    add(f"## `getWindowLayerFromTypeLw` — `{policy_rel}:{fn_line}`")
    add("")
    add("| layer | types (`TYPE_*`, value from `{}`) | source |".format(wm_rel))
    add("|---|---|---|")
    for line, cases, expr in rows:
        names = ", ".join(f"`{c}` ({values.get(c, '?')})" for c in cases) or "_(default)_"
        add(f"| `{expr}` | {names} | `{policy_rel}:{line}` |")
    add("")
    add(f"## `getSubWindowLayerFromTypeLw` — `{policy_rel}:{sub_fn_line}`")
    add("")
    add("| offset (relative to the attached window) | types | source |")
    add("|---|---|---|")
    for line, cases, expr in subs:
        names = ", ".join(f"`{c}`" for c in cases) or "_(default)_"
        add(f"| `{expr}` | {names} | `{policy_rel}:{line}` |")
    add("")
    add(f"## `WindowManagerPolicyConstants` — `{const_rel}`")
    add("")
    add("| constant | value | source |")
    add("|---|---|---|")
    for line, name, value in const_rows:
        add(f"| `{name}` | `{value}` | `{const_rel}:{line}` |")
    add("")
    add("`getMaxWindowLayer()` returns `36` (`{}:{}`).".format(policy_rel, next(
        i + 1 for i, l in enumerate(read(policy)) if "default int getMaxWindowLayer()" in l
    )))
    add("")
    Path(args.out).write_text("\n".join(out), encoding="utf-8")
    print(f"gen-window-layer-table: wrote {args.out} ({len(rows)} policy rows, {len(subs)} sub-window rows)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
