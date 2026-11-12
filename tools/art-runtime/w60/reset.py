#!/usr/bin/env python3
"""W60: reset-cycle repro. launch, then N acos_reset cycles, recording each."""
import json, os, socket, subprocess, sys, threading, time
from pathlib import Path

REPO = Path("/home/Qui/Projects/acos")
APK = REPO / "fixtures/build/apps/counter.apk"
ACOS = Path(os.environ.get("W60_ACOS", str(REPO / "target/release/acos")))
OUT = Path(__file__).resolve().parent / "resets"
OUT.mkdir(parents=True, exist_ok=True)

def utc(): return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
def read(p):
    try: return Path(p).read_text()
    except OSError: return None
def sf(pid):
    s = read("/proc/%d/stat" % pid)
    if not s: return None
    r = s.rfind(')'); f = s[r+2:].split()
    return {"pid":pid,"comm":s[s.find('(')+1:r],"ppid":int(f[1]),"pgrp":int(f[2]),
            "cgroup":(read("/proc/%d/cgroup"%pid) or "").strip()}
def descendants(root):
    allp = {int(n): sf(int(n)) for n in os.listdir("/proc") if n.isdigit()}
    allp = {p:s for p,s in allp.items() if s}
    out={root}; ch=True
    while ch:
        ch=False
        for p,s in allp.items():
            if p not in out and s["ppid"] in out: out.add(p); ch=True
    return {p:allp[p] for p in out if p!=root}

class Client:
    def __init__(self, path, timeout=180.0):
        self.sock=socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(timeout); self.sock.connect(path)
        self.f=self.sock.makefile("rwb"); self.n=0
    def call(self, tool, args):
        self.n+=1; t0=time.time()
        self.f.write((json.dumps({"id":self.n,"tool":tool,"args":args},separators=(",",":"))+"\n").encode())
        self.f.flush(); line=self.f.readline(); dt=time.time()-t0
        if not line: raise ConnectionError("closed during %s"%tool)
        return {"tool":tool,"t0":t0,"wall_ms":dt*1000,"raw":json.loads(line)}
    def close(self):
        try:self.f.close()
        finally:self.sock.close()

def run(rep, cycles):
    tag="rep%02d"%rep; scratch=OUT/tag
    scratch.mkdir(parents=True, exist_ok=True)
    serve=None; client=None; mon=None
    doc={"rep":rep,"utc":utc(),"cycles":[],"ok":False}
    try:
        env=dict(os.environ, ACOS_RUNTIME_DIR=str(scratch/"runtime"),
                 ACOS_CACHE_DIR=str(scratch/"cache"),
                 ACOS_SERVE_ARTIFACTS=str(scratch/"sessions"),
                 ACOS_KILLWATCH_LOG=str(OUT/("%s.kill.log"%tag)),
                 LD_PRELOAD=str(REPO/".cache/w60/killwatch.so"))
        if os.environ.get("W60_ART_BIN"): env["ACOS_ART_BIN"]=os.environ["W60_ART_BIN"]
        sock=str(scratch/"serve.sock")
        so=open(scratch/"serve.stdout","w"); se=open(scratch/"serve.log","w")
        serve=subprocess.Popen([str(ACOS),"serve","--socket",sock,"--engine","art",
                                "--idle-timeout","900","--max-sessions","8"],
                               cwd=str(REPO),env=env,stdin=subprocess.DEVNULL,
                               stdout=so,stderr=se,start_new_session=True)
        doc["serve_pid"]=serve.pid
        deadline=time.time()+30
        while time.time()<deadline:
            if Path(sock).exists(): break
            if serve.poll() is not None: raise RuntimeError("serve died rc=%s"%serve.returncode)
            time.sleep(0.02)
        client=Client(sock); client.call("ping",{})
        base={"request_id":"w60-reset","session_id":"w60_%s"%tag}
        r=client.call("acos_launch",{"base":base,"apk":str(APK)})
        env0=r["raw"].get("envelope") or {}
        if not env0.get("ok"): raise RuntimeError("launch failed: %s"%(env0.get("error"),))
        watch={"stop":False,"events":[]}
        def monitor():
            seen={}
            while not watch["stop"]:
                now=time.time()
                try: cur=descendants(serve.pid)
                except Exception: cur={}
                for p,s in cur.items():
                    if p not in seen: s["first_seen"]=now; seen[p]=s
                for p in list(seen):
                    if p not in cur:
                        ev=dict(seen[p]); ev["died_at"]=now
                        ev["lifetime_ms"]=round((now-ev["first_seen"])*1000,1)
                        watch["events"].append(ev); del seen[p]
                time.sleep(0.01)
        mon=threading.Thread(target=monitor,daemon=True); mon.start()
        for i in range(cycles):
            before=descendants(serve.pid)
            r=client.call("acos_reset",{"base":base})
            after=descendants(serve.pid)
            env=r["raw"].get("envelope") or {}
            val=env.get("result") or {}
            rec={"i":i,"t0":r["t0"],"wall_ms":round(r["wall_ms"],1),"ok":bool(env.get("ok")),
                 "error":env.get("error"),"frame_seq":val.get("frame_seq"),
                 "previous_frame_seq":val.get("previous_frame_seq"),
                 "new_pids":{str(p):{"comm":s["comm"],"ppid":s["ppid"],"pgrp":s["pgrp"]}
                             for p,s in after.items() if p not in before}}
            doc["cycles"].append(rec)
            print("  cycle %d ok=%s %s"%(i,rec["ok"],(rec["error"] or {}).get("code") if isinstance(rec["error"],dict) else ""),flush=True)
            if not rec["ok"]: break
        doc["ok"]=all(c["ok"] for c in doc["cycles"]) and len(doc["cycles"])==cycles
        time.sleep(0.3); watch["stop"]=True; mon.join(timeout=5); doc["deaths"]=watch["events"]
    except Exception as exc:
        doc["error"]="%s: %s"%(type(exc).__name__,exc)
    finally:
        if client is not None:
            try: client.call("acos_destroy",{"base":{"request_id":"d","session_id":"w60_%s"%tag}})
            except Exception: pass
            try: client.close()
            except Exception: pass
        if serve is not None and serve.poll() is None:
            serve.terminate()
            try: serve.wait(timeout=20)
            except subprocess.TimeoutExpired: serve.kill(); serve.wait(timeout=10)
        doc["utc_finished"]=utc()
        (OUT/("%s.json"%tag)).write_text(json.dumps(doc,indent=2))
        print("rep %d ok=%s cycles=%d %s"%(rep,doc.get("ok"),len(doc.get("cycles") or []),doc.get("error") or ""),flush=True)
    return doc

if __name__=="__main__":
    lo,hi=int(sys.argv[1]),int(sys.argv[2]); cyc=int(sys.argv[3]) if len(sys.argv)>3 else 20
    fails=0
    for rep in range(lo,hi):
        d=run(rep,cyc)
        if not d.get("ok"): fails+=1
    print("FAILS %d / %d reps"%(fails,hi-lo))
