#!/usr/bin/env python3
"""P6 diagnostic: what is on screen after `am start -W`?

Boots the oracle emulator, installs counter, then after a force-stop + launch
captures the screen at a few timestamps, prints the current window focus and
saves downscaled PNGs so the frames can be inspected.  Not a metric run.

  python3 -I tools/art-runtime/p6/diag_frames.py --out .cache/p6/diag
"""
import argparse
import json
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import p6_emulator as P                                     # noqa: E402

ap = argparse.ArgumentParser()
ap.add_argument("--out", default=".cache/p6/diag")
ap.add_argument("--keep-boot", action="store_true")
a = ap.parse_args()
out = Path(a.out)
out.mkdir(parents=True, exist_ok=True)
ADB = P.Adb("emulator-5554")
EMU = P.Emulator(ADB, 5554, out / "logs")
rec = {}


def save(tag, shot):
    from PIL import Image
    p = out / ("frame-%s.png" % tag)
    Image.fromarray(shot["px"][:, :, :3]).save(p)
    Image.open(p).resize((270, 600)).save(out / ("small-%s.png" % tag))
    return str(p)


def focus():
    w = ADB.sh("dumpsys window", timeout=120)
    lines = [l.strip() for l in w.splitlines()
             if "mCurrentFocus" in l or "mFocusedApp" in l]
    return " | ".join(lines[:2])


try:
    EMU.start("diag")
    boot = EMU.wait_boot()
    rec["boot_s"] = boot
    ADB.run(["install", "-r", "-t", str(P.APK)], timeout=300)
    ADB.sh("wm dismiss-keyguard", check=False)
    ADB.sh("input keyevent 224", check=False)
    ADB.sh("svc power stayon true", check=False)
    ADB.sh("settings put system screen_off_timeout 2147483647", check=False)
    ADB.sh("settings put global policy_control immersive.full=*", check=False)
    rec["focus_after_boot"] = focus()
    ADB.sh("am force-stop %s" % P.PKG, check=False)
    ADB.sh("pm clear %s" % P.PKG, check=False)
    ADB.sh("am start -W -n %s" % P.ACT, timeout=180)
    time.sleep(6)
    ref = ADB.screencap_raw()
    rec["ref_tree"] = P.count_text(ADB.tree()["nodes"])
    rec["ref_png"] = save("ref", ref)
    rec["focus_ref"] = focus()
    rec["pixels_at"] = {}
    import numpy as np
    for name, y, x in (("0,0", 0, 0), ("mid", 1200, 540), ("count_area", 300, 60),
                       ("bottom", 2380, 540)):
        rec["pixels_at"][name] = ref["px"][y, x][:3].tolist()
    ADB.sh("am force-stop %s" % P.PKG, check=False)
    time.sleep(1.5)
    launch = ADB.screencap_raw()
    rec["launcher_png"] = save("launcher", launch)
    rec["focus_launcher"] = focus()
    rec["diff_launcher_vs_ref"] = P.diff_frac(launch["px"], ref["px"])
    t0 = P.now()
    am = ADB.sh("am start -W -n %s" % P.ACT, timeout=180)
    rec["am_start"] = am
    rec["am_wall_ms"] = (P.now() - t0) * 1000
    steps = []
    for target in (0.4, 1.0, 2.0, 3.0, 4.0, 5.5, 7.0):
        while (P.now() - t0) < target:
            time.sleep(0.02)
        s = ADB.screencap_raw()
        d = P.diff_frac(ref["px"], s["px"])
        steps.append({"t_ms": (P.now() - t0) * 1000, "diff_vs_ref": d,
                      "png": save("t%d" % int(target * 1000), s),
                      "focus": focus()})
        print(json.dumps({k: v for k, v in steps[-1].items() if k != "focus"}), flush=True)
        print("   focus:", steps[-1]["focus"], flush=True)
    rec["steps"] = steps
    rec["tree_after"] = P.count_text(ADB.tree()["nodes"])
finally:
    rec["shutdown"] = EMU.stop()
    (out / "diag.json").write_text(json.dumps(rec, indent=1, default=str))
    print(json.dumps({k: v for k, v in rec.items() if k not in ("steps", "am_start")}, indent=1)[:3000])
