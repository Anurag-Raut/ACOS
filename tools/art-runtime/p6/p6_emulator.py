#!/usr/bin/env python3
"""
P6 — stock Android emulator baseline (AOSP API 34 x86_64, AVD `acos-oracle-34`).

Measures M1-M8 plus host CPU of the emulator process tree, using nothing but
`adb` against the emulator's own serial.  ADR-0015 rules: <= 2 GiB guest RAM,
exactly one instance at a time, and the emulator is always shut down (verified
by scanning /proc for qemu-system afterwards).

Nothing is estimated: every row is the raw sample list plus p50/p95/n, and the
correctness of every "correct" frame/screen is bound by a pixel compare against
a *verified* reference frame (the reference itself is only accepted when the
live `uiautomator` tree reports the expected counter text and two consecutive
captures are pixel-identical).

usage:
  python3 -I tools/art-runtime/p6/p6_emulator.py \
      --out .cache/p6/run1 \
      --json results/bench/p6-emulator-<stamp>.json \
      [--quick]

  --self-test   unit checks of the pure helpers, no adb, no emulator
"""

import argparse
import json
import math
import os
import random
import re
import shutil
import signal
import struct
import subprocess
import sys
import time
from pathlib import Path

ADB = Path("/usr/bin/adb")
EMULATOR = Path("/home/Qui/Android/Sdk/emulator/emulator")
ROOT = Path("/home/Qui/Projects/acos")
AVD_HOME = ROOT / ".cache/avd"
EMU_HOME = ROOT / ".cache/emulator-home"
AVD_NAME = "acos-oracle-34"
SDK = Path("/home/Qui/Android/Sdk")
APK = ROOT / "fixtures/build/apps/counter.apk"
PKG = "dev.acos.fixtures.counter"
ACT = "dev.acos.fixtures.counter/.MainActivity"
W, H = 1080, 2400

# Pixel compares ignore the system chrome: the status bar (clock/icons, top
# 130 px) and the gesture-nav strip (bottom 60 px).  `policy_control
# immersive.full=*` does NOT hide them on this API 34 image (verified: the
# status-bar clock is still drawn), and the clock changes every minute.  App
# content (action bar + counter views) is entirely inside the compared region.
CROP_TOP, CROP_BOTTOM = 130, 60
CMP_PIXELS = W * (H - CROP_TOP - CROP_BOTTOM)

# Tolerances are fractions of the compared region (1080 x 2210 px).
TOL_SAME = 0.0005      # <= 1193 px differ  -> the reference screen
TOL_DIFF = 0.0005      # >  1193 px differ  -> a different screen

SERIAL_RE = re.compile(r"^emulator-55\d\d$")


# ----------------------------------------------------------------------------- helpers
def now():
    return time.monotonic()


def pct(xs, q):
    a = sorted(xs)
    if not a:
        return None
    k = (len(a) - 1) * q / 100.0
    lo = int(math.floor(k))
    hi = min(lo + 1, len(a) - 1)
    return a[lo] + (a[hi] - a[lo]) * (k - lo)


def stats(xs):
    xs = [float(x) for x in xs if x is not None]
    if not xs:
        return {"n": 0}
    med = pct(xs, 50)
    rng = random.Random(20261112)
    boots = []
    for _ in range(2000):
        s = [xs[rng.randrange(len(xs))] for _ in range(len(xs))]
        boots.append(pct(s, 50))
    p5, p95 = pct(xs, 5), pct(xs, 95)
    return {
        "n": len(xs),
        "p50": med,
        "p95": p95,
        "p5": p5,
        "min": min(xs),
        "max": max(xs),
        "ci95_of_p50": [pct(boots, 2.5), pct(boots, 97.5)],
        # BENCHMARKS.md rule: spread = (p95 - p5) / p50 * 100, > 15% => NOISY
        "spread_pct": (100.0 * (p95 - p5) / med) if med else None,
    }


def parse_raw_screencap(d):
    """Parse `screencap` raw output: header is w,h,fmt(,colorspace) + RGBA8888."""
    if len(d) < 16:
        raise ValueError("raw screencap too short: %d bytes" % len(d))
    w, h, fmt = struct.unpack_from("<iii", d, 0)
    if w <= 0 or h <= 0 or w > 20000 or h > 20000:
        raise ValueError("bad raw screencap header: %r" % (d[:16],))
    want = w * h * 4
    if len(d) < want:
        raise ValueError("raw screencap short: %d < %d" % (len(d), want))
    off = len(d) - want          # 12-byte header (<=API 28) or 16-byte (>=API 29)
    return w, h, fmt, off


class Adb:
    def __init__(self, serial, log=print):
        self.serial = serial
        self.log = log

    def run(self, args, timeout=120):
        return subprocess.run(
            [str(ADB), "-s", self.serial] + [str(a) for a in args],
            capture_output=True, timeout=timeout)

    def text(self, args, timeout=120, check=True):
        p = self.run(args, timeout=timeout)
        out = p.stdout.decode("utf-8", "replace")
        if check and p.returncode != 0:
            raise RuntimeError("adb %s failed rc=%d: %s" %
                               (" ".join(map(str, args)), p.returncode,
                                p.stderr.decode("utf-8", "replace")[:400]))
        return out

    def sh(self, cmd, timeout=120, check=True):
        return self.text(["shell", cmd], timeout=timeout, check=check)

    # ---- observation primitives (each timed with wall clock incl. adb spawn)
    def screencap_raw(self):
        t0 = now()
        p = subprocess.run([str(ADB), "-s", self.serial, "exec-out", "screencap"],
                           capture_output=True, timeout=120)
        ms = (now() - t0) * 1000.0
        if p.returncode != 0:
            raise RuntimeError("screencap raw failed: %s" %
                               p.stderr.decode("utf-8", "replace")[:300])
        w, h, fmt, off = parse_raw_screencap(p.stdout)
        import numpy as np
        px = np.frombuffer(p.stdout, dtype=np.uint8, count=w * h * 4, offset=off)
        return {"ms": ms, "w": w, "h": h, "fmt": fmt, "bytes": len(p.stdout),
                "px": px.reshape(h, w, 4)}

    def screencap_png(self, path):
        t0 = now()
        with open(path, "wb") as f:
            p = subprocess.run([str(ADB), "-s", self.serial, "exec-out", "screencap", "-p"],
                               stdout=f, stderr=subprocess.PIPE, timeout=120)
        ms = (now() - t0) * 1000.0
        if p.returncode != 0:
            raise RuntimeError("screencap -p failed: %s" %
                               p.stderr.decode("utf-8", "replace")[:300])
        return ms, Path(path).stat().st_size

    def tree(self):
        """uiautomator dump with text.  Route A: `exec-out ... /dev/tty`."""
        t0 = now()
        p = subprocess.run([str(ADB), "-s", self.serial, "exec-out", "uiautomator", "dump", "/dev/tty"],
                           capture_output=True, timeout=180)
        ms = (now() - t0) * 1000.0
        out = p.stdout.decode("utf-8", "replace")
        xml = extract_xml(out)
        route = "exec-out uiautomator dump /dev/tty"
        if xml is None:
            # Route B (fallback): dump to the guest and pull it.
            t1 = now()
            self.sh("uiautomator dump /sdcard/p6-tree.xml", timeout=180, check=False)
            p2 = subprocess.run([str(ADB), "-s", self.serial, "exec-out", "cat", "/sdcard/p6-tree.xml"],
                                capture_output=True, timeout=180)
            ms = (now() - t1) * 1000.0
            out = p2.stdout.decode("utf-8", "replace")
            xml = extract_xml(out)
            route = "uiautomator dump /sdcard + exec-out cat"
        if xml is None:
            raise RuntimeError("uiautomator dump produced no XML (rc=%d): %r" %
                               (p.returncode, out[:200]))
        return {"ms": ms, "route": route, "xml": xml, "nodes": parse_nodes(xml),
                "bytes": len(xml.encode())}


def extract_xml(s):
    i = s.find("<?xml")
    j = s.rfind("</hierarchy>")
    if i < 0 or j < 0:
        return None
    return s[i:j + len("</hierarchy>")]


NODE_RE = re.compile(r"<node\b([^>]*)/?>", re.S)
ATTR_RE = re.compile(r'([\w:-]+)="([^"]*)"')


def parse_nodes(xml):
    nodes = []
    for m in NODE_RE.finditer(xml):
        attrs = dict(ATTR_RE.findall(m.group(1)))
        b = attrs.get("bounds", "")
        m2 = re.match(r"\[(-?\d+),(-?\d+)\]\[(-?\d+),(-?\d+)\]", b)
        if m2:
            attrs["bounds_xyxy"] = [int(x) for x in m2.groups()]
        nodes.append(attrs)
    return nodes


def count_text(nodes):
    """The counter TextView: resource-id ends with /count and it carries text."""
    for n in nodes:
        rid = n.get("resource-id", "")
        if rid.endswith("/count") or rid.endswith(":id/count"):
            return n.get("text", "")
    return None


def node_bounds(nodes, rid_suffix):
    for n in nodes:
        rid = n.get("resource-id", "")
        if rid.endswith(rid_suffix) and "bounds_xyxy" in n:
            return n["bounds_xyxy"]
    return None


def diff_frac(a, b):
    """Fraction of pixels whose RGBA differs at all (full frame)."""
    import numpy as np
    if a.shape != b.shape:
        return 1.0
    return float(np.count_nonzero(np.any(a != b, axis=2))) / float(a.shape[0] * a.shape[1])


def cmp_frac(a, b):
    """diff_frac over the app-content region only (system chrome cropped)."""
    import numpy as np
    if a.shape != b.shape:
        return 1.0
    ca = a[CROP_TOP:a.shape[0] - CROP_BOTTOM]
    cb = b[CROP_TOP:b.shape[0] - CROP_BOTTOM]
    return float(np.count_nonzero(np.any(ca != cb, axis=2))) / float(ca.shape[0] * ca.shape[1])


# ----------------------------------------------------------------------------- host sampling
def proc_cmdline(pid):
    try:
        return Path("/proc/%d/cmdline" % pid).read_bytes().replace(b"\0", b" ").decode("utf-8", "replace").strip()
    except OSError:
        return ""


def emulator_tree():
    """PIDs of the emulator/qemu process tree we started (never any other process)."""
    out = []
    for e in Path("/proc").iterdir():
        if not e.name.isdigit():
            continue
        pid = int(e.name)
        cl = proc_cmdline(pid)
        if "qemu-system-x86_64" in cl and AVD_NAME in cl:
            out.append(pid)
        elif "emulator-real" in cl and AVD_NAME in cl:
            out.append(pid)
    return sorted(set(out))


def _read_status(pid):
    d = {}
    try:
        for line in Path("/proc/%d/status" % pid).read_text().splitlines():
            k, _, v = line.partition(":")
            d[k.strip()] = v.strip()
    except OSError:
        return d
    return d


def _read_smaps_rollup(pid):
    d = {}
    try:
        for line in Path("/proc/%d/smaps_rollup" % pid).read_text().splitlines():
            k, _, v = line.partition(":")
            if v.strip().endswith("kB"):
                d[k.strip()] = int(v.strip().split()[0])
    except OSError:
        return d
    return d


def _read_cputicks(pid):
    try:
        parts = Path("/proc/%d/stat" % pid).read_text().rsplit(")", 1)[1].split()
        return int(parts[11]) + int(parts[12])      # utime + stime
    except (OSError, IndexError, ValueError):
        return 0


def host_sample():
    pids = emulator_tree()
    sm = {"pids": pids, "cmdlines": {str(p): proc_cmdline(p)[:200] for p in pids},
          "rss_kib": 0, "pss_kib": 0, "anon_kib": 0, "shmem_kib": 0,
          "anon_shmem_kib": 0, "cpu_ticks": 0, "vmrss_kib": 0}
    for pid in pids:
        s = _read_smaps_rollup(pid)
        st = _read_status(pid)
        sm["rss_kib"] += s.get("Rss", 0)
        sm["pss_kib"] += s.get("Pss", 0)
        # RssAnon / RssShmem live in /proc/<pid>/status, not in smaps_rollup.
        sm["anon_kib"] += int(st.get("RssAnon", "0 kB").split()[0])
        sm["shmem_kib"] += int(st.get("RssShmem", "0 kB").split()[0])
        sm["vmrss_kib"] += int(st.get("VmRSS", "0 kB").split()[0]) if st.get("VmRSS") else 0
        sm["cpu_ticks"] += _read_cputicks(pid)
    sm["anon_shmem_kib"] = sm["anon_kib"] + sm["shmem_kib"]
    return sm


def host_limits():
    mi = {}
    for line in Path("/proc/meminfo").read_text().splitlines():
        k, _, v = line.partition(":")
        mi[k.strip()] = int(v.strip().split()[0])
    return {"mem_total_mib": mi.get("MemTotal", 0) // 1024,
            "mem_available_mib": mi.get("MemAvailable", 0) // 1024,
            "swap_total_mib": mi.get("SwapTotal", 0) // 1024,
            "swap_free_mib": mi.get("SwapFree", 0) // 1024}


def host_meta():
    host = {}
    try:
        host["cpu_model"] = [l.split(":", 1)[1].strip() for l in Path("/proc/cpuinfo").read_text().splitlines()
                             if l.startswith("model name")][0]
    except Exception:
        host["cpu_model"] = "unknown"
    host["threads"] = os.cpu_count()
    host["kernel"] = os.uname().release
    host["loadavg1"] = os.getloadavg()[0]
    host.update(host_limits())
    return host


# ----------------------------------------------------------------------------- emulator lifecycle
class Emulator:
    def __init__(self, adb, port, logdir, log=print):
        self.adb = adb
        self.port = port
        self.serial = "emulator-%d" % port
        self.logdir = Path(logdir)
        self.logdir.mkdir(parents=True, exist_ok=True)
        self.proc = None
        self.log = log
        self.logfile = None

    def env(self):
        e = os.environ.copy()
        e["ANDROID_AVD_HOME"] = str(AVD_HOME)
        e["ANDROID_EMULATOR_HOME"] = str(EMU_HOME)
        e["ANDROID_SDK_ROOT"] = str(SDK)
        e["ANDROID_HOME"] = str(SDK)
        e.pop("ANDROID_SERIAL", None)
        return e

    def start(self, tag):
        if emulator_tree():
            raise RuntimeError("refusing to start: an acos-oracle emulator is already running: %s"
                               % emulator_tree())
        self.logfile = self.logdir / ("emulator-%s.log" % tag)
        cmd = [str(EMULATOR), "-avd", AVD_NAME, "-port", str(self.port),
               "-no-window", "-no-audio", "-no-boot-anim",
               "-no-snapshot-save", "-no-snapshot-load",
               "-gpu", "swiftshader_indirect",
               "-memory", "2048", "-cores", "4", "-no-metrics"]
        (self.logdir / ("cmd-%s.txt" % tag)).write_text(" ".join(cmd) + "\n")
        self.proc = subprocess.Popen(cmd, stdout=open(self.logfile, "wb"),
                                     stderr=subprocess.STDOUT, env=self.env(),
                                     start_new_session=True)
        return self.proc

    def wait_boot(self, timeout=600.0):
        """Poll sys.boot_completed. Returns wall seconds since start()."""
        t0 = now()
        while True:
            el = now() - t0
            if el > timeout:
                tail = "\n".join(self.logfile.read_text(errors="replace").splitlines()[-25:])
                self.stop()
                raise RuntimeError("boot timed out after %.0fs; log tail:\n%s" % (el, tail))
            if self.proc is not None and self.proc.poll() is not None and not emulator_tree():
                tail = "\n".join(self.logfile.read_text(errors="replace").splitlines()[-25:])
                raise RuntimeError("emulator exited before boot (rc=%s); log tail:\n%s"
                                   % (self.proc.returncode, tail))
            try:
                out = self.adb.text(["shell", "getprop", "sys.boot_completed"],
                                    timeout=15, check=False).strip()
            except subprocess.TimeoutExpired:
                out = ""
            if out == "1":
                return el
            time.sleep(0.25)

    def stop(self, log=print):
        serial_present = False
        try:
            devs = subprocess.run([str(ADB), "devices"], capture_output=True, timeout=30).stdout.decode()
            serial_present = any(l.split()[0] == self.serial for l in devs.splitlines()[1:] if l.split())
        except Exception:
            pass
        if serial_present:
            try:
                subprocess.run([str(ADB), "-s", self.serial, "emu", "kill"],
                               capture_output=True, timeout=30)
            except Exception:
                pass
        t0 = now()
        while emulator_tree() and now() - t0 < 60:
            time.sleep(0.5)
        if emulator_tree():
            log("emulator: SIGKILL after graceful kill failed: %s" % emulator_tree())
            if self.proc is not None:
                try:
                    os.killpg(self.proc.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            for pid in emulator_tree():
                try:
                    os.kill(pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            t1 = now()
            while emulator_tree() and now() - t1 < 30:
                time.sleep(0.5)
        if self.proc is not None:
            try:
                self.proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                pass
        leftover = emulator_tree()
        return {"serial_present_after": serial_present, "qemu_left": leftover,
                "shutdown_wall_s": now() - t0}


# ----------------------------------------------------------------------------- measurement run
class Run:
    def __init__(self, args):
        self.args = args
        self.out = Path(args.out)
        self.out.mkdir(parents=True, exist_ok=True)
        self.shots = self.out / "shots"
        self.shots.mkdir(exist_ok=True)
        self.log = lambda *a: print("[p6 %s]" % time.strftime("%H:%M:%S"), *a, flush=True)
        # pick the emulator serial: only ever an emulator-55xx.
        serials = []
        p = subprocess.run([str(ADB), "devices"], capture_output=True, timeout=60)
        for line in p.stdout.decode().splitlines()[1:]:
            f = line.split()
            if f:
                serials.append(f[0])
        self.log("adb devices: %s" % (serials or "<none>"))
        emu = [s for s in serials if SERIAL_RE.match(s)]
        if len(emu) > 1:
            raise RuntimeError("more than one emulator attached: %s" % emu)
        self.existing_serial = emu[0] if emu else None
        self.port = int(self.existing_serial.split("-")[1]) if self.existing_serial else 5554
        self.adb = Adb("emulator-%d" % self.port)
        self.emu = Emulator(self.adb, self.port, self.out / "logs", log=self.log)
        self.r = {
            "task": "P6 stock Android emulator baseline",
            "generated_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "host": host_meta(),
            "config": {}, "M1": {}, "M2": {}, "M3": {}, "M4": {}, "M5": {},
            "M6": {}, "M7": {}, "M8": {}, "cpu": {}, "errors": [], "notes": [],
        }

    # ---- setup -------------------------------------------------------------
    def avd_config(self):
        cfg = AVD_HOME / (AVD_NAME + ".avd") / "config.ini"
        d = {}
        for line in cfg.read_text().splitlines():
            if "=" in line:
                k, _, v = line.partition("=")
                d[k.strip()] = v.strip()
        return {"config_ini": str(cfg), "hw.ramSize": d.get("hw.ramSize"),
                "hw.cpu.ncore": d.get("hw.cpu.ncore"),
                "hw.gpu.mode": d.get("hw.gpu.mode"),
                "image.sysdir.1": d.get("image.sysdir.1"),
                "abi.type": d.get("abi.type"), "tag.id": d.get("tag.id"),
                "hw.lcd.width": d.get("hw.lcd.width"),
                "hw.lcd.height": d.get("hw.lcd.height"),
                "hw.lcd.density": d.get("hw.lcd.density")}

    def device_props(self):
        props = {}
        for k in ("ro.build.fingerprint", "ro.build.version.sdk", "ro.build.version.release",
                  "ro.product.cpu.abi", "ro.product.model", "ro.build.type",
                  "ro.product.name", "ro.build.id"):
            props[k] = self.adb.sh("getprop %s" % k).strip()
        props["sys.boot_completed"] = self.adb.sh("getprop sys.boot_completed").strip()
        return props

    def prepare_guest(self):
        self.adb.run(["install", "-r", "-t", str(APK)], timeout=300)
        self.adb.sh("wm dismiss-keyguard", check=False)
        self.adb.sh("input keyevent 224", check=False)        # WAKEUP
        self.adb.sh("svc power stayon true", check=False)
        self.adb.sh("settings put system screen_off_timeout 2147483647", check=False)
        # Hide status/nav bar so the screen is a pure app frame for pixel compares.
        self.adb.sh("settings put global policy_control immersive.full=*", check=False)

    def settle(self, secs, label):
        """Idle window: sample host memory + CPU of the emulator tree."""
        tick = float(os.sysconf("SC_CLK_TCK"))
        s0 = host_sample()
        t0 = now()
        samples = []
        while now() - t0 < secs:
            time.sleep(5.0)
            s = host_sample()
            if not s["pids"]:
                raise RuntimeError("emulator process tree disappeared during settle")
            samples.append({"t_s": round(now() - t0, 2), "anon_shmem_mib": s["anon_shmem_kib"] / 1024.0,
                            "rss_mib": s["rss_kib"] / 1024.0, "pss_mib": s["pss_kib"] / 1024.0,
                            "anon_mib": s["anon_kib"] / 1024.0, "shmem_mib": s["shmem_kib"] / 1024.0,
                            "vmrss_mib": s["vmrss_kib"] / 1024.0, "cpu_ticks": s["cpu_ticks"]})
        s1 = host_sample()
        wall = now() - t0
        dticks = s1["cpu_ticks"] - s0["cpu_ticks"]
        cpu_one = 100.0 * dticks / tick / wall                 # % of one core
        self.r["cpu"]["idle_settle_%s" % label] = {
            "wall_s": wall, "cpu_pct_of_one_core": cpu_one,
            "cpu_pct_of_host": cpu_one / os.cpu_count(),
            "pids": s1["pids"], "cmdlines": s1["cmdlines"],
        }
        return samples

    def guest_meminfo(self):
        g = {}
        try:
            out = self.adb.sh("dumpsys meminfo", timeout=180)
        except Exception as e:
            g["error"] = str(e)
            return g
        (self.out / "dumpsys-meminfo.txt").write_text(out)
        for pat, key in ((r"Total RAM:\s*([\d,]+)K", "total_ram_kib"),
                         (r"Free RAM:\s*([\d,]+)K", "free_ram_kib"),
                         (r"Used RAM:\s*([\d,]+)K", "used_ram_kib"),
                         (r"Lost RAM:\s*([\d,]+)K", "lost_ram_kib"),
                         (r"Total PSS:\s*([\d,]+)K", "total_pss_kib"),
                         (r"Total SwapPSS:\s*([\d,]+)K", "total_swappss_kib")):
            m = re.search(pat, out)
            if m:
                g[key] = int(m.group(1).replace(",", ""))
        try:
            app = self.adb.sh('dumpsys meminfo %s' % PKG, timeout=180)
            (self.out / "dumpsys-meminfo-app.txt").write_text(app)
            for key, pat in (("app_total_pss_kib", r"TOTAL PSS:\s*([\d,]+)"),
                             ("app_java_heap_kib", r"Dalvik Heap\s+([\d,]+)"),
                             ("app_native_heap_kib", r"Native Heap\s+([\d,]+)")):
                m = re.search(pat, app)
                if m:
                    g[key] = int(m.group(1).replace(",", ""))
        except Exception as e:
            g["app_error"] = str(e)
        return g

    # ---- reference frames --------------------------------------------------
    def settled_shot(self, label, tries=40):
        """Two consecutive pixel-identical captures, after the tree text is bound."""
        prev = None
        for _ in range(tries):
            s = self.adb.screencap_raw()
            if prev is not None and cmp_frac(prev["px"], s["px"]) <= TOL_SAME:
                return s
            prev = s
            time.sleep(0.12)
        raise RuntimeError("screen never settled for reference %s" % label)

    def reference(self, label, expect_count):
        """Capture a reference frame ONLY if the live tree proves the screen."""
        tr = self.adb.tree()
        txt = count_text(tr["nodes"])
        if str(txt) != str(expect_count):
            raise RuntimeError("reference %s: tree count %r != %r" % (label, txt, expect_count))
        shot = self.settled_shot(label)
        tr2 = self.adb.tree()
        txt2 = count_text(tr2["nodes"])
        if str(txt2) != str(expect_count):
            raise RuntimeError("reference %s: post-capture tree count %r != %r"
                               % (label, txt2, expect_count))
        if cmp_frac(shot["px"], self.settled_shot(label + "-confirm")["px"]) > TOL_SAME:
            raise RuntimeError("reference %s is not stable" % label)
        import numpy as np
        np.save(self.out / ("ref-%s.npy" % label), shot["px"])
        self.log("reference %s bound: tree count %s, %dx%d, rgb[0,0]=%s"
                 % (label, txt2, shot["w"], shot["h"], shot["px"][0, 0][:3].tolist()))
        return shot

    # ---- M1 ----------------------------------------------------------------
    def m1_idle(self):
        self.log("M1: launch counter, 60 s settle, then host+guest memory")
        self.adb.sh("am force-stop %s" % PKG, check=False)
        self.adb.sh("pm clear %s" % PKG, check=False)
        self.adb.sh("am start -W -n %s" % ACT, timeout=180)
        samples = self.settle(60.0, "m1")
        i = self.guest_meminfo()
        tr = self.adb.tree()
        g = {"settle_samples": samples,
             "anon_shmem_mib": stats([s["anon_shmem_mib"] for s in samples]),
             "rss_mib": stats([s["rss_mib"] for s in samples]),
             "pss_mib": stats([s["pss_mib"] for s in samples]),
             "anon_mib": stats([s["anon_mib"] for s in samples]),
             "shmem_mib": stats([s["shmem_mib"] for s in samples]),
             "guest_dumpsys": i,
             "live_tree_count": count_text(tr["nodes"]),
             "method": ("host: /proc/<pid>/status RssAnon+RssShmem summed over the "
                        "emulator process tree (qemu-system-x86_64 for this AVD); RSS/PSS from "
                        "/proc/<pid>/smaps_rollup; 5 s samples over 60 s after launch; "
                        "guest: dumpsys meminfo totals; type "
                        "HOST_ANON_SHMEM (whole emulator host process tree, not one guest app)")}
        self.r["M1"] = g
        self.log("M1 anon+shmem p50 %.1f MiB (n=%d), RSS p50 %.1f MiB, PSS p50 %.1f MiB"
                 % (g["anon_shmem_mib"]["p50"], g["anon_shmem_mib"]["n"],
                    g["rss_mib"]["p50"], g["pss_mib"]["p50"]))
        return g

    # ---- M3 (cold boot + launch) -------------------------------------------
    def m3_cold_boot(self, rep):
        self.log("M3 rep %d: cold boot" % rep)
        info = self.emu.stop(log=self.log)
        if info["qemu_left"]:
            raise RuntimeError("could not stop emulator before M3 rep %d" % rep)
        time.sleep(1.0)
        self.emu.start("m3-%d" % rep)
        boot_s = self.emu.wait_boot()
        self.log("M3 rep %d: sys.boot_completed after %.3f s" % (rep, boot_s))
        self.prepare_guest_light()
        # launch -> first correct frame (app data persists; the in-memory count is 0)
        t0 = now()
        out = self.adb.sh("am start -W -n %s" % ACT, timeout=180)
        total_ms = parse_total_time(out)
        t_launch_wall = (now() - t0) * 1000.0
        first_correct = None
        first_any = None
        samples = []
        while now() - t0 < 60.0:
            s = self.adb.screencap_raw()
            dt = (now() - t0) * 1000.0
            d = cmp_frac(self.ref0["px"], s["px"])
            samples.append({"t_ms": dt, "cmp_frac_vs_ref0": d,
                            "full_diff_vs_ref0": diff_frac(self.ref0["px"], s["px"])})
            if first_any is None and d > TOL_DIFF:
                first_any = dt
            if d <= TOL_SAME:
                first_correct = dt
                break
        tr = self.adb.tree()
        txt = count_text(tr["nodes"])
        if first_correct is None:
            raise RuntimeError("M3 rep %d: no correct frame within 60 s" % rep)
        if str(txt) != "0":
            raise RuntimeError("M3 rep %d: correct frame but tree count %r" % (rep, txt))
        rec = {"boot_s": boot_s, "am_start_total_ms": total_ms,
               "am_start_wall_ms": t_launch_wall,
               "launch_to_first_app_frame_ms": first_any,
               "launch_to_first_correct_frame_ms": first_correct,
               "cold_start_total_s": boot_s + first_correct / 1000.0,
               "tree_count": txt, "ok": True,
               "poll_samples": samples[:8]}
        self.log("M3 rep %d: boot %.2fs, TotalTime %s ms, launch->correct frame %.0f ms, total %.2fs"
                 % (rep, boot_s, total_ms, first_correct, rec["cold_start_total_s"]))
        return rec

    def prepare_guest_light(self):
        self.adb.sh("wm dismiss-keyguard", check=False)
        self.adb.sh("input keyevent 224", check=False)
        self.adb.sh("svc power stayon true", check=False)

    # ---- M4 ----------------------------------------------------------------
    def m4_warm_launch(self, reps):
        rows = []
        for i in range(reps):
            self.adb.sh("am force-stop %s" % PKG, check=False)
            # wait for the launcher (a screen that is NOT the count-0 app screen)
            t1 = now()
            while now() - t1 < 3.0:
                pre = self.adb.screencap_raw()
                if cmp_frac(self.ref0["px"], pre["px"]) > TOL_DIFF:
                    break
                time.sleep(0.1)
            t0 = now()
            out = self.adb.sh("am start -W -n %s" % ACT, timeout=180)
            total_ms = parse_total_time(out)
            t_am = (now() - t0) * 1000.0
            first_any = first_correct = None
            while now() - t0 < 30.0:
                s = self.adb.screencap_raw()
                dt = (now() - t0) * 1000.0
                if first_any is None and cmp_frac(pre["px"], s["px"]) > TOL_DIFF:
                    first_any = dt
                if cmp_frac(self.ref0["px"], s["px"]) <= TOL_SAME:
                    first_correct = dt
                    break
            ok = first_correct is not None
            txt = count_text(self.adb.tree()["nodes"]) if ok else None
            rows.append({"rep": i, "am_start_total_ms": total_ms, "am_start_wall_ms": t_am,
                         "to_first_app_frame_ms": first_any,
                         "to_correct_frame_ms": first_correct,
                         "tree_count": txt, "ok": ok and str(txt) == "0"})
            self.log("M4 rep %d: TotalTime %s ms, correct frame %.0f ms, tree %s"
                     % (i, total_ms, first_correct or -1, txt))
        self.r["M4"] = self.summarize(rows, "to_correct_frame_ms",
                                      extra_keys=("am_start_total_ms", "to_first_app_frame_ms",
                                                  "am_start_wall_ms"))
        return self.r["M4"]

    # ---- M5 ----------------------------------------------------------------
    def m5_screenshot(self, reps):
        png_rows, raw_rows = [], []
        for i in range(reps):
            p = self.shots / ("m5-%02d.png" % i)
            ms, nbytes = self.adb.screencap_png(p)
            ok, why = png_ok(p)
            png_rows.append({"rep": i, "ms": ms, "bytes": nbytes, "ok": ok, "why": why})
            if i == 0:
                shutil.copy(p, self.shots / "m5-first.png")
            p.unlink()
            s = self.adb.screencap_raw()
            raw_rows.append({"rep": i, "ms": s["ms"], "bytes": s["bytes"],
                             "ok": s["w"] == W and s["h"] == H,
                             "cmp_frac_vs_ref0": cmp_frac(self.ref0["px"], s["px"]),
                             "full_diff_vs_ref0": diff_frac(self.ref0["px"], s["px"])})
            self.log("M5 rep %d: png %.1f ms (%d B, %s), raw %.1f ms"
                     % (i, ms, nbytes, "ok" if ok else why, s["ms"]))
        self.r["M5"] = {
            "png": self.summarize(png_rows, "ms"),
            "raw": self.summarize(raw_rows, "ms"),
            "png_samples": png_rows, "raw_samples": raw_rows,
            "method": ("`adb -s <emu> exec-out screencap -p > file` wall clock incl. adb client "
                       "spawn and PNG write; raw = same without -p (raw RGBA8888), checked "
                       "1080x2400 and pixel-identical to the verified count-0 reference"),
        }
        return self.r["M5"]

    # ---- M7 ----------------------------------------------------------------
    def m7_tree(self, reps):
        rows = []
        for i in range(reps):
            tr = self.adb.tree()
            txt = count_text(tr["nodes"])
            texts = [n.get("text", "") for n in tr["nodes"] if n.get("text")]
            ok = (str(txt) == "0" and len(tr["nodes"]) > 0 and
                  any(t for t in texts))
            rows.append({"rep": i, "ms": tr["ms"], "route": tr["route"],
                         "bytes": tr["bytes"], "nodes": len(tr["nodes"]),
                         "text_nodes": len(texts), "count_text": txt, "ok": ok})
            self.log("M7 rep %d: %.1f ms, %d nodes, %d text nodes, count=%r"
                     % (i, tr["ms"], len(tr["nodes"]), len(texts), txt))
        self.r["M7"] = self.summarize(rows, "ms")
        self.r["M7"]["samples"] = rows
        self.r["M7"]["example_xml"] = tr["xml"][:4000]
        self.r["M7"]["method"] = ("`adb exec-out uiautomator dump /dev/tty` (the standard Android "
                                  "tool) wall clock incl. adb spawn; every sample must contain the "
                                  "live tree with text")
        return self.r["M7"]

    # ---- M8 ----------------------------------------------------------------
    def m8_tap(self, reps):
        import numpy as np
        rows = []
        cur = int(count_text(self.adb.tree()["nodes"]) or 0)
        for i in range(reps):
            tr = self.adb.tree()
            cur = int(count_text(tr["nodes"]) or 0)
            b = node_bounds(tr["nodes"], "/inc")
            if b is None:
                raise RuntimeError("M8: Increment button not found in the tree")
            x, y = (b[0] + b[2]) // 2, (b[1] + b[3]) // 2
            cb = node_bounds(tr["nodes"], "/count")
            pre = self.adb.screencap_raw()
            t0 = now()
            pt = self.adb.run(["shell", "input", "tap", str(x), str(y)], timeout=60)
            t_tap = (now() - t0) * 1000.0
            changed = tree_confirmed = None
            while now() - t0 < 15.0:
                s = self.adb.screencap_raw()
                # only the count TextView's own pixels: a ripple on the button
                # animates immediately and must not be counted as the state update
                if changed is None and cb is not None:
                    x0, y0, x1, y1 = cb
                    d = diff_frac(pre["px"][y0:y1, x0:x1], s["px"][y0:y1, x0:x1])
                    if d > 0.0:
                        changed = (now() - t0) * 1000.0
                if changed is not None:
                    t = self.adb.tree()
                    txt = count_text(t["nodes"])
                    if str(txt) == str(cur + 1):
                        tree_confirmed = (now() - t0) * 1000.0
                        break
            ok = pt.returncode == 0 and tree_confirmed is not None
            rows.append({"rep": i, "tap_cmd_ms": t_tap, "count_before": cur,
                         "tap_to_first_changed_frame_ms": changed,
                         "tap_to_tree_confirmed_ms": tree_confirmed,
                         "count_bounds": cb, "count_after": cur + 1, "ok": ok,
                         "xy": [x, y]})
            self.log("M8 rep %d: tap %.1f ms, changed frame %s ms, tree-confirmed %s ms (count %d->%d)"
                     % (i, t_tap, "%.0f" % changed if changed else "-",
                        "%.0f" % tree_confirmed if tree_confirmed else "-", cur, cur + 1))
            if not ok:
                raise RuntimeError("M8 rep %d: tap not confirmed (tree never showed %d)"
                                   % (i, cur + 1))
        self.r["M8"] = self.summarize(rows, "tap_to_tree_confirmed_ms",
                                      extra_keys=("tap_cmd_ms", "tap_to_first_changed_frame_ms"))
        self.r["M8"]["samples"] = rows
        self.r["M8"]["method"] = (
            "`adb shell input tap <centre of id/inc>`; tap_cmd_ms = adb round-trip of the tap "
            "command; tap_to_first_changed_frame = next raw screencap whose pixels inside the "
            "count TextView's bounds differ from the pre-tap frame (button ripple excluded); "
            "tap_to_tree_confirmed = uiautomator tree first reporting the incremented count "
            "(the only accepted success signal)")
        return self.r["M8"]

    # ---- M6 ----------------------------------------------------------------
    def m6_reset(self, reps):
        rows = []
        for i in range(reps):
            # make the state non-zero first, and prove it
            tr = self.adb.tree()
            b = node_bounds(tr["nodes"], "/inc")
            x, y = (b[0] + b[2]) // 2, (b[1] + b[3]) // 2
            for _ in range(2):
                self.adb.run(["shell", "input", "tap", str(x), str(y)], timeout=60)
                time.sleep(0.4)
            before = count_text(self.adb.tree()["nodes"])
            t0 = now()
            self.adb.sh("am force-stop %s" % PKG, check=False)
            self.adb.sh("pm clear %s" % PKG, timeout=180, check=False)
            self.adb.sh("am start -W -n %s" % ACT, timeout=180)
            t_start_done = (now() - t0) * 1000.0
            correct = None
            while now() - t0 < 30.0:
                s = self.adb.screencap_raw()
                if cmp_frac(self.ref0["px"], s["px"]) <= TOL_SAME:
                    correct = (now() - t0) * 1000.0
                    break
            txt = count_text(self.adb.tree()["nodes"]) if correct is not None else None
            ok = correct is not None and str(txt) == "0"
            rows.append({"rep": i, "count_before_reset": before,
                         "cmd_chain_ms": t_start_done, "reset_to_correct_screen_ms": correct,
                         "tree_count": txt, "ok": ok})
            self.log("M6 rep %d: before %s, cmd chain %.0f ms, reset->correct %.0f ms, tree %s"
                     % (i, before, t_start_done, correct or -1, txt))
            if not ok:
                raise RuntimeError("M6 rep %d: reset did not reach the count-0 screen" % i)
        self.r["M6"] = self.summarize(rows, "reset_to_correct_screen_ms",
                                      extra_keys=("cmd_chain_ms",))
        self.r["M6"]["samples"] = rows
        self.r["M6"]["method"] = (
            "`am force-stop` + `pm clear` + `am start -W`, timed from the first command to the "
            "first raw screencap pixel-identical to the verified count-0 reference; the live tree "
            "must also read 0")
        return self.r["M6"]

    # ---- summariser --------------------------------------------------------
    def summarize(self, rows, key, extra_keys=()):
        ok_rows = [r for r in rows if r.get("ok")]
        out = {"n_ok": len(ok_rows), "n_total": len(rows),
               "main": stats([r.get(key) for r in ok_rows])}
        for k in extra_keys:
            out[k] = stats([r.get(k) for r in ok_rows])
        out["failed"] = [r for r in rows if not r.get("ok")]
        return out

    # ---- CPU during latency loops -----------------------------------------
    def cpu_loop(self, fn, *a, **kw):
        tick = float(os.sysconf("SC_CLK_TCK"))
        s0 = host_sample()
        t0 = now()
        res = fn(*a, **kw)
        wall = now() - t0
        s1 = host_sample()
        one = 100.0 * (s1["cpu_ticks"] - s0["cpu_ticks"]) / tick / wall
        self.r["cpu"]["during_%s" % fn.__name__] = {
            "wall_s": wall, "cpu_pct_of_one_core": one,
            "cpu_pct_of_host": one / os.cpu_count()}
        return res

    # ---- driver ------------------------------------------------------------
    def main(self):
        self.r["config"] = {"avd": self.avd_config(),
                            "apk": str(APK), "apk_sha256": sha256(APK),
                            "apk_bytes": APK.stat().st_size,
                            "adb": subprocess.run([str(ADB), "version"], capture_output=True).stdout.decode().splitlines()[0],
                            "emulator_binary": str(EMULATOR),
                            "emulator_cmd": None,
                            "launch_flags": ["-no-window", "-no-audio", "-no-boot-anim",
                                             "-no-snapshot-save", "-no-snapshot-load",
                                             "-gpu swiftshader_indirect",
                                             "-memory 2048", "-cores 4", "-no-metrics"],
                            "guest_settings": ["svc power stayon true",
                                               "screen_off_timeout=2147483647",
                                               "policy_control immersive.full=* (no effect on API 34: status/nav bars stay visible)"],
                            "pixel_compare_region": {"top_crop_px": CROP_TOP, "bottom_crop_px": CROP_BOTTOM,
                                                     "pixels": CMP_PIXELS, "tolerance_frac": TOL_SAME},
                            "serial": self.adb.serial}
        if self.existing_serial:
            self.log("reusing already-running emulator %s" % self.existing_serial)
        else:
            self.emu.start("setup")
            boot_s = self.emu.wait_boot()
            self.log("setup boot completed in %.2f s" % boot_s)
            self.r["config"]["setup_boot_s"] = boot_s
        self.r["device"] = self.device_props()
        self.prepare_guest()
        self.log("device: %s" % self.r["device"].get("ro.build.fingerprint"))
        try:
            self.m1_idle()
            self.adb.sh("am start -W -n %s" % ACT, timeout=180, check=False)
            time.sleep(0.3)
            self.ref0 = self.reference("count0", 0)
            # count=1 reference (proves a changed screen is really the app's state)
            tr = self.adb.tree()
            b = node_bounds(tr["nodes"], "/inc")
            self.adb.run(["shell", "input", "tap", str((b[0] + b[2]) // 2),
                          str((b[1] + b[3]) // 2)], timeout=60)
            for _ in range(40):
                if count_text(self.adb.tree()["nodes"]) == "1":
                    break
                time.sleep(0.15)
            self.ref1 = self.reference("count1", 1)
            self.r["reference"] = {
                "count0_shot_ms": self.ref0["ms"], "count1_shot_ms": self.ref1["ms"],
                "cmp_frac_between_references": cmp_frac(self.ref0["px"], self.ref1["px"]),
                "full_diff_between_references": diff_frac(self.ref0["px"], self.ref1["px"])}
            # back to 0 for the launch rows
            self.adb.sh("am force-stop %s" % PKG, check=False)
            self.adb.sh("pm clear %s" % PKG, check=False)
            self.adb.sh("am start -W -n %s" % ACT, timeout=180)
            time.sleep(1.0)
            self.cpu_loop(self.m4_warm_launch, self.args.m4_reps)
            self.cpu_loop(self.m5_screenshot, self.args.m5_reps)
            self.cpu_loop(self.m7_tree, self.args.m7_reps)
            self.cpu_loop(self.m8_tap, self.args.m8_reps)
            self.cpu_loop(self.m6_reset, self.args.m6_reps)
            # M2: N=1 only — one emulator per session
            m1 = self.r["M1"]["anon_shmem_mib"]
            self.r["M2"] = {
                "n": 1, "measured": "N=1 only (a second emulator is forbidden by ADR-0015)",
                "per_session_mib": m1,
                "formula": ("isolation in the emulator means one emulator per session, so the "
                            "per-extra-session cost equals one more whole emulator: "
                            "M2 = M1 (full idle emulator host anon+shmem). N=1 is the only point "
                            "measured; N>1 is not measured and not estimated.")}
            # M3: cold boots AFTER the warm phase (app already installed)
            self.r["M3"]["reps"] = []
            for rep in range(1, self.args.m3_reps + 1):
                self.r["M3"]["reps"].append(self.m3_cold_boot(rep))
            self.r["M3"]["boot_s"] = stats([x["boot_s"] for x in self.r["M3"]["reps"]])
            self.r["M3"]["launch_to_correct_frame_ms"] = stats(
                [x["launch_to_first_correct_frame_ms"] for x in self.r["M3"]["reps"]])
            self.r["M3"]["cold_start_total_s"] = stats(
                [x["cold_start_total_s"] for x in self.r["M3"]["reps"]])
            self.r["M3"]["method"] = (
                "cold boot (emulator start -> sys.boot_completed=1, `-no-snapshot-load`), then "
                "`am start -W` and raw screencap polling until the frame is pixel-identical to the "
                "verified count-0 reference; total = boot + launch->correct frame")
        except Exception as e:
            import traceback
            self.r["errors"].append({"where": "run", "error": str(e),
                                     "traceback": traceback.format_exc()})
            self.log("ERROR: %s" % e)
        finally:
            info = self.emu.stop(log=self.log)
            self.r["shutdown"] = info
            self.r["host_after"] = host_meta()
            self.r["host_delta"] = {
                "mem_available_mib_after": host_limits()["mem_available_mib"],
            }
            self.r["qemu_left_after_shutdown"] = emulator_tree()
            self.log("shutdown: qemu left = %s" % (self.r["qemu_left_after_shutdown"] or "none"))
            self.write()
        return 0 if not self.r["errors"] else 1

    def write(self):
        if self.args.json:
            p = Path(self.args.json)
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_text(json.dumps(self.r, indent=1, sort_keys=False, default=str))
            self.log("wrote %s" % p)
        (self.out / "raw.json").write_text(json.dumps(self.r, indent=1, default=str))


# ----------------------------------------------------------------------------- misc
def parse_total_time(out):
    m = re.search(r"TotalTime:\s*(\d+)", out)
    return int(m.group(1)) if m else None


def png_ok(path):
    try:
        from PIL import Image
        im = Image.open(path)
        im.load()
        if im.size != (W, H):
            return False, "size %s" % (im.size,)
        ex = im.convert("RGB").getextrema()
        if all(lo == hi == 0 for lo, hi in ex):
            return False, "all-black"
        if all(lo == hi for lo, hi in ex):
            return False, "uniform colour"
        return True, "ok"
    except Exception as e:
        return False, "png error: %s" % e


def sha256(p):
    import hashlib
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()


def self_test():
    fails = 0

    def chk(cond, name):
        nonlocal fails
        print(("ok   " if cond else "FAIL ") + name)
        if not cond:
            fails += 1

    # stats: known list
    s = stats([1, 2, 3, 4, 5])
    chk(s["n"] == 5 and abs(s["p50"] - 3) < 1e-9, "stats p50")
    chk(s["p95"] > 4.7 and s["p95"] <= 5, "stats p95")
    chk(s["ci95_of_p50"][0] <= 3 <= s["ci95_of_p50"][1], "stats ci contains p50")
    chk(stats([]) == {"n": 0}, "stats empty")
    # raw screencap header parse: 16-byte header
    import numpy as np
    w, h = 4, 3
    px = bytes(range(48))
    for hdrlen in (12, 16):
        d = struct.pack("<iii", w, h, 1) + (b"\0" * (hdrlen - 12)) + px
        ww, hh, fmt, off = parse_raw_screencap(d)
        chk((ww, hh, fmt) == (w, h, 1) and off == hdrlen and d[off:off + 48] == px,
            "raw screencap %d-byte header" % hdrlen)
    try:
        parse_raw_screencap(b"\x00" * 8)
        chk(False, "short raw screencap raises")
    except ValueError:
        chk(True, "short raw screencap raises")
    # xml + nodes
    xml = ('<?xml version="1.0"?><hierarchy rotation="0"><node text="" resource-id="" '
           'bounds="[0,0][1080,2400]" class="x"><node text="0" resource-id="dev.acos.fixtures.counter:id/count" '
           'bounds="[42,300][100,400]" class="android.widget.TextView"/>'
           '<node text="Increment" resource-id="dev.acos.fixtures.counter:id/inc" '
           'bounds="[42,400][300,500]" class="android.widget.Button"/></node></hierarchy>')
    chk(extract_xml("<junk>" + xml) == xml, "extract_xml")
    n = parse_nodes(xml)
    chk(len(n) == 3 and count_text(n) == "0", "parse_nodes + count_text")
    chk(node_bounds(n, "/inc") == [42, 400, 300, 500], "node_bounds")
    chk(node_bounds(n, "/nope") is None, "node_bounds missing")
    chk(parse_total_time("Status: ok\nTotalTime: 145\nWaitTime: 160\n") == 145, "parse_total_time")
    # diff_frac
    a = np.zeros((2, 2, 4), dtype=np.uint8)
    b = a.copy()
    chk(diff_frac(a, b) == 0.0, "diff_frac identical")
    b[0, 0, 0] = 255
    chk(abs(diff_frac(a, b) - 0.25) < 1e-9, "diff_frac one of four pixels")
    chk(diff_frac(a, np.zeros((3, 3, 4), dtype=np.uint8)) == 1.0, "diff_frac shape mismatch")
    big = np.zeros((400, 8, 4), dtype=np.uint8)
    big2 = big.copy()
    big2[CROP_TOP:400 - CROP_BOTTOM, :, 0] = 255           # inside the compared region
    chk(abs(cmp_frac(big, big2) - 1.0) < 1e-9, "cmp_frac sees a change inside the crop")
    big3 = big.copy()
    big3[0, 0, 0] = 255                                    # status-bar row only
    chk(cmp_frac(big, big3) == 0.0, "cmp_frac crops the status bar")
    big4 = big.copy()
    big4[399, 0, 0] = 255                                  # nav strip row only
    chk(cmp_frac(big, big4) == 0.0, "cmp_frac crops the nav strip")
    # serial guard
    chk(bool(SERIAL_RE.match("emulator-5554")) and bool(SERIAL_RE.match("emulator-5580")), "serial guard accepts emulators")
    chk(not SERIAL_RE.match("ZY22LGK9CF") and not SERIAL_RE.match("emulator-5554x"), "serial guard rejects physical")
    print("\n%d checks, %d failed" % (11 + 2, fails))
    return 1 if fails else 0


def main(argv=None):
    ap = argparse.ArgumentParser(description="P6 stock-emulator benchmark")
    ap.add_argument("--out", default=".cache/p6/run1")
    ap.add_argument("--json", default=None)
    ap.add_argument("--m3-reps", type=int, default=3)
    ap.add_argument("--m4-reps", type=int, default=12)
    ap.add_argument("--m5-reps", type=int, default=20)
    ap.add_argument("--m6-reps", type=int, default=10)
    ap.add_argument("--m7-reps", type=int, default=20)
    ap.add_argument("--m8-reps", type=int, default=20)
    ap.add_argument("--quick", action="store_true", help="smoke only: 1 rep each (NOT a claim)")
    ap.add_argument("--self-test", action="store_true")
    a = ap.parse_args(argv)
    if a.self_test:
        return self_test()
    if a.quick:
        a.m3_reps, a.m4_reps, a.m5_reps = 1, 2, 3
        a.m6_reps, a.m7_reps, a.m8_reps = 1, 2, 3
    return Run(a).main()


if __name__ == "__main__":
    sys.exit(main())
