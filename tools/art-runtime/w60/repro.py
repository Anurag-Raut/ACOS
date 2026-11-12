#!/usr/bin/env python3
"""W60: reproduce GUEST_EXITED at launch with full lineage + kill attribution."""
import json, os, signal, socket, subprocess, sys, threading, time
from pathlib import Path

REPO = Path("/home/Qui/Projects/acos")
APK = REPO / "fixtures/build/apps/counter.apk"
ACOS = Path(os.environ.get("W60_ACOS", str(REPO / "target/release/acos")))
OUT = Path(__file__).resolve().parent / "reps"
OUT.mkdir(parents=True, exist_ok=True)

def utc():
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())

def read(path):
    try: return Path(path).read_text()
    except OSError: return None

def stat_fields(pid):
    s = read("/proc/%d/stat" % pid)
    if not s: return None
    r = s.rfind(')')
    f = s[r+2:].split()
    return {"pid": pid, "comm": s[s.find('(')+1:r], "state": f[0],
            "ppid": int(f[1]), "pgrp": int(f[2]), "session": int(f[3]),
            "starttime": f[19], "cgroup": (read("/proc/%d/cgroup" % pid) or "").strip()}

def descendants(root):
    pids = {}
    for name in os.listdir("/proc"):
        if not name.isdigit(): continue
        st = stat_fields(int(name))
        if st: pids[st["pid"]] = st
    out, changed = set(), True
    out.add(root)
    while changed:
        changed = False
        for pid, st in pids.items():
            if pid not in out and st["ppid"] in out:
                out.add(pid); changed = True
    return {p: pids[p] for p in out if p != root}

class Serve:
    def __init__(self, scratch, killlog):
        self.scratch = Path(scratch)
        self.scratch.mkdir(parents=True, exist_ok=True)
        self.sock = str(self.scratch / "serve.sock")
        self.stdout = open(self.scratch / "serve.stdout", "w")
        self.stderr = open(self.scratch / "serve.log", "w")
        env = dict(os.environ,
                   ACOS_RUNTIME_DIR=str(self.scratch / "runtime"),
                   ACOS_CACHE_DIR=str(self.scratch / "cache"),
                   ACOS_SERVE_ARTIFACTS=str(self.scratch / "sessions"),
                   ACOS_KILLWATCH_LOG=str(killlog),
                   LD_PRELOAD=str(REPO/".cache/w60/killwatch.so"))
        if os.environ.get("W60_ART_BIN"):
            env["ACOS_ART_BIN"] = os.environ["W60_ART_BIN"]
        self.argv = [str(ACOS), "serve", "--socket", self.sock, "--engine", "art",
                     "--idle-timeout", "900", "--max-sessions", "8"]
        self.proc = subprocess.Popen(self.argv, cwd=str(REPO), env=env,
                                     stdin=subprocess.DEVNULL, stdout=self.stdout,
                                     stderr=self.stderr, start_new_session=True)
        deadline = time.time() + 30
        while time.time() < deadline:
            if Path(self.sock).exists(): return
            if self.proc.poll() is not None:
                raise RuntimeError("serve died rc=%s" % self.proc.returncode)
            time.sleep(0.02)
        raise RuntimeError("serve never bound")

    def close(self):
        if self.proc.poll() is None:
            self.proc.terminate()
            try: self.proc.wait(timeout=20)
            except subprocess.TimeoutExpired:
                self.proc.kill(); self.proc.wait(timeout=10)
        for fh in (self.stdout, self.stderr):
            try: fh.close()
            except OSError: pass
        return self.proc.returncode

class Client:
    def __init__(self, path, timeout=180.0):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(timeout); self.sock.connect(path)
        self.f = self.sock.makefile("rwb"); self.n = 0
    def call(self, tool, args):
        self.n += 1
        t0 = time.time()
        self.f.write((json.dumps({"id": self.n, "tool": tool, "args": args},
                                 separators=(",", ":")) + "\n").encode())
        self.f.flush()
        line = self.f.readline()
        dt = time.time() - t0
        if not line: raise ConnectionError("closed during %s" % tool)
        return {"tool": tool, "t0": t0, "wall_ms": dt*1000, "raw": json.loads(line)}
    def close(self):
        try: self.f.close()
        finally: self.sock.close()

def run_rep(rep, n=5):
    tag = "rep%02d" % rep
    scratch = OUT / tag
    killlog = OUT / ("%s.kill.log" % tag)
    doc = {"rep": rep, "utc": utc(), "sessions": [], "ok": False}
    serve = None; client = None; mon = None
    try:
        serve = Serve(scratch, killlog)
        doc["serve_pid"] = serve.proc.pid
        doc["serve_lineage"] = descendants(serve.proc.pid)
        client = Client(serve.sock)
        client.call("ping", {})
        time.sleep(float(os.environ.get("W60_BARE_SETTLE", "10")))
        watch = {"stop": False, "pids": {}, "events": []}
        def monitor():
            seen = {}
            while not watch["stop"]:
                now = time.time()
                try: cur = descendants(serve.proc.pid)
                except Exception: cur = {}
                for pid, st in cur.items():
                    if pid not in seen:
                        st["first_seen"] = now
                        seen[pid] = st
                for pid in list(seen):
                    if pid not in cur:
                        ev = dict(seen[pid]); ev["died_at"] = now
                        ev["lifetime_ms"] = round((now - ev["first_seen"])*1000, 1)
                        ev["session_artifacts"] = None
                        try:
                            ap = Path(ev["cgroup"].split("::/")[-1])
                        except Exception: pass
                        watch["events"].append(ev)
                        del seen[pid]
                time.sleep(0.01)
        mon = threading.Thread(target=monitor, daemon=True); mon.start()
        for i in range(n):
            before = descendants(serve.proc.pid)
            sid = "w60_%s_%d" % (tag, i)
            base = {"request_id": "w60-%s-%d" % (tag, i), "session_id": sid}
            t0 = time.time()
            r = client.call("acos_launch", {"base": base, "apk": str(APK)})
            new = descendants(serve.proc.pid)
            env = r["raw"].get("envelope") or {}
            val = env.get("result") or {}
            fr = val.get("frame") if isinstance(val.get("frame"), dict) else {}
            rec = {"i": i, "session_id": sid, "t0": t0, "wall_ms": round(r["wall_ms"], 1),
                   "t1": time.time(), "ok": bool(env.get("ok")),
                   "error": env.get("error"),
                   "frame_seq": fr.get("frame_seq"),
                   "new_pids": {str(p): {"comm": s["comm"], "ppid": s["ppid"],
                                         "pgrp": s["pgrp"], "session": s["session"],
                                         "cgroup": s["cgroup"]}
                                for p, s in new.items() if p not in before}}
            doc["sessions"].append(rec)
            if not rec["ok"]:
                break
        doc["ok"] = all(s["ok"] for s in doc["sessions"]) and len(doc["sessions"]) == n
        time.sleep(0.5)
        watch["stop"] = True; mon.join(timeout=5)
        doc["deaths"] = watch["events"]
    except Exception as exc:
        doc["error"] = "%s: %s" % (type(exc).__name__, exc)
    finally:
        if client is not None:
            for s in doc.get("sessions") or []:
                try: client.call("acos_destroy", {"base": {"request_id": "d", "session_id": s["session_id"]}})
                except Exception: pass
            try: client.close()
            except Exception: pass
        if serve is not None:
            doc["serve_rc"] = serve.close()
        time.sleep(0.3)
        doc["utc_finished"] = utc()
        (OUT / ("%s.json" % tag)).write_text(json.dumps(doc, indent=2))
        print("rep %d ok=%s sessions=%d deaths=%d %s" % (
            rep, doc.get("ok"), len(doc.get("sessions") or []),
            len(doc.get("deaths") or []), doc.get("error") or ""), flush=True)
    return doc

if __name__ == "__main__":
    lo, hi = int(sys.argv[1]), int(sys.argv[2])
    fails = 0
    for rep in range(lo, hi):
        d = run_rep(rep)
        if not d.get("ok"): fails += 1
    print("FAILS %d / %d" % (fails, hi-lo))
