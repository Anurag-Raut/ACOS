#!/usr/bin/env python3
"""D-M6 per-phase reset diagnosis harness (scratch, not committed to crates).

One `acos serve` per rep; N `acos_reset` calls; parses the env-gated traces:
  DM6_RESET_TRACE      (acos-cli)  spawn/ready/reset host-side events
  DM6_ART_STAGE_TRACE  (acos-art)  stage markers per launcher pid
  ACOS_ART_INPUT_TRACE  (acos-art)  guest MARK/TRACE lines, elapsed_us from spawn
"""
import argparse, json, os, socket, statistics, subprocess, sys, time
from pathlib import Path

REPO = Path("/home/Qui/Projects/acos")
APK = REPO / "fixtures/build/apps/counter.apk"


def utc():
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


class Client:
    def __init__(self, path, timeout=180.0):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(timeout)
        self.sock.connect(path)
        self.f = self.sock.makefile("rwb")
        self.n = 0

    def call(self, tool, args):
        self.n += 1
        t0 = time.time()
        self.f.write((json.dumps({"id": self.n, "tool": tool, "args": args},
                                 separators=(",", ":")) + "\n").encode())
        self.f.flush()
        line = self.f.readline()
        dt = (time.time() - t0) * 1000
        if not line:
            raise ConnectionError("closed during %s" % tool)
        return {"tool": tool, "wall_ms": dt, "raw": json.loads(line)}

    def close(self):
        try:
            self.f.close()
        finally:
            self.sock.close()


def read_lines(path):
    try:
        return Path(path).read_text(errors="replace").splitlines()
    except OSError:
        return []


def host_gate(max_load=6.0, min_avail_gib=20.0):
    load = float(Path("/proc/loadavg").read_text().split()[0])
    avail = 0.0
    for line in Path("/proc/meminfo").read_text().splitlines():
        if line.startswith("MemAvailable:"):
            avail = int(line.split()[1]) / 1024 / 1024
    return load, avail, (load <= max_load and avail >= min_avail_gib)


def run_rep(rep, resets, acos, outdir, tap=True):
    scratch = outdir / ("rep%02d" % rep)
    scratch.mkdir(parents=True, exist_ok=True)
    trace_stage = scratch / "stage.log"
    trace_reset = scratch / "reset.log"
    trace_input = scratch / "input.log"
    serve = None
    client = None
    doc = {"rep": rep, "utc": utc(), "samples": [], "ok": True}
    doc["host_before"] = {"load1": open("/proc/loadavg").read().split()[0],
                          "mem_available_gib": host_gate()[1]}
    try:
        env = dict(os.environ,
                   ACOS_RUNTIME_DIR=str(scratch / "runtime"),
                   ACOS_CACHE_DIR=str(scratch / "cache"),
                   ACOS_SERVE_ARTIFACTS=str(scratch / "sessions"),
                   ACOS_ART_BIN=str(Path(acos).parent / "acos-art"),
                   ACOS_ART_STAGE_TIMING="1",
                   DM6_ART_STAGE_TRACE=str(trace_stage),
                   DM6_RESET_TRACE=str(trace_reset),
                   ACOS_ART_INPUT_TRACE=str(trace_input),
                   DM5_TRACE=str(scratch / "dm5.log"))
        sock = str(scratch / "serve.sock")
        so = open(scratch / "serve.stdout", "w")
        se = open(scratch / "serve.log", "w")
        serve = subprocess.Popen([acos, "serve", "--socket", sock, "--engine", "art",
                                  "--idle-timeout", "900", "--max-sessions", "8"],
                                 cwd=str(REPO), env=env, stdin=subprocess.DEVNULL,
                                 stdout=so, stderr=se, start_new_session=True)
        deadline = time.time() + 30
        while time.time() < deadline:
            if Path(sock).exists():
                break
            if serve.poll() is not None:
                raise RuntimeError("serve died rc=%s" % serve.returncode)
            time.sleep(0.02)
        client = Client(sock)
        client.call("ping", {})
        base = {"request_id": "dm6", "session_id": "dm6_%02d" % rep}
        t0 = time.time()
        r = client.call("acos_launch", {"base": base, "apk": str(APK)})
        env0 = r["raw"].get("envelope") or {}
        if not env0.get("ok"):
            raise RuntimeError("launch failed: %s" % (env0.get("error"),))
        doc["launch_ms"] = round((time.time() - t0) * 1000, 1)
        for i in range(resets):
            pre = None
            if tap:
                pre = client.call("acos_tap", {"base": base, "id": "inc"})
            t0 = time.time()
            r = client.call("acos_reset", {"base": base})
            wall = (time.time() - t0) * 1000
            e = r["raw"].get("envelope") or {}
            val = e.get("result") or {}
            timing = val.get("timing") or {}
            verify = client.call("acos_observe_tree", {"base": base, "max_nodes": 2000})
            vtree = (verify["raw"].get("envelope") or {}).get("result") or {}
            doc["samples"].append({
                "i": i, "wall_ms": round(wall, 3), "server_total_ms": timing.get("total_ms"),
                "ok": bool(e.get("ok")),
                "previous_frame_seq": val.get("previous_frame_seq"),
                "frame_seq": val.get("frame_seq"),
                "tap_ok": bool((pre or {}).get("raw", {}).get("envelope", {}).get("ok")) if tap else None,
                "after_tree_ms": round(verify["wall_ms"], 3),
                "after_frame_seq": (vtree.get("frame") or {}).get("frame_seq")
                if isinstance(vtree.get("frame"), dict) else None,
                "t_unix_ms": int(time.time() * 1000),
            })
            if not e.get("ok"):
                doc["ok"] = False
                break
        doc["samples_ok"] = sum(1 for s in doc["samples"] if s["ok"])
        # give the drain threads a moment to flush the trace files
        time.sleep(0.5)
    except Exception as exc:
        doc["ok"] = False
        doc["error"] = "%s: %s" % (type(exc).__name__, exc)
    finally:
        if client is not None:
            try:
                client.close()
            except Exception:
                pass
        if serve is not None and serve.poll() is None:
            serve.terminate()
            try:
                serve.wait(timeout=20)
            except subprocess.TimeoutExpired:
                serve.kill()
                serve.wait(timeout=10)
        doc["host_after"] = {"load1": open("/proc/loadavg").read().split()[0],
                             "mem_available_gib": host_gate()[1]}
        doc["utc_finished"] = utc()
        (scratch / "rep.json").write_text(json.dumps(doc, indent=2))
        print("rep %d: %d/%d resets ok %s" % (rep, doc.get("samples_ok", 0), len(doc["samples"]),
                                             doc.get("error") or ""), flush=True)
    return doc


# ---------------------------------------------------------------- parsing
def parse_stage(path):
    """pid -> {name: (exec_ms, unix_ms)}, plus launch order."""
    out = {}
    for line in read_lines(path):
        if not line.startswith("DM6STAGE "):
            continue
        parts = dict(p.split("=", 1) for p in line.split()[1:])
        try:
            pid = int(parts["pid"])
            out.setdefault(pid, {})[parts["name"]] = (
                int(parts["exec"].rstrip("ms")), int(parts["unix_ms"]))
        except (KeyError, ValueError):
            continue
    return out


def parse_reset_trace(path):
    events = []
    for line in read_lines(path):
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            events.append(json.loads(line))
        except ValueError:
            continue
    return events


def parse_guest(path):
    """pid -> [(elapsed_us, text)] in file order."""
    out = {}
    for line in read_lines(path):
        # "{:>9}us pid={} {} {}"  ->  12345us pid=123 GUEST ACOSLAUNCH MARK ...
        try:
            us_part, rest = line.split("us pid=", 1)
            pid_part, tag, text = rest.split(" ", 2)
            pid = int(pid_part)
            us = int(us_part.strip())
        except ValueError:
            continue
        out.setdefault(pid, []).append((us, text))
    return out


MARKERS = {
    "java": "ACOSLAUNCH MARK resume-budget",
    "art_ready": "ACOSLAUNCH MARK android-runtime ready",
    "looper": "ACOSLAUNCH MARK binder-looper started",
    "bind_pushed": "ACOSLAUNCH MARK bind-request-pushed",
    "attach": "ACOSLAUNCH MARK attach-returned",
    "app_created": "ACOSLAUNCH MARK application-created",
    "txn": "ACOSLAUNCH MARK schedule-transaction-delivered",
    "resumed": "ACOSLAUNCH MARK activity-resumed",
    "probe": "ACOSLAUNCH MARK live-probe-started",
}


def guest_marks(lines):
    marks = {}
    trace_h = []
    for us, text in lines:
        for name, prefix in MARKERS.items():
            if text.startswith(prefix) and name not in marks:
                marks[name] = us
        if text.startswith("ACOSLAUNCH TRACE") and "$H)" in text:
            trace_h.append((us, text))
    # Looper messages: ">>>>> Dispatching to Handler (android.app.ActivityThread$H) …: 110"
    # and the matching "<<<<< Finished to Handler (…)" line, which carries no
    # message number: pair it with the most recent dispatch.
    pending = None
    for us, text in trace_h:
        if ">>>>> Dispatching" in text:
            what = text.rsplit(":", 1)[-1].strip()
            if what in ("110", "159"):
                pending = what
                key = "bind_dispatch" if what == "110" else "launch_dispatch"
                marks.setdefault(key, us)
        elif "<<<<< Finished" in text and pending is not None:
            key = "bind_finish" if pending == "110" else "launch_finish"
            marks.setdefault(key, us)
            pending = None
    return marks


def parse_dm5(path):
    """[(event, t_start_us, dur_us)] from D-M5's DM5_TRACE sink."""
    out = []
    for line in read_lines(path):
        if not line.startswith("DM5TRACE"):
            continue
        parts = dict(p.split("=", 1) for p in line.split()[2:])
        try:
            t_us = int(parts.get("t_us", "0"))
            dur = int(parts.get("dur_us", "0"))
        except ValueError:
            continue
        event = line.split("ev=", 1)[1].split()[0]
        out.append((event, t_us - dur, dur))
    return out


def per_sample_phases(stage_by_pid, guest_by_pid, events, samples):
    spawns = {}
    readys = {}
    resets = []
    for e in events:
        if e.get("event") == "spawn":
            spawns[e.get("pid")] = e
        elif e.get("event") == "ready":
            readys[e.get("pid")] = e
        elif e.get("event") == "reset":
            resets.append(e)
    rows = []
    for sample, r in zip(samples, resets):
        if not sample["ok"]:
            continue
        pid = r.get("new_pid")
        st = stage_by_pid.get(pid, {})
        sp = spawns.get(pid, {})
        rd = readys.get(pid, {})
        gul = guest_by_pid.get(pid, [])
        gm = guest_marks(gul)
        row = {"i": sample["i"], "pid": pid, "wall_ms": sample["wall_ms"],
               "server_total_ms": sample["server_total_ms"],
               "engine_total_ms": r.get("engine_total_ms"), "raw": {}}
        # host wall-clock alignment: guest elapsed_us base is the spawn Instant,
        # spawn_unix_ms is taken 1-2 instructions earlier.
        base_ms = sp.get("unix_ms", 0)
        def gabs(name):
            if name in gm:
                return base_ms + gm[name] / 1000.0
            return None
        row["spawn_unix_ms"] = base_ms
        row["ready_unix_ms"] = rd.get("unix_ms")
        if "launch-entry" in st:
            row["launcher_start_ms"] = st["launch-entry"][1] - base_ms
        if "launch-entry" in st and "guest-exec" in st:
            row["sandbox_prep_ms"] = st["guest-exec"][0] - st["launch-entry"][0]
        if "guest-exec" in st and "guest-spawned" in st:
            row["guest_spawn_ms"] = st["guest-spawned"][0] - st["guest-exec"][0]
        if "run-dir" in st and "apk-stage" in st:
            row["run_dir_ms"] = st["apk-stage"][0] - st["run-dir"][0]
        row["log_flush_ms"] = r.get("log_flush_ms")
        row["stop_ms"] = r.get("stop_ms")
        row["start_ms"] = r.get("start_ms")
        row["bind_ms"] = r.get("bind_ms")
        row["_g"] = {k: round(v / 1000.0, 3) for k, v in gm.items()}
        rows.append(row)
    return rows


def summarize(values):
    vals = sorted(v for v in values if v is not None)
    if not vals:
        return None
    def pct(p):
        idx = min(len(vals) - 1, max(0, int(round((p / 100.0) * (len(vals) - 1)))))
        return round(vals[idx], 1)
    spread = (pct(95) - round(vals[0], 1)) / pct(50) * 100 if pct(50) else 0
    return {"n": len(vals), "p50": pct(50), "p95": pct(95), "min": round(vals[0], 1),
            "max": round(vals[-1], 1), "spread_pct": round(spread, 1)}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--acos", required=True)
    ap.add_argument("--outdir", required=True)
    ap.add_argument("--reps", type=int, default=2)
    ap.add_argument("--resets", type=int, default=7)
    ap.add_argument("--warmup", type=int, default=1)
    ap.add_argument("--no-tap", action="store_true")
    ap.add_argument("--max-load", type=float, default=6.0)
    ap.add_argument("--analyze", help="analyse an existing outdir instead")
    args = ap.parse_args()
    outdir = Path(args.outdir)
    outdir.mkdir(parents=True, exist_ok=True)
    if not args.analyze:
        load, avail, ok = host_gate(args.max_load)
        print("host load1=%.2f memAvailable=%.1fGiB gate=%s" % (load, avail, ok), flush=True)
        docs = []
        for rep in range(args.reps):
            docs.append(run_rep(rep, args.resets, args.acos, outdir, tap=not args.no_tap))
        (outdir / "run.json").write_text(json.dumps(docs, indent=2))
    docs = json.loads((outdir / "run.json").read_text())
    rows = []
    for rep in sorted(p.name for p in outdir.glob("rep*") if p.is_dir()):
        d = json.loads((outdir / rep / "rep.json").read_text())
        st = parse_stage(outdir / rep / "stage.log")
        gu = parse_guest(outdir / rep / "input.log")
        ev = parse_reset_trace(outdir / rep / "reset.log")
        new_rows = per_sample_phases(st, gu, ev, d["samples"])
        dm5 = parse_dm5(outdir / rep / "dm5.log")
        for row in new_rows:
            # the bind is the last thing before the reset event ends: take the
            # capture_total that starts after the guest was ready
            ready_ms = row.get("ready_unix_ms") or 0
            for ev_name, key in (("art.capture_total", "capture_total_ms"),
                                 ("art.capture_composite", "capture_composite_ms"),
                                 ("art.png_encode", "png_encode_ms"),
                                 ("art.png_write", "png_write_ms")):
                hits = [e for e in dm5 if e[0] == ev_name and e[1] >= ready_ms * 1000]
                if hits:
                    row[key] = round(hits[-1][2] / 1000.0, 1)
        rows.extend(new_rows)
    (outdir / "phases.json").write_text(json.dumps(rows, indent=2))
    # drop the first `warmup` sample of each rep
    keep = [r for r in rows if r["i"] >= args.warmup]
    print("usable samples:", len(keep))
    cols = {
        "wall (client, reset->ready)": lambda r: r["wall_ms"],
        "server total": lambda r: r["server_total_ms"],
        "engine total": lambda r: r["engine_total_ms"],
        "  serve pre+post": lambda r: (r["server_total_ms"] or 0) - (r["engine_total_ms"] or 0),
        "  log flush": lambda r: r["log_flush_ms"],
        "  guest stop": lambda r: r["stop_ms"],
        "  launcher start (spawn->entry)": lambda r: r.get("launcher_start_ms"),
        "  sandbox prep (entry->exec)": lambda r: r.get("sandbox_prep_ms"),
        "  bwrap/dalvik spawn": lambda r: r.get("guest_spawn_ms"),
        "  guest: spawn->java": lambda r: r["_g"].get("java"),
        "  art init (java->android-runtime)": lambda r: (
            r["_g"]["art_ready"] - r["_g"]["java"]) if "art_ready" in r["_g"] and "java" in r["_g"] else None,
        "  fw: art->attach": lambda r: (
            r["_g"]["attach"] - r["_g"]["art_ready"]) if "attach" in r["_g"] and "art_ready" in r["_g"] else None,
        "  bind msg dispatch": lambda r: r["_g"].get("bind_dispatch"),
        "  onCreate (BIND_APPLICATION msg)": lambda r: (
            r["_g"]["bind_finish"] - r["_g"]["bind_dispatch"]) if "bind_finish" in r["_g"] and "bind_dispatch" in r["_g"] else None,
        "  bind->app-created marker": lambda r: (
            r["_g"]["app_created"] - r["_g"]["bind_finish"]) if "app_created" in r["_g"] and "bind_finish" in r["_g"] else None,
        "  activity launch msg (159)": lambda r: (
            r["_g"]["launch_finish"] - r["_g"]["txn"]) if "launch_finish" in r["_g"] and "txn" in r["_g"] else None,
        "  post-resume fixed sleep": lambda r: (
            r["_g"]["resumed"] - r["_g"]["launch_finish"]) if "resumed" in r["_g"] and "launch_finish" in r["_g"] else None,
        "  activity launch total (txn->resumed)": lambda r: (
            r["_g"]["resumed"] - r["_g"]["txn"]) if "resumed" in r["_g"] and "txn" in r["_g"] else None,
        "  capture_total (bind)": lambda r: r.get("capture_total_ms"),
        "    composite": lambda r: r.get("capture_composite_ms"),
        "    png encode": lambda r: r.get("png_encode_ms"),
        "    png write+md5": lambda r: r.get("png_write_ms"),
        "  first traversal+probe": lambda r: (
            r["_g"]["probe"] - r["_g"]["resumed"]) if "probe" in r["_g"] and "resumed" in r["_g"] else None,
        "  ready (resumed->ready line)": lambda r: (
            (r["ready_unix_ms"] - r["spawn_unix_ms"]) / 1.0 - r["_g"]["resumed"]) if r.get("ready_unix_ms") and "resumed" in r["_g"] else None,
        "  frame bind (tree cmd)": lambda r: r["bind_ms"],
    }
    table = {}
    for name, fn in cols.items():
        vals = []
        for r in keep:
            try:
                vals.append(fn(r))
            except (KeyError, TypeError):
                vals.append(None)
        table[name] = summarize(vals)
    for name, st in table.items():
        if st:
            print("%-36s n=%2d p50=%8.1f p95=%8.1f min=%8.1f max=%8.1f spread=%5.1f%%"
                  % (name, st["n"], st["p50"], st["p95"], st["min"], st["max"], st["spread_pct"]))
        else:
            print("%-36s (no data)" % name)
    (outdir / "table.json").write_text(json.dumps(table, indent=2))
    # raw marker times for the record
    for r in keep[:3]:
        print("sample", r["i"], "pid", r["pid"], json.dumps(r["_g"]))


if __name__ == "__main__":
    main()
