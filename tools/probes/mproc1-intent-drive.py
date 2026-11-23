#!/usr/bin/env python3
"""MPROC1 step 2 driver: the intent capture target + a scripted activity result.

Drives one real `acos-art launch` interactive session (line-JSON on stdin/stdout):
reads the ready line, asks the session control op for the newest capture
(`ctl intents last`), scripts the answer a photo picker would give
(`ctl intents result`), then reads the guest's own logd entries for the
`ACOSPROBE` lines. Every printed fact is a reply from the session; nothing is
inferred.

    python3 tools/probes/mproc1-intent-drive.py <acos-art> <apk> <out-dir>

Run it inside the shared ART lock like any other ART run:

    tools/art-runtime/run-exclusive.sh --task MPROC1 -- \
        tools/guard/guarded.sh --mem 8G --time 420 -- \
        python3 tools/probes/mproc1-intent-drive.py .cache/<track>-target/debug/acos-art \
        fixtures/build/apps/intent-probe.apk .cache/<track>/out/step2
"""

import json
import os
import subprocess
import sys
import time

art, apk, out = sys.argv[1], sys.argv[2], sys.argv[3]
env = dict(os.environ)
env["ACOSLAUNCH_INTERACTIVE"] = "1"
env.pop("ACOS_HOME", None)
os.makedirs(out, exist_ok=True)
proc = subprocess.Popen(
    [art, "launch", "--apk", apk, "--until", "activity-resumed", "--out", out],
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    stderr=subprocess.DEVNULL,
    text=True,
    env=env,
)


def reply():
    line = proc.stdout.readline()
    if not line:
        print("EOF")
        return None
    try:
        return json.loads(line)
    except json.JSONDecodeError:
        print("RAW", line[:200])
        return None


def send(request):
    proc.stdin.write(json.dumps(request) + "\n")
    proc.stdin.flush()
    return reply()


ready = reply() or {}
print("ready ok=", ready.get("ok"), "package=", ready.get("package"))

last = send({"cmd": "ctl", "module": "intents", "op": "last"}) or {}
print("last:", json.dumps(last)[:600])

result = send(
    {
        "cmd": "ctl",
        "module": "intents",
        "op": "result",
        "args": {
            "data": "content://com.acos.capture.images/pick.png",
            "mime_type": "image/png",
            "extras": {"dev.acos.capture.NOTE": "scripted picker"},
        },
    }
) or {}
print("result:", json.dumps(result)[:600])

# The delivery is asynchronous in the guest (a watcher thread); give it a beat
# before asking for the log snapshot the app itself wrote.
time.sleep(1.0)
logs = send({"cmd": "logs"}) or {}
for entry in logs.get("entries", []):
    message = str(entry.get("message", ""))
    if "ACOSPROBE" in message or "activity-result" in message:
        print("LOG", entry.get("tag"), message[:220])

send({"cmd": "close"})
proc.wait(timeout=30)
