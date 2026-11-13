#!/usr/bin/env python3
"""Agent-loop benchmark driver: what an agent pays per decision through `acos serve`.

ADR/ADVISOR item 9. The daemon is driven over its real transport (newline JSON
on a Unix socket, `acos_mcp::daemon` requests), with the fixed 10-request script
the advisor specified:

    launch -> tree -> tap -> tree -> type -> key -> screenshot -> assert -> logs -> reset

The loop is run twice per app; every iteration after one warm-up is a sample.
The per-verb table, round-trips, payload bytes and gate results are written in
the BENCHMARKS.md section 11 / ADVISOR item 5 result schema. Statistics,
provenance, host gate, error classification and JSON writing come from
``tools/benchlib.py`` (imported, never duplicated).

Honesty rules encoded here:
  * a verb that returns ``ok: false`` is never counted as a successful sample;
  * a tree payload or verdict that was never produced is ``NOT_MEASURED``, not
    a pass;
  * round-trips to a verdict are only reported for a task that actually reached
    a verdict; otherwise the first terminal request is reported instead;
  * every gate is reported PASS / FAIL / NOT_MEASURED / EXCLUDED explicitly.

Usage:
    python3 tools/art-runtime/agent_loop.py --socket S --out OUT.json \
        --markdown W49.md [--loops 12] [--warmup 1] [--runs 2]

The caller (tools/art-runtime/bench-agent-loop.sh) owns the host lock, starts
`acos serve` and removes the scratch directory after this process exits.
"""

from __future__ import annotations

import argparse
import json
import os
import socket
import sys
import time
import uuid
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
sys.path.insert(0, str(REPO / "tools"))

import benchlib  # noqa: E402  (path inserted above)

# The fixed script. `verb` is the benchmark column; `tool` is the daemon tool.
SCRIPT = [
    ("launch", "acos_launch"),
    ("tree", "acos_observe_tree"),
    ("tap", "acos_tap"),
    ("tree", "acos_observe_tree"),
    ("type", "acos_text"),
    ("key", "acos_key"),
    ("screenshot", "acos_observe"),
    ("assert", "acos_observe_tree"),
    ("logs", "acos_logs"),
    ("reset", "acos_reset"),
]
# Verbs excluded from the 500 ms warm p95 gate (cold by nature).
GATE_EXCLUDED = {"launch", "reset"}
LATENCY_GATE_MS = 500.0
TREE_PAYLOAD_GATE_BYTES = 64 * 1024
ROUND_TRIP_GATE = 12


def app_config(name: str) -> dict:
    """Per-app inputs. Both apps are real fixtures; the only difference is the
    target the fixed script acts on (the script sequence is identical)."""
    apps = {
        "counter": {
            "apk": "fixtures/build/apps/counter.apk",
            "requested_scenario": "scenarios/v2-counter.yaml",
            "scenario": "scenarios/v2-art-counter.yaml",
            "tap_id": "inc",
            "type_text": "1",
            "assert_id": "count",
            "assert_text": "1",
            "assert_mode": "exact",
            "log_tag": "AcosCounter",
        },
        "form-validation": {
            "apk": "fixtures/build/apps/form-validation.apk",
            "requested_scenario": "scenarios/form-validation.yaml",
            "scenario": "scenarios/form-validation.yaml",
            "tap_id": "name",
            "type_text": "Ada",
            "assert_id": "name",
            "assert_text": "Ada",
            "assert_mode": "contains",
            "log_tag": "AcosForm",
        },
    }
    cfg = dict(apps[name])
    cfg["name"] = name
    for key in ("apk", "scenario"):
        cfg[key] = str(REPO / cfg[key])
    return cfg


class Daemon:
    """One persistent connection; one request/response at a time (the wire
    protocol is ordered)."""

    def __init__(self, path: str, timeout: float = 180.0):
        self.path = path
        self.timeout = timeout
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(timeout)
        self.sock.connect(path)
        self.file = self.sock.makefile("rwb")
        self.next_id = 0

    def close(self):
        try:
            self.file.close()
        finally:
            self.sock.close()

    def call(self, tool: str, args: dict) -> dict:
        self.next_id += 1
        request = {"id": self.next_id, "tool": tool, "args": args}
        line = (json.dumps(request, separators=(",", ":")) + "\n").encode()
        t0 = time.perf_counter()
        self.file.write(line)
        self.file.flush()
        response = self.file.readline()
        wall_ms = (time.perf_counter() - t0) * 1000.0
        if not response:
            raise ConnectionError("daemon closed the connection during %s" % tool)
        raw = json.loads(response)
        envelope = raw.get("envelope")
        if envelope is None:
            # Transport-level error object (no ACOS envelope).
            envelope = {
                "ok": False,
                "status": "error",
                "error": raw.get("error") or {
                    "code": "INTERNAL",
                    "message": "daemon returned no envelope",
                },
            }
        return {
            "tool": tool,
            "request_id": self.next_id,
            "wall_ms": wall_ms,
            "response_bytes": len(response),
            "envelope": envelope,
        }


def node_text(node: dict) -> str:
    value = node.get("text")
    if value is None:
        value = node.get("content_desc")
    return "" if value is None else str(value)


def walk(nodes):
    for node in nodes or []:
        if not isinstance(node, dict):
            continue
        yield node
        yield from walk(node.get("children"))


def find_node(tree: dict, resource_id: str):
    """Match `pkg:id/name` or the bare `name` (same rule as Selector::by_id)."""
    for node in walk((tree or {}).get("nodes")):
        rid = node.get("resource_id") or ""
        if rid == resource_id or rid.endswith(":id/" + resource_id):
            return node
    return None


def error_block(envelope: dict):
    err = envelope.get("error")
    if not isinstance(err, dict):
        return None
    code = err.get("code")
    classified = benchlib.classify_error(code, err.get("message"),
                                        retryable=err.get("retryable"),
                                        hint=err.get("hint"))
    # Keep the protocol hint when the code table would supply a generic one.
    if err.get("hint"):
        classified["hint"] = err["hint"]
    return classified


def status_of(envelope: dict) -> str:
    if envelope.get("ok"):
        return "ok"
    status = envelope.get("status")
    if status in ("unsupported", "blocked"):
        return "unsupported"
    return "error"


class Runner:
    def __init__(self, args, daemon: Daemon, provenance: dict, host: dict):
        self.args = args
        self.daemon = daemon
        self.provenance = provenance
        self.host = host
        self.rows = []
        self.verbs = {}
        self.controls = {}
        self.scratch = Path(args.scratch)
        self.scratch.mkdir(parents=True, exist_ok=True)
        (self.scratch / "screens").mkdir(exist_ok=True)

    # -- one request ------------------------------------------------------- #
    def request(self, cfg: dict, run: int, iteration: int, step: int,
                verb: str, tool: str, args: dict):
        """One daemon round-trip. Returns (step record, parsed result value)."""
        result = self.daemon.call(tool, args)
        env = result["envelope"]
        status = status_of(env)
        record = {
            "step": step,
            "kind": verb,
            "verb": verb,
            "tool": tool,
            "ms": round(result["wall_ms"], 3),
            "status": status,
            "ok": bool(env.get("ok")),
            "response_bytes": result["response_bytes"],
            "result_bytes": 0,
            "tree_bytes": None,
            "png_bytes": None,
            "error_code": None,
            "error": None,
        }
        value = env.get("result")
        if value is not None:
            record["result_bytes"] = len(
                json.dumps(value, separators=(",", ":")).encode())
        err = error_block(env)
        if err is not None:
            record["error_code"] = err["code"]
            record["error"] = err
        if tool == "acos_observe_tree" and isinstance(value, dict) and isinstance(
                value.get("tree"), dict):
            record["tree_bytes"] = len(
                json.dumps(value["tree"], separators=(",", ":")).encode())
            record["tree_max_nodes"] = value["tree"].get("max_nodes")
            record["tree_node_count"] = sum(1 for _ in walk(value["tree"].get("nodes")))
            record["tree_truncated"] = value["tree"].get("truncated")
        if verb == "launch" and isinstance(value, dict):
            events = [str(e) for e in (value.get("events") or [])]
            record["events_evidence"] = [
                e for e in events
                if "BadParcelable" in e or "Exception" in e or "not fully consumed" in e][:3]
        if tool == "acos_logs" and isinstance(value, dict):
            lines = [str(l.get("line", "")) for l in (value.get("logs") or [])
                     if isinstance(l, dict)]
            record["log_evidence"] = [
                l for l in lines
                if "BadParcelable" in l or "not fully consumed" in l
                or "no root view" in l][:5]
        if isinstance(value, dict) and isinstance(value.get("settle"), dict):
            record["settle"] = value["settle"]
            timing = value.get("timing") or {}
            if isinstance(timing.get("input"), dict):
                record["input_timing"] = timing["input"]
        if tool == "acos_observe" and isinstance(value, dict):
            shot = value.get("screenshot") or {}
            if isinstance(shot, dict):
                record["png_bytes"] = shot.get("bytes")
                path = shot.get("path")
                if path and os.path.exists(path):
                    record["png_bytes"] = os.path.getsize(path)
                    record["png_path"] = path
        return record, value

    # -- the fixed 10-request script --------------------------------------- #
    def run_task(self, cfg: dict, run: int, iteration: int):
        session = "s_w49_%s" % cfg["name"]
        base = {"request_id": "w49-%s-%d-%d" % (cfg["name"], run, iteration),
                "session_id": session}
        shot_path = self.scratch / "screens" / (
            "%s-r%d-i%02d.png" % (cfg["name"], run, iteration))
        steps = []
        tree_before = tree_after = None
        verdict = "error"
        terminal_at = None

        for index, (verb, tool) in enumerate(SCRIPT, 1):
            args = {"base": base}
            if verb == "launch":
                args["apk"] = cfg["apk"]
            elif verb == "tap":
                args["id"] = cfg["tap_id"]
                if self.args.settle != "strict":
                    args["settle"] = self.args.settle
            elif verb == "type":
                args["text"] = cfg["type_text"]
                if self.args.settle != "strict":
                    args["settle"] = self.args.settle
            elif verb == "key":
                args["code"] = 66  # KEYCODE_ENTER
                if self.args.settle != "strict":
                    args["settle"] = self.args.settle
            elif verb == "screenshot":
                args.update({"tree": False, "text": False, "png": True,
                             "screenshot": str(shot_path)})
            elif verb == "assert":
                args["max_nodes"] = 2000
            elif verb == "tree":
                args["max_nodes"] = 2000

            # `assert` keeps its own evidence request so the server call is real
            # (serve has no assert verb; the check itself is client-side).
            record, value = self.request(cfg, run, iteration, index, verb, tool, args)
            steps.append(record)
            if record["tree_bytes"] is not None:
                if verb == "tree" and tree_before is None:
                    tree_before = record
                elif verb == "tree":
                    tree_after = record
            if not record["ok"] and terminal_at is None:
                terminal_at = index

            if verb == "assert":
                verdict = self.evaluate_assert(cfg, record, value)
                record["assert_verdict"] = verdict

        wall_ms = sum(s["ms"] for s in steps)
        to_verdict = None
        if verdict == "pass":
            to_verdict = 8  # launch..assert inclusive
        first_failure = next((s for s in steps if not s["ok"]), None)
        row = {
            "run_id": str(uuid.uuid4()),
            "utc": benchlib.utc_now(),
            "git": self.provenance.get("git_head"),
            "dirty_hash": self.provenance.get("git_status_porcelain_sha256"),
            "profile": "release",
            "engine": "acos-native-session",
            "verdict_eligible": bool(self.provenance.get("verdict_eligible")),
            "scenario": cfg["scenario"],
            "requested_scenario": cfg["requested_scenario"],
            "scenario_sha256": benchlib.sha256_file(cfg["scenario"]),
            "apk_sha256": benchlib.sha256_file(cfg["apk"]),
            "variant": "good",
            "expected": "pass",
            "verdict": "pass" if verdict == "pass" else "error",
            "false_pass": False,
            "error": (first_failure or {}).get("error"),
            "state": "warm",
            "n_concurrent": 1,
            "wall_ms": round(wall_ms, 3),
            "first_frame_ms": (steps[0]["ms"]
                               if steps and verdict == "pass" else None),
            "launch_ms": steps[0]["ms"] if steps else None,
            "steps": steps,
            "mem": {
                "cgroup_peak_mib": None,
                "cgroup_gib_min": None,
                "anon_shmem_settled_mib": None,
                "pss_mib": None,
                "metric_type": "CGROUP_ANON_SHMEM",
                "note": "agent-loop latency benchmark: memory is not measured here",
            },
            "host": {
                "loadavg1": self.host.get("loadavg1"),
                "mem_available_mib": self.host.get("mem_available_mib"),
            },
            "negative": False,
            "app": cfg["name"],
            "run": run,
            "iteration": iteration,
            "warmup": False,
            "round_trips": len(steps),
            "round_trips_to_verdict": to_verdict,
            "round_trips_to_first_terminal": terminal_at,
        }
        # Per-task PNGs are samples, not an ever-growing corpus (S3ab: 28 GB).
        row["screens_pruned"] = benchlib.prune_screens(
            self.scratch / "screens", getattr(self.args, "keep_screens", 20))
        return row

    def evaluate_assert(self, cfg: dict, step: dict, value) -> str:
        """Client-side assertion over the evidence tree. serve has no assert verb,
        so this is honest only when the tree request actually succeeded."""
        if not step["ok"] or step.get("tree_bytes") is None:
            return "not_reached"
        if not isinstance(value, dict):
            return "not_reached"
        tree = value.get("tree") or {}
        node = find_node(tree, cfg["assert_id"])
        text = node_text(node) if node else ""
        if cfg["assert_mode"] == "exact":
            return "pass" if text.strip() == cfg["assert_text"] else "fail"
        return "pass" if cfg["assert_text"] in text else "fail"


def collect_controls(daemon: Daemon, cfg: dict, count: int = 5) -> dict:
    """Decomposition controls, outside the task's round-trip count."""
    base = {"request_id": "w49-control-%s" % cfg["name"],
            "session_id": "s_w49_control_%s" % cfg["name"]}
    out = {"ping_ms": [], "tree_error_ms": [], "frame_ms": [],
           "screenshot_ms": [], "frame_available": False, "png_available": False}
    pings = []
    for _ in range(count):
        r = daemon.call("ping", {})
        pings.append(round(r["wall_ms"], 3))
    out["ping_ms"] = {
        "stats": benchlib.stats(pings),
        "note": "transport floor; the request never reaches a guest",
    }
    daemon.call("acos_launch", {"base": base, "apk": cfg["apk"]})
    tree = []
    frame = []
    for _ in range(count):
        r = daemon.call("acos_observe_tree", {"base": base, "max_nodes": 2000})
        tree.append(round(r["wall_ms"], 3))
        r = daemon.call("acos_observe", {"base": base, "tree": False,
                                        "png": False})
        frame.append(round(r["wall_ms"], 3))
    out["tree_ms"] = benchlib.stats(tree)
    out["frame_ms"] = benchlib.stats(frame)
    out["frame_available"] = any(
        (r.get("envelope") or {}).get("ok") for r in
        [daemon.call("acos_observe", {"base": base, "tree": False, "png": False})])
    out["note"] = ("control phase after one launch; tree/frame calls here are "
                   "diagnostic and not part of the 10-request task count")
    daemon.call("acos_reset", {"base": base})
    # Free the slot: the daemon enforces --max-sessions and a leaked control
    # session would make the next app's launch fail with RESOURCE_LIMIT.
    daemon.call("acos_destroy", {"base": base})
    return out


def aggregate(rows, cfg):
    """Per-verb stats and bytes for one app; only real samples are used for the
    latency gate, error responses are reported separately."""
    per = {}
    samples = [r for r in rows if not r.get("warmup")]
    for verb, _tool in SCRIPT:
        times = []
        ok_times = []
        bytes_total = []
        tree_bytes = []
        png_bytes = []
        ok = 0
        errors = {}
        for row in samples:
            matched = [s for s in row["steps"] if s["verb"] == verb]
            if not matched:
                continue
            for step in matched:
                times.append(step["ms"])
                if step["ok"]:
                    ok += 1
                    ok_times.append(step["ms"])
                else:
                    code = step.get("error_code") or "UNKNOWN"
                    errors[code] = errors.get(code, 0) + 1
                bytes_total.append(step.get("response_bytes") or 0)
                if step.get("tree_bytes") is not None:
                    tree_bytes.append(step["tree_bytes"])
                if step.get("png_bytes") is not None:
                    png_bytes.append(step["png_bytes"])
        st = benchlib.stats(times)
        if verb in GATE_EXCLUDED:
            gate = "EXCLUDED"
            reason = "cold verb: excluded from the 500 ms warm gate by the benchmark spec"
        elif st["n"] < 20:
            gate = "INSUFFICIENT"
            reason = "fewer than 20 samples (n=%d)" % st["n"]
        elif st["p95"] is None:
            gate = "NOT_MEASURED"
            reason = "no samples"
        elif st["p95"] < LATENCY_GATE_MS:
            gate = "PASS"
            reason = "p95 %.1f ms < %.0f ms" % (st["p95"], LATENCY_GATE_MS)
        else:
            gate = "FAIL"
            reason = "p95 %.1f ms >= %.0f ms" % (st["p95"], LATENCY_GATE_MS)
        per[verb] = {
            "stats": st,
            "ok": ok,
            "error_path_samples": len(times) - ok,
            "ok_rate": (ok / len(times)) if times else None,
            "errors": errors,
            "gate": gate,
            "gate_reason": reason,
            "bytes": {
                "response": benchlib.stats(bytes_total),
                "tree": benchlib.stats(tree_bytes) if tree_bytes else None,
                "png": benchlib.stats(png_bytes) if png_bytes else None,
            },
        }
    return per


def probe_daemon_engine(socket_path):
    """Ask the daemon which engine `acos_launch` drives when none is named."""
    try:
        probe = Daemon(socket_path, timeout=10.0)
        try:
            ping = probe.call("ping", {})
        finally:
            probe.close()
        result = (ping.get("envelope") or {}).get("result") or {}
        return result.get("default_engine") or "unknown"
    except Exception:
        return "unknown"


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--socket", required=True)
    parser.add_argument("--out", required=True)
    parser.add_argument("--markdown")
    parser.add_argument("--scratch", default=str(REPO / ".cache/w49-agent-loop"))
    parser.add_argument("--apps", default="counter,form-validation")
    parser.add_argument("--loops", type=int, default=12)
    parser.add_argument("--warmup", type=int, default=1)
    parser.add_argument("--runs", type=int, default=2)
    parser.add_argument("--settle", choices=("strict", "fast"), default="strict",
                        help="P1 settle mode for tap/type/key (default: strict)")
    parser.add_argument("--strict", action="store_true",
                        help="exit 1 when any gate fails (default: report only)")
    parser.add_argument("--allow-dirty", action="store_true",
                        help="write a result even when provenance is not verdict eligible")
    parser.add_argument("--keep-screens", type=int, default=20,
                        help="max per-task PNG samples retained on disk (default 20)")
    args = parser.parse_args(argv)

    names = [a.strip() for a in args.apps.split(",") if a.strip()]
    host = benchlib.read_meminfo()
    host.update(benchlib.read_loadavg())
    gate = benchlib.host_gate(REPO, check_lock=False)
    binary = Path(os.environ.get("ACOS_BENCH_BINARY",
                                str(REPO / ".cache/w49-target/release/acos")))
    # W50: the engine label is read from the daemon, never assumed. The daemon
    # answers `result.default_engine` (art | rust-vm); a pre-W50 daemon has no
    # such field and is reported as `unknown`.
    serve_engine = probe_daemon_engine(args.socket)
    serve_has_art = serve_engine == "art"
    provenance = benchlib.provenance(
        REPO, [binary], profile="release", gate=gate,
        engine=serve_engine,
        extra={"transport": "unix newline JSON (acos_mcp::daemon)",
               "serve_binary": str(binary),
               "serve_has_art_engine": serve_has_art,
               "serve_default_engine": serve_engine,
               "note": ("acos serve drives the real-ART session engine by default "
                        "(one acos-art guest per session); engine: rust-vm is the "
                        "frozen Rust VM and is labelled in every reply")
                       if serve_has_art else
                       ("acos serve reports default_engine=%s; the launch path is "
                        "not the real-ART session engine" % serve_engine)})
    if not provenance.get("verdict_eligible"):
        msg = ("provenance is not verdict eligible: %s"
               % provenance.get("not_verdict_eligible_reason"))
        if not args.allow_dirty:
            print("agent_loop: " + msg, file=sys.stderr)
            print("agent_loop: pass --allow-dirty to write a labelled result",
                  file=sys.stderr)
            return 2

    daemon = Daemon(args.socket, timeout=float(os.environ.get("ACOS_BENCH_TIMEOUT", "180")))
    try:
        daemon.call("ping", {})
        runner = Runner(args, daemon, provenance, host)
        rows = []
        per_app = {}
        controls = {}
        for name in names:
            cfg = app_config(name)
            if not os.path.exists(cfg["apk"]):
                print("agent_loop: missing APK %s (gap, not a measurement)"
                      % cfg["apk"], file=sys.stderr)
                per_app[name] = {"error": "missing APK", "verbs": {}}
                continue
            controls[name] = collect_controls(daemon, cfg)
            app_rows = []
            for run in range(1, args.runs + 1):
                for i in range(args.loops):
                    warm = i < args.warmup
                    row = runner.run_task(cfg, run, i)
                    row["warmup"] = warm
                    app_rows.append(row)
                    rows.append(row)
            per_app[name] = {
                "config": cfg,
                "verbs": aggregate(app_rows, cfg),
                "rows": len(app_rows),
                "samples": sum(1 for r in app_rows if not r.get("warmup")),
            }
            # The task session is kept across iterations (that is the point of
            # a daemon); free it before the next app so max_sessions is not hit.
            daemon.call("acos_destroy", {"base": {
                "request_id": "w49-destroy-%s" % name,
                "session_id": "s_w49_%s" % name}})
        runner.controls = controls
    finally:
        daemon.close()

    # ---- gates ---------------------------------------------------------- #
    gates = {}
    verb_failures = []
    for name, app in per_app.items():
        if "verbs" not in app:
            gates["%s:verbs" % name] = "NOT_MEASURED"
            continue
        for verb, entry in app["verbs"].items():
            gates["%s:%s:p95<500ms" % (name, verb)] = entry["gate"]
            if entry["gate"] == "FAIL":
                verb_failures.append((name, verb, entry))
    tree_measured = any(
        app.get("verbs", {}).get("tree", {}).get("bytes", {}).get("tree")
        for app in per_app.values())
    counter_tree = (per_app.get("counter", {}).get("verbs", {})
                    .get("tree", {}).get("bytes", {}).get("tree"))
    if not tree_measured or counter_tree is None:
        gates["counter:tree_payload<=64KiB"] = "NOT_MEASURED"
    else:
        counter_max = counter_tree.get("max")
        gates["counter:tree_payload<=64KiB"] = (
            "PASS" if counter_max <= TREE_PAYLOAD_GATE_BYTES else "FAIL")
    verdict_round_trips = [r["round_trips_to_verdict"] for r in rows
                           if r.get("round_trips_to_verdict") is not None]
    if not verdict_round_trips:
        gates["task:round_trips<=12"] = "NOT_MEASURED"
    else:
        gates["task:round_trips<=12"] = (
            "PASS" if max(verdict_round_trips) <= ROUND_TRIP_GATE else "FAIL")
    reached_tree = any(
        (app.get("verbs", {}).get("tree", {}).get("ok") or 0) > 0
        for app in per_app.values())
    gates["runtime:task_reaches_verdict"] = "PASS" if any(
        r["verdict"] == "pass" for r in rows) else "FAIL"
    gates["runtime:tree_reached"] = "PASS" if reached_tree else "FAIL"
    gates["overall"] = "PASS" if all(
        v in ("PASS", "EXCLUDED") for k, v in gates.items()) else "FAIL"

    gaps = []
    if not reached_tree:
        first_err = next((s for s in rows[0]["steps"] if not s["ok"]), None) if rows else None
        gaps.append({
            "id": "runtime-no-window",
            "kind": "functional",
            "gate": "runtime:tree_reached",
            "verbs": ["launch", "tree", "tap", "type", "screenshot", "assert"],
            "evidence": {
                "first_error": (first_err or {}).get("error"),
                "error_count": len([r for r in rows if r["verdict"] != "pass"]),
                "total_rows": len(rows),
                "launch_p50_ms": (per_app.get("counter", {}).get("verbs", {})
                                  .get("launch", {}).get("stats", {}).get("p50")),
                "tree_p50_ms": (per_app.get("counter", {}).get("verbs", {})
                                .get("tree", {}).get("stats", {}).get("p50")),
            },
            "biggest_cost": ("no guest work: the tree/tap/type/screenshot replies "
                             "return in transport time (see controls.ping); the "
                             "agent pays a launch (%s ms p50) that cannot produce "
                             "a window, so every later verb is an error path"
                             % _fmt_ms(per_app.get("counter", {}).get("verbs", {})
                                       .get("launch", {}).get("stats", {}).get("p50"))),
        })
        gaps.append({
            "id": "serve-no-art-engine",
            "kind": "missing-verb",
            "gate": "runtime:task_reaches_verdict",
            "verbs": ["launch"],
            "evidence": {
                "serve_engine": "acos-native-session (SessionBackend/AgentSession)",
                "art_engine_in_serve": False,
                "how_seen": "crates/acos-cli/src/serve.rs dispatch has no engine "
                            "argument; acos_launch ignores unknown args; "
                            "crates/acos-cli/src/engine/art.rs is only reachable "
                            "through `acos verify --engine art`",
            },
            "biggest_cost": ("this benchmark cannot measure real-ART agent-loop cost "
                             "at all: 0 ART requests were possible through serve"),
        })
    for name, verb, entry in verb_failures:
        gaps.append({
            "id": "%s-%s-latency" % (name, verb),
            "kind": "latency",
            "gate": "%s:%s:p95<500ms" % (name, verb),
            "verbs": [verb],
            "evidence": {"p50_ms": entry["stats"]["p50"],
                         "p95_ms": entry["stats"]["p95"],
                         "n": entry["stats"]["n"]},
            "biggest_cost": _biggest_cost(name, verb, entry, per_app, controls),
        })
    if gates["counter:tree_payload<=64KiB"] == "NOT_MEASURED":
        gaps.append({
            "id": "tree-payload-not-measured",
            "kind": "measurement",
            "gate": "counter:tree_payload<=64KiB",
            "verbs": ["tree", "assert"],
            "evidence": {"tree_samples": 0,
                         "why": "no observe_tree response carried a tree"},
            "biggest_cost": "not measurable without a window/frame",
        })
    if gates["task:round_trips<=12"] == "NOT_MEASURED":
        gaps.append({
            "id": "round-trips-not-measured",
            "kind": "measurement",
            "gate": "task:round_trips<=12",
            "verbs": ["assert"],
            "evidence": {"round_trips_to_verdict": None,
                         "round_trips_to_first_terminal": [
                             r.get("round_trips_to_first_terminal") for r in rows[:2]]},
            "biggest_cost": "no verdict was reachable",
        })
    if any(r.get("requested_scenario") and not os.path.exists(r["requested_scenario"])
           for r in rows):
        gaps.append({
            "id": "requested-scenario-missing",
            "kind": "harness",
            "gate": "scenario",
            "verbs": ["launch"],
            "evidence": {"requested": "scenarios/v2-counter.yaml",
                         "present_alternatives": ["scenarios/v2-art-counter.yaml",
                                                  "scenarios/v1-counter.yaml"]},
            "biggest_cost": "the counter loop used scenarios/v2-art-counter.yaml",
        })
    gaps.append({
        "id": "serve-assert-verb-missing",
        "kind": "missing-verb",
        "gate": "task:round_trips<=12",
        "verbs": ["assert"],
        "evidence": {"serve_session_tools": [
            "acos_launch", "acos_observe_tree", "acos_tap", "acos_key",
            "acos_text", "acos_swipe", "acos_logs", "acos_status", "acos_reset",
            "acos_destroy", "acos_doctor"]},
        "biggest_cost": ("assertion needs an extra observe_tree round-trip "
                         "client-side; serve has no assert/capture verb"),
    })
    gaps.append({
        "id": "serve-screenshot-verb-missing",
        "kind": "missing-verb",
        "gate": "screenshot",
        "verbs": ["screenshot"],
        "evidence": {"screenshot_via": "acos_observe {png:true, screenshot:path}",
                     "dedicated_tool": None},
        "biggest_cost": ("the PNG is written host-side by observe; encode vs guest "
                         "capture cannot be separated without a frame"),
    })

    counted = [r for r in rows if not r.get("warmup")]
    summary = {
        "total_runs": len(counted),
        "passed": sum(1 for r in counted if r["verdict"] == "pass"),
        "failed": sum(1 for r in counted if r["verdict"] != "pass"),
        "false_pass": sum(1 for r in rows if r["false_pass"]),
        "negative_runs": 0,
        "unclassified": sum(1 for r in counted for s in r["steps"]
                            if s.get("error") and s["error"].get("class") == "unknown"),
    }
    doc = {
        "schema": benchlib.MATRIX_SCHEMA,
        "run_id": str(uuid.uuid4()),
        "utc": benchlib.utc_now(),
        "git": provenance.get("git_head"),
        "dirty_hash": provenance.get("git_status_porcelain_sha256"),
        "profile": "release",
        "engine": provenance.get("engine"),
        "provenance": provenance,
        "agent_loop": {
            "script": [v for v, _ in SCRIPT],
            "tools": [t for _, t in SCRIPT],
            "samples_per_verb": args.runs * max(0, args.loops - args.warmup),
            "apps": per_app,
            "controls": runner.controls,
            "gates": gates,
            "gaps": gaps,
            "host": {k: host.get(k) for k in
                     ("loadavg1", "loadavg5", "mem_available_mib", "mem_available_gib")},
        },
        "runs": rows,
        "summary": summary,
    }
    errors = benchlib.validate_matrix(doc)
    out = Path(args.out)
    if errors:
        bad = out.with_suffix(".invalid.json")
        benchlib.write_json(bad, doc)
        print("agent_loop: schema validation failed (%d): %s"
              % (len(errors), "; ".join(errors[:5])), file=sys.stderr)
        print("agent_loop: wrote %s for debugging" % bad, file=sys.stderr)
        return 3
    benchlib.write_json(out, doc)

    if args.markdown:
        Path(args.markdown).write_text(render_markdown(doc))
    print(render_table(doc))
    print("agent_loop: wrote %s" % out)
    failed = [k for k, v in gates.items() if v == "FAIL"]
    if args.strict and failed:
        return 1
    return 0


def _fmt_ms(value):
    return "n/a" if value is None else "%.1f" % value


def _biggest_cost(name, verb, entry, per_app, controls):
    st = entry["stats"]
    c = (controls or {}).get(name) or {}
    ping = (c.get("ping_ms") or {}).get("stats", {}).get("p50")
    tree = (c.get("tree_ms") or {}).get("p50")
    frame = (c.get("frame_ms") or {}).get("p50")
    parts = ["p50 %.1f ms; error-path samples %d/%d; response bytes p50 %s" % (
        st.get("p50") or 0.0, entry["error_path_samples"], st.get("n") or 0,
        (entry["bytes"]["response"] or {}).get("p50"))]
    if ping is not None:
        parts.append("transport floor ping p50 %.2f ms" % ping)
    if tree is not None and frame is not None:
        parts.append("control tree p50 %.2f ms, frame-only p50 %.2f ms"
                     % (tree, frame))
    if verb in ("tree", "assert"):
        parts.append("biggest cost is the guest tree probe + JSON serialization "
                     "(bounded by tree control minus ping)")
    elif verb == "screenshot":
        parts.append("biggest cost is guest frame capture + PNG encode/write "
                     "(frame-only control is the same call without PNG)")
    else:
        parts.append("biggest cost is the guest input dispatch plus its probe")
    return "; ".join(parts)


def render_table(doc):
    lines = []
    loop = doc["agent_loop"]
    lines.append("app            verb        n    p50      p95      bytes(p50)  ok/total  gate")
    for app, data in loop["apps"].items():
        for verb, e in data.get("verbs", {}).items():
            st = e["stats"]
            total = (e["ok"] + e["error_path_samples"])
            lines.append("%-14s %-10s %3d  %8s %8s  %10s  %d/%-6d %s" % (
                app, verb, st["n"], _fmt_ms(st["p50"]), _fmt_ms(st["p95"]),
                (e["bytes"]["response"] or {}).get("p50") or "-",
                e["ok"], total, e["gate"]))
    lines.append("")
    lines.append("gates:")
    for k, v in loop["gates"].items():
        lines.append("  %-38s %s" % (k, v))
    lines.append("gaps: %d" % len(loop["gaps"]))
    for g in loop["gaps"]:
        lines.append("  - %s (%s)" % (g["id"], g["kind"]))
    return "\n".join(lines) + "\n"


def render_markdown(doc):
    loop = doc["agent_loop"]
    prov = doc["provenance"]
    lines = ["# Agent-loop benchmark (acos serve, engine %s)" % doc.get("engine"), ""]
    lines.append("Run: %s  UTC %s  profile %s  engine %s"
                 % (doc["run_id"], doc["utc"], doc["profile"], doc["engine"]))
    lines.append("Git %s (dirty hash %s), binary %s sha256 %s"
                 % (prov.get("git_head"), (prov.get("git_status_porcelain_sha256") or "")[:16],
                    (prov.get("binaries") or [{}])[0].get("path"),
                    ((prov.get("binaries") or [{}])[0].get("sha256") or "")[:16]))
    lines.append("")
    lines.append("Fixed script: " + " -> ".join(loop["script"]))
    lines.append("Two runs per app; one warm-up iteration per run; "
                 "%d samples per verb." % loop["samples_per_verb"])
    lines.append("")
    lines.append("## Per-verb cost")
    lines.append("")
    lines.append("**Read the `ok/total` column with the gate.** A latency gate can PASS")
    lines.append("on a verb that never succeeded: that is a *fast error path*, not a")
    lines.append("working verb. `runtime:tree_reached` / `runtime:task_reaches_verdict`")
    lines.append("are the functional gates and they are reported separately.")
    lines.append("")
    lines.append("| app | verb | n | p50 ms | p95 ms | response bytes p50 | tree bytes | png bytes | ok/total | ok rate | gate |")
    lines.append("|---|---|---|---|---|---|---|---|---|---|---|")
    for app, data in loop["apps"].items():
        for verb, e in data.get("verbs", {}).items():
            st = e["stats"]
            total = e["ok"] + e["error_path_samples"]
            tree = e["bytes"]["tree"]
            png = e["bytes"]["png"]
            rate = ("%.0f%%" % (100.0 * e["ok_rate"])) if e.get("ok_rate") is not None else "-"
            lines.append("| %s | %s | %d | %s | %s | %s | %s | %s | %d/%d | %s | %s |" % (
                app, verb, st["n"], _fmt_ms(st["p50"]), _fmt_ms(st["p95"]),
                (e["bytes"]["response"] or {}).get("p50") or "-",
                (tree or {}).get("p50") if tree else "-",
                (png or {}).get("p50") if png else "-",
                e["ok"], total, rate, e["gate"]))
    lines.append("")
    lines.append("## Gates")
    lines.append("")
    lines.append("| gate | result |")
    lines.append("|---|---|")
    for k, v in loop["gates"].items():
        lines.append("| %s | **%s** |" % (k, v))
    lines.append("")
    lines.append("## Gaps")
    lines.append("")
    for g in loop["gaps"]:
        lines.append("### %s (%s)" % (g["id"], g["kind"]))
        lines.append("")
        lines.append("- gate: `%s`  verbs: %s" % (g["gate"], ", ".join(g["verbs"])))
        lines.append("- evidence: `%s`" % json.dumps(g["evidence"], sort_keys=True))
        lines.append("- biggest cost: %s" % g["biggest_cost"])
        lines.append("")
    lines.append("## Controls (cost decomposition)")
    lines.append("")
    for app, c in loop.get("controls", {}).items():
        lines.append("**%s**" % app)
        lines.append("")
        for key in ("ping_ms", "tree_ms", "frame_ms"):
            if key in c:
                st = c[key].get("stats", c[key]) if isinstance(c[key], dict) else c[key]
                lines.append("- %s: n=%s p50=%s p95=%s ms" % (
                    key, st.get("n"), _fmt_ms(st.get("p50")), _fmt_ms(st.get("p95"))))
        lines.append("- frame available: %s" % c.get("frame_available"))
        lines.append("")
    lines.append("## Host")
    lines.append("")
    lines.append("loadavg1 %s, MemAvailable %s MiB, host gate ok=%s"
                 % (loop["host"].get("loadavg1"), loop["host"].get("mem_available_mib"),
                    (prov.get("host_gate") or {}).get("ok")))
    return "\n".join(lines) + "\n"


if __name__ == "__main__":
    sys.exit(main())
