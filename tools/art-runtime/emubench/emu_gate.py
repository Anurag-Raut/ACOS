#!/usr/bin/env python3
"""emu_gate.py -- the stock-emulator baseline gate (owner direction 2026-11-18).

ACOS must be better than the normal Android emulator on EVERY tracked metric.
This script pairs an ACOS benchmark document (the release gate's own
`v0.3.0-gate/timing/02-ab/v0.3.0.json`-style file, or a future gate run) with an
EMUBENCH emulator document and prints the side-by-side table plus a verdict.

Exit status: 0 when every comparable metric is better in ACOS, 1 when at least
one metric is worse (strictly, or beyond the 5 % noise floor with --noise), 2 on
a usage error.  A metric that only one side measured is reported as
NOT_MEASURED -- never as a pass.  A row listed in REFERENCE_ONLY is printed for
the record and never gated (today: the frame-change-only warm launch, whose
boundary is not the one ACOS publishes).

  emu_gate.py --acos results/release/v0.3.0-gate/timing/02-ab/v030.json \
              --emu results/bench/emubench-2026-11-18.json \
              [--noise 5] [--md FILE] [--json FILE]
"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

# (id, label, unit, direction, acos key, emulator key, comparability note)
METRICS = [
    ("cold_first_frame", "Cold boot -> first app frame", "ms", "lower",
     "m3_cold_start_ms", "cold_first_frame_ms",
     "ACOS: process start -> first correct frame. Emulator: process start -> "
     "sys.boot_completed -> first frame whose app region differs from the idle "
     "launcher (tree-confirmed count 0)."),
    ("warm_launch", "Warm launch -> readable settled UI tree", "ms", "lower",
     "m4_launch_strict_ms", "warm_launch_tree_ms",
     "The SAME observation boundary on both sides (MERGE040D(b), BENCHMARKS §13.3): "
     "`am start` -> a UI tree that can be read, reports the expected content and is "
     "identical to the previous read. ACOS: strict settle (its own content-stability "
     "rule + one tree read). Emulator: launcher-frame change, then `uiautomator dump` "
     "until two consecutive dumps agree with count 0. The old frame-change-only row "
     "(`warm_launch_ms`) is still in the baseline for the record but is NOT gated: it "
     "pays no tree read the ACOS number pays."),
    ("tap_counter", "Tap -> settled UI result (counter)", "ms", "lower",
     "m8_tap_counter_strict_ms", "tap_counter_ms",
     "ACOS strict settle vs `adb input tap` + `uiautomator dump` polling."),
    ("tap_form", "Tap -> settled UI result (form)", "ms", "lower",
     "m8_tap_form_strict_ms", "tap_form_ms",
     "Same route; the form row waits for the 'Name is required' error text."),
    ("ui_tree", "UI tree read with text", "ms", "lower",
     "m7_tree_strict_ms", "ui_tree_ms",
     "ACOS strict tree read vs one `uiautomator dump` (fresh app_process per call)."),
    ("screenshot", "Fresh PNG screenshot", "ms", "lower",
     "m5_screenshot_ms", "screenshot_ms",
     "ACOS PNG shot vs `adb exec-out screencap -p`."),
    ("reset", "Reset -> first correct screen", "ms", "lower",
     "m6_reset_strict_ms", "reset_ms",
     "ACOS app reset vs `am force-stop` + `pm clear` + launch to the launcher-frame change."),
    ("memory_pss", "Instance memory, PSS", "MiB", "lower",
     "m1_idle_pss_mib", "idle_pss_mib",
     "ACOS core PSS vs the qemu process PSS (the whole emulator; one instance)."),
    ("memory_anon_shmem", "Instance memory, anon+shmem", "MiB", "lower",
     "m1_idle_mem_mib", "idle_anon_shmem_mib",
     "ACOS core cgroup anon+shmem vs RssAnon+RssShmem of the qemu process."),
    ("cpu_idle", "Idle CPU", "% of one core", "lower",
     "cpu_idle_n1_pct", "cpu_idle_pct",
     "ACOS N=1 idle window vs the qemu process utime+stime over the same kind of window."),
    ("throughput", "Verified tasks / GiB-minute", "tasks/GiB-min", "higher",
     "m11_tasks_per_gib_min_n10", "n4_tasks_per_gib_min",
     "ACOS north star at N=10 (BENCHMARKS.md section 12, W52) vs the emulator at "
     "N=4 (its maximum under the owner's limit; the per-GiB rate is flat in N, so "
     "N=1/2/4 are 2.5-2.6 - the N=2 and N=1-equivalent rows are in the baseline)."),
]

# Rows that are recorded but deliberately not gated, with the reason the gate
# prints next to them (a number is never silently dropped: it becomes a row).
REFERENCE_ONLY = {
    "warm_launch_frame": (
        "Warm launch -> launcher-frame change (frame only, not gated)", "ms",
        "m4_launch_strict_ms", "warm_launch_ms",
        "The frame-change-only row. ACOS has no frame-change-only number to pair it "
        "with (its publishable number is the strict settle), so pairing it would "
        "compare two different boundaries — the defect MERGE040D(b) removed. "
        "Reported for the record, never a pass/fail."),
}

# ACOS M11 is published in BENCHMARKS.md section 12, not in the bench JSON.
ACOS_FALLBACK = {"m11_tasks_per_gib_min_n10": 62.84, "m11_tasks_per_gib_min_n1": 67.81}


def load_acos(path):
    d = json.loads(Path(path).read_text())
    m = d.get("metrics") or {}
    out = {}
    for k, v in m.items():
        if isinstance(v, dict) and v.get("p50") is not None:
            out[k] = v["p50"]
    out.update({k: v for k, v in ACOS_FALLBACK.items() if k not in out})
    out["_commit"] = d.get("commit")
    out["_branch"] = d.get("branch")
    out["_utc"] = d.get("utc")
    return out


def load_emu(path):
    d = json.loads(Path(path).read_text())
    rows = d.get("rows") or {}
    out = {}
    for k, v in rows.items():
        if isinstance(v, dict) and v.get("p50") is not None:
            out[k] = v["p50"]
    out["_generated"] = d.get("generated_utc")
    out["_phases"] = sorted((d.get("phases") or {}).keys())
    return out


def fmt(v, unit):
    if v is None:
        return "--"
    if unit == "ms" and v >= 1000:
        return "%.0f" % v
    if unit in ("MiB",):
        return "%.1f" % v
    if unit == "tasks/GiB-min":
        return "%.2f" % v
    return "%.1f" % v


def main(argv=None):
    ap = argparse.ArgumentParser()
    ap.add_argument("--acos", required=True)
    ap.add_argument("--emu", required=True)
    ap.add_argument("--noise", type=float, default=5.0,
                    help="noise floor %% for the 'worse beyond noise' verdict (default 5)")
    ap.add_argument("--md", default=None)
    ap.add_argument("--json", default=None)
    a = ap.parse_args(argv)
    acos, emu = load_acos(a.acos), load_emu(a.emu)
    lines = ["| Metric | ACOS | Emulator | Unit | Better | Delta | Verdict |",
             "|---|---:|---:|---|---|---:|---|"]
    rows, worse, worse_noise, not_measured = [], [], [], []
    for mid, label, unit, direction, akey, ekey, note in METRICS:
        av, ev = acos.get(akey), emu.get(ekey)
        rec = {"id": mid, "label": label, "unit": unit, "direction": direction,
               "acos_key": akey, "emu_key": ekey, "acos": av, "emulator": ev,
               "note": note}
        if av is None or ev is None:
            rec["verdict"] = "NOT_MEASURED"
            not_measured.append(mid)
            lines.append("| %s | %s | %s | %s | -- | -- | **NOT_MEASURED** |"
                         % (label, fmt(av, unit), fmt(ev, unit), unit))
            rows.append(rec)
            continue
        if direction == "lower":
            better = av < ev
            delta_pct = (av - ev) / ev * 100.0
        else:
            better = av > ev
            delta_pct = (ev - av) / av * 100.0 if av else 0.0
        rec["delta_pct"] = delta_pct
        rec["better"] = "ACOS" if better else "emulator"
        rec["worse_beyond_noise"] = (not better) and abs(delta_pct) > a.noise
        verdict = "better" if better else ("WORSE" if abs(delta_pct) > a.noise else "worse")
        if not better:
            worse.append(mid)
            if rec["worse_beyond_noise"]:
                worse_noise.append(mid)
        lines.append("| %s | %s | %s | %s | %s | %+.1f %% | %s |"
                     % (label, fmt(av, unit), fmt(ev, unit), unit,
                        rec["better"], delta_pct, verdict.upper() if verdict == "WORSE" else verdict))
        rows.append(rec)
    for rid, (label, unit, akey, ekey, note) in REFERENCE_ONLY.items():
        av, ev = acos.get(akey), emu.get(ekey)
        rec = {"id": rid, "label": label, "unit": unit,
               "acos_key": akey, "emu_key": ekey, "acos": av, "emulator": ev,
               "note": note, "reference_only": True,
               "verdict": "REFERENCE" if (av is not None and ev is not None) else "NOT_MEASURED"}
        if rec["verdict"] == "REFERENCE":
            rec["delta_pct"] = (av - ev) / ev * 100.0 if ev else 0.0
        rows.append(rec)
        lines.append("| %s | %s | %s | %s | -- | -- | %s (not gated) |"
                     % (label, fmt(av, unit), fmt(ev, unit), unit, rec["verdict"]))
    lines.append("")
    lines.append("ACOS side: `%s` (%s, commit %s, %s). Emulator side: `%s` (%s, phases %s)."
                 % (a.acos, acos.get("_branch"), (acos.get("_commit") or "?")[:8],
                    acos.get("_utc"), a.emu, emu.get("_generated"),
                    ", ".join(emu.get("_phases") or [])))
    md = "\n".join(lines)
    print(md)
    print()
    print("gate: %d metric(s) compared, %d worse for ACOS, %d worse beyond the %.0f %% noise floor"
          % (len(METRICS) - len(not_measured), len(worse), len(worse_noise), a.noise))
    if worse:
        print("worse: %s" % ", ".join(worse))
    if not_measured:
        print("not measured: %s" % ", ".join(not_measured))
    doc = {"schema": "emubench-gate/1", "acos": a.acos, "emulator": a.emu,
           "noise_floor_pct": a.noise, "metrics": rows,
           "worse": worse, "worse_beyond_noise": worse_noise,
           "not_measured": not_measured,
           "pass": not worse}
    if a.md:
        Path(a.md).write_text(md + "\n")
        print("markdown -> %s" % a.md)
    if a.json:
        Path(a.json).write_text(json.dumps(doc, indent=1) + "\n")
        print("json -> %s" % a.json)
    return 0 if not worse else 1


if __name__ == "__main__":
    sys.exit(main())
