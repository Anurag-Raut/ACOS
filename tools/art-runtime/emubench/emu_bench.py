#!/usr/bin/env python3
"""EMUBENCH -- the stock Android emulator baseline for the ACOS release gate.

Owner direction 2026-11-18: ACOS must beat the *normal Android emulator* on every
tracked metric, so the emulator is a tracked baseline.  This harness measures the
emulator with the SAME apps (fixtures/build/apps/{counter,form-validation}.apk),
the SAME screen/density (1080x2400 @ 420 dpi) and the SAME metric definitions
ACOS uses (BENCHMARKS.md section 1):

  cold      process start -> sys.boot_completed, -> first correct app frame
  warm      boot from a snapshot, app launch -> correct frame
  tap       input tap -> settled UI result (tree-confirmed), counter and form
  tree      one UI tree read with text (`uiautomator dump`, the standard tool)
  shot      one fresh PNG screenshot (`screencap -p`)
  install   `adb install -r -t <apk>`
  memory    host PSS / anon+shmem of the qemu process + guest `dumpsys meminfo`
  cpu       /proc/<qemu>/stat utime+stime delta over a window
  N=1/2/4   correctly verified counter tasks per GiB-minute (primary metric)

ADR-0015 is relaxed for this benchmark only, under the owner's limits, all
enforced here: N <= 4 instances, <= 2048 MiB guest RAM each, MemAvailable is
checked before EVERY instance start and the N is aborted if that start would
push host usage past 85 %, and every instance is shut down on every exit path
(finally + SIGINT/SIGTERM).  free -m is logged per step (`host_log`).

Correctness: a sample only counts when the live UI tree reports the expected
state (count text, error text); 0 false passes.  Nothing is estimated.

Usage:
  emu_bench.py --setup-avds
  emu_bench.py --phase n1-cold --out DIR --json FILE
  emu_bench.py --phase n1-warm --out DIR --json FILE
  emu_bench.py --phase n2      --out DIR --json FILE
  emu_bench.py --phase n4      --out DIR --json FILE
  emu_bench.py --merge --json results/bench/emubench-<date>.json PHASE.json...
  emu_bench.py --self-test
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import os
import signal
import subprocess
import sys
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
SDK = Path("/home/Qui/Android/Sdk")
ADB = Path("/usr/bin/adb")
EMULATOR = SDK / "emulator" / "emulator"
AVDMANAGER = SDK / "cmdline-tools" / "latest" / "bin" / "avdmanager"
AVD_HOME = ROOT / ".cache/avd"
EMU_HOME = ROOT / ".cache/emulator-home"
IMAGE = "system-images;android-34;google_apis;x86_64"
PORTS = [5554, 5556, 5558, 5560]
GUEST_MB = 2048
CORES = 4
HOST_PER_INSTANCE_MIB = 4400        # measured qemu PSS is ~3.3-3.5 GiB; 4.3 GiB reserve
HOST_CEILING = 0.85                 # abort a start that would cross this
MAIN_FIXTURES = Path("/home/Qui/Projects/acos/fixtures/build/apps")

COUNTER_PKG = "dev.acos.fixtures.counter"
COUNTER_ACT = "dev.acos.fixtures.counter/.MainActivity"
FORM_PKG = "dev.acos.fixtures.formvalidation"
FORM_ACT = "dev.acos.fixtures.formvalidation/.MainActivity"
FORM_ERROR = "Name is required"

CROP_TOP, CROP_BOTTOM = 130, 60      # system chrome (status bar / gesture strip)
TOL_SAME = 0.0005                    # <= 1193 of 2 386 800 compared px


# --------------------------------------------------------------------------- p6 helpers
def _load_p6():
    """Reuse the validated P6 harness primitives (adb routes, tree parse, host read)."""
    spec = importlib.util.spec_from_file_location(
        "p6_emulator", ROOT / "tools/art-runtime/p6/p6_emulator.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


sys.dont_write_bytecode = True     # never leave __pycache__ in tools/
p6 = _load_p6()
now = p6.now
pct = p6.pct


def stats(xs):
    xs = [float(x) for x in xs if x is not None]
    if not xs:
        return {"n": 0}
    s = p6.stats(xs)
    s["p90"] = pct(xs, 90)
    return s


def sha256(path):
    return p6.sha256(path)


# --------------------------------------------------------------------------- host
def meminfo():
    mi = {}
    for line in Path("/proc/meminfo").read_text().splitlines():
        k, _, v = line.partition(":")
        mi[k.strip()] = int(v.strip().split()[0]) // 1024      # MiB
    return {"mem_total_mib": mi["MemTotal"], "mem_available_mib": mi["MemAvailable"],
            "swap_total_mib": mi.get("SwapTotal", 0), "swap_free_mib": mi.get("SwapFree", 0)}


def free_m_line():
    """The `free -m` Mem: line, verbatim, as the acceptance criterion asks for."""
    p = subprocess.run(["free", "-m"], capture_output=True, text=True)
    for line in p.stdout.splitlines():
        if line.startswith("Mem:"):
            return line
    return ""


class HostLog:
    def __init__(self, log):
        self.log = log
        self.entries = []

    def step(self, tag):
        mi = meminfo()
        used_pct = 100.0 * (mi["mem_total_mib"] - mi["mem_available_mib"]) / mi["mem_total_mib"]
        e = {"t": round(time.time(), 3), "step": tag, "load1": round(os.getloadavg()[0], 2),
             **mi, "used_pct": round(used_pct, 2), "free_m": free_m_line()}
        self.entries.append(e)
        self.log("host %-34s avail=%6d MiB used=%5.1f%% load1=%.2f | %s"
                 % (tag, mi["mem_available_mib"], used_pct, e["load1"], e["free_m"]))
        return e

    def require_ram(self, n_more, tag):
        """Owner limit: abort the N if this start would push usage past HOST_CEILING."""
        e = self.step(tag)
        avail_after = e["mem_available_mib"] - HOST_PER_INSTANCE_MIB * n_more
        floor = (1.0 - HOST_CEILING) * e["mem_total_mib"]
        if avail_after < floor:
            raise RuntimeError(
                "RAM guard: starting %d more instance(s) would leave %d MiB available "
                "(floor %d MiB = %.0f%% usage) -- aborting this N"
                % (n_more, avail_after, floor, HOST_CEILING * 100))
        return e


# --------------------------------------------------------------------------- processes
def proc_cmdline(pid):
    try:
        return Path("/proc/%d/cmdline" % pid).read_bytes().replace(b"\0", b" ").decode(
            "utf-8", "replace").strip()
    except OSError:
        return ""


def qemu_pids():
    out = []
    for e in Path("/proc").iterdir():
        if e.name.isdigit() and "qemu-system-x86_64" in proc_cmdline(int(e.name)):
            out.append(int(e.name))
    return sorted(out)


def qemu_pid_for_port(port):
    for pid in qemu_pids():
        cl = proc_cmdline(pid)
        if " -port %d " % port in cl + " ":
            return pid
    return None


def pss_kib(pid):
    return p6._read_smaps_rollup(pid).get("Pss", 0)


def anon_shmem_kib(pid):
    st = p6._read_status(pid)
    return (int(st.get("RssAnon", "0 kB").split()[0])
            + int(st.get("RssShmem", "0 kB").split()[0]))


def rss_kib(pid):
    return p6._read_smaps_rollup(pid).get("Rss", 0)


def cputicks(pid):
    return p6._read_cputicks(pid)


# --------------------------------------------------------------------------- AVDs
CONFIG_OVERRIDES = {
    "hw.ramSize": str(GUEST_MB), "hw.cpu.ncore": str(CORES),
    "hw.gpu.enabled": "yes", "hw.gpu.mode": "swiftshader_indirect",
    "hw.keyboard": "yes", "hw.mainKeys": "no",
    "disk.dataPartition.size": "2G", "vm.heapSize": "256",
    "fastboot.forceColdBoot": "no", "fastboot.forceFastBoot": "yes",
    "firstboot.saveToLocalSnapshot": "no", "showDeviceFrame": "no",
}


def avd_name(idx):
    return "emb-%d" % idx


def patch_config(cfg: Path):
    lines = cfg.read_text().splitlines()
    seen, out = set(), []
    for line in lines:
        k = line.split("=", 1)[0].strip()
        if k in CONFIG_OVERRIDES:
            out.append("%s=%s" % (k, CONFIG_OVERRIDES[k]))
            seen.add(k)
        else:
            out.append(line)
    for k, v in CONFIG_OVERRIDES.items():
        if k not in seen:
            out.append("%s=%s" % (k, v))
    cfg.write_text("\n".join(out) + "\n")


def setup_avds(count=4, log=print):
    AVD_HOME.mkdir(parents=True, exist_ok=True)
    EMU_HOME.mkdir(parents=True, exist_ok=True)
    env = os.environ.copy()
    env.update({"ANDROID_SDK_ROOT": str(SDK), "ANDROID_HOME": str(SDK),
                "ANDROID_AVD_HOME": str(AVD_HOME), "ANDROID_EMULATOR_HOME": str(EMU_HOME)})
    for i in range(1, count + 1):
        name = avd_name(i)
        cfg = AVD_HOME / (name + ".avd") / "config.ini"
        if not (AVD_HOME / (name + ".ini")).exists() or not cfg.exists():
            log("creating AVD %s" % name)
            p = subprocess.run([str(AVDMANAGER), "create", "avd", "-n", name, "-k", IMAGE,
                                "-d", "pixel_6", "--force"], input=b"no\n",
                               capture_output=True, env=env, timeout=300)
            if p.returncode != 0 or not cfg.exists():
                raise RuntimeError("avdmanager create %s failed: %s"
                                   % (name, p.stderr.decode("utf-8", "replace")[-500:]))
        patch_config(cfg)
    log("AVDs ready: %s" % ", ".join(avd_name(i) for i in range(1, count + 1)))


# --------------------------------------------------------------------------- instance
class Instance:
    def __init__(self, idx, logdir, log):
        self.idx = idx
        self.avd = avd_name(idx)
        self.port = PORTS[idx - 1]
        self.serial = "emulator-%d" % self.port
        self.adb = p6.Adb(self.serial, log=log)
        self.logdir = Path(logdir)
        self.log = log
        self.proc = None
        self.t0 = None
        self.boot_ms = None
        self.snapshot = False

    def env(self):
        e = os.environ.copy()
        e.update({"ANDROID_SDK_ROOT": str(SDK), "ANDROID_HOME": str(SDK),
                  "ANDROID_AVD_HOME": str(AVD_HOME), "ANDROID_EMULATOR_HOME": str(EMU_HOME)})
        e.pop("ANDROID_SERIAL", None)
        return e

    def cmd(self, snapshot=False):
        c = [str(EMULATOR), "-avd", self.avd, "-port", str(self.port),
             "-no-window", "-no-audio", "-no-boot-anim",
             "-gpu", "swiftshader_indirect",
             "-memory", str(GUEST_MB), "-cores", str(CORES), "-no-metrics"]
        if not snapshot:
            c += ["-no-snapshot"]
        return c

    def start(self, snapshot=False, tag=""):
        self.snapshot = snapshot
        self.logdir.mkdir(parents=True, exist_ok=True)
        logfile = self.logdir / ("emulator-%s%s.log" % (self.avd, ("-" + tag) if tag else ""))
        cmd = self.cmd(snapshot)
        (self.logdir / ("cmd-%s%s.txt" % (self.avd, ("-" + tag) if tag else ""))).write_text(
            " ".join(cmd) + "\n")
        self.t0 = now()
        self.proc = subprocess.Popen(cmd, stdout=open(logfile, "wb"),
                                     stderr=subprocess.STDOUT, env=self.env(),
                                     start_new_session=True)
        self.log("start %s port %d snapshot=%s pid %d" % (self.avd, self.port, snapshot,
                                                          self.proc.pid))
        return self

    def wait_boot(self, timeout=420.0):
        t0 = now()
        while True:
            el = now() - t0
            if el > timeout:
                raise RuntimeError("%s: boot timed out after %.0fs" % (self.serial, el))
            if self.proc.poll() is not None and qemu_pid_for_port(self.port) is None:
                raise RuntimeError("%s: emulator exited before boot (rc=%s)"
                                   % (self.serial, self.proc.returncode))
            try:
                out = self.adb.text(["shell", "getprop", "sys.boot_completed"],
                                    timeout=15, check=False).strip()
            except subprocess.TimeoutExpired:
                out = ""
            if out == "1":
                self.boot_ms = (now() - self.t0) * 1000.0
                return self.boot_ms
            time.sleep(0.2)

    def pid(self):
        return qemu_pid_for_port(self.port)

    def prep(self):
        """Guest settings that only remove animation jitter; no behaviour change."""
        for k in ("window_animation_scale", "transition_animation_scale",
                  "animator_duration_scale"):
            self.adb.sh("settings put global %s 0" % k, check=False)
        self.adb.sh("svc power stayon true", check=False)
        self.adb.sh("settings put system screen_off_timeout 2147483647", check=False)

    def stop(self):
        pids = []
        try:
            devs = subprocess.run([str(ADB), "devices"], capture_output=True,
                                  timeout=30).stdout.decode()
            if any(l.split() and l.split()[0] == self.serial
                   for l in devs.splitlines()[1:]):
                subprocess.run([str(ADB), "-s", self.serial, "emu", "kill"],
                               capture_output=True, timeout=30)
        except Exception:
            pass
        t0 = now()
        while qemu_pid_for_port(self.port) and now() - t0 < 90:
            time.sleep(0.5)
        if qemu_pid_for_port(self.port):
            self.log("WARN %s: SIGKILL after graceful kill failed" % self.serial)
            pid = qemu_pid_for_port(self.port)
            try:
                os.kill(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            if self.proc is not None:
                try:
                    os.killpg(self.proc.pid, signal.SIGKILL)
                except (ProcessLookupError, PermissionError):
                    pass
            t1 = now()
            while qemu_pid_for_port(self.port) and now() - t1 < 30:
                time.sleep(0.5)
        if self.proc is not None:
            try:
                self.proc.wait(timeout=15)
            except subprocess.TimeoutExpired:
                pass
        left = qemu_pid_for_port(self.port)
        self.log("stop %s (%.1fs, qemu left=%s)" % (self.serial, now() - t0, left))
        return {"serial": self.serial, "qemu_left": left, "shutdown_s": now() - t0}

    # ---- observation / action primitives -----------------------------------
    def am_start(self, act, timeout=180):
        t0 = now()
        p = subprocess.run([str(ADB), "-s", self.serial, "shell", "am", "start", "-W", "-n", act],
                           capture_output=True, timeout=timeout)
        ms = (now() - t0) * 1000.0
        out = p.stdout.decode("utf-8", "replace")
        return {"ms": ms, "rc": p.returncode, "out": out,
                "total_time_ms": p6.parse_total_time(out)}

    def force_stop(self, pkg):
        self.adb.sh("am force-stop %s" % pkg, check=False)

    def pm_clear(self, pkg):
        self.adb.sh("pm clear %s" % pkg, check=False)

    def tap_id(self, rid):
        """Tap the centre of the node with resource-id suffix `rid`; return the tap cost."""
        t = self.adb.tree()
        b = p6.node_bounds(t["nodes"], rid)
        if not b:
            raise RuntimeError("tap_id: no node id/%s in tree (%d nodes)" % (rid, len(t["nodes"])))
        x, y = (b[0] + b[2]) // 2, (b[1] + b[3]) // 2
        t0 = now()
        self.adb.sh("input tap %d %d" % (x, y), check=False)
        return {"tap_ms": (now() - t0) * 1000.0, "x": x, "y": y, "bounds": b}

    def tree_count(self, rid="count"):
        t = self.adb.tree()
        return t, p6.count_text(t["nodes"])

    def node_text(self, rid):
        t = self.adb.tree()
        for n in t["nodes"]:
            if n.get("resource-id", "").endswith("/" + rid):
                return n.get("text", "")
        return None


# --------------------------------------------------------------------------- frames
def frame_frac(a, b):
    import numpy as np
    if a.shape != b.shape:
        return 1.0
    ca = a[CROP_TOP:a.shape[0] - CROP_BOTTOM]
    cb = b[CROP_TOP:b.shape[0] - CROP_BOTTOM]
    return float(np.count_nonzero(np.any(ca != cb, axis=2))) / float(ca.shape[0] * ca.shape[1])


def save_ref(px, name="counter"):
    import numpy as np
    (ROOT / ".cache/emubench").mkdir(parents=True, exist_ok=True)
    np.save(str(ROOT / ".cache/emubench" / ("ref-%s.npy" % name)), px)


def load_ref(name="counter"):
    import numpy as np
    p = ROOT / ".cache/emubench" / ("ref-%s.npy" % name)
    return np.load(str(p)) if p.exists() else None


def wait_change(inst, pre, min_frac=0.02, timeout=60.0):
    """Poll raw screencaps until the app region differs from the pre-launch frame.

    `min_frac` is the share of compared pixels that must change: 2 % is far above
    the status-bar clock / nav pill noise (cropped) and far below "the app window
    replaced the launcher".  Returns (ms, frames, last_frac).
    """
    t0 = now()
    n = 0
    last = 0.0
    while now() - t0 < timeout:
        s = inst.adb.screencap_raw()
        n += 1
        last = frame_frac(pre, s["px"])
        if last >= min_frac:
            return (now() - t0) * 1000.0, n, last
    try:
        import numpy as np
        from PIL import Image
        (ROOT / ".cache/emubench").mkdir(parents=True, exist_ok=True)
        np.save(str(ROOT / ".cache/emubench/last-nomatch.npy"), s["px"])
        Image.fromarray(s["px"][:, :, :3]).save(str(ROOT / ".cache/emubench/last-nomatch.png"))
    except Exception:
        pass
    return None, n, last


def stable_frame(inst, tries=20, tol=0.005):
    """Two consecutive screencaps that differ by < 0.5 % of the app region."""
    prev = inst.adb.screencap_raw()["px"]
    for _ in range(tries):
        cur = inst.adb.screencap_raw()["px"]
        if frame_frac(prev, cur) <= tol:
            return cur
        prev = cur
    return prev


def launch_to_frame(inst, act, pre, timeout=60.0):
    """am start (async) -> first frame whose app region differs from `pre`."""
    box = {}

    def _go():
        box["st"] = inst.am_start(act)

    th = threading.Thread(target=_go)
    t0 = now()
    th.start()
    ms, nf, frac = wait_change(inst, pre, timeout=timeout)
    th.join(timeout=120)
    st = box.get("st", {})
    return {"frame_ms": ms, "frames": nf, "frac": frac,
            "wall_ms": (now() - t0) * 1000.0,
            "am_start_ms": st.get("ms"), "total_time_ms": st.get("total_time_ms")}


def settled_tree(prev, nodes, expect):
    """The observation boundary, as a predicate (see `launch_to_tree`).

    A tree is *settled* when it can be read, it says what the caller expects, and
    it is identical to the previous read (content stable, not mid-animation). One
    read alone never settles: that is the rule that makes the boundary the same
    one ACOS's strict settle uses.
    """
    return prev is not None and nodes == prev and bool(expect(nodes))


def launch_to_tree(inst, act, pre, expect, timeout=90.0):
    """am start (async) -> first frame change -> a readable, **settled** UI tree.

    This is the observation boundary MERGE040D(b)/BENCHMARKS §13 defines for BOTH
    sides, so the warm-launch row compares like with like instead of ACOS's
    strict settle against a bare launcher-frame change:

    * the launch is issued once, asynchronously, and `t0` is taken before it;
    * `frame_ms` is the first screencap whose app region differs from `pre` (the
      idle launcher frame) — the same number the old row reported;
    * then `uiautomator dump` is polled until two **consecutive** dumps agree and
      `expect(nodes)` is True: a tree that can be read and has stopped changing,
      which is what ACOS's strict settle means (`m4_launch_strict_ms`).

    Returns frame_ms, settle_ms (frame change -> settled tree) and tree_ms (the
    whole boundary, `t0` -> settled tree).
    """
    box = {}

    def _go():
        box["st"] = inst.am_start(act)

    th = threading.Thread(target=_go)
    t0 = now()
    th.start()
    ms, nf, frac = wait_change(inst, pre, timeout=timeout)
    th.join(timeout=120)
    settle = None
    prev = None
    reads = 0
    while ms is not None and now() - t0 < timeout:
        t = inst.adb.tree()
        reads += 1
        nodes = t["nodes"]
        if settled_tree(prev, nodes, expect):
            settle = (now() - t0) * 1000.0
            break
        prev = nodes
    st = box.get("st", {})
    return {"frame_ms": ms, "frames": nf, "frac": frac,
            "tree_ms": settle, "tree_reads": reads,
            "settle_ms": None if settle is None else settle - (ms or 0.0),
            "wall_ms": (now() - t0) * 1000.0,
            "am_start_ms": st.get("ms"), "total_time_ms": st.get("total_time_ms"),
            "count": None if not prev else p6.count_text(prev)}


# --------------------------------------------------------------------------- phases
class Bench:
    def __init__(self, args):
        self.args = args
        self.out = Path(args.out)
        self.out.mkdir(parents=True, exist_ok=True)
        self.log = lambda *a: print("[emubench %s]" % time.strftime("%H:%M:%S"), *a, flush=True)
        self.host = HostLog(self.log)
        self.rows = {}
        self.reps = []
        self.errors = []
        self.instances = []
        self.launcher = {}      # serial -> idle-launcher frame (session-constant)

    # ---- plumbing ---------------------------------------------------------
    def add(self, metric, value, ok=True, **extra):
        if value is None:
            return
        d = {"v": value, "ok": bool(ok), "t": round(time.time(), 3)}
        d.update(extra)
        self.rows.setdefault(metric, []).append(d)

    def doc(self, phase):
        rows = {}
        for k, samples in self.rows.items():
            good = [s["v"] for s in samples if s["ok"]]
            st = stats(good)
            st["n_total"] = len(samples)
            st["n_failed"] = len(samples) - len(good)
            st["unit"] = "ms" if k.endswith("_ms") else (
                "MiB" if k.endswith("_mib") else ("%" if k.endswith("_pct") else ""))
            st["samples"] = [round(s["v"], 4) for s in samples]
            rows[k] = st
        return {"schema": "emubench/1", "phase": phase,
                "generated_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                "config": {"avds": [i.avd for i in self.instances],
                           "ports": [i.port for i in self.instances],
                           "guest_mb": GUEST_MB, "cores": CORES,
                           "gpu": "swiftshader_indirect", "window": False,
                           "snapshot_cold": False, "host_ceiling_pct": HOST_CEILING * 100,
                           "host_per_instance_reserve_mib": HOST_PER_INSTANCE_MIB},
                "host": p6.host_meta(), "host_log": self.host.entries,
                "rows": rows, "reps": self.reps, "errors": self.errors}

    def write(self, phase):
        doc = self.doc(phase)
        path = Path(self.args.json) if self.args.json else (
            self.out / ("phase-%s.json" % phase))
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(doc, indent=1, sort_keys=True) + "\n")
        self.log("wrote %s" % path)
        for k in sorted(doc["rows"]):
            r = doc["rows"][k]
            if r.get("n"):
                self.log("  %-34s n=%-3d p50=%-9.2f p90=%-9.2f spread=%s%%"
                         % (k, r["n"], r["p50"], r["p90"],
                            None if r.get("spread_pct") is None else round(r["spread_pct"], 1)))
        return path

    def capture_launcher(self, inst):
        """Idle launcher frame for this session: the 'app is not on screen' baseline."""
        self.launcher[inst.serial] = stable_frame(inst)
        self.log("launcher frame captured for %s" % inst.serial)
        return self.launcher[inst.serial]

    def inst(self, idx):
        while len(self.instances) < idx:
            n = len(self.instances) + 1
            self.instances.append(Instance(n, self.out / "emu", self.log))
        return self.instances[idx - 1]

    def stop_all(self):
        for i in reversed(self.instances):
            if qemu_pid_for_port(i.port):
                try:
                    i.stop()
                except Exception as e:      # never mask the original failure
                    self.log("WARN stop %s: %s" % (i.serial, e))

    # ---- metrics on one instance -----------------------------------------
    def install(self, inst, apk, metric):
        t0 = now()
        p = subprocess.run([str(ADB), "-s", inst.serial, "install", "-r", "-t", str(apk)],
                           capture_output=True, timeout=600)
        ms = (now() - t0) * 1000.0
        ok = p.returncode == 0 and b"Success" in p.stdout
        if not ok:
            self.errors.append({"install": str(apk), "rc": p.returncode,
                                "out": (p.stdout + p.stderr).decode("utf-8", "replace")[-400:]})
        self.add(metric, ms, ok, apk=apk.name)
        self.log("install %-24s %.0f ms ok=%s" % (apk.name, ms, ok))
        return ok

    def cold_rep(self, ref):
        inst = self.inst(1)
        self.host.require_ram(1, "cold rep: before emulator start")
        inst.start(snapshot=False, tag="cold")
        boot_ms = inst.wait_boot()
        self.add("cold_boot_ms", boot_ms, True)
        inst.prep()
        self.host.step("cold rep: booted")
        self.install(inst, APPS["counter"], "install_ms")
        if ref is None:
            self.install(inst, APPS["form"], "install_form_ms")
        pre = self.capture_launcher(inst)
        r = launch_to_frame(inst, COUNTER_ACT, pre)
        self.add("cold_am_start_w_ms", r["am_start_ms"], r["am_start_ms"] is not None,
                 total_time_ms=r["total_time_ms"])
        t = inst.adb.tree()
        cnt = p6.count_text(t["nodes"])
        ok = r["frame_ms"] is not None and cnt == "0"
        self.add("cold_launch_ms", r["frame_ms"], ok, frames=r["frames"],
                 frac=r["frac"], count=cnt, wall_ms=r["wall_ms"])
        if r["frame_ms"] is not None:
            self.add("cold_first_frame_ms", boot_ms + r["frame_ms"], ok,
                     boot_ms=boot_ms, launch_ms=r["frame_ms"])
        self.log("cold rep: boot %.0f ms + launch %.0f ms (frames %d, frac %s, tree count %s)"
                 % (boot_ms, r["frame_ms"] or -1, r["frames"], r["frac"], cnt))

    def n1_warm_tree(self):
        """Only the warm-launch row at the settled-tree boundary (MEASUREMENT).

        The rest of `n1_warm` is unchanged and already recorded; this phase exists
        so re-measuring one row does not re-pay the whole 20-minute baseline. Same
        limits as every phase: one instance, ≤2048 MiB guest, RAM-guarded, and
        `stop_all()` in `run()`'s `finally`.
        """
        inst = self.inst(1)
        self.host.require_ram(1, "n1-warm-tree: before emulator start")
        inst.start(snapshot=False, tag="warm-tree")
        inst.wait_boot()
        inst.prep()
        self.host.step("n1-warm-tree: booted")
        self.install(inst, APPS["counter"], "install_ms")
        pre = self.capture_launcher(inst)
        inst.force_stop(COUNTER_PKG)
        inst.am_start(COUNTER_ACT)
        if p6.count_text(inst.adb.tree()["nodes"]) != "0":
            raise RuntimeError("counter did not start at count 0")
        inst.force_stop(COUNTER_PKG)
        for _ in range(self.args.warm_reps):
            inst.force_stop(COUNTER_PKG)
            time.sleep(0.3)
            r = launch_to_tree(inst, COUNTER_ACT, pre,
                               lambda nodes: p6.count_text(nodes) == "0")
            cnt = r["count"]
            ok = r["frame_ms"] is not None and cnt == "0"
            settled = r["tree_ms"] is not None and ok
            self.add("warm_launch_ms", r["frame_ms"], ok, frames=r["frames"],
                     frac=r["frac"], count=cnt, wall_ms=r["wall_ms"],
                     am_start_ms=r["am_start_ms"], total_time_ms=r["total_time_ms"])
            self.add("warm_launch_tree_ms", r["tree_ms"], settled,
                     frame_ms=r["frame_ms"], settle_ms=r["settle_ms"],
                     tree_reads=r["tree_reads"], count=cnt,
                     am_start_ms=r["am_start_ms"], total_time_ms=r["total_time_ms"])
            inst.force_stop(COUNTER_PKG)
        self.host.step("n1-warm-tree: after warm launches")

    def n1_warm(self):
        inst = self.inst(1)
        self.host.require_ram(1, "n1-warm: before emulator start")
        inst.start(snapshot=False, tag="warm")
        inst.wait_boot()
        inst.prep()
        self.host.step("n1-warm: booted")
        self.install(inst, APPS["counter"], "install_ms")
        self.install(inst, APPS["form"], "install_form_ms")
        pre = self.capture_launcher(inst)

        # verified count 0 (the tree is the correctness oracle everywhere)
        inst.force_stop(COUNTER_PKG)
        inst.am_start(COUNTER_ACT)
        t = inst.adb.tree()
        if p6.count_text(t["nodes"]) != "0":
            raise RuntimeError("counter did not start at count 0")
        inst.force_stop(COUNTER_PKG)

        # --- M4 warm launch, at the SAME observation boundary as ACOS --------
        # ACOS's warm launch is its strict settle: a readable tree that stopped
        # changing. The emulator row used to stop at the launcher-frame change,
        # so it paid no tree read the ACOS row pays — BENCHMARKS §13.6(2). One
        # launch now yields both numbers: `warm_launch_ms` (frame change, the old
        # row, kept for the record) and `warm_launch_tree_ms` (am start -> a
        # settled tree with count 0), which is what the gate compares.
        for _ in range(self.args.warm_reps):
            inst.force_stop(COUNTER_PKG)
            time.sleep(0.3)
            r = launch_to_tree(inst, COUNTER_ACT, pre,
                               lambda nodes: p6.count_text(nodes) == "0")
            cnt = r["count"]
            ok = r["frame_ms"] is not None and cnt == "0"
            settled = r["tree_ms"] is not None and ok
            self.add("warm_launch_ms", r["frame_ms"], ok, frames=r["frames"],
                     frac=r["frac"], count=cnt, wall_ms=r["wall_ms"],
                     am_start_ms=r["am_start_ms"], total_time_ms=r["total_time_ms"])
            self.add("warm_launch_tree_ms", r["tree_ms"], settled,
                     frame_ms=r["frame_ms"], settle_ms=r["settle_ms"],
                     tree_reads=r["tree_reads"], count=cnt,
                     am_start_ms=r["am_start_ms"], total_time_ms=r["total_time_ms"])
            inst.force_stop(COUNTER_PKG)
        self.host.step("n1-warm: after warm launches")

        # --- M5 screenshot --------------------------------------------------
        for _ in range(self.args.shot_reps):
            p = self.out / "shots"
            p.mkdir(exist_ok=True)
            f = p / ("shot-%d.png" % len(self.rows.get("screenshot_ms", [])))
            ms, size = inst.adb.screencap_png(f)
            ok, why = p6.png_ok(f)
            self.add("screenshot_ms", ms, ok, bytes=size, why=why)

        # --- M7 UI tree -----------------------------------------------------
        for _ in range(self.args.tree_reps):
            t = inst.adb.tree()
            self.add("ui_tree_ms", t["ms"], len(t["nodes"]) > 0,
                     nodes=len(t["nodes"]), bytes=t["bytes"], route=t["route"])

        # --- M8 counter: tap -> settled (tree-confirmed) ---------------------
        inst.am_start(COUNTER_ACT)
        time.sleep(1.0)
        expect = 0
        for _ in range(self.args.tap_reps):
            expect += 1
            tap = inst.tap_id("inc")
            t0 = now()
            got, t = None, None
            while now() - t0 < 15.0:
                t = inst.adb.tree()
                got = p6.count_text(t["nodes"])
                if got == str(expect):
                    break
            ms = (now() - t0) * 1000.0
            ok = got == str(expect)
            self.add("tap_counter_ms", ms, ok, count=got, expect=expect,
                     tap_ms=tap["tap_ms"])
        # reset for the form row
        inst.force_stop(COUNTER_PKG)

        # --- M8 form: tap submit on an empty form -> error text -------------
        inst.force_stop(FORM_PKG)
        inst.am_start(FORM_ACT)
        time.sleep(0.5)
        for _ in range(self.args.tap_reps):
            inst.pm_clear(FORM_PKG)
            inst.force_stop(FORM_PKG)
            inst.am_start(FORM_ACT)
            tap = inst.tap_id("submit")
            t0 = now()
            got = None
            while now() - t0 < 15.0:
                got = inst.node_text("error")
                if got == FORM_ERROR:
                    break
            ms = (now() - t0) * 1000.0
            self.add("tap_form_ms", ms, got == FORM_ERROR, error=got, tap_ms=tap["tap_ms"])
        inst.force_stop(FORM_PKG)
        self.host.step("n1-warm: after tap rows")

        # --- M6 reset -> first correct screen -------------------------------
        inst.am_start(COUNTER_ACT)
        time.sleep(1.0)
        for _ in range(self.args.reset_reps):
            inst.tap_id("inc")
            time.sleep(0.4)
            inst.force_stop(COUNTER_PKG)
            inst.pm_clear(COUNTER_PKG)
            pre = self.launcher.get(inst.serial)
            if pre is None:
                pre = self.capture_launcher(inst)
            r = launch_to_frame(inst, COUNTER_ACT, pre)
            t = inst.adb.tree()
            cnt = p6.count_text(t["nodes"])
            ok = r["frame_ms"] is not None and cnt == "0"
            self.add("reset_ms", r["wall_ms"], ok, frames=r["frames"], frac=r["frac"],
                     count=cnt, frame_ms=r["frame_ms"])
        self.host.step("n1-warm: after reset rows")

        # --- memory (host qemu + guest) -------------------------------------
        inst.am_start(COUNTER_ACT)
        time.sleep(1.5)
        pid = inst.pid()
        if not pid:
            raise RuntimeError("no qemu pid for %s" % inst.serial)
        for _ in range(self.args.mem_reps):
            time.sleep(2.0)
            self.add("idle_pss_mib", pss_kib(pid) / 1024.0, True)
            self.add("idle_anon_shmem_mib", anon_shmem_kib(pid) / 1024.0, True)
            self.add("idle_rss_mib", rss_kib(pid) / 1024.0, True)
            g = self.guest_meminfo(inst)
            self.add("guest_used_mib", g["used_mib"], True, free_mib=g["free_mib"])
            self.add("app_pss_mib", g["app_pss_mib"], True)

        # --- CPU idle -------------------------------------------------------
        for _ in range(self.args.cpu_reps):
            c0 = cputicks(pid)
            w0 = now()
            time.sleep(self.args.cpu_window)
            c1 = cputicks(pid)
            wall = now() - w0
            self.add("cpu_idle_pct", 100.0 * (c1 - c0) * 0.01 / wall, True, window_s=wall)
            self.host.step("n1-warm: cpu window")

        # --- warm boot from a snapshot (owner: snapshot for warm rows) ------
        self.host.step("n1-warm: before snapshot phase")
        inst.stop()
        for rep in range(self.args.snap_reps + 1):
            self.host.require_ram(1, "snapshot boot: before emulator start")
            inst.start(snapshot=True, tag="snap")
            try:
                ms = inst.wait_boot()
            finally:
                pass
            loaded = self.snapshot_loaded(inst)
            if rep == 0:
                self.log("snapshot warmup boot %.0f ms loaded=%s" % (ms, loaded))
            else:
                self.add("warm_boot_snapshot_ms", ms, loaded)
            inst.stop()
        self.host.step("n1-warm: after snapshot phase")

    def snapshot_loaded(self, inst):
        """True when the emulator log says it loaded a quickboot snapshot."""
        logs = sorted((self.out / "emu").glob("emulator-%s-snap*.log" % inst.avd))
        if not logs:
            return False
        txt = logs[-1].read_text(errors="replace").lower()
        return "snapshot" in txt and ("loading snapshot" in txt or "loaded" in txt)

    def guest_meminfo(self, inst):
        out = inst.adb.sh("dumpsys meminfo", check=False, timeout=180)
        used = free = None
        for line in out.splitlines():
            if line.strip().startswith("Free RAM:"):
                try:
                    free = int(line.split()[2]) / 1024.0
                except (IndexError, ValueError):
                    pass
            if line.strip().startswith("Used RAM:"):
                try:
                    used = int(line.split()[2]) / 1024.0
                except (IndexError, ValueError):
                    pass
        app = inst.adb.sh("dumpsys meminfo %s" % COUNTER_PKG, check=False, timeout=180)
        app_pss = None
        for line in app.splitlines():
            if "TOTAL PSS:" in line:
                try:
                    app_pss = int(line.split("TOTAL PSS:")[1].split()[0]) / 1024.0
                except (IndexError, ValueError):
                    pass
        return {"used_mib": used, "free_mib": free, "app_pss_mib": app_pss}

    # ---- N-instance throughput -------------------------------------------
    def counter_task(self, inst, task_no):
        """One predeclared counter task: launch -> verify 0 -> tap inc -> verify 1 -> reset.

        A task is *verified* only when the live UI tree reports the expected
        count at each check (0 false passes).
        """
        t0 = now()
        detail = {"task": task_no, "serial": inst.serial}
        try:
            inst.force_stop(COUNTER_PKG)
            inst.pm_clear(COUNTER_PKG)
            pre = self.launcher.get(inst.serial)
            if pre is None:
                pre = self.capture_launcher(inst)
            r = launch_to_frame(inst, COUNTER_ACT, pre)
            detail["am_start_ms"] = r["am_start_ms"]
            detail["launch_ms"] = r["frame_ms"]
            detail["launch_frac"] = r["frac"]
            detail["launch_frames"] = r["frames"]
            t = inst.adb.tree()
            c0 = p6.count_text(t["nodes"])
            detail["count0"] = c0
            if c0 != "0":
                detail["fail"] = "count_after_launch=%r" % c0
                return False, (now() - t0) * 1000.0, detail
            tap = inst.tap_id("inc")
            detail["tap_ms"] = tap["tap_ms"]
            tt = now()
            got = None
            while now() - tt < 20.0:
                got = inst.node_text("count")
                if got == "1":
                    break
            detail["tap_settle_ms"] = (now() - tt) * 1000.0
            detail["count1"] = got
            if got != "1":
                detail["fail"] = "count_after_tap=%r" % got
                return False, (now() - t0) * 1000.0, detail
            inst.force_stop(COUNTER_PKG)
            inst.pm_clear(COUNTER_PKG)
            return True, (now() - t0) * 1000.0, detail
        except Exception as e:
            detail["fail"] = "exception: %s" % e
            return False, (now() - t0) * 1000.0, detail

    def n_phase(self, n):
        self.log("=== N=%d throughput phase ===" % n)
        K = self.args.tasks
        for rep in range(1, self.args.n_reps + 1):
            repdoc = {"rep": rep, "n": n, "started": round(time.time(), 3)}
            self.host.step("n%d rep%d: before starts" % (n, rep))
            insts = [self.inst(i + 1) for i in range(n)]
            boots = {}
            try:
                for i, inst in enumerate(insts):
                    self.host.require_ram(1, "n%d rep%d: before start of instance %d"
                                          % (n, rep, i + 1))
                    inst.start(snapshot=False, tag="n%d-r%d" % (n, rep))
                for i, inst in enumerate(insts):
                    boots[inst.serial] = inst.wait_boot()
                    inst.prep()
                    self.capture_launcher(inst)
                self.host.step("n%d rep%d: all %d booted" % (n, rep, n))
                repdoc["boot_ms"] = boots
                self.add("n%d_boot_ms" % n, sum(boots.values()) / len(boots), True,
                         per_instance=boots)
                for inst in insts:
                    if not self.install(inst, APPS["counter"], "install_ms"):
                        raise RuntimeError("install failed on %s" % inst.serial)
                # warm the app once so the task loop is steady-state
                for inst in insts:
                    r = launch_to_frame(inst, COUNTER_ACT, self.launcher[inst.serial])
                    self.log("warmup %s: frame %.0f ms frames %d frac %.3f"
                             % (inst.serial, r["frame_ms"] or -1, r["frames"],
                                r["frac"] if r["frac"] is not None else -1))
                    inst.force_stop(COUNTER_PKG)
                self.host.step("n%d rep%d: tasks starting" % (n, rep))

                # ---- concurrent task loop + 1 s memory/CPU sampler -------
                samples = []
                stop_evt = threading.Event()

                def sampler():
                    while not stop_evt.is_set():
                        row = {"t": round(time.time(), 3), "pss_kib": 0, "anon_shmem_kib": 0,
                               "cpu_ticks": 0, "per_instance": {}}
                        for inst in insts:
                            pid = inst.pid()
                            if not pid:
                                continue
                            pss = pss_kib(pid)
                            a_s = anon_shmem_kib(pid)
                            ct = cputicks(pid)
                            row["pss_kib"] += pss
                            row["anon_shmem_kib"] += a_s
                            row["cpu_ticks"] += ct
                            row["per_instance"][inst.serial] = {"pss_kib": pss,
                                                                "anon_shmem_kib": a_s,
                                                                "cpu_ticks": ct}
                        samples.append(row)
                        stop_evt.wait(1.0)

                th = threading.Thread(target=sampler, daemon=True)
                th.start()
                results = [None] * n
                t_start = now()

                def worker(idx, inst):
                    results[idx] = [self.counter_task(inst, k + 1) for k in range(K)]

                workers = [threading.Thread(target=worker, args=(i, inst))
                           for i, inst in enumerate(insts)]
                for w in workers:
                    w.start()
                for w in workers:
                    w.join()
                wall = now() - t_start
                stop_evt.set()
                th.join(timeout=10)

                verified = sum(1 for r in results for (ok, _ms, _d) in r if ok)
                tasks = sum(len(r) for r in results)
                task_ms = [ms for r in results for (_ok, ms, _d) in r]
                gib_samples = [s["pss_kib"] / (1024.0 * 1024.0) for s in samples] or [0.0]
                gib_mean = sum(gib_samples) / len(gib_samples)
                gib_integral = gib_mean * (wall / 60.0)
                rate = verified / gib_integral if gib_integral else 0.0
                cpu_pct = 0.0
                if len(samples) >= 2:
                    cpu_pct = (100.0 * (samples[-1]["cpu_ticks"] - samples[0]["cpu_ticks"])
                               * 0.01 / (samples[-1]["t"] - samples[0]["t"]))
                repdoc.update({"wall_s": wall, "verified": verified, "tasks": tasks,
                               "task_ms": task_ms, "gib_mean": gib_mean,
                               "gib_integral": gib_integral,
                               "tasks_per_gib_min": rate,
                               "host_cpu_pct_all": cpu_pct,
                               "cpu_pct_per_instance": cpu_pct / n,
                               "pss_mib_total": gib_mean * 1024.0,
                               "pss_mib_per_instance": gib_mean * 1024.0 / n,
                               "anon_shmem_mib_total":
                                   (sum(s["anon_shmem_kib"] for s in samples)
                                    / max(1, len(samples))) / 1024.0,
                               "sample_count": len(samples),
                               "failures": [d for r in results for (ok, _m, d) in r if not ok]})
                self.add("n%d_tasks_per_gib_min" % n, rate, verified == tasks, verified=verified,
                         tasks=tasks, gib_mean=gib_mean, wall_s=wall)
                self.add("n%d_task_ms" % n, pct(task_ms, 50) if task_ms else None,
                         verified == tasks)
                self.add("n%d_pss_mib_per_instance" % n, repdoc["pss_mib_per_instance"], True)
                self.add("n%d_cpu_pct_per_instance" % n, repdoc["cpu_pct_per_instance"], True)
                self.log("n%d rep%d: verified %d/%d in %.1fs, %.2f GiB mean, "
                         "%.2f tasks/GiB-min, cpu %.1f%% total"
                         % (n, rep, verified, tasks, wall, gib_mean, rate, cpu_pct))
            except Exception as e:
                self.errors.append({"n": n, "rep": rep, "error": str(e)})
                self.log("ERROR n%d rep%d: %s" % (n, rep, e))
            finally:
                for inst in insts:
                    try:
                        inst.stop()
                    except Exception as e:
                        self.log("WARN stop %s: %s" % (inst.serial, e))
                repdoc["stopped"] = {i.serial: qemu_pid_for_port(i.port) for i in insts}
                self.host.step("n%d rep%d: after shutdown" % (n, rep))
                self.reps.append(repdoc)

    # ---- dispatch ---------------------------------------------------------
    def run(self):
        phase = self.args.phase
        if phase == "n1-cold":
            for rep in range(1, self.args.cold_reps + 1):
                self.log("=== n1-cold rep %d/%d ===" % (rep, self.args.cold_reps))
                self.host.step("n1-cold rep%d: before" % rep)
                try:
                    self.cold_rep(None)
                except Exception as e:
                    self.errors.append({"phase": phase, "rep": rep, "error": str(e)})
                    self.log("ERROR rep %d: %s" % (rep, e))
                finally:
                    self.stop_all()
                    self.host.step("n1-cold rep%d: after shutdown" % rep)
        elif phase == "n1-warm-tree":
            try:
                self.n1_warm_tree()
            except Exception as e:
                self.errors.append({"phase": phase, "error": str(e)})
                self.log("ERROR: %s" % e)
            finally:
                self.stop_all()
        elif phase == "n1-warm":
            try:
                self.n1_warm()
            except Exception as e:
                self.errors.append({"phase": phase, "error": str(e)})
                self.log("ERROR: %s" % e)
            finally:
                self.stop_all()
        elif phase in ("n2", "n4"):
            try:
                self.n_phase(int(phase[1:]))
            finally:
                self.stop_all()
        else:
            raise SystemExit("unknown phase %r" % phase)
        self.host.step("phase %s: end" % phase)
        return self.write(phase)


# --------------------------------------------------------------------------- frame diagnosis
def diag_frame(args):
    """Why does a captured frame not match the saved reference? (diagnostic, no metric)"""
    import numpy as np
    b = Bench(args)
    setup_avds(4, log=b.log)
    inst = b.inst(1)
    try:
        b.host.require_ram(1, "diag: before emulator start")
        inst.start(snapshot=False, tag="diag")
        inst.wait_boot()
        inst.prep()
        b.install(inst, APPS["counter"], "install_ms")
        inst.force_stop(COUNTER_PKG)
        inst.am_start(COUNTER_ACT)
        time.sleep(2.0)
        t = inst.adb.tree()
        cnt = p6.count_text(t["nodes"])
        a1 = inst.adb.screencap_raw()["px"]
        time.sleep(1.5)
        a2 = inst.adb.screencap_raw()["px"]
        ref = load_ref()
        out = {"tree_count": cnt, "shape_a1": list(a1.shape), "shape_ref": None if ref is None else list(ref.shape)}
        out["a1_vs_a2_frac"] = frame_frac(a1, a2)
        out["a1_alpha_unique"] = [int(x) for x in np.unique(a1[:, :, 3])[:5]]
        if ref is not None:
            out["a1_vs_ref_frac"] = frame_frac(a1, ref)
            out["a1_vs_ref_frac_rgb"] = float(np.count_nonzero(
                np.any(a1[CROP_TOP:-CROP_BOTTOM, :, :3] != ref[CROP_TOP:-CROP_BOTTOM, :, :3], axis=2))
            ) / float(a1[CROP_TOP:-CROP_BOTTOM].shape[0] * a1.shape[1])
            d = np.any(a1 != ref, axis=2)
            rows = np.where(d.any(axis=1))[0]
            out["diff_rows_range"] = [int(rows.min()), int(rows.max())] if rows.size else None
            out["diff_rows_count"] = int(rows.size)
            out["diff_rows_first20"] = [int(r) for r in rows[:20]]
            out["ref_alpha_unique"] = [int(x) for x in np.unique(ref[:, :, 3])[:5]]
        from PIL import Image
        Image.fromarray(a1[:, :, :3]).save(str(b.out / "diag-a1.png"))
        if ref is not None:
            Image.fromarray(ref[:, :, :3]).save(str(b.out / "diag-ref.png"))
        b.log("diag: %s" % json.dumps(out, sort_keys=True))
        (b.out / "diag.json").write_text(json.dumps(out, indent=1) + "\n")
    finally:
        b.stop_all()
        b.host.step("diag: end")
    return 0


# --------------------------------------------------------------------------- fixtures
def resolve_apks():
    wt = ROOT / "fixtures/build/apps"
    base = wt if (wt / "counter.apk").exists() else MAIN_FIXTURES
    apks = {"counter": base / "counter.apk", "form": base / "form-validation.apk"}
    missing = [str(p) for p in apks.values() if not p.exists()]
    if missing:
        raise SystemExit("missing fixtures: %s (run tools/build/build-apps.sh)" % missing)
    return apks


APPS = {}


# --------------------------------------------------------------------------- merge
def merge(paths, out):
    doc = {"schema": "emubench/1",
           "generated_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
           "phases": {}, "rows": {}, "host_log": [], "errors": [], "reps": []}
    for p in paths:
        d = json.loads(Path(p).read_text())
        ph = d.get("phase") or Path(p).stem
        doc["phases"][ph] = {k: v for k, v in d.items() if k not in ("rows", "host_log")}
        doc["host_log"].extend(d.get("host_log", []))
        doc["errors"].extend(d.get("errors", []))
        doc["reps"].extend(d.get("reps", []))
        for k, v in d.get("rows", {}).items():
            # keep every phase's samples: a metric measured in two phases
            # (install in n1-cold and in the N phases) must not be overwritten
            doc["rows"][k if k not in doc["rows"] else "%s.%s" % (ph, k)] = v
    doc["host_log"].sort(key=lambda e: e.get("t", 0))
    Path(out).parent.mkdir(parents=True, exist_ok=True)
    Path(out).write_text(json.dumps(doc, indent=1, sort_keys=True) + "\n")
    print("merged %d phase(s) -> %s (%d rows)" % (len(paths), out, len(doc["rows"])))
    return doc


# --------------------------------------------------------------------------- self test
def self_test():
    import numpy as np
    checks, failed = [], 0

    def ck(name, cond):
        nonlocal failed
        checks.append((name, bool(cond)))
        if not cond:
            failed += 1

    a = np.zeros((2400, 1080, 4), dtype=np.uint8)
    b = a.copy()
    ck("identical frames compare equal", frame_frac(a, b) == 0.0)
    b[150:250, 10:110] = 255         # 10 000 px inside the compared region
    ck("a changed region is detected", frame_frac(a, b) > TOL_SAME)
    c = a.copy()
    c[0:100, :] = 255                # inside the cropped status bar
    ck("status-bar changes are cropped out", frame_frac(a, c) == 0.0)
    ck("meminfo has MemAvailable", meminfo()["mem_available_mib"] > 0)
    ck("free -m line parses", free_m_line().startswith("Mem:"))
    ck("qemu_pids returns a list", isinstance(qemu_pids(), list))
    ck("stats p90 present", stats([1, 2, 3, 4])["p90"] == pct([1, 2, 3, 4], 90))
    ck("no qemu for a free port", qemu_pid_for_port(5998) is None)
    ck("form error string", FORM_ERROR == "Name is required")
    ck("ram guard floor math", abs((1 - HOST_CEILING) * 100 - 15.0) < 1e-9)
    tree = [{"text": "0", "resource-id": "id/count"}]
    want = lambda nodes: p6.count_text(nodes) == "0"
    ck("a first tree read never settles", not settled_tree(None, tree, want))
    ck("two equal expected reads settle", settled_tree(tree, list(tree), want))
    ck("an unexpected tree does not settle", not settled_tree(tree, list(tree),
                                                             lambda n: p6.count_text(n) == "1"))
    ck("a changing tree does not settle",
       not settled_tree(tree, tree + [{"text": "1"}], lambda n: True))
    for name, ok in checks:
        print("  %-46s %s" % (name, "ok" if ok else "FAIL"))
    print("self-test: %d checks, %d failed" % (len(checks), failed))
    return 0 if failed == 0 else 1


# --------------------------------------------------------------------------- main
def main(argv=None):
    ap = argparse.ArgumentParser()
    ap.add_argument("--phase", choices=["n1-cold", "n1-warm", "n1-warm-tree", "n2", "n4"])
    ap.add_argument("--out", default=str(ROOT / ".cache/emubench/run"))
    ap.add_argument("--json", default=None)
    ap.add_argument("--setup-avds", action="store_true")
    ap.add_argument("--merge", action="store_true")
    ap.add_argument("files", nargs="*")
    ap.add_argument("--self-test", action="store_true")
    ap.add_argument("--diag-frame", action="store_true")
    ap.add_argument("--cold-reps", type=int, default=5)
    ap.add_argument("--warm-reps", type=int, default=5)
    ap.add_argument("--tap-reps", type=int, default=10)
    ap.add_argument("--tree-reps", type=int, default=10)
    ap.add_argument("--shot-reps", type=int, default=10)
    ap.add_argument("--reset-reps", type=int, default=5)
    ap.add_argument("--mem-reps", type=int, default=5)
    ap.add_argument("--cpu-reps", type=int, default=3)
    ap.add_argument("--cpu-window", type=float, default=10.0)
    ap.add_argument("--snap-reps", type=int, default=5)
    ap.add_argument("--n-reps", type=int, default=5)
    ap.add_argument("--tasks", type=int, default=3)
    ap.add_argument("--smoke", action="store_true",
                    help="1-2 samples per row: wiring check, never a claim")
    a = ap.parse_args(argv)
    if a.self_test:
        return self_test()
    if a.diag_frame:
        APPS.update(resolve_apks())
        return diag_frame(a)
    if a.setup_avds:
        setup_avds(4)
        return 0
    if a.merge:
        if not a.json or not a.files:
            raise SystemExit("--merge needs --json OUT and phase JSON files")
        merge(a.files, a.json)
        return 0
    if a.smoke:
        a.cold_reps, a.warm_reps, a.tap_reps = 1, 1, 2
        a.tree_reps, a.shot_reps, a.reset_reps = 2, 2, 1
        a.mem_reps, a.cpu_reps, a.snap_reps = 1, 1, 1
        a.n_reps, a.tasks = 1, 1
        a.cpu_window = 3.0
    if not a.phase:
        raise SystemExit("--phase is required")
    APPS.update(resolve_apks())
    b = Bench(a)
    setup_avds(4, log=b.log)

    def _sig(signum, _frame):
        b.log("signal %d: shutting every instance down" % signum)
        b.stop_all()
        sys.exit(128 + signum)

    signal.signal(signal.SIGINT, _sig)
    signal.signal(signal.SIGTERM, _sig)
    return 0 if b.run() else 1


if __name__ == "__main__":
    sys.exit(main())
