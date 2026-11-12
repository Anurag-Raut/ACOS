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

# Honest rungs. "Application.onCreate" is split into entered/completed because
# a real-world app can be instantiated and then throw inside onCreate: the
# guest's marker is written when the framework hands the Application object to
# the app's code, not when onCreate returns.
STAGES = [
    "inspect",
    "process_start",
    "oncreate_entered",
    "oncreate_completed",
    "first_frame",
    "tree",
    "screenshot",
    "tap",
]

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
    return m.strip()[:400]


def classify(code, message):
    blob = f"{code or ''} {message or ''}"
    for pattern, component, fix in WALLS:
        if re.search(pattern, blob, re.I):
            return component, fix
    return "unclassified", "read the envelope"


def read_session_exit(run_root, pkg):
    """The ART guest's own exit record (written by acos-art, not by the harness).

    `acos serve` names the session itself, so the artefacts directory is found
    by globbing `art-sessions/*/art-frames`.
    """
    import glob as _glob

    for d in sorted(_glob.glob(os.path.join(run_root, pkg, "art-sessions", "*", "art-frames"))):
        p = os.path.join(d, "session-exit.json")
        if os.path.isfile(p):
            try:
                with open(p) as f:
                    rec = json.load(f)
            except (OSError, json.JSONDecodeError):
                continue
            up = os.path.join(d, "unsupported.jsonl")
            rec["unsupported"] = []
            if os.path.isfile(up):
                with open(up, errors="replace") as f:
                    for line in f:
                        line = line.strip()
                        if line:
                            try:
                                rec["unsupported"].append(json.loads(line))
                            except json.JSONDecodeError:
                                pass
            return rec
    return None


UNSUPPORTED_RE = re.compile(
    r"unsupported Android API: ([\w.$]+) code (\d+) \(([\w$<>]+)\)")
CRASH_MSG_RE = re.compile(r'"message":"((?:[^"\\]|\\.)*)"')


def crash_summary(rec):
    """Pull the app-level exception out of the guest's ACOSLAUNCH crash line."""
    if not rec:
        return None, None, None
    raw = rec.get("guest_crash") or ""
    m = UNSUPPORTED_RE.search(raw)
    api = f"{m.group(1)}.{m.group(3)} (code {m.group(2)})" if m else None
    cm = CRASH_MSG_RE.search(raw)
    msg = cm.group(1) if cm else (raw[:300] or None)
    return api, msg, raw[:600] or None


NULL_CALL_RE = re.compile(
    r"Attempt to invoke (?:virtual|interface) method '([^']+)")


def wall_key(api, msg, code):
    """Collapse app-specific crash text to the framework call that failed."""
    if api:
        return api
    if not msg:
        return code or "unknown"
    m = re.search(r"unsupported Android API: ([\w.$]+) code (\d+) \(([\w$<>]+)\)", msg)
    if m:
        return f"{m.group(1)}.{m.group(3)} (code {m.group(2)})"
    # A `bad request for <iface>::<method>` is the peer rejecting the parcel.
    m = re.search(r"bad request for ([\w.$]+)::([\w$]+)", msg)
    if m:
        return f"{m.group(1)}.{m.group(2)} (parcel rejected)"
    # A null where the platform never returns null: name the missing return.
    m = NULL_CALL_RE.search(msg)
    if m:
        sig = m.group(1)
        mm = re.search(r"([\w.$]+)\.([\w$]+)\([^)]*\)$", sig)
        if mm:
            return f"null return: {mm.group(1)}.{mm.group(2)}"
        return f"null return: {sig}"
    if "For security reasons, WebView is not allowed" in msg:
        return "WebView rejected: process reported as privileged"
    if "No implementation found for" in msg:
        m = re.search(r"No implementation found for (?:[\w.$\[\]]+ )?([\w.$]+)\.([\w$]+)\(", msg)
        if m:
            return f"JNI native missing: {m.group(1)}.{m.group(2)}"
        return "JNI native missing"
    if "Could not create instance for" in msg or "InstanceCreationException" in msg:
        return "DI graph failed during onCreate (root cause hidden)"
    if "CertificateException" in msg or "conscrypt" in msg:
        return "conscrypt X.509 certificate parsing"
    if "AndroidKeyStore not found" in msg or "KeyStoreException" in msg:
        return "AndroidKeyStore provider (no keystore backend)"
    if "dsn is required" in msg.lower():
        # K3c: the DSN is a `<meta-data>` value; the provider runs (so the
        # framework path is right) and then reads metadata PMS never sent.
        return "app <meta-data> not delivered (PMS Bundle)"
    if "java.lang.ExceptionInInitializerError" in msg:
        return "static initializer failed (cause hidden)"
    return code or msg[:120]


def load(run_root, manifest):
    apps = {a["package"]: a for a in manifest.get("apps", [])}
    reports, exits = {}, {}
    for pkg in sorted(apps):
        p = os.path.join(run_root, pkg, "report.json")
        if os.path.isfile(p):
            with open(p) as f:
                reports[pkg] = json.load(f)
            exits[pkg] = read_session_exit(run_root, pkg)
    return apps, reports, exits


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
    apps, reports, exits = load(args.run, manifest)

    rows = []
    for pkg, meta in apps.items():
        rep = reports.get(pkg)
        if not rep:
            rows.append({"package": pkg, "status": "not-run", **{
                k: meta[k] for k in ("version", "ui_toolkit", "target_sdk", "has_native", "abis")
            }})
            continue
        fe = first_error(rep)
        rec = exits.get(pkg)
        api, crash_msg, crash_raw = crash_summary(rec)
        stage = stage_of(rep, (fe or {}).get("step"))
        if rec:
            if rec.get("application_created") and not rep.get("stages", {}).get("process_start", {}).get("ok"):
                rep.setdefault("stages", {})["process_start"] = {"ok": True, "rung": "application_on_create"}
                rep["stages"]["application_on_create"] = {"ok": True, "rung": "application_on_create"}
            if rec.get("activity_resumed"):
                rep["stages"]["activity_resumed"] = {"ok": True}
            stage = stage_of(rep, (fe or {}).get("step"))
        # The guest's own crash line names the exact framework call; it is a
        # better wall key than the transport code (GUEST_EXITED).
        comp, fix = classify(fe.get("code") if fe else None,
                             (api or "") + " " + (crash_msg or "")) if (fe or api) else (None, None)
        if api:
            comp, fix = classify("UNSUPPORTED_JAVA_API", api)
        launch_ok = bool(((rep.get("stages") or {}).get("first_frame") or {}).get("ok"))
        facts = {
            "inspect": bool(((rep.get("stages") or {}).get("inspect") or {}).get("ok")),
            # The guest record is written only after the ART guest process ran.
            "process_start": bool(rec or launch_ok or (rep.get("peak_rss_mib") or 0) > 20),
            "oncreate_entered": bool((rec or {}).get("application_created") or launch_ok),
            "oncreate_completed": bool((rec or {}).get("activity_resumed") or launch_ok),
            "first_frame": launch_ok,
            "tree": bool(((rep.get("stages") or {}).get("tree") or {}).get("ok")),
            "screenshot": bool(((rep.get("stages") or {}).get("screenshot") or {}).get("ok")),
            "tap": bool(((rep.get("stages") or {}).get("tap") or {}).get("ok")),
        }
        reached = [x for x in STAGES if facts[x]]
        stage = reached[-1] if reached else "inspect"
        rows.append({
            "package": pkg,
            "status": "ok" if rep.get("ok") else ("fail" if rep.get("stages", {}).get("first_frame", {}).get("ok") else "blocked"),
            "apk": meta.get("name"),
            "apk_sha256": meta.get("sha256"),
            "source_url": meta.get("source_url"),
            "apk_size_bytes": meta.get("size_bytes"),
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
            "unsupported_api": api,
            "crash_message": norm_msg(crash_msg),
            "crash_raw": crash_raw,
            "application_created": (rec or {}).get("application_created"),
            "activity_resumed": (rec or {}).get("activity_resumed"),
            "guest_signal": (rec or {}).get("guest_signal"),
            "engine_stage_exit": (rec or {}).get("stage"),
            "unsupported_calls": (rec or {}).get("unsupported") or [],
            "error_stage": (fe or {}).get("stage"),
            "wall_component": comp,
            "wall_fix": fix,
            "engine_stage": ((rep.get("stages") or {}).get("engine_stage")),
            "markers_present": rep.get("markers_present"),
            "stages": facts,
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
        if r["status"] == "ok":
            continue
        key = r["error_code"] or "unknown"
        msg = r["error_message"] or ""
        key = wall_key(r.get("unsupported_api"), r.get("crash_message"), r["error_code"])
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
