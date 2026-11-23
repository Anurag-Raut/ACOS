#!/usr/bin/env python3
"""ARTGC: prove no artifact report dangles after retention.

Every `acos verify` run directory under the given roots is opened and every
JSON/JSONL document in it is scanned for artifact paths (`frames/<n>.png`,
`tree/<n>.json`, `steps/NN-*.json`, `logs/logcat.txt`, `art-frames/...`). Each
one must resolve inside the run directory, or the run is reported. It also
reports the verdict status and the retention decision so one run of this script
is the whole picture.

usage: tools/verify/check-artifact-references.py [--json] <root> [<root> ...]

Exit code 1 when any reference dangles.
"""
import json
import os
import sys

DIR_PREFIXES = ("frames/", "tree/", "steps/", "logs/", "art-frames/", "./frames/")
SUFFIX_MARKERS = ("art-frames/live-frames/", "art-frames/", "frames/", "tree/", "steps/", "logs/")


def looks_like_path(value):
    if not isinstance(value, str) or not value:
        return False
    if len(value) > 400 or "<" in value or " " in value or "\n" in value:
        return False
    return any(prefix in value for prefix in DIR_PREFIXES)


def resolve(run_dir, value):
    """Resolve an artifact path name against its run directory."""
    candidates = [os.path.join(run_dir, value.lstrip("./"))]
    for marker in SUFFIX_MARKERS:
        index = value.find(marker)
        if index >= 0:
            candidates.append(os.path.join(run_dir, value[index:]))
    for candidate in candidates:
        if os.path.isfile(candidate):
            return candidate
    return None


def walk_json(run_dir):
    for dirpath, _dirnames, filenames in os.walk(run_dir):
        for name in filenames:
            if name.endswith((".json", ".jsonl")):
                yield os.path.join(dirpath, name)


# Keys whose values are a deletion receipt, not a reference: `retention.json`
# records the paths it *removed* so the audit trail survives.
RECEIPT_KEYS = ("pruned",)


def strings(node, key=None):
    if isinstance(node, str):
        yield node
    elif isinstance(node, dict):
        for name, value in node.items():
            if name in RECEIPT_KEYS:
                continue
            yield from strings(value, name)
    elif isinstance(node, list):
        for value in node:
            yield from strings(value, key)


def check_run(run_dir):
    dangling = []
    checked = 0
    for path in walk_json(run_dir):
        try:
            with open(path, "r", encoding="utf-8", errors="replace") as handle:
                body = handle.read()
        except OSError as error:
            dangling.append({"file": os.path.relpath(path, run_dir), "path": "", "error": str(error)})
            continue
        documents = []
        if path.endswith(".jsonl"):
            for line in body.splitlines():
                line = line.strip()
                if not line:
                    continue
                try:
                    documents.append(json.loads(line))
                except json.JSONDecodeError:
                    pass
        else:
            try:
                documents.append(json.loads(body))
            except json.JSONDecodeError:
                continue
        for document in documents:
            for value in strings(document):
                if not looks_like_path(value):
                    continue
                checked += 1
                if resolve(run_dir, value) is None:
                    dangling.append(
                        {"file": os.path.relpath(path, run_dir), "path": value}
                    )
    return checked, dangling


def read_meta(run_dir):
    status = "unknown"
    scenario = os.path.basename(run_dir)
    decision = None
    try:
        with open(os.path.join(run_dir, "verdict.json"), "r", encoding="utf-8") as handle:
            verdict = json.load(handle)
        status = verdict.get("verdict", status)
        scenario = (verdict.get("scenario") or {}).get("name", scenario)
    except (OSError, json.JSONDecodeError):
        pass
    try:
        with open(os.path.join(run_dir, "retention.json"), "r", encoding="utf-8") as handle:
            decision = json.load(handle).get("decision")
    except (OSError, json.JSONDecodeError):
        pass
    return status, scenario, decision


def main(argv):
    as_json = "--json" in argv
    roots = [argument for argument in argv if not argument.startswith("--")]
    if not roots:
        print(__doc__.strip(), file=sys.stderr)
        return 2
    runs = []
    for root in roots:
        for dirpath, dirnames, filenames in os.walk(root):
            if "verdict.json" in filenames:
                dirnames[:] = []
                runs.append(dirpath)
    runs.sort()
    total_dangling = 0
    report = []
    for run_dir in runs:
        checked, dangling = check_run(run_dir)
        status, scenario, decision = read_meta(run_dir)
        total_dangling += len(dangling)
        report.append(
            {
                "dir": run_dir,
                "scenario": scenario,
                "status": status,
                "retention": decision,
                "references": checked,
                "dangling": dangling,
            }
        )
        if not as_json:
            flag = "OK  " if not dangling else "BAD "
            print(
                f"{flag} {status:<11} {str(decision):<20} refs={checked:<4} "
                f"{os.path.basename(run_dir)}"
            )
            for entry in dangling:
                print(f"       dangling: {entry['file']}: {entry['path']}")
    if as_json:
        print(json.dumps({"runs": report, "dangling": total_dangling}, indent=2))
    else:
        print(
            f"artifact references: {len(runs)} run(s) checked, {total_dangling} dangling"
        )
    return 1 if total_dangling else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
