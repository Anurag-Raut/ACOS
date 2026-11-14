#!/usr/bin/env python3
"""S3a/S3b reproducer: repeated P5 verified loops, evidence kept on failure.

The H6/P5 loop already runs the exact task that S3 found the two reset defects
in (`launch -> inspect -> tap -> observe -> assert -> reset`). Two things make
it unsuitable as a *failure* reproducer at scale:

* `acos_bench.py` deletes the loop scratch directory in `finally`, so the
  per-session `session-exit.json` (the guard line, the guest's own crash line,
  its stdout/stderr tail and the C1/layer evidence) is gone by the time a
  failure is visible in the merged JSON; and
* every task writes a ~9.5 MiB PNG twice (the caller's screenshot and the
  engine's frame mirror), so a 600-task run costs ~12 GiB and evicts the page
  cache the measurement wants.

This harness runs the unmodified P5 loop in short reps (default: one measured
task per session, so a failing reset is the **last** action of that session and
its evidence cannot be overwritten by the next task), records the same JSON,
and after every rep:

* copies `session-exit.json` / `live-session.json` / `session-timing.json` from
  every session of a rep that has any failed task;
* deletes the PNGs and the large traces of successful reps (a failure rep keeps
  its traces), so the scratch stays small and the disk is never the variable.

It is a harness, not product code. Nothing in `crates/**` is changed.

Usage (always under the ART lock + guard):

  tools/art-runtime/run-exclusive.sh --task S3AB -- \
    tools/guarded.sh --mem 24G --time 590 -- \
    python3 tools/bench/s3ab_repro.py --acos target/release/acos \
      --out .cache/s3ab/repro-n20-on.json \
      --scratch .cache/s3ab/repro-n20-on \
      --n 20 --tasks 1 --reps 30 --animations on

`--tasks` is the number of **measured** tasks per session per rep; a rep is
`n * tasks` tasks. `--reps` repeats whole serve cohorts (a fresh `acos serve`
each time, like S3's per-cell reps).
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import os
import shutil
import signal
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools"))
sys.path.insert(0, str(ROOT / "tools/art-runtime"))
sys.path.insert(0, str(ROOT / "results/art-runtime/evidence/P5"))

import benchlib  # noqa: E402

_spec = importlib.util.spec_from_file_location(
    "p5_loop_s3ab", ROOT / "results/art-runtime/evidence/P5/p5_loop.py"
)
p5 = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(p5)  # type: ignore[union-attr]


_TERMINATING = False


def _on_term(signum, _frame):
    """Make SIGTERM/SIGINT unwind through p5's `finally` (serve.close()).

    `guarded.sh` sends SIGTERM at its wall clock. Without this, Python dies
    without running `finally`, and `acos serve` (started in its own session by
    `d2_warm.Serve`) survives as an orphan holding 20–40 sessions. The outer
    cleanup below is the second line of defence.
    """
    global _TERMINATING
    _TERMINATING = True
    raise SystemExit(128 + signum)


def cleanup_scratch(scratch_root: Path) -> int:
    """Kill every process whose command line names our scratch (serve + trees)."""
    killed = 0
    own = os.getpid()
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit():
            continue
        pid = int(entry.name)
        if pid == own:
            continue
        try:
            cmd = (entry / "cmdline").read_bytes().replace(b"\0", b" ").decode(
                "utf-8", "replace"
            )
        except OSError:
            continue
        if str(scratch_root) not in cmd:
            continue
        try:
            os.kill(pid, signal.SIGTERM)
        except OSError:
            continue
        killed += 1
    if killed:
        time.sleep(0.5)
        for entry in Path("/proc").iterdir():
            if not entry.name.isdigit():
                continue
            pid = int(entry.name)
            if pid == own:
                continue
            try:
                cmd = (entry / "cmdline").read_bytes().replace(b"\0", b" ").decode(
                    "utf-8", "replace"
                )
            except OSError:
                continue
            if str(scratch_root) in cmd:
                try:
                    os.kill(pid, signal.SIGKILL)
                except OSError:
                    pass
    return killed


def with_arm(tool: str, args: dict, animations: str) -> dict:
    """Attach the animation arm to the launch (reset inherits the session arm)."""
    if tool == "acos_launch" and "animations" not in args:
        args = dict(args)
        args["animations"] = animations
    return args


def unlink_pngs(root: Path) -> int:
    """Delete every PNG under `root` (caller screenshots and engine mirrors)."""
    removed = 0
    for path in root.rglob("*.png"):
        try:
            path.unlink()
            removed += 1
        except OSError:
            pass
    return removed


def collect_relaunch_retries(session_root: Path, dest: Path) -> list[str]:
    """Copy every engine-recorded bounded relaunch retry (S3b) aside."""
    copied: list[str] = []
    if not session_root.is_dir():
        return copied
    for record in session_root.glob("*/relaunch-retry.json"):
        target = dest / record.parent.name / "relaunch-retry.json"
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(record, target)
        copied.append(str(target))
        evidence = record.parent / "relaunch-retry-1"
        if evidence.is_dir():
            shutil.copytree(evidence, target.parent / "relaunch-retry-1", dirs_exist_ok=True)
            # One list entry per retry **event** (record + its preserved evidence),
            # so `relaunch_retries` counts events and never double-counts the pair.
            copied[-1] = f"{target} (+ {target.parent / 'relaunch-retry-1'})"
    return copied


def copy_evidence(session_root: Path, dest: Path) -> list[str]:
    """Copy the small, diagnostic files of every session into `dest`."""
    copied: list[str] = []
    if not session_root.is_dir():
        return copied
    for session in sorted(session_root.iterdir()):
        if not session.is_dir():
            continue
        frames = session / "art-frames"
        if not frames.is_dir():
            continue
        target = dest / session.name
        target.mkdir(parents=True, exist_ok=True)
        for name in (
            "session-exit.json",
            "live-session.json",
            "session-timing.json",
            "native-crash.json",
            "native-crash.txt",
        ):
            src = frames / name
            if src.is_file():
                shutil.copy2(src, target / name)
                copied.append(str(target / name))
        # The launcher's own session contract (paths, guest argv) is tiny and
        # explains which APK/run root the failing guest used.
        for name in ("session.json", "argv"):
            src = frames / name
            if src.is_file():
                shutil.copy2(src, target / name)
                copied.append(str(target / name))
    return copied


def run_rep(args, rep: int, scratch: Path) -> dict:
    if scratch.exists():
        shutil.rmtree(scratch)
    scratch.mkdir(parents=True)
    out_json = scratch / "loop.json"

    original_call = p5.TaskRunner.call

    def call_with_arm(self, step, tool, args_, pngs_before):
        return original_call(self, step, tool,
                             with_arm(tool, args_, args.animations), pngs_before)

    p5.TaskRunner.call = call_with_arm
    argv = [
        "--acos", str(Path(args.acos).resolve()),
        "--out", str(out_json),
        "--scratch", str(scratch),
        "--n", str(args.n),
        "--loops", str(args.tasks + 1),
        "--warmup", "1",
    ]
    t0 = time.time()
    rc = 0
    try:
        rc = p5.main(argv)
    except SystemExit as exc:  # p5 calls SystemExit on its own error paths
        if _TERMINATING:
            raise
        rc = int(exc.code or 0)
    finally:
        p5.TaskRunner.call = original_call
    wall = round(time.time() - t0, 3)
    if not out_json.is_file():
        return {
            "rep": rep,
            "ok": False,
            "harness_error": "p5 wrote no loop.json (rc=%s, wall_s=%s)" % (rc, wall),
            "wall_s": wall,
            "measured": 0,
            "verified": 0,
            "false_pass": 0,
            "failures": [],
            "scratch": str(scratch),
        }
    doc = json.loads(out_json.read_text())
    failures = [
        {
            "agent": task.get("agent"),
            "index": task.get("index"),
            "checks": task.get("checks"),
            "steps": {
                step: {
                    "error": row.get("error"),
                    "error_message": row.get("error_message"),
                    "ms": row.get("ms"),
                }
                for step, row in (task.get("steps") or {}).items()
                if row.get("error")
            },
        }
        for task in doc.get("tasks") or []
        if task.get("verdict") == "fail"
    ]
    measured = int(doc.get("measured_tasks") or 0)
    verified = int(doc.get("verified") or 0)
    false_pass = int(doc.get("false_pass") or 0)
    retries = collect_relaunch_retries(
        scratch / "sessions", scratch / "evidence" / ("rep-%03d" % rep)
    )
    return {
        "rep": rep,
        "ok": bool(measured and verified == measured and false_pass == 0),
        "wall_s": wall,
        "measured": measured,
        "verified": verified,
        "false_pass": false_pass,
        "failures": failures,
        "relaunch_retries": retries,
        "loop": doc.get("loop_total"),
        "host": doc.get("host_before"),
        "scratch": str(scratch),
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--acos", required=True)
    parser.add_argument("--out", required=True)
    parser.add_argument("--scratch", required=True)
    parser.add_argument("--n", type=int, default=20)
    parser.add_argument("--tasks", type=int, default=1,
                        help="measured tasks per session per rep (default 1)")
    parser.add_argument("--reps", type=int, default=30)
    parser.add_argument("--animations", choices=("on", "off"), default="on")
    parser.add_argument("--evidence-dir", default="",
                        help="where failing reps' session evidence is copied")
    parser.add_argument("--stop-on-fail", action="store_true",
                        help="stop after the first rep with a failure")
    parser.add_argument("--keep-success-scratch", action="store_true")
    args = parser.parse_args(argv)
    signal.signal(signal.SIGTERM, _on_term)
    signal.signal(signal.SIGINT, _on_term)

    # Resolve every path before p5 hands it to `acos serve`/`Serve`, whose
    # subprocesses run with cwd=REPO (the main checkout): a relative scratch
    # would put the socket in the main tree and never be found here.
    scratch_root = Path(args.scratch).resolve()
    scratch_root.mkdir(parents=True, exist_ok=True)
    evidence_root = (
        Path(args.evidence_dir).resolve() if args.evidence_dir
        else scratch_root / "evidence"
    )
    evidence_root.mkdir(parents=True, exist_ok=True)
    out = Path(args.out).resolve()
    started = benchlib.utc_now()
    host = benchlib.read_meminfo()
    host.update(benchlib.read_loadavg())

    total_measured = 0
    total_verified = 0
    total_false_pass = 0
    total_retries = 0
    failed_reps: list[int] = []
    evidence: list[str] = []
    reps: list[dict] = []

    try:
        for rep in range(1, args.reps + 1):
            scratch = scratch_root / ("rep-%03d" % rep)
            row = run_rep(args, rep, scratch)
            reps.append(row)
            total_measured += row["measured"]
            total_verified += row["verified"]
            total_false_pass += row["false_pass"]
            total_retries += len(row.get("relaunch_retries") or [])
            status = "ok" if row["ok"] else "FAIL"
            print(
                "s3ab_repro: rep %d/%d %s measured=%d verified=%d false_pass=%d wall=%.1fs"
                % (rep, args.reps, status, row["measured"], row["verified"],
                   row["false_pass"], row["wall_s"]),
                flush=True,
            )
            if not row["ok"]:
                failed_reps.append(rep)
                dest = evidence_root / ("rep-%03d" % rep)
                copied = copy_evidence(scratch / "sessions", dest)
                # Keep the failing rep's traces and JSON; they name the phase split
                # and the request that failed.
                for name in ("dm6.trace", "p3.trace", "dm5.trace", "loop.json",
                             "serve.log", "serve.stdout"):
                    src = scratch / name
                    if src.is_file():
                        shutil.copy2(src, dest / name)
                        copied.append(str(dest / name))
                evidence.extend(copied)
                print("s3ab_repro: evidence -> %s (%d files)" % (dest, len(copied)),
                      flush=True)
                if args.stop_on_fail:
                    break
            # Reclaim the frames immediately; they are the only large artefact.
            unlink_pngs(scratch)
            if row["ok"] and not args.keep_success_scratch:
                # Keep the small loop.json only; a success has no evidence to
                # inspect and the next rep gets a fresh serve/scratch.
                for name in ("dm6.trace", "p3.trace", "dm5.trace"):
                    try:
                        (scratch / name).unlink()
                    except OSError:
                        pass
                shutil.rmtree(scratch / "sessions", ignore_errors=True)

    finally:
        cleanup_scratch(scratch_root)

    summary = {
        "schema": "acos-s3ab-repro/1",
        "started_utc": started,
        "finished_utc": benchlib.utc_now(),
        "acos": str(Path(args.acos).resolve()),
        "binary_sha256": benchlib.binary_provenance(str(Path(args.acos).resolve())).get("sha256"),
        "n": args.n,
        "tasks_per_session": args.tasks,
        "reps": len(reps),
        "animations": args.animations,
        "measured": total_measured,
        "verified": total_verified,
        "false_pass": total_false_pass,
        "relaunch_retries": total_retries,
        "failed_reps": failed_reps,
        "evidence": evidence,
        "host_before": host,
        "host_after": {**benchlib.read_meminfo(), **benchlib.read_loadavg()},
        "reps_detail": reps,
    }
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(summary, indent=1) + "\n")
    print(
        "s3ab_repro: %s measured=%d verified=%d false_pass=%d relaunch_retries=%d failed_reps=%s"
        % ("DONE" if total_verified == total_measured and total_false_pass == 0
           else "DEGRADED",
           total_measured, total_verified, total_false_pass, total_retries, failed_reps),
        flush=True,
    )
    return 0 if total_measured and total_verified == total_measured else 1


if __name__ == "__main__":
    raise SystemExit(main())
