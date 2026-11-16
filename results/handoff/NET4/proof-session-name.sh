#!/usr/bin/env bash
# NET4: a one-shot `verify` session is addressable by `acos_net`.
#
# NET3 named the relay session `art-<pid>` for a one-shot run and `acos-<id>` for
# a serve session, so `acos_net` could only ever address the second kind. The fix
# is one naming rule (`acos-<session id>`) plus an id for every session the engine
# starts; this script watches the relay's own session table while a `verify` run
# holds its session, and compares it with the id the run published.
#
#   results/handoff/NET4/proof-session-name.sh OUTDIR
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
WT=$(cd "$HERE/../../.." && pwd)
OUT=${1:-$HERE/run/session-name}
mkdir -p "$OUT"
BIN=${ACOS:-$WT/.cache/net4-target/release/acos}
NETD=${ACOS_NETD_BIN:-$WT/.cache/net4-target/release/acos-netd}
LOG=$OUT/proof.log
: >"$LOG"
say() { printf '%s\n' "$*" | tee -a "$LOG"; }

export ACOS_SPIKE_NET=1 ACOS_NET_ARM=relay
rm -rf "$OUT/art"
( cd "$WT" && env -u ACOS_HOME "$BIN" verify --engine art \
    --apk fixtures/build/apps/net-check.apk --scenario scenarios/net-check.yaml \
    --artifacts "$OUT/art" >"$OUT/verify.out" 2>"$OUT/verify.err" ) &
run=$!
# Poll the supervisor's own table for as long as the session exists.
: >"$OUT/names.txt"
for _ in $(seq 1 200); do
  "$NETD" status 2>/dev/null | python3 -c '
import json,sys
try:
    status = json.load(sys.stdin)
except Exception:
    raise SystemExit(0)
for session in status.get("sessions", []):
    print(session.get("name"))
' >>"$OUT/names.txt" 2>/dev/null
  kill -0 $run 2>/dev/null || break
  sleep 0.05
done
wait $run
id=$(python3 -c '
import json,sys
try:
    print(json.load(open(sys.argv[1]))["engine"].get("session_id") or "")
except Exception:
    print("")
' "$OUT/art/engine.json")
names=$(sort -u "$OUT/names.txt" | grep -v '^$' | tr '\n' ' ')
say "engine_session_id=$id"
say "relay_session_names=$names"
if [ -n "$id" ] && printf '%s' "$names" | grep -q "acos-$id"; then
  say "check=one_shot_session_is_addressable PASS id=$id relay_name=acos-$id"
else
  say "check=one_shot_session_is_addressable FAIL id=$id names=$names"
fi
if printf '%s' "$names" | grep -q "art-"; then
  say "check=no_art_pid_names FAIL names=$names"
else
  say "check=no_art_pid_names PASS"
fi
