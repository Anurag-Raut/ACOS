#!/usr/bin/env python3
"""Summarise one KEYPROBE result file (one JSON object per line).

Exit status 0 only when every check passed, the summary line agrees with the
check count, and at least one check ran. A missing file, an unparseable line or
a disagreement is a failure, never a skip.
"""
import json
import sys


def main(path: str, label: str) -> int:
    checks = 0
    failures = 0
    summary = None
    try:
        handle = open(path)
    except OSError as exc:
        print(f"[k5b] {label}: cannot open {path}: {exc}")
        return 1
    with handle:
        for line in handle:
            line = line.strip()
            if not line:
                continue
            try:
                entry = json.loads(line)
            except Exception as exc:  # noqa: BLE001 - report, do not skip
                print(f"[k5b] {label}: unparseable line: {line[:140]} ({exc})")
                failures += 1
                continue
            if entry.get("activity"):
                summary = entry
                continue
            checks += 1
            ok = bool(entry.get("ok"))
            if not ok:
                failures += 1
            print(f"[k5b] {'ok  ' if ok else 'FAIL'} {label}: "
                  f"{entry.get('check')} :: {entry.get('detail')}")
    if summary is None:
        print(f"[k5b] {label}: no summary line -> failure")
        failures += 1
    else:
        if summary.get("fail") or summary.get("pass") != checks or checks == 0:
            print(f"[k5b] {label}: summary {summary} disagrees with "
                  f"{checks} checks -> failure")
            failures += 1
    print(f"[k5b] {label}: {checks - min(failures, checks)}/{checks} checks pass")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1], sys.argv[2]))
