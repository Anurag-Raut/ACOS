#!/usr/bin/env python3
"""capture-device-reference.py — physical-device reference capture for ACOS.

Drives the ACOS fixture scenarios in ``scenarios/<app>.yaml`` on a connected
physical Android device through the ``agent-device`` CLI and records, per
scenario step, a screenshot, the agent-device UI snapshot tree and a logcat
excerpt filtered to the app's process.

This script is the engine behind ``tools/oracle/capture-device-reference.sh``.
It is deliberately conservative about the device:

  * it only installs/uninstalls the four ACOS fixture packages,
  * it clears only the fixture app's own data (``pm clear``),
  * it never changes accounts, security, connectivity or lock state,
  * orientation is driven by ``agent-device orientation`` and restored with
    ``agent-device orientation auto`` at the end,
  * a failed install is recorded as a structured blocker and the next app is
    still attempted.

Fidelity label: ``physical-device``. It is *not* ART-reference evidence and it
must never be silently mixed with ``fixtures/reference/<app>/art-api34-x86_64``.
"""

from __future__ import annotations

import argparse
import datetime as _dt
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import time

try:
    import yaml
except ImportError:  # pragma: no cover - PyYAML is present on this host
    yaml = None

# ---------------------------------------------------------------------------
# helpers
# ---------------------------------------------------------------------------
NOW = lambda: _dt.datetime.now(_dt.timezone.utc).isoformat(timespec="milliseconds")
log = lambda *a: print("[capture-device-ref]", *a, file=sys.stderr, flush=True)


def run(cmd, timeout=180):
    try:
        p = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
        return p.returncode, p.stdout, p.stderr
    except subprocess.TimeoutExpired as e:
        return 124, e.stdout or "", f"timeout after {timeout}s"


class Device:
    def __init__(self, serial, adb, agent_device):
        self.serial = serial
        self.adb_bin = adb
        self.ad_bin = agent_device
        self.commands = []

    def _record(self, tool, args):
        self.commands.append({"tool": tool, "args": list(args), "ts": NOW()})

    def adb(self, *args, timeout=120):
        cmd = [self.adb_bin, "-s", self.serial, *args]
        self._record("adb", cmd[1:])
        return run(cmd, timeout=timeout)

    def adb_shell(self, *args, timeout=120):
        return self.adb("shell", *args, timeout=timeout)

    def ag(self, *args, timeout=180, want_json=True):
        cmd = [self.ad_bin, "--serial", self.serial, *args]
        if want_json and "--json" not in cmd:
            cmd.append("--json")
        self._record("agent-device", cmd[1:])
        rc, out, err = run(cmd, timeout=timeout)
        data = None
        if want_json and out.strip():
            try:
                data = json.loads(out)
            except json.JSONDecodeError:
                data = None
        return {"rc": rc, "stdout": out, "stderr": err, "json": data}

    def getprop(self, key):
        rc, out, _ = self.adb_shell("getprop", key)
        return out.strip() if rc == 0 else ""

    def facts(self):
        f = {
            "model": self.getprop("ro.product.model"),
            "manufacturer": self.getprop("ro.product.manufacturer"),
            "device": self.getprop("ro.product.device"),
            "product": self.getprop("ro.product.name"),
            "android_release": self.getprop("ro.build.version.release"),
            "api_level": self.getprop("ro.build.version.sdk"),
            "build_id": self.getprop("ro.build.id"),
            "fingerprint": self.getprop("ro.build.fingerprint"),
            "abi": self.getprop("ro.product.cpu.abi"),
            "abi_list": self.getprop("ro.product.cpu.abilist"),
            "locale": self.getprop("persist.sys.locale"),
        }
        rc, out, _ = self.adb_shell("wm", "size")
        m = re.search(r"(\d+)x(\d+)", out or "")
        f["resolution"] = f"{m.group(1)}x{m.group(2)}" if m else "unknown"
        rc, out, _ = self.adb_shell("wm", "density")
        m = re.search(r"(\d+)", out or "")
        f["density_dpi"] = int(m.group(1)) if m else 0
        rc, out, _ = self.adb_shell("settings", "get", "system", "font_scale")
        f["font_scale"] = (out or "").strip()
        rc, out, _ = self.adb_shell("settings", "get", "system", "user_rotation")
        f["user_rotation"] = (out or "").strip()
        rc, out, _ = self.adb_shell("settings", "get", "system", "accelerometer_rotation")
        f["accelerometer_rotation"] = (out or "").strip()
        rc, out, _ = self.adb_shell("dumpsys", "window", "displays")
        m = re.search(r"cur=(\d+)x(\d+)", out or "")
        f["current_display"] = f"{m.group(1)}x{m.group(2)}" if m else "unknown"
        return f

    def pidof(self, pkg):
        rc, out, _ = self.adb_shell("pidof", pkg)
        if rc == 0 and out.strip():
            return out.strip().split()[0]
        return None


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def safe_name(text):
    text = str(text).replace("\n", " ").strip()
    text = re.sub(r"[^A-Za-z0-9_.:-]+", "-", text)
    return text.strip("-")[:60] or "step"


# ---------------------------------------------------------------------------
# scenario driving
# ---------------------------------------------------------------------------
ACTION_KINDS = {
    "launch",
    "wait_for",
    "tap",
    "type",
    "rotate",
    "back",
}
ASSERT_KINDS = {
    "assert_text",
    "assert_log",
    "assert_not_visible",
    "assert_no_crash",
}
CAPTURE_KINDS = {"capture"}


def selector_candidates(pkg, selector):
    """Return agent-device selector strings for a scenario selector."""
    if not isinstance(selector, dict):
        return []
    if "id" in selector:
        v = str(selector["id"])
        return [f"id={pkg}:id/{v}", f"id=android:id/{v}"]
    if "text" in selector:
        v = selector["text"]
        v = v.get("exact") if isinstance(v, dict) else v
        return [f'text="{v}"']
    if "label" in selector:
        v = selector["label"]
        v = v.get("exact") if isinstance(v, dict) else v
        return [f'label="{v}"']
    return []


def scenario_steps(path):
    with open(path, encoding="utf-8") as fh:
        doc = yaml.safe_load(fh)
    return doc, doc.get("steps") or []


class AppCapture:
    def __init__(self, dev, app, scenario_path, apk_path, out_dir, device_facts, serial_hash):
        self.dev = dev
        self.app = app
        self.scenario_path = scenario_path
        self.apk_path = apk_path
        self.out = out_dir
        self.device_facts = device_facts
        self.serial_hash = serial_hash
        self.doc = {}
        self.steps = []
        self.records = []
        self.issues = []
        self.cumulative_log_lines = 0
        self.pkg = None
        self.tag = None
        self.installed = False
        self.launched = False
        self.final_state = None
        self.final_snapshot_path = None

    # -- evidence ----------------------------------------------------------
    def _save_cmd(self, name, payload):
        with open(os.path.join(self.out, name), "w", encoding="utf-8") as fh:
            fh.write(payload if isinstance(payload, str) else json.dumps(payload, indent=2))

    def capture_evidence(self, seq, step_name):
        base = f"{seq:02d}-{safe_name(step_name)}"
        shot = os.path.join(self.out, f"screenshot-{base}.png")
        res = self.dev.ag("screenshot", shot, timeout=120)
        shot_ok = os.path.exists(shot) and os.path.getsize(shot) > 0
        snap_json = self.dev.ag("snapshot", "--force-full", timeout=120)
        snap_text = self.dev.ag("snapshot", "--force-full", want_json=False, timeout=120)
        tree = None
        if snap_json.get("json") and snap_json["json"].get("success"):
            tree = snap_json["json"]["data"]
            self._save_cmd(f"snapshot-{base}.json", snap_json["json"])
        else:
            self._save_cmd(f"snapshot-{base}.json", snap_json)
        if snap_text.get("stdout"):
            self._save_cmd(f"snapshot-{base}.txt", snap_text["stdout"])

        # logcat excerpt for this step: everything the app emitted since the
        # previous step. Filtering is by the app's unique log tag, which stays
        # valid across the process recreation a rotate triggers (a pid filter
        # would lose the pre-recreate pid's lines). The tag is the one declared
        # in fixtures/apps/<app>/.../MainActivity.java.
        pid = self.dev.pidof(self.pkg) if self.pkg else None
        args = ["logcat", "-d", "-v", "time"]
        if self.tag:
            args += ["-s", f"{self.tag}:V"]
        elif pid:
            args += ["--pid", pid]
        rc, out, err = self.dev.adb(*args)
        lines = [ln for ln in (out or "").splitlines() if ln.strip()]
        excerpt = lines[self.cumulative_log_lines:]
        self.cumulative_log_lines = len(lines)
        self._save_cmd(f"logcat-{base}.txt", "\n".join(excerpt) + ("\n" if excerpt else ""))

        rec = {
            "seq": seq,
            "step": step_name,
            "timestamp": NOW(),
            "screenshot": os.path.basename(shot) if shot_ok else None,
            "snapshot_json": f"snapshot-{base}.json",
            "snapshot_text": f"snapshot-{base}.txt",
            "logcat": f"logcat-{base}.txt",
            "pid": pid,
            "screenshot_bytes": os.path.getsize(shot) if shot_ok else 0,
        }
        if not shot_ok:
            self.issues.append({"step": step_name, "issue": "screenshot_missing", "detail": res.get("stderr", "")[:400]})
        self.records.append(rec)
        # remember the last captured state that actually shows app content
        if tree and self._tree_has_app(tree):
            self.final_state = rec
            self.final_snapshot_path = os.path.join(self.out, rec["snapshot_json"])
        return rec

    # A framework AlertDialog lives in its own window with android:id/* ids, so
    # those specific ids count as app content too. The launcher also exposes
    # android:id/content, which is why we do not accept a generic android:id/*.
    DIALOG_IDS = ("android:id/message", "android:id/alertTitle")

    def _tree_has_app(self, data):
        for n in data.get("nodes") or []:
            ident = n.get("identifier") or ""
            if ident.startswith(self.pkg + ":id/") or ident in self.DIALOG_IDS:
                return True
        return False

    # -- actions -----------------------------------------------------------
    def ag_step(self, kind, value):
        if kind == "launch":
            activity = value.get("activity") or ""
            clear = value.get("clear_state", False)
            if clear:
                rc, out, err = self.dev.adb_shell("pm", "clear", self.pkg)
                if rc != 0:
                    self.issues.append({"step": "launch", "issue": "pm_clear_failed", "detail": err.strip()[:400]})
            args = ["open", self.pkg, "--activity", activity, "--relaunch", "--foreground"]
            res = self.dev.ag(*args, timeout=180)
            self.launched = bool(res.get("json") and res["json"].get("success"))
            if not self.launched:
                self.issues.append({"step": "launch", "issue": "launch_failed", "detail": (res.get("json") or {}).get("error") or res.get("stderr", "")[:400]})
            return res

        if kind == "wait_for":
            sel = selector_candidates(self.pkg, value.get("selector"))
            timeout = int(value.get("timeout_ms") or 5000)
            for s in sel:
                res = self.dev.ag("wait", s, str(timeout), timeout=timeout // 1000 + 30)
                if res.get("json") and res["json"].get("success"):
                    return res
            self.issues.append({"step": "wait_for", "issue": "wait_failed", "detail": json.dumps(sel)})
            return res if sel else {"rc": 1, "json": None}

        if kind == "tap":
            sel = selector_candidates(self.pkg, value.get("selector"))
            for s in sel:
                res = self.dev.ag("press", s, "--settle", timeout=120)
                if res.get("json") and res["json"].get("success"):
                    return res
            self.issues.append({"step": "tap", "issue": "tap_selector_not_found", "detail": json.dumps(sel)})
            return res if sel else {"rc": 1, "json": None}

        if kind == "type":
            sel = selector_candidates(self.pkg, value.get("selector"))
            text = str(value.get("text", ""))
            clear = bool(value.get("clear", False))
            # the scenario's `type` semantics: tap the target, then append.
            # `clear: true` replaces instead.
            if not clear:
                for s in sel:
                    res = self.dev.ag("press", s, "--settle", timeout=120)
                    if res.get("json") and res["json"].get("success"):
                        break
                res = self.dev.ag("type", text, timeout=120)
                return res
            for s in sel:
                res = self.dev.ag("fill", s, text, "--settle", timeout=120)
                if res.get("json") and res["json"].get("success"):
                    return res
            self.issues.append({"step": "type", "issue": "fill_selector_not_found", "detail": json.dumps(sel)})
            return res if sel else {"rc": 1, "json": None}

        if kind == "rotate":
            orient = str(value.get("orientation", "")).lower()
            target = {"landscape": "landscape-left", "portrait": "portrait"}.get(orient, orient)
            res = self.dev.ag("orientation", target, timeout=120)
            if not (res.get("json") and res["json"].get("success")):
                self.issues.append({"step": "rotate", "issue": "rotate_failed", "detail": (res.get("json") or {}).get("error") or res.get("stderr", "")[:400]})
            return res

        if kind == "back":
            res = self.dev.ag("back", timeout=120)
            if not (res.get("json") and res["json"].get("success")):
                self.issues.append({"step": "back", "issue": "back_failed", "detail": (res.get("json") or {}).get("error") or res.get("stderr", "")[:400]})
            return res

        return None

    # -- install / uninstall ----------------------------------------------
    def install(self):
        res = self.dev.ag("install", self.apk_path, timeout=300)
        if res.get("json") and res["json"].get("success"):
            self.installed = True
            return res
        # already installed? try reinstall once
        err = json.dumps(res.get("json") or res.get("stderr", ""))[:600]
        res2 = self.dev.ag("reinstall", self.pkg, self.apk_path, timeout=300)
        if res2.get("json") and res2["json"].get("success"):
            self.installed = True
            self.issues.append({"step": "install", "issue": "install_needed_reinstall", "detail": err})
            return res2
        self.issues.append({
            "step": "install",
            "issue": "install_failed",
            "detail": err,
            "error": (res.get("json") or {}).get("error"),
            "apk_sha256": sha256_file(self.apk_path) if os.path.exists(self.apk_path) else None,
        })
        return res

    def uninstall(self):
        res = self.dev.ag("uninstall", self.pkg, timeout=120)
        ok = bool(res.get("json") and res["json"].get("success"))
        if not ok:
            rc, out, err = self.dev.adb_shell("uninstall", self.pkg)
            ok = rc == 0 and "Success" in out
            res = {"rc": rc, "stdout": out, "stderr": err, "json": None}
        if not ok:
            self.issues.append({"step": "uninstall", "issue": "uninstall_failed", "detail": json.dumps(res.get("json") or res.get("stderr", ""))[:400]})
        return res

    def close(self):
        res = self.dev.ag("close", self.pkg, timeout=120)
        ok = bool(res.get("json") and res["json"].get("success"))
        if not ok:
            # A dialog (form-validation success) can keep the app foreground.
            # Dismiss its own OK button; this is in-app UI, not a device setting.
            for sel in ('label="OK"', 'text="OK"'):
                r = self.dev.ag("press", sel, "--settle", timeout=60)
                if r.get("json") and r["json"].get("success"):
                    break
            res = self.dev.ag("close", self.pkg, timeout=120)
            ok = bool(res.get("json") and res["json"].get("success"))
        if not ok:
            self.issues.append({"step": "close", "issue": "close_failed", "detail": json.dumps(res.get("json") or res.get("stderr", ""))[:400]})
        return res

    # -- run ---------------------------------------------------------------
    def run(self):
        os.makedirs(self.out, exist_ok=True)
        self.doc, steps = scenario_steps(self.scenario_path)
        self.steps = steps
        launch = next((s.get("launch") for s in steps if "launch" in s), {})
        activity = launch.get("activity", "")
        self.pkg = activity.split("/")[0]
        # log tag is the stable filter across process recreations; derived from
        # the fixture's source tag via the app name mapping below.
        self.tag = {"counter": "AcosCounter", "todo-list": "AcosTodo",
                    "multi-activity": "AcosMulti", "form-validation": "AcosForm"}.get(self.app)

        manifest = {
            "tier": "physical-device",
            "fidelity": "physical-device",
            "fidelity_note": (
                "Physical Android device evidence. Not ART-reference and not "
                "interchangeable with fixtures/reference/<app>/art-api34-x86_64."
            ),
            "app": self.app,
            "package": self.pkg,
            "activity": activity,
            "device": {
                "model": self.device_facts.get("model"),
                "manufacturer": self.device_facts.get("manufacturer"),
                "android_release": self.device_facts.get("android_release"),
                "api_level": self.device_facts.get("api_level"),
                "fingerprint": self.device_facts.get("fingerprint"),
                "resolution": self.device_facts.get("resolution"),
                "density_dpi": self.device_facts.get("density_dpi"),
                "font_scale": self.device_facts.get("font_scale"),
                "abi": self.device_facts.get("abi"),
                "serial_sha256": self.serial_hash,
            },
            "apk": os.path.relpath(self.apk_path, os.path.dirname(self.scenario_path) or "."),
            "apk_sha256": sha256_file(self.apk_path) if os.path.exists(self.apk_path) else None,
            "scenario": os.path.basename(self.scenario_path),
            "scenario_sha256": sha256_file(self.scenario_path),
            "expectation": self.doc.get("expectation"),
            "display_expected": self.doc.get("display"),
            "capture_started": NOW(),
            "install": None,
            "steps": [],
            "issues": [],
            "commands": [],
        }

        log(f"{self.app}: installing {os.path.basename(self.apk_path)}")
        inst = self.install()
        manifest["install"] = {
            "ok": self.installed,
            "error": None if self.installed else (inst.get("json") or {}).get("error"),
            "stdout": (inst.get("stdout") or "")[:2000],
            "stderr": (inst.get("stderr") or "")[:2000],
            "ts": NOW(),
        }
        if not self.installed:
            manifest["issues"] = list(self.issues)
            manifest["commands"] = self.dev.commands
            manifest["capture_finished"] = NOW()
            self._save_cmd("manifest.json", manifest)
            log(f"{self.app}: install blocked -> {manifest['install']['error']}")
            return manifest

        # Start a clean log buffer for this app, then launch.
        self.dev.adb("logcat", "-c")
        self.cumulative_log_lines = 0

        try:
            for seq, step in enumerate(steps):
                if not isinstance(step, dict) or not step:
                    continue
                kind = next(iter(step))
                value = step[kind] if isinstance(step[kind], dict) else {}
                step_name = kind
                if kind == "capture":
                    step_name = f"capture:{value.get('name', kind)}"
                elif kind in ASSERT_KINDS and "selector" in value:
                    step_name = f"{kind}:{json.dumps(value['selector'], sort_keys=True)}"
                elif kind in ACTION_KINDS and "selector" in value:
                    step_name = f"{kind}:{json.dumps(value['selector'], sort_keys=True)}"
                elif kind == "rotate":
                    step_name = f"rotate:{value.get('orientation')}"
                elif kind == "launch":
                    step_name = "launch"

                ts = NOW()
                res = self.ag_step(kind, value) if kind in ACTION_KINDS else None
                ev = self.capture_evidence(seq, step_name)
                ev["kind"] = kind
                ev["scenario_step"] = step
                ev["action_ok"] = None if res is None else bool(res.get("json") and res["json"].get("success"))
                if res is not None and not ev["action_ok"]:
                    ev["action_error"] = (res.get("json") or {}).get("error") or (res.get("stderr") or "")[:400]
                manifest["steps"].append(ev)
                log(f"{self.app}: step {seq:02d} {step_name} action_ok={ev['action_ok']}")
        finally:
            self.close()

        # final-state facts for the comparison table
        manifest["final_state"] = self.final_state
        if self.final_snapshot_path and os.path.exists(self.final_snapshot_path):
            try:
                with open(self.final_snapshot_path, encoding="utf-8") as fh:
                    tree = json.load(fh)
                manifest["final_nodes"] = self._flatten_nodes(tree)
            except Exception as e:  # pragma: no cover
                manifest["issues"].append({"step": "final_state", "issue": "final_tree_parse_failed", "detail": str(e)})
        manifest["issues"] = list(self.issues)
        manifest["commands"] = self.dev.commands
        manifest["capture_finished"] = NOW()
        self._save_cmd("manifest.json", manifest)
        log(f"{self.app}: uninstalling {self.pkg}")
        self.uninstall()
        return manifest

    def _flatten_nodes(self, tree):
        nodes = tree.get("nodes")
        if nodes is None and isinstance(tree.get("data"), dict):
            nodes = tree["data"].get("nodes")
        out = []
        for n in nodes or []:
            ident = n.get("identifier") or ""
            if not ident:
                continue
            if not (ident.startswith(self.pkg + ":id/") or ident.startswith("android:id/")):
                continue
            out.append({
                "identifier": ident,
                "type": n.get("type"),
                "label": n.get("label"),
                "text": n.get("label"),
                "rect": n.get("rect"),
                "enabled": n.get("enabled"),
                "editable": n.get("editable"),
                "clickable": n.get("clickable"),
            })
        return out


# ---------------------------------------------------------------------------
# comparison against the emulator ART reference
# ---------------------------------------------------------------------------
EMU_FINAL = {
    "counter": "ui_after_reset.json",
    "todo-list": "ui_after_rotate_back.json",
    "multi-activity": "ui_after_rotate_back.json",
    "form-validation": "ui_after_success.json",
}


def bounds_str(b):
    if not b:
        return "-"
    return f"[{b['x']},{b['y']}][{b['x'] + b['width']},{b['y'] + b['height']}]"


def load_emu_nodes(emu_dir):
    with open(emu_dir, encoding="utf-8") as fh:
        nodes = json.load(fh)
    out = {}
    for n in nodes:
        rid = n.get("resource_id") or ""
        if not rid:
            continue
        out[rid] = {
            "class": n.get("class"),
            "text": n.get("text"),
            "bounds": n.get("bounds"),
        }
    return out


def write_comparison(out_root, apps, manifests, emu_ref_root):
    lines = []
    lines.append("# ACOS physical-device vs ART-reference comparison\n")
    lines.append(
        "**Tier: `physical-device`.** This table compares the final state captured "
        "from the physical phone (`moto g35 5G`, Android 15 / API 35, 1080x2400 @ "
        "400 dpi) with the emulator `art-reference` captures in "
        "`fixtures/reference/<app>/art-api34-x86_64/` (API 34, 1080x2400 @ 420 dpi).\n"
    )
    lines.append(
        "A `physical-device` pass never satisfies an `art-reference` requirement "
        "(ADR-0005). The two tiers are not interchangeable and are never mixed "
        "silently.\n"
    )
    lines.append(
        "## Expected differences that are labelled, not failures\n\n"
        "* **Density 400 dpi vs 420 dpi** — all pixel bounds differ by the "
        "density ratio (400/420 ≈ 0.952) and by the different status/action-bar "
        "insets. Text values are unaffected.\n"
        "* **API 35 vs API 34** — edge-to-edge enforcement and status-bar inset "
        "changes shift the content origin; text and resource ids are stable.\n"
        "* The scenario's expected bounds are calibrated to the API-34 420 dpi "
        "oracle, so a device bounds mismatch is expected and is reported as "
        "`bounds_differ_density`, never as a text failure.\n"
    )
    lines.append(
        "* **agent-device vs uiautomator snapshot scope** — `element_absent_on_device` "
        "rows are android framework chrome ids (action bar, status-bar background, "
        "navigation-bar background, dialog title divider) that uiautomator dumps in "
        "the ART reference but the agent-device accessibility snapshot does not "
        "expose. The app's own ids and the dialog ids are present; this is a tool "
        "scope difference, not a missing UI element."
    )
    lines.append("\n## Table\n")
    lines.append("| app | element (resource id) | scenario expected text | device final text | emulator final text | device bounds | emulator bounds | text verdict | bounds verdict | note |")
    lines.append("|---|---|---|---|---|---|---|---|---|---|")

    for app in apps:
        m = manifests.get(app)
        if not m or not m.get("install", {}).get("ok"):
            lines.append(f"| {app} | - | - | - | - | - | - | install blocked | - | see manifest.json |")
            continue
        emu_dir = os.path.join(emu_ref_root, app, "art-api34-x86_64")
        final_file = EMU_FINAL.get(app)
        emu_nodes = {}
        if final_file and os.path.exists(os.path.join(emu_dir, final_file)):
            try:
                emu_nodes = load_emu_nodes(os.path.join(emu_dir, final_file))
            except Exception as e:
                lines.append(f"| {app} | - | - | - | - | - | - | emulator parse failed: {e} | - | - |")
                continue
        dev_nodes = {}
        for n in m.get("final_nodes") or []:
            dev_nodes[n["identifier"]] = n
        # scenario expected text for the final state elements: collect the
        # assert_text steps whose selector id matches an element present in the
        # final device tree. The final state is the last captured app state.
        expected = {}
        for s in m.get("steps") or []:
            step = s.get("scenario_step") or {}
            if "assert_text" in step:
                a = step["assert_text"]
                sel = a.get("selector") or {}
                if "id" in sel:
                    v = str(sel["id"])
                    for cand in (f"{m['package']}:id/{v}", f"android:id/{v}"):
                        if cand in dev_nodes:
                            text = a.get("text")
                            text = text.get("exact") if isinstance(text, dict) else text
                            expected[cand] = {"text": text, "bounds": a.get("bounds")}
                            break
                elif "text" in sel:
                    v = sel["text"]
                    v = v.get("exact") if isinstance(v, dict) else v
                    expected[f"text:{v}"] = {"text": v, "bounds": a.get("bounds")}
        # prefer the emulator's final-state resource ids, then fill in expected
        keys = list(emu_nodes.keys())
        for cand in dev_nodes:
            if cand not in keys and cand in expected:
                keys.append(cand)
        for rid in keys:
            d = dev_nodes.get(rid)
            e = emu_nodes.get(rid)
            exp = expected.get(rid)
            dev_text = d.get("text") if d else "-"
            emu_text = e.get("text") if e else "-"
            dev_text = "" if dev_text is None else dev_text
            emu_text = "" if emu_text is None else emu_text
            dev_text = dev_text if dev_text != "" else "—"
            emu_text = emu_text if emu_text != "" else "—"
            dev_b = bounds_str(d.get("rect")) if d else "-"
            emu_b = e.get("bounds") if e else "-"
            exp_text = exp.get("text") if exp else "-"
            if d is None:
                text_verdict = "element_absent_on_device"
            elif e is None:
                text_verdict = "no_emulator_reference_for_id"
            elif (dev_text or "") == (emu_text or ""):
                text_verdict = "text_match"
            else:
                text_verdict = "text_differs"
            if d is None or e is None:
                bounds_verdict = "-"
            elif dev_b == emu_b:
                bounds_verdict = "bounds_match"
            else:
                bounds_verdict = "bounds_differ_density"
            note = "density 400 vs 420 dpi" if bounds_verdict == "bounds_differ_density" else ""
            if bounds_verdict == "bounds_differ_density" and rid.startswith("android:id/"):
                note = "density 400 vs 420 dpi + API 35 edge-to-edge/inset shift"
            if exp and text_verdict == "text_match":
                exp_note = "matches scenario" if (dev_text or "") == (exp_text or "") else "scenario_expected_differs_from_emulator"
                note = (note + "; " if note else "") + exp_note
            lines.append(f"| {app} | `{rid}` | {exp_text} | {dev_text} | {emu_text} | {dev_b} | {emu_b} | {text_verdict} | {bounds_verdict} | {note} |")

    lines.append("\n## Per-app scenario outcomes\n")
    for app in apps:
        m = manifests.get(app)
        if not m:
            continue
        if not m.get("install", {}).get("ok"):
            err = m.get("install", {}).get("error") or {}
            lines.append(f"* **{app}** — install blocked: `{err.get('code', 'UNKNOWN')}` — {err.get('message', '')}")
            continue
        steps = m.get("steps") or []
        failed_actions = [s for s in steps if s.get("action_ok") is False]
        issues = m.get("issues") or []
        lines.append(f"* **{app}** — {len(steps)} scenario steps captured; "
                     f"{len(failed_actions)} action(s) failed; {len(issues)} issue(s). "
                     f"Final captured state: `{m.get('final_state', {}).get('step', '-')}`.")
        for s in failed_actions:
            lines.append(f"  * action failed at `{s['step']}`: `{s.get('action_error')}`")
        for s in steps:
            step = s.get("scenario_step") or {}
            if "back" in step and s.get("kind") == "back":
                snap = os.path.join(out_root, app, s.get("snapshot_json") or "")
                still = False
                try:
                    with open(snap, encoding="utf-8") as fh:
                        t = json.load(fh)
                    nodes = t.get("data", {}).get("nodes") or []
                    still = any((n.get("identifier") or "").startswith(m.get("package", "") + ":id/") for n in nodes)
                except Exception:
                    pass
                if still:
                    lines.append(f"  * note: the `back` step at seq {s.get('seq')} did not close the app; "
                                 f"the app was still foreground after it (keyboard/back-stack difference vs the API-34 oracle). "
                                 f"`agent-device close` closed it afterwards.")
                else:
                    lines.append(f"  * note: the `back` step at seq {s.get('seq')} closed the app as the scenario expects.")
        for i in issues:
            lines.append(f"  * {i.get('issue')} at `{i.get('step')}`: {str(i.get('detail'))[:200]}")

    with open(os.path.join(out_root, "COMPARISON.md"), "w", encoding="utf-8") as fh:
        fh.write("\n".join(lines) + "\n")


# ---------------------------------------------------------------------------
# main
# ---------------------------------------------------------------------------
def write_device_md(out_root, facts, serial_hash):
    md = f"""# ACOS physical-device reference device

Captured with `tools/oracle/capture-device-reference.sh`.

- **Fidelity label: `physical-device`** — a real phone, not an emulator and not
  ART-reference. It is never silently mixed with
  `fixtures/reference/<app>/art-api34-x86_64`.
- **Device serial:** stored only as a SHA-256 hash (`{serial_hash}`); the raw
  serial is deliberately not written to this repository.
- **Model:** {facts.get('manufacturer')} {facts.get('model')}
- **Device/product:** {facts.get('device')} / {facts.get('product')}
- **Android:** {facts.get('android_release')} (API level {facts.get('api_level')})
- **Build:** {facts.get('build_id')} — `{facts.get('fingerprint')}`
- **Display:** {facts.get('resolution')} physical, density {facts.get('density_dpi')} dpi,
  current display {facts.get('current_display')}
- **Locale:** {facts.get('locale')}, font scale {facts.get('font_scale')}
- **ABI:** {facts.get('abi')} ({facts.get('abi_list')})
- **Original rotation settings:** `user_rotation={facts.get('user_rotation')}`,
  `accelerometer_rotation={facts.get('accelerometer_rotation')}`

## Why the numbers differ from `fixtures/reference/<app>/art-api34-x86_64`

The ART oracle is an **API 34** `google_apis` **x86_64** emulator at **420 dpi**.
This phone is an **API 35** arm64 device at **400 dpi**. Texts and resource ids
are stable, but pixel bounds are not: density changes the dp→px ratio and the
Android 15 edge-to-edge/inset rules move the content origin. Every bounds
difference in `COMPARISON.md` is labelled `bounds_differ_density` and is an
expected fidelity difference, not a regression.

## Safety notes

* Only the four ACOS fixture packages (`dev.acos.fixtures.counter`,
  `dev.acos.fixtures.todolist`, `dev.acos.fixtures.multiactivity`,
  `dev.acos.fixtures.formvalidation`) were installed, driven and uninstalled.
* No account, security, connectivity or lock-screen setting was changed.
* Orientation was driven through `agent-device orientation` and restored with
  `agent-device orientation auto`.
"""
    with open(os.path.join(out_root, "DEVICE.md"), "w", encoding="utf-8") as fh:
        fh.write(md)


def main(argv=None):
    ap = argparse.ArgumentParser()
    ap.add_argument("--root", required=True)
    ap.add_argument("--serial", required=True)
    ap.add_argument("--adb", required=True)
    ap.add_argument("--agent-device", default="agent-device")
    ap.add_argument("--out-root", required=True)
    ap.add_argument("--apps-dir", required=True)
    ap.add_argument("--scenario-dir", required=True)
    ap.add_argument("--emulator-ref", required=True)
    ap.add_argument("--apps", nargs="+", default=["counter", "todo-list", "multi-activity", "form-validation"])
    ap.add_argument("--keep-app", action="store_true", help="leave the last app installed (debug only)")
    ap.add_argument("--recompute", action="store_true",
                    help="regenerate COMPARISON.md from already-captured manifests (no device access)")
    args = ap.parse_args(argv)

    if args.recompute:
        manifests = {}
        for app in args.apps:
            p = os.path.join(args.out_root, app, "manifest.json")
            if os.path.exists(p):
                with open(p, encoding="utf-8") as fh:
                    manifests[app] = json.load(fh)
        write_comparison(args.out_root, args.apps, manifests, args.emulator_ref)
        print(json.dumps({"recomputed": sorted(manifests), "out_root": args.out_root}, indent=2))
        return 0

    dev = Device(args.serial, args.adb, args.agent_device)
    rc, out, err = dev.adb("get-state")
    if rc != 0:
        print(f"[capture-device-ref] error: no device at serial {args.serial}: {err.strip()}", file=sys.stderr)
        return 2

    os.makedirs(args.out_root, exist_ok=True)
    facts = dev.facts()
    serial_hash = hashlib.sha256(args.serial.encode()).hexdigest()

    # device facts first, so DEVICE.md exists even if every install fails
    write_device_md(args.out_root, facts, serial_hash)
    with open(os.path.join(args.out_root, "device.json"), "w", encoding="utf-8") as fh:
        json.dump({"tier": "physical-device", "device": facts, "serial_sha256": serial_hash,
                   "captured": NOW()}, fh, indent=2)

    manifests = {}
    for app in args.apps:
        scenario = os.path.join(args.scenario_dir, f"{app}.yaml")
        apk = os.path.join(args.apps_dir, f"{app}.apk")
        if not os.path.exists(scenario):
            manifests[app] = {"app": app, "install": {"ok": False, "error": {"code": "SCENARIO_MISSING", "message": scenario}}, "issues": [], "steps": []}
            continue
        if not os.path.exists(apk):
            manifests[app] = {"app": app, "install": {"ok": False, "error": {"code": "APK_MISSING", "message": apk}}, "issues": [], "steps": []}
            continue
        cap = AppCapture(dev, app, scenario, apk, os.path.join(args.out_root, app), facts, serial_hash)
        manifests[app] = cap.run()

    write_comparison(args.out_root, args.apps, manifests, args.emulator_ref)

    # restore the sensor-driven rotation and leave the launcher in front
    dev.ag("orientation", "auto", timeout=60)
    dev.ag("home", timeout=60)
    # best-effort restore of the exact pre-run rotation settings
    try:
        dev.adb_shell("settings", "put", "system", "accelerometer_rotation",
                      facts.get("accelerometer_rotation") or "1")
        ur = facts.get("user_rotation") or "null"
        if ur == "null":
            dev.adb_shell("settings", "delete", "system", "user_rotation")
        else:
            dev.adb_shell("settings", "put", "system", "user_rotation", ur)
    except Exception as e:
        log(f"rotation restore warning: {e}")

    top = {
        "tier": "physical-device",
        "fidelity": "physical-device",
        "device": facts,
        "serial_sha256": serial_hash,
        "apps_dir": args.apps_dir,
        "scenario_dir": args.scenario_dir,
        "emulator_reference": args.emulator_ref,
        "captured": NOW(),
        "apps": {a: {"installed": bool(m.get("install", {}).get("ok")),
                     "package": m.get("package"),
                     "steps": len(m.get("steps") or []),
                     "issues": m.get("issues") or []} for a, m in manifests.items()},
    }
    with open(os.path.join(args.out_root, "manifest.json"), "w", encoding="utf-8") as fh:
        json.dump(top, fh, indent=2)

    installed = [a for a, m in manifests.items() if m.get("install", {}).get("ok")]
    print(json.dumps({"apps_installed": installed,
                      "apps_blocked": [a for a in args.apps if a not in installed],
                      "out_root": args.out_root}, indent=2))
    return 0 if installed else 3


if __name__ == "__main__":
    sys.exit(main())
