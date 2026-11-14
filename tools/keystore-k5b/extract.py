#!/usr/bin/env python3
"""Extract this probe's KEYPROBE records from an `acos logs --json` envelope.

`acos launch` does not surface a guest's stdout, but the guest log buffer is
readable through the session daemon (`acos logs --tag KEYPROBE --json`). The
probe writes every check to both stdout and logd, so this is the same set of
records, just carried by the channel the harness can read.

Usage: extract.py <logs.json> <out.jsonl> <label>
Exit 0 when at least one record was found, 1 otherwise (a missing channel is a
failure, not a skip).
"""
import json
import sys


def main(logs_path: str, out_path: str, label: str) -> int:
    with open(logs_path) as handle:
        envelope = json.load(handle)
    result = envelope.get("result") or {}
    records = []
    for entry in result.get("logs") or []:
        message = entry.get("message") or entry.get("line") or ""
        start = message.find('{"check"')
        if start < 0:
            if message.startswith("SUMMARY "):
                records.append(message[len("SUMMARY "):])
            continue
        records.append(message[start:])
    if not records:
        print(f"[k5b] {label}: the session log buffer holds no KEYPROBE records")
        return 1
    with open(out_path, "w") as out:
        for record in records:
            # Validate here so a truncated record cannot pass as a check.
            json.loads(record)
            out.write(record + "\n")
    print(f"[k5b] {label}: {len(records)} record(s) from the log buffer")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1], sys.argv[2], sys.argv[3]))
