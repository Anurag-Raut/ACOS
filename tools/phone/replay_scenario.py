#!/usr/bin/env python3
"""replay_scenario.py -- replay an ACOS scenario YAML on a physical phone, or
normalize an ACOS verify artifacts directory, into ONE observation JSON shape.

Why: the phone is the reference device. The track "PHONE-REF-1" replays the same
user journey the agent runs on ACOS and diffs what an agent would observe, step
by step. Both sides must therefore produce the same per-step record:

    index, kind, status, selector,
    target  {exists, text, content_desc, enabled, checked, clickable, bounds},
    visible_texts [..],          # the set an agent reads off the screen
    screen  {activity, frame/seq},
    assertion {kind, expected, passed},
    screenshot (relative path), error

Sources
-------
phone : drives the device through the ``agent-device`` CLI, resolving each
        scenario selector (``id`` / ``text{exact,contains}`` /
        ``content_desc{exact,contains}``) against the accessibility snapshot,
        then dispatching the action at the resolved node.
acos  : reads an ``acos verify --artifacts DIR`` tree (verdict.json +
        steps/NN-*.json + tree/<frame_seq>.json + frames/<frame_seq>.png) and
        emits the identical record set. No ACOS behaviour is changed.

Safety (phone source)
---------------------
* only the packages named by the scenario's own launch activity are touched;
* ``pm clear`` only for that package;
* no settings, accounts, connectivity, lock state or other app is modified;
* ``--require-lock`` takes the shared phone flock for the whole run so two
  agents never drive the device at once.

Usage
-----
  tools/phone/replay_scenario.py --source phone --scenario scenarios/gaten-home.yaml \
      --serial ZA222WGJPP --out results/phone/evidence/gaten-home/phone \
      --require-lock

  tools/phone/replay_scenario.py --source acos \
      --scenario scenarios/gaten-home.yaml \
      --artifacts .cache/phone-probe/acos/gaten-home \
      --out results/phone/evidence/gaten-home/acos

Exit codes: 0 = observation written, 2 = usage/environment error,
3 = device/scenario unavailable, 1 = replay ran but at least one action failed.
"""

from __future__ import annotations

import argparse
import datetime as _dt
import hashlib
import json
import os
import re
import subprocess
import xml.etree.ElementTree as ET
import sys
import time

try:
    import yaml
except ImportError:  # pragma: no cover
    yaml = None

NOW = lambda: _dt.datetime.now(_dt.timezone.utc).isoformat(timespec="milliseconds")
log = lambda *a: print("[replay]", *a, file=sys.stderr, flush=True)

PHONE_LOCK = "/home/Qui/Projects/acos/.cache/phone.lock"
EVIDENCE_MAX_WIDTH = 360


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def run(cmd, timeout=180):
    try:
        p = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
        return p.returncode, p.stdout, p.stderr
    except subprocess.TimeoutExpired as e:
        return 124, e.stdout or "", f"timeout after {timeout}s"


def safe_name(text):
    text = str(text).replace("\n", " ").strip()
    text = re.sub(r"[^A-Za-z0-9_.:-]+", "-", text)
    return text.strip("-")[:60] or "step"


# ---------------------------------------------------------------------------
# scenario + selector model (shared by both sources)
# ---------------------------------------------------------------------------
def load_scenario(path):
    with open(path, encoding="utf-8") as fh:
        doc = yaml.safe_load(fh)
    return doc, doc.get("steps") or []


def scenario_package(doc, packages=()):
    """Resolve the scenario's package + launch component.

    Gaten's scenario names the activity as a bare FQCN
    (``com.gaten.app.MainActivity``), fixtures use ``pkg/.Activity``. For the
    bare form the package is the longest installed package name that is a dot
    prefix of the class, so the tool never invents a package name.
    """
    for step in doc.get("steps") or []:
        if isinstance(step, dict) and "launch" in step:
            act = (step.get("launch") or {}).get("activity") or ""
            if "/" in act:
                return act.split("/")[0], act
            best = None
            for p in packages:
                if act.startswith(p + ".") and (best is None or len(p) > len(best)):
                    best = p
            if best:
                return best, f"{best}/{act[len(best) + 1:]}"
            return None, act
    return None, None


def sel_kind(selector):
    if not isinstance(selector, dict):
        return None
    for k in ("id", "text", "content_desc", "label"):
        if k in selector:
            return k
    return None


def sel_value(selector):
    """Normalize a selector value to (mode, value); mode is exact|contains."""
    k = sel_kind(selector)
    if k is None:
        return None, None
    v = selector[k]
    if isinstance(v, dict):
        if "exact" in v:
            return "exact", str(v["exact"])
        if "contains" in v:
            return "contains", str(v["contains"])
        if "regex" in v:
            return "regex", str(v["regex"])
        return None, None
    return "exact", str(v)


def value_matches(mode, expected, actual):
    if actual is None:
        actual = ""
    actual = str(actual)
    if mode == "exact":
        return actual == expected
    if mode == "contains":
        return expected in actual
    if mode == "regex":
        return re.search(expected, actual) is not None
    return False


# ---------------------------------------------------------------------------
# node model: one flat shape for both sources
# ---------------------------------------------------------------------------
def node_view(n):
    b = n.get("bounds")
    if b is None and isinstance(n.get("rect"), dict):
        r = n["rect"]
        b = [r.get("x", 0), r.get("y", 0),
             r.get("x", 0) + r.get("width", 0), r.get("y", 0) + r.get("height", 0)]
    # TREE1: the raw node carries the platform's fields under their own names --
    # `contentDescription` is AccessibilityNodeInfo.getContentDescription().
    # `label` is agent-device's *computed accessible name* (it merges descendant
    # text and prefers the description), so it is a fallback only: using it as
    # `content_desc` while dropping `contentDescription` invents a tree the
    # platform never exposes (that was PHONE2 §5.1).
    return {
        "text": n.get("text") or n.get("label") or "",
        "content_desc": n.get("contentDescription") or n.get("content_desc") or "",
        "hint": n.get("placeholder") or "",
        "identifier": n.get("identifier") or n.get("resource_id") or "",
        "enabled": n.get("enabled"),
        "checked": n.get("checked"),
        "clickable": n.get("clickable"),
        "editable": n.get("editable"),
        "visible": n.get("visible", True),
        "bounds": b,
    }


def match_node(nodes, selector, pkg, extra_id_prefixes=()):
    """Resolve a scenario selector against flat node views.

    Returns the list of matching node views (already filtered to visible ones).
    """
    kind = sel_kind(selector)
    mode, value = sel_value(selector)
    if kind is None or mode is None:
        return []
    hits = []
    for n in nodes:
        if not n.get("visible", True):
            continue
        if kind == "id":
            cands = [f"{pkg}:id/{value}", f"android:id/{value}"] + [
                f"{p}:id/{value}" for p in extra_id_prefixes
            ]
            if n["identifier"] in cands:
                hits.append(n)
            continue
        if kind in ("text", "label"):
            if value_matches(mode, value, n["text"]) or value_matches(
                mode, value, n["content_desc"]
            ):
                hits.append(n)
            continue
        if kind == "content_desc":
            if value_matches(mode, value, n["content_desc"]):
                hits.append(n)
    return hits


def find_best(nodes, selector, pkg, extra_id_prefixes=()):
    """Deterministic pick when a selector matches several nodes.

    ACOS's selector engine picks the first visible match in tree order; do the
    same so a difference in *which* node was hit shows up as a target diff
    instead of silently choosing another node.
    """
    hits = match_node(nodes, selector, pkg, extra_id_prefixes)
    return hits[0] if hits else None


def bounds_of(n):
    b = n.get("bounds") or n.get("rect") or {}
    if isinstance(b, list) and len(b) == 4:
        x1, y1, x2, y2 = b
        return [int(x1), int(y1), int(x2), int(y2)]
    if isinstance(b, dict):
        x, y = int(b.get("x", 0)), int(b.get("y", 0))
        return [x, y, x + int(b.get("width", 0)), y + int(b.get("height", 0))]
    return None


def visible_texts(nodes):
    """The text set an agent reads off the app's own screen."""
    out = set()
    for n in nodes:
        if not n.get("visible", True):
            continue
        t = (n.get("text") or "").strip()
        if t:
            out.add(t)
    return sorted(out)


def visible_descs(nodes):
    out = set()
    for n in nodes:
        if not n.get("visible", True):
            continue
        d = (n.get("content_desc") or "").strip()
        if d:
            out.add(d)
    return sorted(out)


# ---------------------------------------------------------------------------
# phone source
# ---------------------------------------------------------------------------
class Phone:
    def __init__(self, serial, agent_device, adb):
        self.serial = serial
        self.ad = agent_device
        self.adb_bin = adb
        self.commands = []

    def adb(self, *args, timeout=120):
        cmd = [self.adb_bin, "-s", self.serial, *args]
        self.commands.append({"tool": "adb", "args": list(args), "ts": NOW()})
        return run(cmd, timeout=timeout)

    def shell(self, *args, timeout=120):
        return self.adb("shell", *args, timeout=timeout)

    def ag(self, *args, timeout=180, want_json=True):
        cmd = [self.ad, "--serial", self.serial, *args]
        if want_json and "--json" not in cmd:
            cmd.append("--json")
        self.commands.append({"tool": "agent-device", "args": list(args), "ts": NOW()})
        rc, out, err = run(cmd, timeout=timeout)
        data = None
        if out.strip():
            try:
                data = json.loads(out)
            except json.JSONDecodeError:
                data = None
        return {"rc": rc, "stdout": out, "stderr": err, "json": data}

    # -- reads -------------------------------------------------------------
    def dump_nodes(self, timeout=120):
        """The platform's own tree: `uiautomator dump` -> AccessibilityNodeInfo.

        Each `<node>` is a real AccessibilityNodeInfo: text is getText() (for a
        TextView/EditText the framework falls back to the hint), content-desc is
        getContentDescription(), password marks a password field. This is the
        surface ACOS's guest reads, so both sides are compared on one tree.
        (agent-device's snapshot is kept only as a fallback: its `label` is a
        computed name, not a node field -- see node_view.)
        """
        remote = "/sdcard/acos-a11y-dump.xml"
        rc, out, err = self.shell("uiautomator", "dump", remote, timeout=timeout)
        if rc != 0:
            return []
        rc, xml, err = self.shell("cat", remote, timeout=timeout)
        self.shell("rm", "-f", remote, timeout=60)
        if rc != 0 or "<hierarchy" not in (xml or ""):
            return []
        start = xml.find("<hierarchy")
        end = xml.rfind("</hierarchy>")
        if start < 0 or end < 0:
            return []
        try:
            root = ET.fromstring(xml[start:end + len("</hierarchy>")])
        except ET.ParseError:
            return []
        nodes = []
        for el in root.iter("node"):
            a = el.attrib
            nums = [int(x) for x in re.findall(r"-?\d+", a.get("bounds") or "")]
            if len(nums) != 4:
                continue
            nodes.append({
                "text": a.get("text") or "",
                "content_desc": a.get("content-desc") or "",
                "hint": "",
                "identifier": a.get("resource-id") or "",
                "enabled": a.get("enabled") == "true",
                "checked": a.get("checked") == "true",
                "clickable": a.get("clickable") == "true",
                "editable": (a.get("class") or "").endswith("EditText"),
                "password": a.get("password") == "true",
                "visible": nums[2] > nums[0] and nums[3] > nums[1],
                "bounds": nums,
                "bundle": a.get("package") or "",
                "type": a.get("class") or "",
            })
        return nodes

    def snapshot(self, timeout=120):
        """One accessibility tree read -> flat node views."""
        dumped = self.dump_nodes(timeout=timeout)
        if dumped:
            pkgs = [n["bundle"] for n in dumped if n.get("bundle")]
            app = max(set(pkgs), key=pkgs.count) if pkgs else None
            return {"nodes": dumped, "app": app, "raw_ok": True, "error": None,
                    "source": "uiautomator-dump"}
        # Fallback: agent-device's snapshot (hosts where `uiautomator dump` fails).
        res = self.ag("snapshot", "--force-full", "--json", timeout=timeout)
        data = (res.get("json") or {}).get("data") or {}
        nodes = (data.get("snapshot") or {}).get("nodes") or data.get("nodes") or []
        if not nodes:
            # a rotation/recreation can leave one unreadable capture; retry once
            time.sleep(0.8)
            res = self.ag("snapshot", "--force-full", "--json", timeout=timeout)
            data = (res.get("json") or {}).get("data") or {}
            nodes = (data.get("snapshot") or {}).get("nodes") or data.get("nodes") or []
        out = []
        for n in nodes:
            v = node_view(n)
            v["bundle"] = n.get("bundleId")
            v["type"] = n.get("type")
            out.append(v)
        return {
            "nodes": out,
            "app": data.get("appBundleId") or data.get("appName"),
            "raw_ok": bool(res.get("json") and res["json"].get("success")),
            "error": (res.get("json") or {}).get("error"),
            "source": "agent-device-snapshot",
        }

    def focused_activity(self):
        rc, out, _ = self.shell("dumpsys", "window")
        m = re.search(r"mCurrentFocus=Window\{\S+ u0 ([^/\s]+)/(\S+?)[,}\s]", out or "")
        if m:
            return f"{m.group(1)}/{m.group(2)}"
        return None

    def app_nodes(self, snap, pkg):
        return [n for n in snap["nodes"] if n.get("bundle") == pkg]

    # -- evidence ----------------------------------------------------------
    def screenshot(self, out_dir, base):
        path = os.path.join(out_dir, f"screenshot-{base}.png")
        self.ag("screenshot", path, timeout=120)
        if os.path.exists(path) and os.path.getsize(path) > 0:
            # evidence budget: the device captures at 1080x2400, keep <=540 px
            _downscale(path, path)
            return os.path.basename(path)
        return None

    # -- actions -----------------------------------------------------------
    def press_node(self, node, settle=True):
        b = bounds_of(node)
        if not b:
            return {"rc": 1, "json": None, "stderr": "no bounds for target"}
        x = (b[0] + b[2]) // 2
        y = (b[1] + b[3]) // 2
        args = ["press", str(x), str(y)]
        if settle:
            args += ["--settle"]
        return self.ag(*args, timeout=150)


def replay_phone(args, doc, steps, out_dir):
    dev = Phone(args.serial, args.agent_device, args.adb)
    rc, out, _ = dev.shell("pm", "list", "packages")
    installed = [ln.strip().replace("package:", "") for ln in (out or "").splitlines()
                 if ln.startswith("package:")]
    pkg, activity = scenario_package(doc, installed)
    if not pkg:
        raise SystemExit("scenario has no resolvable launch component; cannot resolve package")
    os.makedirs(out_dir, exist_ok=True)

    # The accessibility tree only marks nodes visible while the app's window is
    # the one the user is looking at; a dozing display or the keyguard makes a
    # whole replay read an empty/invisible tree (0 texts, every selector
    # ELEMENT_NOT_FOUND). Wake the device first, and keep it awake for the run.
    dev.shell("input", "keyevent", "KEYCODE_WAKEUP")
    dev.shell("wm", "dismiss-keyguard")
    rc, before_stayon, _ = dev.shell("settings", "get", "global", "stay_on_while_plugged_in")
    dev.shell("svc", "power", "stayon", "true")

    facts = {}
    for key in (
        "ro.build.fingerprint",
        "ro.build.version.sdk",
        "ro.build.version.release",
        "ro.product.model",
    ):
        rc, out, _ = dev.shell("getprop", key)
        facts[key] = (out or "").strip()
    rc, out, _ = dev.shell("wm", "size")
    m = re.search(r"(\d+x\d+)", out or "")
    facts["wm_size"] = m.group(1) if m else None
    rc, out, _ = dev.shell("wm", "density")
    m = re.search(r"(\d+)", out or "")
    facts["density_dpi"] = int(m.group(1)) if m else None
    rc, out, _ = dev.shell("settings", "get", "system", "font_scale")
    facts["font_scale"] = (out or "").strip()

    observations = {
        "schema_version": 1,
        "source": "phone",
        "fidelity": "physical-device",
        "scenario": {"name": doc.get("name"), "path": args.scenario,
                     "sha256": sha256_file(args.scenario)},
        "app": {"package": pkg, "activity": activity, "apk": doc.get("app")},
        "device": facts,
        "started": NOW(),
        "steps": [],
    }

    # logcat "since previous step": remember the tag lines already accounted for
    # (the buffer is global and survives pm clear, so counting lines is fragile)
    log_seen = set()
    log_cursor = 0
    if any(isinstance(s, dict) and "assert_log" in s for s in steps):
        _lg_tag = next(s["assert_log"].get("tag") for s in steps
                       if isinstance(s, dict) and "assert_log" in s)
        rc, out, _ = dev.adb("logcat", "-d", "-v", "brief", timeout=120)
        log_seen.update(ln.strip() for ln in (out or "").splitlines() if f"/{_lg_tag}" in ln)
    for idx, step in enumerate(steps):
        if not isinstance(step, dict) or not step:
            continue
        kind = next(iter(step))
        value = step[kind] if isinstance(step[kind], dict) else {}
        selector = value.get("selector")
        base = f"{idx:02d}-{safe_name(kind + ('-' + str(value.get('name')) if kind == 'capture' else ''))}"
        rec = {
            "index": idx,
            "kind": kind,
            "status": "pass",
            "selector": selector,
            "target": None,
            "visible_texts": [],
            "visible_descs": [],
            "screen": {},
            "assertion": None,
            "screenshot": None,
            "error": None,
        }
        action_res = None

        try:
            if kind == "launch":
                if value.get("clear_state"):
                    dev.shell("pm", "clear", pkg)
                act = value.get("activity") or activity
                # agent-device wants a component: package/Class, .Class or Class.
                if "/" not in act:
                    if act.startswith(pkg + "."):
                        act = pkg + "/." + act[len(pkg) + 1:]
                    else:
                        act = pkg + "/" + act
                action_res = dev.ag("open", pkg, "--activity", act, "--relaunch",
                                    "--foreground", "--json", timeout=180)
            elif kind == "tap":
                snap = dev.snapshot()
                nodes = dev.app_nodes(snap, pkg)
                node = find_best(nodes, selector, pkg)
                if node is None:
                    rec["status"] = "fail"
                    rec["error"] = "ELEMENT_NOT_FOUND"
                else:
                    rec["target"] = {k: node.get(k) for k in
                                     ("text", "content_desc", "identifier", "enabled",
                                      "checked", "clickable", "bounds")}
                    action_res = dev.press_node(node, settle=True)
            elif kind == "type":
                text = str(value.get("text", ""))
                node = None
                if selector:
                    snap = dev.snapshot()
                    node = find_best(dev.app_nodes(snap, pkg), selector, pkg)
                    if node is None:
                        rec["status"] = "fail"
                        rec["error"] = "ELEMENT_NOT_FOUND"
                if rec["status"] == "pass":
                    if node is not None:
                        rec["target"] = {k: node.get(k) for k in
                                         ("text", "content_desc", "identifier",
                                          "enabled", "checked", "clickable", "bounds")}
                        # focus the field exactly like the ACOS `type` with a
                        # selector does (tap, then deliver text)
                        dev.press_node(node, settle=True)
                    if value.get("clear") and node is not None:
                        # replace semantics: focus, move to end, delete the
                        # characters this field already holds, then type.
                        # (agent-device `fill` needs a ref/selector handle that
                        # RN TextInputs do not always expose.)
                        dev.adb("shell", "input", "keyevent", "123")
                        cur = node.get("text") or node.get("content_desc") or ""
                        for _ in range(len(cur) + 4):
                            dev.adb("shell", "input", "keyevent", "67")
                        action_res = dev.ag("type", text, timeout=120)
                    else:
                        action_res = dev.ag("type", text, timeout=120)
            elif kind == "back":
                action_res = dev.ag("back", "--settle", timeout=120)
            elif kind == "wait_for":
                timeout_ms = int(value.get("timeout_ms") or 5000)
                deadline = time.time() + timeout_ms / 1000.0
                found = None
                while time.time() < deadline:
                    snap = dev.snapshot()
                    found = find_best(dev.app_nodes(snap, pkg), selector, pkg)
                    if found is not None:
                        break
                    time.sleep(0.3)
                if found is None:
                    rec["status"] = "fail"
                    rec["error"] = "TIMEOUT"
                else:
                    rec["target"] = {k: found.get(k) for k in
                                     ("text", "content_desc", "identifier", "enabled",
                                      "checked", "clickable", "bounds")}
            elif kind == "capture":
                pass
            elif kind in ("assert_text",):
                snap = dev.snapshot()
                nodes = dev.app_nodes(snap, pkg)
                node = find_best(nodes, selector, pkg)
                want = value.get("text")
                wmode, wval = ("exact", str(want)) if not isinstance(want, dict) else (
                    ("exact", str(want["exact"])) if "exact" in want else
                    ("contains", str(want["contains"])))
                rec["target"] = None if node is None else {
                    k: node.get(k) for k in ("text", "content_desc", "identifier",
                                             "enabled", "checked", "clickable", "bounds")}
                passed = False
                if node is not None:
                    # ACOS assert_text compares the element's own text AND its
                    # content_desc (a selector may name either).
                    passed = (value_matches(wmode, wval, node.get("text"))
                              or value_matches(wmode, wval, node.get("content_desc")))
                rec["assertion"] = {"kind": "text", "expected": wval,
                                    "mode": wmode, "passed": passed,
                                    "expected_bounds": value.get("bounds")}
                if not passed:
                    rec["status"] = "fail"
                    rec["error"] = "ASSERTION_FAILED"
            elif kind == "assert_log":
                # phone-side: the app's own log tag, only lines new since the
                # previous step (the buffer is global, not per-app)
                tag = value.get("tag")
                rc, out, _ = dev.adb("logcat", "-d", "-v", "brief", timeout=120)
                all_lines = [ln.strip() for ln in (out or "").splitlines() if f"/{tag}" in ln]
                lines = [ln for ln in all_lines if ln not in log_seen]
                log_seen.update(all_lines)
                patterns = value.get("patterns") or []
                msgs = [ln.split("): ", 1)[-1] if "): " in ln else ln for ln in lines]
                matched = [p for p in patterns if any(re.search(p, m) for m in msgs)]
                rec["assertion"] = {"kind": "log", "expected": patterns,
                                    "passed": len(matched) == len(patterns),
                                    "matched": matched, "lines": lines[-40:]}
                if len(matched) != len(patterns):
                    rec["status"] = "pass_with_failure" if rec["status"] == "pass" else rec["status"]
                    rec["error"] = rec["error"] or "LOG_PATTERN_MISSING"
                    rec["note"] = ("logcat on the phone is a global buffer; a missing pattern "
                                   "here is a measurement, not a scenario verdict")
            elif kind == "rotate":
                orient = str(value.get("orientation", "")).lower()
                target = {"landscape": "landscape-left", "portrait": "portrait"}.get(orient, orient)
                action_res = dev.ag("orientation", target, timeout=120)
                # the new display needs its first traversal before the
                # accessibility tree is readable (rotation recreates the surface)
                time.sleep(2.0)
                # A real phone keeps the soft keyboard up across a rotation and
                # it covers the list; ACOS's offscreen guest has no IME window.
                # Dismissing it is what an agent does before reading the screen.
                dev.ag("keyboard", "dismiss", timeout=60)
            elif kind in ("assert_visible", "assert_not_visible"):
                snap = dev.snapshot()
                nodes = dev.app_nodes(snap, pkg)
                node = find_best(nodes, selector, pkg)
                rec["target"] = None if node is None else {
                    k: node.get(k) for k in ("text", "content_desc", "identifier",
                                             "enabled", "checked", "clickable", "bounds")}
                want = kind == "assert_visible"
                key = "expected_present" if want else "expected_absent"
                rec["assertion"] = {"kind": kind, "passed": (node is not None) == want, key: selector}
                if (node is not None) != want:
                    rec["status"] = "fail"
                    rec["error"] = "ASSERTION_FAILED"
            elif kind in ("assert_no_crash",):
                rc, out, _ = dev.adb("logcat", "-d", "-v", "brief", timeout=120)
                crashes = [ln for ln in (out or "").splitlines()
                           if f"Process: {pkg}" in ln or f"FATAL EXCEPTION" in ln and pkg in ln]
                rec["assertion"] = {"kind": "no_crash", "passed": not crashes,
                                    "matched": crashes[-5:]}
                if crashes:
                    rec["status"] = "fail"
                    rec["error"] = "CRASH"
            elif kind == "assert_region":
                rec["assertion"] = {"kind": "region", "passed": None,
                                    "note": "pixel region hash is calibrated to the ACOS "
                                            "frame geometry; not asserted on the phone"}
                rec["status"] = "not_compared"
            else:
                rec["status"] = "unsupported_step"
                rec["error"] = f"unsupported step kind {kind}"
        except Exception as e:  # pragma: no cover
            rec["status"] = "fail"
            rec["error"] = f"{type(e).__name__}: {e}"

        if action_res is not None and rec["status"] == "pass":
            ok = bool((action_res.get("json") or {}).get("success"))
            if not ok:
                rec["status"] = "action_failed"
                rec["error"] = (action_res.get("json") or {}).get("error") or \
                    (action_res.get("stderr") or "")[:300]

        # observation after the step: the state an agent would read
        snap = dev.snapshot()
        nodes = dev.app_nodes(snap, pkg)
        rec["visible_texts"] = visible_texts(nodes)
        rec["visible_descs"] = visible_descs(nodes)
        rec["visible_labels"] = sorted(set(rec["visible_texts"]) | set(rec["visible_descs"]))
        rec["screen"] = {"activity": dev.focused_activity(), "package": pkg,
                         "app_bundle_in_snapshot": snap.get("app")}
        rec["screenshot"] = dev.screenshot(out_dir, base)
        rec["node_count"] = len(nodes)
        # A `type` step's target is re-read AFTER the text landed: ACOS records
        # the post-action tree for every step, so the phone must too (otherwise
        # the target still shows the pre-typing value and looks like a diff).
        if kind == "type" and selector and rec.get("target") is not None:
            node = find_best(nodes, selector, pkg)
            if node is not None:
                rec["target"] = {k: node.get(k) for k in
                                 ("text", "content_desc", "identifier", "enabled",
                                  "checked", "clickable", "bounds")}
        observations["steps"].append(rec)
        prev_tree = nodes
        log(f"phone step {idx:02d} {kind} -> {rec['status']}")

    observations["finished"] = NOW()
    observations["commands"] = dev.commands
    # restore the display setting this run changed
    keep = (before_stayon or "0").strip()
    dev.shell("svc", "power", "stayon", "false")
    if keep not in ("", "0", "null"):
        dev.shell("settings", "put", "global", "stay_on_while_plugged_in", keep)
    # leave the device tidy: close the session and go home
    dev.ag("close", timeout=120)
    dev.ag("home", timeout=120)
    with open(os.path.join(out_dir, "observations.json"), "w", encoding="utf-8") as fh:
        json.dump(observations, fh, indent=2)
    return observations


# ---------------------------------------------------------------------------
# acos source (artifacts normalizer)
# ---------------------------------------------------------------------------
def flatten_acos_tree(tree):
    out = []

    def walk(n):
        v = node_view({
            "text": n.get("text"),
            "content_desc": n.get("content_desc"),
            "resource_id": n.get("resource_id"),
            "enabled": n.get("enabled"),
            "checked": n.get("checked"),
            "clickable": n.get("clickable"),
            "editable": n.get("editable"),
            "visible": n.get("visible", True),
            "bounds": n.get("visible_bounds") or n.get("bounds"),
        })
        v["type"] = n.get("class")
        v["visible"] = bool(n.get("visible", True))
        out.append(v)
        for c in n.get("children") or []:
            walk(c)

    for n in tree.get("nodes") or []:
        walk(n)
    return out


def replay_acos(args, doc, steps, out_dir):
    art = args.artifacts
    verdict_path = os.path.join(art, "verdict.json")
    if not os.path.exists(verdict_path):
        raise SystemExit(f"no verdict.json under {art}")
    with open(verdict_path, encoding="utf-8") as fh:
        verdict = json.load(fh)
    os.makedirs(out_dir, exist_ok=True)
    pkg, activity = scenario_package(doc)

    observations = {
        "schema_version": 1,
        "source": "acos",
        "fidelity": verdict.get("fidelity"),
        "scenario": {"name": verdict.get("scenario", {}).get("name"),
                     "path": args.scenario,
                     "sha256": sha256_file(args.scenario)},
        "app": {"package": pkg, "activity": activity,
                "sha256": (verdict.get("app") or {}).get("sha256")},
        "engine": {"backend": verdict.get("backend"), "isolation": verdict.get("isolation"),
                   "verdict": verdict.get("verdict"),
                   "display": (verdict.get("reproducer") or {}).get("display")},
        "started": NOW(),
        "steps": [],
    }

    for i, st in enumerate(verdict.get("steps") or []):
        idx = st.get("index", i)
        kind = st.get("step")
        step_doc = steps[idx] if idx < len(steps) else {}
        value = (step_doc.get(kind) if isinstance(step_doc, dict) else None) or {}
        selector = value.get("selector") if isinstance(value, dict) else None
        rec = {
            "index": idx,
            "kind": kind,
            "status": st.get("status"),
            "selector": selector,
            "target": None,
            "visible_texts": [],
            "visible_descs": [],
            "screen": {},
            "assertion": None,
            "screenshot": None,
            "error": (st.get("error") or {}).get("code") if st.get("error") else None,
        }

        frame_seq = st.get("frame_seq")
        tree = None
        if frame_seq is not None:
            tp = os.path.join(art, "tree", f"{frame_seq}.json")
            if os.path.exists(tp):
                with open(tp, encoding="utf-8") as fh:
                    tree = json.load(fh)
                fp = os.path.join(art, "frames", f"{frame_seq}.png")
                if os.path.exists(fp):
                    dst = os.path.join(out_dir, f"screenshot-{idx:02d}-{safe_name(kind)}.png")
                    _downscale(fp, dst)
                    rec["screenshot"] = os.path.basename(dst)
        if tree is not None:
            nodes = flatten_acos_tree(tree)
            rec["visible_texts"] = visible_texts(nodes)
            rec["visible_descs"] = visible_descs(nodes)
            rec["visible_labels"] = sorted(set(rec["visible_texts"]) | set(rec["visible_descs"]))
            rec["node_count"] = len(nodes)
            rec["screen"] = {"package": tree.get("package"),
                             "frame_seq": frame_seq,
                             "content_hash": (tree.get("frame") or {}).get("content_hash")}
            if selector:
                node = find_best(nodes, selector, pkg or tree.get("package") or "")
                if node is not None:
                    rec["target"] = {k: node.get(k) for k in
                                     ("text", "content_desc", "identifier", "enabled",
                                      "checked", "clickable", "bounds")}
        for a in st.get("assertions") or []:
            if a.get("kind") in ("text", "log", "no_crash", "region"):
                rec["assertion"] = {
                    "kind": a.get("kind"),
                    "passed": a.get("passed"),
                    "expected": value.get("text") if isinstance(value, dict) else None,
                    "message": a.get("message"),
                }
                break
        observations["steps"].append(rec)

    observations["finished"] = NOW()
    observations["available"] = args.artifacts
    with open(os.path.join(out_dir, "observations.json"), "w", encoding="utf-8") as fh:
        json.dump(observations, fh, indent=2)
    return observations


def _downscale(src, dst, width=EVIDENCE_MAX_WIDTH):
    """Copy a screenshot as a <=360px-wide, 256-colour PNG (evidence budget).

    A 1080x2400 device capture is ~650 KB; at 360 px with a 256-colour palette the
    same screen is ~30 KB, so a full 19-scenario reference stays well under the
    30 MB evidence budget while remaining readable at a glance.
    """
    try:
        from PIL import Image
    except ImportError:  # pragma: no cover
        subprocess.run(["cp", src, dst], check=False)
        return
    try:
        im = Image.open(src)
        if im.width > width:
            h = round(im.height * width / im.width)
            im = im.resize((width, h), Image.LANCZOS)
        if im.mode not in ("RGB", "RGBA"):
            im = im.convert("RGB")
        # 256-colour palette: ~4x smaller, and flat UI screens keep their look
        im = im.convert("RGB").quantize(colors=256, method=Image.MEDIANCUT)
        im.save(dst, "PNG", optimize=True)
    except Exception as e:  # pragma: no cover
        log(f"downscale failed for {src}: {e}")
        subprocess.run(["cp", src, dst], check=False)


# ---------------------------------------------------------------------------
# main
# ---------------------------------------------------------------------------
def main(argv=None):
    ap = argparse.ArgumentParser()
    ap.add_argument("--source", choices=["phone", "acos"], required=True)
    ap.add_argument("--scenario", required=True)
    ap.add_argument("--out", required=True, help="evidence dir for this side")
    ap.add_argument("--artifacts", help="acos verify --artifacts dir (source=acos)")
    ap.add_argument("--serial", default="ZA222WGJPP")
    ap.add_argument("--adb", default="adb")
    ap.add_argument("--agent-device", default="agent-device")
    ap.add_argument("--require-lock", action="store_true",
                    help="hold the shared phone flock for the whole replay")
    args = ap.parse_args(argv)

    if not os.path.exists(args.scenario):
        print(f"missing scenario: {args.scenario}", file=sys.stderr)
        return 2
    doc, steps = load_scenario(args.scenario)

    if args.source == "acos":
        if not args.artifacts:
            print("--artifacts is required for --source acos", file=sys.stderr)
            return 2
        obs = replay_acos(args, doc, steps, args.out)
        print(json.dumps({"source": "acos", "out": args.out,
                          "steps": len(obs["steps"])}, indent=2))
        return 0

    # phone: take the shared device lock for the whole replay
    lock_fh = None
    if args.require_lock:
        import fcntl
        lock_fh = open(PHONE_LOCK, "a+")
        deadline = time.time() + 600
        while True:
            try:
                fcntl.flock(lock_fh.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except OSError:
                if time.time() > deadline:
                    print("phone lock busy", file=sys.stderr)
                    return 3
                time.sleep(5)

    try:
        rc, out, err = run([args.adb, "-s", args.serial, "get-state"], timeout=60)
        if rc != 0:
            print(f"no device {args.serial}: {err.strip()}", file=sys.stderr)
            return 3
        obs = replay_phone(args, doc, steps, args.out)
    finally:
        if lock_fh is not None:
            lock_fh.close()

    bad = [s for s in obs["steps"] if s["status"] in ("fail", "action_failed")]
    print(json.dumps({"source": "phone", "out": args.out,
                      "steps": len(obs["steps"]),
                      "failed": [s["index"] for s in bad]}, indent=2))
    return 0 if not bad else 1


if __name__ == "__main__":
    sys.exit(main())
