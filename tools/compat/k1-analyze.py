#!/usr/bin/env python3
"""Aggregate the K1 per-app reports into a stage funnel + ranked failure walls.

Reads `$ACOS_K1_CORPUS/manifest.json` and every
`$ACOS_K1_RUN/<package>/report.json` written by `k1-run.py`, then prints (and
writes) the corpus-level result.

Usage:
  tools/compat/k1-analyze.py                       # summary + JSON
  tools/compat/k1-analyze.py --markdown            # also print the per-app table
  tools/compat/k1-analyze.py --json out.json
"""

import argparse
import glob
import json
import os
import re
import sys
from collections import Counter, defaultdict

STAGES = ["inspect", "process_start", "application_on_create", "first_frame", "tree", "screenshot", "tap"]

# First-failure attribution. Keyed by an `error.code` (or a substring of the
# code/message). `component` names the ACOS side that owns the wall; `fix` is
# the smallest plausible change with an AOSP reference where one exists. These
# are the *hypotheses* the corpus supports, not verdicts: `K1.md` states which
# ones were confirmed by reading the log/marker.
WALLS = [
    (r"UNSUPPORTED_ABI|WRONG_ABI", "native linker / ABI gate",
     "crates/acos-art/src/launch.rs (native-libs stage)"),
    (r"UNSUPPORTED_JAVA_API|UNSUPPORTED_ANDROID_API|UNSUPPORTED_FRAMEWORK",
     "framework shim missing an API",
     "crates/acos-services (framework peer)"),
    (r"UNSUPPORTED_NATIVE_SYMBOL|UNSUPPORTED_JNI|JNI", "JNI/native symbol surface",
     "crates/acos-jni + crates/acos-native-loader"),
    (r"SEGFAULT|SIGSEGV|SIGABRT|SIGBUS|SIGILL", "guest crash (native)", "native loader / JNI dispatch"),
    (r"SECCOMP|SANDBOX|LANDLOCK", "sandbox/seccomp policy", "crates/acos-seccomp-guard, crates/acos-sandbox"),
    (r"NO_LIVE_FRAME|NO_FRESH_FRAME|RENDER|GLES|EGL", "GL/rendering / frame production",
     "crates/acos-render, crates/acos-gles"),
    (r"WEBVIEW|android.webkit", "WebView provider", "crates/acos-services (webview peer)"),
    (r"SERVICE|BINDER|IContentProvider|ContentProvider", "missing system service",
     "crates/acos-services"),
    (r"SQLITE|ROOM|SQLite", "SQLite/framework database", "crates/acos-sqlite"),
    (r"FONT|TEXT|TYPEFACE", "fonts/typography", "ACOS_FONT_DIR + crates/acos-render"),
    (r"TIMEOUT|APP_NOT_RESPONDING|ANR", "wall-clock / main-thread stall", "crates/acos-art/watchdog.rs"),
    (r"HOST_MEMORY_LIMIT|RESOURCE_LIMIT|RAM_GUARD", "host memory ceiling", "harness budget + crates/acos-art/cgroup.rs"),
    (r"GUEST_EXITED|GUEST_EXIT|NONZERO_EXIT|NO_ACTIVITY_RESUMED|CRASH", "guest process died",
     "crates/acos-cli/src/failure.rs + the guest log"),
    (r"REQUIRES_GMS|PLAY_SERVICES|GoogleApi", "app needs Play Services (not an engine gap)",
     "out of scope: no GMS target"),
    (r"UNSUPPORTED_RN|react", "React Native driver", "crates/acos-runtime/src/rn_session.rs"),
    (r"FLUTTER", "Flutter engine (not modelled)", "out of scope"),
    (r"APK_|AXML|ARSC|SIGNING", "static APK parsing", "crates/acos-apk"),
    (r"HARNESS_TIMEOUT", "harness budget hit (first-frame 20 s)", "tools/compat/k1-run.py"),
]


def norm_msg(msg):
    if not msg:
        return ""
    m = re.sub(r"0x[0-9a-fA-F]+", "0x…", msg)
    m = re.sub(r"\d+", "N", m)
    return m.strip()[:180]


def classify(code, message):
    blob = f"{code or ''} {message or ''}"
    for pattern, component, fix in WALLS:
        if re.search(pattern, blob, re.I):
            return component, fix
    return "unclassified", "read the envelope"


def load(run_root, manifest):
    apps = {a["package"]: a for a in manifest.get("apps", [])}
    reports = {}
    for pkg in sorted(apps):
        p = os.path.join(run_root, pkg, "report.json")
        if os.path.isfile(p):
            with open(p) as f:
                reports[pkg] = json.load(f)
    return apps, reports


def first_error(rep):
    errs = [e for e in (rep.get("errors") or []) if e.get("code")]
    if errs:
        return errs[0]
    for step in rep.get("steps") or []:
        e = step.get("error")
        if e and e.get("code"):
            return {"step": step.get("step"), **e}
    return None


def stage_of(rep, first_failure_step=None):
    st = rep.get("stages") or {}
    reached = None
    for s in STAGES:
        v = st.get(s) or {}
        if v.get("ok"):
            reached = s
        else:
            break
    if reached in (None, "inspect") and first_failure_step == "launch":
        # Launch failed before the first frame; the marker rungs say where.
        return reached or "inspect"
    return reached or "inspect"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--corpus", default=os.environ.get("ACOS_K1_CORPUS",
                                                       "/home/Qui/Projects/acos/.cache/corpus/k1"))
    ap.add_argument("--run", default=os.environ.get("ACOS_K1_RUN",
                                                    "/home/Qui/Projects/acos/.cache/k1/runs"))
    ap.add_argument("--json", default="results/compat/k1-results.json")
    ap.add_argument("--markdown", action="store_true")
    args = ap.parse_args()

    with open(os.path.join(args.corpus, "manifest.json")) as f:
        manifest = json.load(f)
    apps, reports = load(args.run, manifest)

    rows = []
    for pkg, meta in apps.items():
        rep = reports.get(pkg)
        if not rep:
            rows.append({"package": pkg, "status": "not-run", **{
                k: meta[k] for k in ("version", "ui_toolkit", "target_sdk", "has_native", "abis")
            }})
            continue
        fe = first_error(rep)
        stage = stage_of(rep, (fe or {}).get("step"))
        comp, fix = classify(fe.get("code"), fe.get("message")) if fe else (None, None)
        rows.append({
            "package": pkg,
            "status": "ok" if rep.get("ok") else ("fail" if rep.get("stages", {}).get("first_frame", {}).get("ok") else "blocked"),
            "version": meta["version"],
            "ui_toolkit": meta["ui_toolkit"],
            "target_sdk": meta["target_sdk"],
            "has_native": meta["has_native"],
            "abis": meta["abis"],
            "stage": stage,
            "stage_reached": ((rep.get("stages") or {}).get("launch") or {}).get("reached_stage"),
            "time_to_first_frame_ms": rep.get("time_to_first_frame_ms"),
            "tree_nodes": ((rep.get("stages") or {}).get("tree") or {}).get("node_count"),
            "tap": rep.get("tap"),
            "screenshot_bytes": (rep.get("screenshot") or {}).get("bytes"),
            "peak_rss_mib": rep.get("peak_rss_mib"),
            "crash_class": rep.get("crash_class"),
            "error_code": (fe or {}).get("code"),
            "error_message": norm_msg((fe or {}).get("message")),
            "error_stage": (fe or {}).get("stage"),
            "wall_component": comp,
            "wall_fix": fix,
            "engine_stage": ((rep.get("stages") or {}).get("engine_stage")),
            "markers_present": rep.get("markers_present"),
            "stages": {s: bool((rep.get("stages") or {}).get(s, {}).get("ok")) for s in STAGES},
        })

    ran = [r for r in rows if r["status"] != "not-run"]
    funnel = {s: sum(1 for r in ran if r["stages"].get(s)) for s in STAGES}
    # Funnel by *reached* stage, so "Application.onCreate" counts apps that got
    # past it regardless of a later rung.
    ordered = [r for r in ran]

    def reached_at_least(r, stage):
        idx = STAGES.index(stage)
        return all(r["stages"].get(s) for s in STAGES[: idx + 1])

    ladder = {s: sum(1 for r in ordered if reached_at_least(r, s)) for s in STAGES}

    walls = defaultdict(lambda: {"apps": [], "codes": Counter(), "components": Counter()})
    for r in ran:
        if r["stage"] == "tap" and r["status"] == "ok":
            continue
        key = r["error_code"] or "unknown"
        msg = r["error_message"] or ""
        if r["error_code"] in ("UNSUPPORTED_JAVA_API", "UNSUPPORTED_ANDROID_API") and msg:
            key = f"{r['error_code']}: {msg}"
        w = walls[key]
        w["apps"].append(r["package"])
        w["codes"][r["error_code"]] += 1
        w["components"][r["wall_component"]] += 1
        w["message"] = msg
        w["component"] = r["wall_component"]
        w["fix"] = r["wall_fix"]

    ttff = sorted(r["time_to_first_frame_ms"] for r in ran if r["time_to_first_frame_ms"])
    peaks = sorted(r["peak_rss_mib"] for r in ran if r["peak_rss_mib"])

    def pct(lst, q):
        if not lst:
            return None
        return lst[min(len(lst) - 1, int(round(q * (len(lst) - 1))))]

    out = {
        "corpus": {
            "manifest_apps": len(apps),
            "ran": len(ran),
            "not_run": len(ran) and len(apps) - len(ran),
            "fetch_failures": manifest.get("failures"),
            "index_sha256": manifest.get("index_sha256"),
            "framework_api": manifest.get("framework_api"),
        },
        "funnel": funnel,
        "funnel_at_least": ladder,
        "rates": {s: round(ladder[s] / len(ran), 3) if ran else None for s in STAGES},
        "time_to_first_frame_ms": {
            "n": len(ttff),
            "p50": pct(ttff, 0.5),
            "p90": pct(ttff, 0.9),
            "max": ttff[-1] if ttff else None,
        },
        "peak_rss_mib": {"p50": pct(peaks, 0.5), "max": peaks[-1] if peaks else None},
        "walls": {k: {
            "apps": v["apps"],
            "count": len(v["apps"]),
            "message": v["message"],
            "component": v["component"],
            "fix": v["fix"],
        } for k, v in sorted(walls.items(), key=lambda kv: -len(kv[1]["apps"]))},
        "apps": rows,
    }

    os.makedirs(os.path.dirname(args.json) or ".", exist_ok=True)
    with open(args.json, "w") as f:
        json.dump(out, f, indent=1)
        f.write("\n")

    print(f"corpus: {len(ran)}/{len(apps)} apps ran")
    print("stage ladder (apps reaching at least this rung):")
    for s in STAGES:
        print(f"  {s:24s} {ladder[s]:3d}/{len(ran)}  {100.0*ladder[s]/max(1,len(ran)):5.1f}%")
    print(f"time to first frame: p50={out['time_to_first_frame_ms']['p50']}ms "
          f"p90={out['time_to_first_frame_ms']['p90']}ms max={out['time_to_first_frame_ms']['max']}ms")
    print(f"peak RSS: p50={out['peak_rss_mib']['p50']}MiB max={out['peak_rss_mib']['max']}MiB")
    print(f"\nwalls ({len(walls)}):")
    for k, v in out["walls"].items():
        print(f"  {v['count']:3d}  {k[:64]:64s}  {v['component']}")
        print(f"       apps: {', '.join(a.split('.')[-1] for a in v['apps'])[:140]}")

    if args.markdown:
        print("\n| app | toolkit | tgt | stage | first frame ms | tree | shot | tap | first error | code |")
        print("|---|---|---|---|---|---|---|---|---|---|")
        for r in sorted(rows, key=lambda r: (r["status"] == "ok", r["package"])):
            if r["status"] == "not-run":
                continue
            print("| {pkg} | {tk} | {tgt} | {stage} | {ff} | {tree} | {shot} | {tap} | {code} |".format(
                pkg=r["package"], tk=r["ui_toolkit"], tgt=r["target_sdk"],
                stage=r["stage"],
                ff="" if r["time_to_first_frame_ms"] is None else round(r["time_to_first_frame_ms"]),
                tree="yes" if r["stages"]["tree"] else "no",
                shot=("yes" if r["stages"]["screenshot"] else "no"),
                tap=("yes" if r["stages"]["tap"] else "no"),
                code=(r["error_code"] or ""),
            ))
    return 0


if __name__ == "__main__":
    sys.exit(main())
