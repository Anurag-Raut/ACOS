#!/usr/bin/env bash
# test-agent-loop.sh -- tests for the W49 agent-loop benchmark (fake daemon).
#
# The real driver (tools/art-runtime/agent_loop.py) is exercised against a fake
# `acos serve` Unix socket, so no guest runs. The suite has teeth in both
# directions:
#
#   A. a working loop (all verbs ok, small tree) is reported overall PASS, with
#      8 round-trips to a verdict and the tree-payload gate PASS;
#   B. a loop whose launch is blocked (the real HEAD failure) is overall FAIL,
#      tree payload and round-trips are NOT_MEASURED (never a pass), and the gap
#      list names the window handoff and the missing ART engine;
#   C. a 600 ms tree p95 is a latency FAIL and produces a named gap;
#   D. a tree payload above 64 KiB is a payload FAIL;
#   E. every case still writes a schema-valid result with false_pass == 0.
#
# Usage: tools/tests/test-agent-loop.sh
# Scratch: .cache/tests/agent-loop-<pid>/ (removed on success, kept on failure).
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
export ACOS_REPO="$ROOT"
TMP="$ROOT/.cache/tests/agent-loop-$$"
rm -rf "$TMP"; mkdir -p "$TMP/fake/release" "$TMP/out"
printf '#!/bin/sh\nexit 0\n' > "$TMP/fake/release/acos"
chmod +x "$TMP/fake/release/acos"

PASSES=0; FAILURES=0
check() { # check NAME CONDITION [DETAIL]
    if [ "$2" = "1" ]; then PASSES=$((PASSES+1)); echo "PASS $1";
    else FAILURES=$((FAILURES+1)); echo "FAIL $1${3:+ -- $3}"; fi
}

cat > "$TMP/fake_daemon.py" <<'PY'
import argparse, json, os, socket, sys, time

def envelope(ok, status="pass", result=None, error=None):
    out = {"request_id": "fake", "ok": ok, "status": status,
           "backend": "acos-native", "fidelity": "native-runtime",
           "metrics": {"wall_ms": 0}}
    if result is not None:
        out["result"] = result
    if error is not None:
        out["error"] = error
    return out

BLOCKED = {"code": "UNSUPPORTED_BACKEND", "message":
           "WindowManagerGlobal has no root view: the app has not added a window yet",
           "retryable": False, "class": "unsupported",
           "hint": "The addToDisplayAsUser reply left 796 unread bytes."}

def tree(mode, session):
    app = "counter" if "counter" in session else "form-validation"
    if app == "counter":
        nodes = [
            {"id": 1, "class": "android.widget.TextView", "resource_id":
             "dev.acos.fixtures.counter:id/count", "text": "1", "children": []},
            {"id": 2, "class": "android.widget.Button", "resource_id":
             "dev.acos.fixtures.counter:id/inc", "text": "Increment", "children": []},
        ]
    else:
        nodes = [
            {"id": 1, "class": "android.widget.EditText", "resource_id":
             "dev.acos.fixtures.formvalidation:id/name", "text": "Ada", "children": []},
            {"id": 2, "class": "android.widget.Button", "resource_id":
             "dev.acos.fixtures.formvalidation:id/submit", "text": "Submit", "children": []},
        ]
    if mode == "big_tree":
        nodes.append({"id": 3, "class": "android.view.View", "resource_id": "",
                      "text": "x" * 70000, "children": []})
    return {"windows": 1, "truncated": False, "max_nodes": 2000, "nodes": nodes}

def handle(req, mode):
    tool = req.get("tool"); args = req.get("args") or {}
    session = (args.get("base") or {}).get("session_id", "")
    if tool == "ping":
        return envelope(True, result={"pong": True, "pid": os.getpid()})
    if mode == "broken" and tool not in ("acos_reset",):
        return envelope(False, "unsupported", error=dict(BLOCKED))
    if tool == "acos_launch":
        return envelope(True, result={"reached_stage": "first_frame",
                                      "published_frames": 1})
    if tool == "acos_observe_tree":
        if mode == "slow_tree":
            # Slow every 4th tree call: p95 is then ~600 ms without making the
            # whole test wait for every sample.
            handle.calls = getattr(handle, "calls", 0) + 1
            if handle.calls % 4 == 0:
                time.sleep(0.6)
        return envelope(True, result={"session_id": session, "tree": tree(mode, session),
                                      "truncated": False})
    if tool == "acos_observe":
        path = args.get("screenshot")
        if path:
            with open(path, "wb") as f:
                f.write(b"\x89PNG\r\n\x1a\n" + b"\0" * 64)
            return envelope(True, result={"screenshot": {"path": path, "bytes": 72}})
        return envelope(True, result={"frame": {"frame_seq": 1}})
    if tool in ("acos_tap", "acos_text", "acos_key"):
        return envelope(True, result={"accepted": True, "state_changed": True})
    if tool == "acos_logs":
        return envelope(True, result={"count": 1, "logs": [
            {"tag": "AcosFixture", "line": "onCreate"},
            {"tag": "AcosFixture", "line": "onResume"}]})
    if tool == "acos_reset":
        return envelope(True, result={"state_reset": True})
    return envelope(False, "error", error={"code": "INTERNAL", "message": tool,
                                           "retryable": False, "class": "harness"})

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--socket", required=True)
    ap.add_argument("--mode", default="ok")
    a = ap.parse_args()
    try: os.unlink(a.socket)
    except OSError: pass
    srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    srv.bind(a.socket); srv.listen(4)
    while True:
        conn, _ = srv.accept()
        f = conn.makefile("rwb")
        for line in f:
            if not line.strip():
                continue
            req = json.loads(line)
            resp = {"id": req.get("id"), "envelope": handle(req, a.mode)}
            f.write((json.dumps(resp) + "\n").encode()); f.flush()

if __name__ == "__main__":
    main()
PY

run_case() { # run_case NAME MODE
    local name="$1" mode="$2" sock="$TMP/$1.sock"
    python3 -I "$TMP/fake_daemon.py" --socket "$sock" --mode "$mode" &
    local fake=$!
    for _ in $(seq 1 100); do [ -S "$sock" ] && break; sleep 0.05; done
    ACOS_BENCH_BINARY="$TMP/fake/release/acos" python3 -I \
        "$ROOT/tools/art-runtime/agent_loop.py" --socket "$sock" \
        --out "$TMP/out/$name.json" --markdown "$TMP/out/$name.md" \
        --scratch "$TMP/scratch-$name" --apps counter,form-validation \
        --loops 11 --runs 2 >"$TMP/out/$name.log" 2>&1
    local rc=$?
    kill "$fake" 2>/dev/null; wait "$fake" 2>/dev/null
    echo "$rc"
}

jsonval() { # jsonval FILE EXPR
    python3 -I - "$1" "$2" <<'PY'
import json, os, sys
d = json.load(open(sys.argv[1]))
sys.path.insert(0, os.environ.get("ACOS_REPO", ".") + "/tools/bench")
import benchlib
if sys.argv[2] == "schema_ok":
    print("1" if not benchlib.validate_matrix(d) else "0")
elif sys.argv[2] == "gates":
    print(json.dumps(d["agent_loop"]["gates"], sort_keys=True))
elif sys.argv[2] == "gap_ids":
    print(",".join(g["id"] for g in d["agent_loop"]["gaps"]))
elif sys.argv[2] == "summary":
    print(json.dumps(d["summary"], sort_keys=True))
elif sys.argv[2] == "rtt":
    vals = {r["round_trips_to_verdict"] for r in d["runs"] if not r["warmup"]}
    print(json.dumps(sorted(v for v in vals if v is not None)))
else:
    print("?")
PY
}

# --- A: a working loop ------------------------------------------------------
RC=$(run_case ok ok)
check "A1 working loop exits 0" "$([ "$RC" = 0 ] && echo 1 || echo 0)" "rc=$RC"
check "A2 working result is schema-valid" "$(jsonval "$TMP/out/ok.json" schema_ok)" \
      "$(head -5 "$TMP/out/ok.log")"
GATES=$(jsonval "$TMP/out/ok.json" gates)
check "A3 working loop overall PASS" "$(echo "$GATES" | grep -q '"overall": "PASS"' && echo 1 || echo 0)" "$GATES"
check "A4 tree payload gate PASS" "$(echo "$GATES" | grep -q 'counter:tree_payload<=64KiB": "PASS"' && echo 1 || echo 0)" "$GATES"
check "A5 round-trip gate PASS" "$(echo "$GATES" | grep -q 'task:round_trips<=12": "PASS"' && echo 1 || echo 0)" "$GATES"
check "A6 verdict reached in 8 round-trips" "$(jsonval "$TMP/out/ok.json" rtt | grep -q '\[8\]' && echo 1 || echo 0)" "$(jsonval "$TMP/out/ok.json" rtt)"
SUM=$(jsonval "$TMP/out/ok.json" summary)
check "A7 every counted run passed" "$(echo "$SUM" | grep -q '"passed": 40' && echo 1 || echo 0)" "$SUM"
check "A8 no false pass" "$(echo "$SUM" | grep -q '"false_pass": 0' && echo 1 || echo 0)" "$SUM"

# --- B: the real HEAD blocker (no window) ----------------------------------
RC=$(run_case broken broken)
check "B1 blocked loop exits 0 (report written)" "$([ "$RC" = 0 ] && echo 1 || echo 0)" "rc=$RC"
check "B2 blocked result is schema-valid" "$(jsonval "$TMP/out/broken.json" schema_ok)" \
      "$(head -5 "$TMP/out/broken.log")"
GATES=$(jsonval "$TMP/out/broken.json" gates)
check "B3 blocked loop overall FAIL" "$(echo "$GATES" | grep -q '"overall": "FAIL"' && echo 1 || echo 0)" "$GATES"
check "B4 tree payload NOT_MEASURED" "$(echo "$GATES" | grep -q 'counter:tree_payload<=64KiB": "NOT_MEASURED"' && echo 1 || echo 0)" "$GATES"
check "B5 round-trips NOT_MEASURED" "$(echo "$GATES" | grep -q 'task:round_trips<=12": "NOT_MEASURED"' && echo 1 || echo 0)" "$GATES"
GAPS=$(jsonval "$TMP/out/broken.json" gap_ids)
check "B6 gap names the window handoff" "$(echo "$GAPS" | grep -q 'runtime-no-window' && echo 1 || echo 0)" "$GAPS"
check "B7 gap names the missing ART engine" "$(echo "$GAPS" | grep -q 'serve-no-art-engine' && echo 1 || echo 0)" "$GAPS"
SUM=$(jsonval "$TMP/out/broken.json" summary)
check "B8 no false pass on the error path" "$(echo "$SUM" | grep -q '"false_pass": 0' && echo 1 || echo 0)" "$SUM"
check "B9 all counted runs failed" "$(echo "$SUM" | grep -q '"failed": 40' && echo 1 || echo 0)" "$SUM"

# --- C: a 600 ms tree p95 must fail the latency gate -----------------------
RC=$(run_case slow slow_tree)
GATES=$(jsonval "$TMP/out/slow.json" gates)
GAPS=$(jsonval "$TMP/out/slow.json" gap_ids)
check "C1 slow tree fails its p95 gate" "$(echo "$GATES" | grep -q 'counter:tree:p95<500ms": "FAIL"' && echo 1 || echo 0)" "$GATES"
check "C2 slow tree produces a named gap" "$(echo "$GAPS" | grep -q 'counter-tree-latency' && echo 1 || echo 0)" "$GAPS"
check "C3 slow loop is overall FAIL" "$(echo "$GATES" | grep -q '"overall": "FAIL"' && echo 1 || echo 0)" "$GATES"

# --- D: a >64 KiB tree must fail the payload gate --------------------------
RC=$(run_case big big_tree)
GATES=$(jsonval "$TMP/out/big.json" gates)
check "D1 big tree fails the payload gate" "$(echo "$GATES" | grep -q 'counter:tree_payload<=64KiB": "FAIL"' && echo 1 || echo 0)" "$GATES"
check "D2 big tree still passes latency" "$(echo "$GATES" | grep -q 'counter:tree:p95<500ms": "PASS"' && echo 1 || echo 0)" "$GATES"

echo
echo "test-agent-loop: $PASSES passed, $FAILURES failed"
if [ "$FAILURES" -eq 0 ]; then
    rm -rf "$TMP"
    exit 0
fi
echo "test-agent-loop: scratch kept at $TMP" >&2
exit 1
