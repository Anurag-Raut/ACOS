#!/usr/bin/env python3
"""diff_trees.py -- per-step UI-tree diff for one ACOS scenario.

Reads the two sides a parity run produced and writes `report.json` + `report.md`:

  ACOS ART   the artifacts of `acos verify <scenario> --engine art` (verdict.json
             + tree/<frame_seq>.json, the tree the step bound its frame to)
  phone      one of
               * a live replay directory (`tree-<NN>-<step>.json` +
                 observations.json written by tools/phone/replay_scenario.py)
               * a recorded agent-device capture directory
                 (`fixtures/reference-device/<app>/snapshot-<NN>-...json`)
               * a recorded phone observations directory
                 (`results/phone/.../phone/observations.json`, text-set coverage)

For every scenario step it diffs the two trees at that step:
nodes are paired by resource-id / text / content-desc / class+role, then text,
content-desc, enabled/checked/focused/selected, visibility and dp-normalised
bounds are compared. Differences matching the expected-difference allowlist are
reported as documented platform differences; the rest are findings.

Usage:
  tools/parity/diff_trees.py --scenario scenarios/counter.yaml \
      --acos-artifacts .cache/parity1/runs/counter \
      --phone-source recorded-snapshot --phone-dir fixtures/reference-device/counter \
      --out results/parity/counter [--allowlist tools/parity/allowlist.yaml]

Exit codes: 0 = compared, no unexpected difference; 1 = compared, unexpected
differences; 2 = usage/environment error; 3 = nothing to compare on one side
(never a pass).
"""

from __future__ import annotations

import argparse
import datetime as _dt
import glob
import hashlib
import json
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import parity_nodes as pn  # noqa: E402

try:
    import yaml
except ImportError:  # pragma: no cover
    yaml = None

ACTIONS = {"launch", "tap", "type", "back", "rotate", "swipe", "scroll", "key",
           "long_press", "press"}


def now():
    return _dt.datetime.now(_dt.timezone.utc).isoformat(timespec="seconds")


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def load_json(path):
    with open(path, encoding="utf-8") as fh:
        return json.load(fh)


def load_yaml(path):
    if yaml is None:
        raise SystemExit("PyYAML is required to read a scenario/allowlist")
    with open(path, encoding="utf-8") as fh:
        return yaml.safe_load(fh)


def scenario_name(doc):
    return (doc or {}).get("name") or "(unnamed)"


def scenario_package(doc):
    for step in (doc or {}).get("steps") or []:
        if isinstance(step, dict) and "launch" in step:
            act = str((step["launch"] or {}).get("activity") or "")
            if "/" in act:
                return act.split("/")[0]
            return act.rsplit(".", 1)[0] if "." in act else act
    return ""


def selector_of(step):
    if not isinstance(step, dict) or not step:
        return None
    kind = next(iter(step))
    value = step[kind]
    if isinstance(value, dict):
        return value.get("selector")
    return None


# ---------------------------------------------------------------------------
# ACOS side
# ---------------------------------------------------------------------------

class AcosSide:
    def __init__(self, artifacts):
        self.dir = artifacts
        verdict = os.path.join(artifacts, "verdict.json")
        if not os.path.exists(verdict):
            raise SystemExit(f"no verdict.json under {artifacts}")
        self.verdict = load_json(verdict)
        display = (self.verdict.get("reproducer") or {}).get("display") or {}
        self.dpi = display.get("density_dpi") or 420
        self.size = [display.get("width"), display.get("height")]
        self.package = ((self.verdict.get("app") or {}).get("package") or "")
        self.steps = self.verdict.get("steps") or []
        self._trees = {}

    def frame_seq(self, index):
        if index < 0 or index >= len(self.steps):
            return None
        return self.steps[index].get("frame_seq")

    def status(self, index):
        if index < 0 or index >= len(self.steps):
            return None
        return self.steps[index].get("status")

    def tree(self, seq):
        """Normalised ACOS node list for a frame sequence (cached)."""
        if seq is None:
            return None
        if seq not in self._trees:
            path = os.path.join(self.dir, "tree", f"{seq}.json")
            if not os.path.exists(path):
                self._trees[seq] = (None, None)
            else:
                tree_doc = load_json(path)
                self._trees[seq] = (pn.acos_nodes(tree_doc),
                                    pn.tree_display(tree_doc))
        return self._trees[seq][0]

    def display(self, seq):
        """The display (px bounds) the tree was captured on."""
        self.tree(seq)
        return self._trees.get(seq, (None, None))[1]

    def step_record(self, index, kind):
        """The executor's own record of a step (`steps/NN-<kind>.json`)."""
        if not hasattr(self, "_records"):
            self._records = {}
        key = (index, kind)
        if key not in self._records:
            path = os.path.join(self.dir, "steps", f"{index:02d}-{kind}.json")
            self._records[key] = load_json(path) if os.path.exists(path) else None
        return self._records[key]

    def action_target(self, index, kind):
        """The node a `tap` actually resolved to, as the executor recorded it.

        A tap's selector addresses the *pre-action* tree; resolving it against
        the post-action tree (where the tapped control may be gone) would report
        a target that never existed. The executor's `input.target.node` is the
        authoritative pre-action node.
        """
        record = self.step_record(index, kind)
        if not record:
            return None
        target = (record.get("input") or {}).get("target") or {}
        node = target.get("node") or (target.get("hit") or {}).get("suggest")
        if not node:
            return None
        return pn.make_node(
            resource_id=node.get("resource_id") or "",
            class_name=node.get("class") or "",
            text=node.get("text") or "",
            desc=node.get("content_desc") or "",
            enabled=node.get("enabled"),
            checked=node.get("checked"),
            focused=node.get("focused"),
            visible=node.get("visible_bounds") is not None,
            bounds=node.get("bounds") or node.get("visible_bounds"),
        )

    def windows(self, seq):
        """The `windows[]` metadata of a tree (z order, labels)."""
        path = os.path.join(self.dir, "tree", f"{seq}.json")
        if not os.path.exists(path):
            return []
        if not hasattr(self, "_window_meta"):
            self._window_meta = {}
        if seq not in self._window_meta:
            self._window_meta[seq] = load_json(path).get("windows") or []
        return self._window_meta[seq]

    def step_kind(self, index):
        if index < 0 or index >= len(self.steps):
            return ""
        return self.steps[index].get("step") or ""

    def tree_for_step(self, index):
        """(frame_seq, tree, source) for the step's post-action state.

        A step without its own bound frame (`assert_log`, `launch`,
        `assert_no_crash`) still has a state an agent reads. Its tree is the
        first tree of the same epoch: the next step's tree is that state as
        long as no action step runs in between. Never a later state.
        """
        seq = self.frame_seq(index)
        if seq is not None and self.tree(seq) is not None:
            return seq, self.tree(seq), "own"
        j = index + 1
        while j < len(self.steps):
            kind = self.step_kind(j)
            if kind in ACTIONS:
                return None, None, None
            nseq = self.frame_seq(j)
            if nseq is not None and nseq != seq and self.tree(nseq) is not None:
                return nseq, self.tree(nseq), "next_step"
            j += 1
        return None, None, None


# ---------------------------------------------------------------------------
# phone side
# ---------------------------------------------------------------------------

class PhoneSide:
    """One of the recorded/live phone sources, normalised to per-step nodes."""

    def __init__(self, source, directory, package):
        self.source = source
        self.dir = directory
        self.package = package
        self.dpi = None
        self.device = {}
        self.coverage = None
        self.observations = None
        self._steps = {}
        if source == "recorded-snapshot":
            self._load_snapshots()
        elif source == "live":
            self._load_live()
        elif source == "recorded-observations":
            self._load_observations()
        else:
            raise SystemExit(f"unknown phone source {source!r}")

    # -- loaders ----------------------------------------------------------
    def _load_snapshots(self):
        self.coverage = "full-tree"
        device = {}
        for name in ("device.json", "manifest.json"):
            path = os.path.join(self.dir, name)
            if os.path.exists(path):
                doc = load_json(path)
                device = doc.get("device") or device
        self.device = device
        self.dpi = device.get("density_dpi")
        files = sorted(glob.glob(os.path.join(self.dir, "snapshot-*.json")))
        if not files:
            raise SystemExit(f"no snapshot-*.json under {self.dir}")
        for path in files:
            m = re.match(r"snapshot-(\d+)-", os.path.basename(path))
            if not m:
                continue
            idx = int(m.group(1))
            self._steps[idx] = {"tree_file": path, "nodes": None}

    def _load_live(self):
        self.coverage = "full-tree"
        obs_path = os.path.join(self.dir, "observations.json")
        if os.path.exists(obs_path):
            self.observations = load_json(obs_path)
            self.device = self.observations.get("device") or {}
            self.dpi = self.device.get("density_dpi")
        for path in sorted(glob.glob(os.path.join(self.dir, "tree-*.json"))):
            m = re.match(r"tree-(\d+)-", os.path.basename(path))
            if not m:
                continue
            idx = int(m.group(1))
            self._steps[idx] = {"tree_file": path, "nodes": None}
        if not self._steps:
            raise SystemExit(f"no tree-*.json under {self.dir} (live replay)")

    def _load_observations(self):
        path = os.path.join(self.dir, "observations.json")
        if not os.path.exists(path):
            raise SystemExit(f"no observations.json under {self.dir}")
        self.observations = load_json(path)
        self.coverage = "text-set+target"
        self.device = self.observations.get("device") or {}
        self.dpi = self.device.get("density_dpi")
        for rec in self.observations.get("steps") or []:
            self._steps[int(rec.get("index", 0))] = {"observation": rec}

    # -- per step ---------------------------------------------------------
    def step(self, index):
        return self._steps.get(index)

    def failed_steps(self):
        """Steps whose phone-side action or assertion failed in the replay.

        Past the first failure the phone is not showing the screen the
        scenario asked for, so a tree difference there is a phone-side replay
        failure, not an ACOS defect.
        """
        if not self.observations:
            return []
        return [int(rec.get("index", 0))
                for rec in self.observations.get("steps") or []
                if rec.get("status") in ("fail", "action_failed")]

    def nodes(self, index):
        entry = self._steps.get(index)
        if not entry or "observation" in entry:
            return None, 0
        if entry["nodes"] is None:
            doc = load_json(entry["tree_file"])
            if isinstance(doc, dict) and ("data" in doc or "nodes" in doc):
                raw = pn.phone_nodes_from_snapshot(doc)
            else:
                raise SystemExit(f"unreadable phone tree {entry['tree_file']}")
            kept, dropped = pn.filter_package(raw, self.package)
            entry["nodes"] = kept
            entry["dropped_other_windows"] = dropped
        return entry["nodes"], entry.get("dropped_other_windows", 0)


# ---------------------------------------------------------------------------
# comparisons
# ---------------------------------------------------------------------------

def compare_full_tree(acos_nodes, phone_nodes, dpi_a, dpi_p, tol_dp,
                      strict_class, scenario, allowlist, phone_source=None,
                      tree_display_px=None):
    a = [pn.with_dp(dict(n), dpi_a) for n in acos_nodes]
    p = [pn.with_dp(dict(n), dpi_p) for n in phone_nodes]
    matched, missing, extra, stats = pn.match_nodes(a, p)
    shift = pn.decor_shift(matched)
    # The window root spans the display on both sides; it is the one node whose
    # pixel bounds describe the display in each orientation.
    display_a = display_p = None
    if matched and matched[0][2] == "root":
        display_a = matched[0][0].get("bounds_px")
        display_p = matched[0][1].get("bounds_px")

    diffs = []
    exact = 0
    for an, pn_node, why in matched:
        node_diffs = pn.diff_pair(an, pn_node, why, tol_dp=tol_dp,
                                  strict_class=strict_class, decor_shift=shift,
                                  display_a=display_a, display_p=display_p)
        if node_diffs:
            diffs.extend(node_diffs)
        else:
            exact += 1
    # `missing` = ACOS-only nodes, `extra` = phone-only nodes.
    for node in missing:
        diffs.append(pn.missing_record(node, "acos"))
    for node in extra:
        diffs.append(pn.missing_record(node, "phone"))
    if tree_display_px:
        diffs.extend(pn.tree_inconsistencies(a, tree_display_px))
    pn.apply_allowlist(diffs, allowlist, scenario, phone_source)
    return {
        "matched_nodes": stats["pairs"],
        "exact_matches": exact,
        "acos_nodes": stats["acos_nodes"],
        "phone_nodes": stats["phone_nodes"],
        "decor_shift_dp": shift,
        "differences": diffs,
    }


def resolve_selector(nodes, selector, package):
    """First visible ACOS tree node matching a scenario selector."""
    if not selector:
        return None
    kind = next(iter(selector))
    value = selector[kind]
    if isinstance(value, dict):
        mode = "exact" if "exact" in value else ("contains" if "contains" in value else None)
        want = str(value.get("exact", value.get("contains", "")))
    else:
        mode, want = "exact", str(value)

    def match(text):
        if mode == "exact":
            return text == want
        if mode == "contains":
            return want in text
        return False

    for node in nodes:
        if not node.get("visible", True):
            continue
        if kind == "id":
            if node.get("id") in (f"{package}:id/{want}", f"android:id/{want}"):
                return node
        elif kind in ("text", "label"):
            if match(node.get("text", "")) or match(node.get("desc", "")):
                return node
        elif kind == "content_desc":
            if match(node.get("desc", "")):
                return node
    return None


def compare_observations(acos_nodes, observation, dpi_a, dpi_p, tol_dp,
                         scenario, allowlist, target_a=None,
                         phone_source=None):
    """Label-set + resolved-target comparison against a recorded observation.

    The recorded phone evidence (`observations.json`) keeps the visible text
    and content-desc sets and the resolved target, not the whole node list.
    Labels are therefore compared as the union of text and content-desc (the
    accessible name an agent reads), and a string that is present on both
    sides but under a different attribute is reported once as an
    `accessibility_name_attribute` difference instead of twice as a set
    difference.
    """
    a = [pn.with_dp(dict(n), dpi_a) for n in acos_nodes]
    diffs = []
    visible = [n for n in a if n.get("visible", True)]
    acos_texts = {n["text"] for n in visible if n["text"]}
    acos_descs = {n["desc"] for n in visible if n["desc"]}
    phone_texts = set(observation.get("visible_texts") or [])
    phone_descs = set(observation.get("visible_descs") or [])
    acos_labels = acos_texts | acos_descs
    phone_labels = phone_texts | phone_descs

    for label in sorted(acos_labels - phone_labels):
        diffs.append({"kind": "label_only_in_acos", "key": "label:" + label,
                      "text": label,
                      "reason": "ACOS shows this text/content-desc; the recorded phone screen did not"})
    for label in sorted(phone_labels - acos_labels):
        diffs.append({"kind": "label_only_on_phone", "key": "label:" + label,
                      "text": label,
                      "reason": "the recorded phone screen shows this text/content-desc; ACOS did not"})

    # Same string, different attribute: on the phone an RN `View` carries its
    # accessible name in content-desc, ACOS's tree reports the visible
    # `TextView` text. One record per step, with examples.
    placement = sorted((acos_texts & phone_descs) | (acos_descs & phone_texts))
    if placement:
        diffs.append({
            "kind": "accessibility_name_attribute",
            "key": "attr:accessible-name-placement",
            "field": "text|content_desc",
            "count": len(placement),
            "examples": placement[:10],
            "reason": ("the same string is a node's text on one side and its "
                       "content-desc on the other"),
        })

    target = observation.get("target")
    resolved = target_a
    if target is not None:
        if resolved is None:
            diffs.append({"kind": "target_missing_in_acos",
                          "key": (target.get("identifier") or target.get("text") or "target"),
                          "node": target})
        else:
            pseudo = pn.make_node(
                resource_id=resolved.get("id", ""),
                class_name=resolved.get("class", ""),
                text=resolved.get("text", ""),
                desc=resolved.get("desc", ""),
                enabled=resolved.get("enabled"),
                checked=resolved.get("checked"),
                focused=resolved.get("focused"),
                visible=resolved.get("visible", True),
                bounds=resolved.get("bounds_px"),
            )
            pn.with_dp(pseudo, dpi_a)
            phone_node = pn.make_node(
                resource_id=target.get("identifier") or "",
                class_name=target.get("type") or "",
                text=target.get("text") or "",
                desc=target.get("content_desc") or "",
                enabled=target.get("enabled"),
                checked=target.get("checked"),
                visible=True,
                bounds=target.get("bounds"),
            )
            pn.with_dp(phone_node, dpi_p)
            diffs.extend(pn.diff_pair(pseudo, phone_node, "target", tol_dp=tol_dp))

    pn.apply_allowlist(diffs, allowlist, scenario, phone_source)
    return {
        "matched_nodes": len(acos_labels & phone_labels),
        "exact_matches": 0 if diffs else 1,
        "acos_nodes": len(a),
        "phone_nodes": observation.get("node_count"),
        "decor_shift_dp": None,
        "differences": diffs,
        "labels": {"acos": len(acos_labels), "phone": len(phone_labels),
                   "shared": len(acos_labels & phone_labels)},
    }


# ---------------------------------------------------------------------------
# findings + report
# ---------------------------------------------------------------------------

CLASSIFICATION = {
    "text_changed": "compatibility-bug/text",
    "desc_changed": "compatibility-bug/content-desc",
    "text_vs_desc": "compatibility-bug/accessibility-name",
    "state_changed": "compatibility-bug/state",
    "visibility_changed": "compatibility-bug/visibility",
    "class_changed": "compatibility-bug/widget",
    "bounds_changed": "layout/bounds",
    "missing_in_acos": "structure/missing-in-acos",
    "extra_in_acos": "structure/extra-in-acos",
    "label_only_in_acos": "labels/only-in-acos",
    "label_only_on_phone": "labels/only-on-phone",
    "accessibility_name_attribute": "compatibility-bug/accessibility-name-attribute",
    "target_missing_in_acos": "structure/target-missing-in-acos",
    "class_name_differs": "informational/class-name",
    "acos_tree_inconsistent": "acos-evidence/tree-consistency",
}


def findings_from_steps(steps, fast_settle=(), phone_failed_from=None):
    """Aggregate unexpected differences into findings (one per kind+key)."""
    findings = {}
    for step in steps:
        for diff in step.get("differences") or []:
            if diff.get("allowed"):
                continue
            key = (diff.get("kind"), diff.get("key") or "")
            finding = findings.setdefault(key, {
                "kind": diff.get("kind"),
                "key": diff.get("key"),
                "field": diff.get("field"),
                "classification": CLASSIFICATION.get(diff.get("kind"), "unclassified"),
                "steps": [],
                "first": diff,
                "count": 0,
            })
            finding["count"] += 1
            if step["index"] not in finding["steps"]:
                finding["steps"].append(step["index"])
    for finding in findings.values():
        # A `settle: fast` step is navigation, not evidence: the scenario says
        # so itself, and its tree is captured mid-transition on both sides.
        if finding["steps"] and set(finding["steps"]) <= set(fast_settle):
            finding["classification"] = "timing/unslept-step"
            finding["note"] = ("every step in this finding is a `settle: fast` "
                               "navigation step; the scenario does not claim its "
                               "frame as evidence")

    return sorted(findings.values(), key=lambda f: (f["classification"], f["kind"], str(f["key"])))


def allowed_summary(steps, allowlist):
    entries = {e.get("id"): e.get("reason") for e in (allowlist or {}).get("entries") or []}
    out = {}
    for step in steps:
        for diff in step.get("differences") or []:
            if not diff.get("allowed"):
                continue
            entry = out.setdefault(diff.get("allowed_by"), {
                "id": diff.get("allowed_by"),
                "reason": entries.get(diff.get("allowed_by")) or diff.get("allow_reason"),
                "count": 0,
                "kinds": {},
            })
            entry["count"] += 1
            entry["kinds"][diff["kind"]] = entry["kinds"].get(diff["kind"], 0) + 1
    return out


def step_label(scenario_steps, index):
    if index < len(scenario_steps) and isinstance(scenario_steps[index], dict) \
            and scenario_steps[index]:
        step = scenario_steps[index]
        kind = next(iter(step))
        value = step[kind] if isinstance(step[kind], dict) else {}
        sel = value.get("selector")
        extra = ""
        if sel:
            extra = " " + json.dumps(sel, separators=(",", ":"), ensure_ascii=False)
        elif value.get("name"):
            extra = " " + str(value["name"])
        elif value.get("activity"):
            extra = " " + str(value["activity"])
        elif value.get("text"):
            extra = " " + str(value["text"])[:24]
        return kind + extra
    return f"step-{index}"


def write_markdown(path, report):
    s = report["summary"]
    lines = []
    lines.append(f"# Parity: {report['scenario']['name']}")
    lines.append("")
    lines.append(f"* generated: {report['generated']}")
    lines.append(f"* scenario: `{report['scenario']['path']}` "
                 f"(sha256 `{report['scenario']['sha256'][:12]}`)")
    lines.append(f"* ACOS: `{report['acos']['artifacts']}` "
                 f"({report['acos']['engine']} / {report['acos']['verdict']}, "
                 f"{report['acos']['device']['width']}x{report['acos']['device']['height']} "
                 f"@{report['acos']['device']['dpi']} dpi)")
    p = report["phone"]
    lines.append(f"* phone: **{p['source']}** ({p['tier']}), "
                 f"coverage **{p['coverage']}**, "
                 f"{p['device'].get('resolution') or p['device'].get('wm_size')} "
                 f"@{p['device'].get('density_dpi')} dpi"
                 + (f", `{p['path']}`" if p.get("path") else ""))
    if p.get("apk"):
        lines.append(f"* phone APK: `{p['apk']}` sha256 `{(p.get('apk_sha256') or '')[:12]}`")
    lines.append(f"* bounds tolerance: {report['settings']['bounds_tolerance_dp']} dp "
                 f"(bounds converted with each side's own density)")
    lines.append("")
    lines.append("## Summary")
    lines.append("")
    lines.append(f"* steps compared: **{s['compared_steps']}/{s['steps']}** "
                 f"({s['skipped_steps']} without an ACOS tree)")
    lines.append(f"* matched nodes: **{s['matched_nodes']}**, exact matches: **{s['exact_matches']}**")
    lines.append(f"* differences: **{s['differences']}** "
                 f"({s['allowed']} documented platform differences, "
                 f"**{s['unexpected']} unexpected**) in "
                 f"**{s['findings']} findings**")
    if s.get("invalid_differences"):
        lines.append(f"* excluded as invalid evidence: {s['invalid_differences']} "
                     f"differences on steps {s['invalid_steps']} (phone replay "
                     f"failed earlier)")
    if p.get("failed_steps"):
        lines.append("")
        lines.append(f"> **Phone replay failed at steps {p['failed_steps']}.** "
                     "Past the first failure the phone is not on the screen the "
                     "scenario asked for; those differences are classified "
                     "`phone-scenario-failed` and say nothing about ACOS.")
    if p.get("caveats"):
        lines.append("")
        for caveat in p["caveats"]:
            lines.append(f"> {caveat}")
    lines.append("")
    lines.append("## Per step")
    lines.append("")
    lines.append("| # | step | ACOS frame | nodes A/P | matched | exact | allowed | unexpected |")
    lines.append("|---|---|---|---|---|---|---|---|")
    for step in report["steps"]:
        if step.get("status") != "compared":
            lines.append(f"| {step['index']} | `{step['step']}` | - | - | - | - | - | "
                         f"{step.get('status')} |")
            continue
        if step.get("invalid"):
            lines.append(
                f"| {step['index']} | `{step['step']}` | {step.get('acos_frame')} "
                f"({step.get('tree_source')}) | {step['acos_nodes']}/{step['phone_nodes']} "
                f"| {step['matched_nodes']} | {step['exact_matches']} "
                f"| invalid | invalid |")
            continue
        lines.append(
            f"| {step['index']} | `{step['step']}` | {step.get('acos_frame')} "
            f"({step.get('tree_source')}) | {step['acos_nodes']}/{step['phone_nodes']} "
            f"| {step['matched_nodes']} | {step['exact_matches']} "
            f"| {step['allowed']} | {step['unexpected']} |")
    lines.append("")
    if report["findings"]:
        lines.append("## Findings (unexpected differences)")
        lines.append("")
        lines.append("| classification | kind | node | steps | # |")
        lines.append("|---|---|---|---|---|")
        for finding in report["findings"]:
            lines.append(f"| {finding['classification']} | {finding['kind']} "
                         f"| `{finding['key']}` | {', '.join(map(str, finding['steps']))} "
                         f"| {finding['count']} |")
        lines.append("")
        for finding in report["findings"][:8]:
            first = finding["first"]
            detail = {k: first.get(k) for k in
                      ("field", "acos", "phone", "delta_dp", "reason") if first.get(k) is not None}
            lines.append(f"* **{finding['kind']}** `{finding['key']}` "
                         f"({finding['classification']}): `{json.dumps(detail, ensure_ascii=False)}`")
        lines.append("")
    else:
        lines.append("## Findings (unexpected differences)")
        lines.append("")
        lines.append("None: every difference is on the expected-difference allowlist.")
        lines.append("")
    allowed = report["allowed"]
    lines.append("## Documented platform differences (allowlisted)")
    lines.append("")
    if allowed:
        lines.append("| allowlist id | kinds | # | reason |")
        lines.append("|---|---|---|---|")
        for entry in sorted(allowed.values(), key=lambda e: e["id"] or ""):
            kinds = ", ".join(f"{k}×{v}" for k, v in sorted(entry["kinds"].items()))
            lines.append(f"| `{entry['id']}` | {kinds} | {entry['count']} "
                         f"| {entry['reason']} |")
    else:
        lines.append("None.")
    lines.append("")
    lines.append("## Settings")
    lines.append("")
    lines.append("```json")
    lines.append(json.dumps(report["settings"], indent=2, sort_keys=True))
    lines.append("```")
    with open(path, "w", encoding="utf-8") as fh:
        fh.write("\n".join(lines) + "\n")


# ---------------------------------------------------------------------------
# main
# ---------------------------------------------------------------------------

def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--scenario", required=True)
    ap.add_argument("--acos-artifacts", required=True)
    ap.add_argument("--phone-source", required=True,
                    choices=["live", "recorded-snapshot", "recorded-observations"])
    ap.add_argument("--phone-dir", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--allowlist", default=os.path.join(
        os.path.dirname(os.path.abspath(__file__)), "allowlist.yaml"))
    ap.add_argument("--tolerance-dp", type=float, default=1.5)
    ap.add_argument("--strict-class", action="store_true")
    ap.add_argument("--phone-apk", default=None)
    ap.add_argument("--phone-apk-sha256", default=None)
    args = ap.parse_args(argv)

    doc = load_yaml(args.scenario) or {}
    steps_doc = doc.get("steps") or []
    name = scenario_name(doc)
    package = scenario_package(doc)

    allowlist = load_yaml(args.allowlist) if os.path.exists(args.allowlist) else {"entries": []}
    acos = AcosSide(args.acos_artifacts)
    phone = PhoneSide(args.phone_source, args.phone_dir, package or acos.package)

    if not phone.dpi:
        print(f"diff_trees: {args.phone_source} has no density_dpi; "
              f"cannot normalise bounds (refusing to guess)", file=sys.stderr)
        return 2

    tier = {
        "live": "physical-device",
        "recorded-snapshot": "physical-device (recorded capture)",
        "recorded-observations": "physical-device (recorded capture)",
    }[args.phone_source]

    caveats = []
    if args.phone_source != "live":
        caveats.append(
            "The phone was not attached for this run: the comparison uses the "
            "checked-in physical-device capture below. It is real moto g35 data, "
            "but it is not a fresh replay; re-run with the phone attached to "
            "refresh it.")
    if args.phone_source == "recorded-snapshot":
        caveats.append(
            "Recorded agent-device snapshots do not expose the framework decor "
            "chrome (action bar container/toolbar, status/navigation bar "
            "backgrounds) that a live `uiautomator dump` does; those ids are "
            "allowlisted as a capture-tool scope difference "
            "(fixtures/reference-device/COMPARISON.md).")

    fast_settle = set()
    for index, step in enumerate(steps_doc):
        if not isinstance(step, dict) or not step:
            continue
        value = step[next(iter(step))]
        if isinstance(value, dict) and (
                value.get("settle") == "fast"
                or ("settle_timeout_ms" in value
                    and int(value["settle_timeout_ms"]) < 500)):
            fast_settle.add(index)

    out_steps = []
    for index, step in enumerate(steps_doc):
        kind = next(iter(step)) if isinstance(step, dict) and step else ""
        rec = {
            "index": index,
            "kind": kind,
            "step": step_label(steps_doc, index),
            "status": "skipped",
            "acos_status": acos.status(index),
            "phone_status": (phone.step(index) or {}).get("observation", {}).get("status"),
            "acos_frame": None,
            "tree_source": None,
            "coverage": phone.coverage,
            "settle": "fast" if index in fast_settle else "strict",
            "differences": [],
        }
        seq, acos_tree, source = acos.tree_for_step(index)
        if acos_tree is None:
            rec["status"] = "no_acos_tree"
            out_steps.append(rec)
            continue
        rec["acos_frame"] = seq
        rec["tree_source"] = source
        entry = phone.step(index)
        if entry is None:
            rec["status"] = "no_phone_capture"
            out_steps.append(rec)
            continue

        if phone.coverage == "full-tree":
            phone_nodes, dropped = phone.nodes(index)
            if not phone_nodes:
                rec["status"] = "no_phone_capture"
                out_steps.append(rec)
                continue
            acos_window, acos_others = pn.select_window(
                acos_tree, acos.windows(seq))
            phone_window, phone_others = pn.select_window(phone_nodes)
            rec["acos_other_windows"] = [
                {"nodes": len(g),
                 "root": {"class": g[0].get("class"), "id": g[0].get("id"),
                          "bounds_px": g[0].get("bounds_px")}}
                for g in acos_others]
            rec["phone_other_windows"] = [
                {"nodes": len(g),
                 "root": {"class": g[0].get("class"), "id": g[0].get("id"),
                          "bounds_px": g[0].get("bounds_px")}}
                for g in phone_others]
            result = compare_full_tree(
                acos_window, phone_window, acos.dpi, phone.dpi,
                args.tolerance_dp, args.strict_class, name, allowlist,
                phone_source=args.phone_source,
                tree_display_px=acos.display(seq))
            rec["phone_other_windows_dropped"] = dropped
        else:
            observation = entry["observation"]
            observation = dict(observation, package=package or acos.package)
            target_a = None
            if kind == "tap":
                target_a = acos.action_target(index, kind)
            if target_a is None:
                target_a = resolve_selector(
                    [pn.with_dp(dict(n), acos.dpi) for n in acos_tree],
                    selector_of(step), package or acos.package)
            result = compare_observations(
                acos_tree, observation, acos.dpi, phone.dpi, args.tolerance_dp,
                name, allowlist, target_a=target_a,
                phone_source=args.phone_source)
        rec.update(result)
        rec["status"] = "compared"
        rec["differences"] = result["differences"]
        rec["allowed"] = sum(1 for d in result["differences"] if d.get("allowed"))
        rec["unexpected"] = sum(1 for d in result["differences"] if not d.get("allowed"))
        out_steps.append(rec)

    compared = [s for s in out_steps if s["status"] == "compared"]
    phone_failures = phone.failed_steps()
    phone_failed_from = min(phone_failures) if phone_failures else None
    # A difference on a step at or after the phone's first replay failure is
    # not evidence about ACOS: the phone is not on the screen the scenario
    # asked for. Report it, but keep it out of the findings.
    valid, invalid = [], []
    for step in compared:
        if phone_failed_from is not None and step["index"] >= phone_failed_from:
            step["invalid"] = (f"the phone replay failed at step "
                               f"{phone_failed_from}; this step compares a "
                               f"screen the phone never reached")
            step["invalid_differences"] = len(step["differences"])
            step["allowed"] = None
            step["unexpected"] = None
            invalid.append(step)
        else:
            valid.append(step)
    all_diffs = [d for s in valid for d in s["differences"]]
    invalid_diffs = [d for s in invalid for d in s["differences"]]
    findings = findings_from_steps(valid, fast_settle)
    summary = {
        "steps": len(out_steps),
        "compared_steps": len(compared),
        "skipped_steps": len(out_steps) - len(compared),
        "matched_nodes": sum(s["matched_nodes"] for s in compared),
        "exact_matches": sum(s["exact_matches"] for s in compared),
        "differences": len(all_diffs),
        "allowed": sum(1 for d in all_diffs if d.get("allowed")),
        "unexpected": sum(1 for d in all_diffs if not d.get("allowed")),
        "coverage": phone.coverage,
        "findings": len(findings),
        "invalid_differences": len(invalid_diffs),
        "invalid_steps": [s["index"] for s in invalid],
        "phone_failed_steps": phone_failures,
    }
    report = {
        "schema_version": 1,
        "generated": now(),
        "scenario": {"name": name, "path": args.scenario,
                     "sha256": sha256_file(args.scenario),
                     "package": package or acos.package},
        "acos": {
            "artifacts": args.acos_artifacts,
            "engine": acos.verdict.get("engine"),
            "backend": acos.verdict.get("backend"),
            "verdict": acos.verdict.get("verdict"),
            "steps": len(acos.steps),
            "device": {"width": acos.size[0], "height": acos.size[1],
                       "dpi": acos.dpi},
            "apk_sha256": ((acos.verdict.get("app") or {}).get("sha256")),
        },
        "phone": {
            "source": args.phone_source,
            "tier": tier,
            "path": os.path.relpath(args.phone_dir) if os.path.isabs(args.phone_dir)
                    and args.phone_dir.startswith(os.getcwd() + os.sep) else args.phone_dir,
            "coverage": phone.coverage,
            "device": phone.device,
            "apk": args.phone_apk,
            "apk_sha256": args.phone_apk_sha256,
            "caveats": caveats,
        },
        "settings": {
            "bounds_tolerance_dp": args.tolerance_dp,
            "strict_class": bool(args.strict_class),
            "bounds_normalisation": "px * 160 / dpi, per side",
            "allowlist": os.path.relpath(args.allowlist) if os.path.exists(args.allowlist) else None,
        },
        "steps": out_steps,
        "summary": summary,
    }
    report["findings"] = findings
    report["phone"]["failed_steps"] = phone_failures
    report["allowed"] = allowed_summary(valid, allowlist)

    os.makedirs(args.out, exist_ok=True)
    with open(os.path.join(args.out, "report.json"), "w", encoding="utf-8") as fh:
        json.dump(report, fh, indent=2, ensure_ascii=False)
    write_markdown(os.path.join(args.out, "report.md"), report)

    print(f"parity: {name}: {summary['compared_steps']}/{summary['steps']} steps compared "
          f"({phone.coverage}), {summary['matched_nodes']} matched nodes, "
          f"{summary['differences']} differences "
          f"({summary['allowed']} allowed, {summary['unexpected']} unexpected)")
    if summary["unexpected"]:
        for finding in report["findings"]:
            print(f"  UNEXPECTED {finding['classification']}: {finding['kind']} "
                  f"{finding['key']} steps={finding['steps']} x{finding['count']}")
        return 1
    if not compared:
        return 3
    return 0


if __name__ == "__main__":
    sys.exit(main())
