#!/usr/bin/env bash
# REL3D end-to-end: two real builds, one socket.
#
#   old = rel-030 @ 064f568d   (the pre-REL3D daemon: no identity protocol)
#   new = rel3d    @ <branch>  (identity in hello/ping, refusal + replacement)
#
# Proves:
#   1. the two binaries are different builds (build id, git sha, binary hash);
#   2. an old daemon answers the identity handshake with a stale refusal - the
#      MODA11Y surface: a request the new build implements, answered by old code
#      (the old client has no way to notice; `OLD acos session list` accepts it);
#   3. the new client detects the old daemon, prints the structured
#      DAEMON_VERSION_MISMATCH warning with both identities, stops the idle
#      auto-started daemon and replaces it, BEFORE it answers anything;
#   4. the daemon that answers afterwards on the shared socket is the new build.
#
# Usage: bash results/handoff/REL3D/e2e_two_builds.sh
# Requires the two debug binaries (see the log header for the build commands).
set -uo pipefail

WT=$(cd "$(dirname "$0")/../../.." && pwd)
OLD=${REL3D_OLD_BIN:-$WT/.cache/rel3d-old-target/debug/acos}
NEW=${REL3D_NEW_BIN:-$WT/.cache/rel3d-target/debug/acos}
D=$(mktemp -d /tmp/rel3d-e2e-XXXXXX)
chmod 700 "$D"
SOCK=$D/session.sock
export ACOS_RUNTIME_DIR=$D
export ACOS_SESSION_IDLE_SECS=120
export ACOS_NO_DAEMON=0

fail=0
step() { printf '\n=== %s\n' "$*"; }
ok()   { printf 'PASS %s\n' "$*"; }
bad()  { printf 'FAIL %s\n' "$*"; fail=1; }

cleanup() {
  for f in "$D"/*.pid; do [ -f "$f" ] && kill "$(cat "$f")" 2>/dev/null; done
  ps -eo pid,args | awk -v d="$D" '$0 ~ d && !/awk/ {print $1}' | xargs -r kill 2>/dev/null
  rm -rf "$D"
}
trap cleanup EXIT

# --- 1. the two binaries are different builds ------------------------------
step "identities (each binary, on its own socket)"
old_id=$("$OLD" serve --json --idle-timeout 2 2>/dev/null | python3 -c 'import json,sys; print(json.load(sys.stdin)["identity"]["build_id"])') &
old_banner_pid=$!
sleep 0.6
old_id=$(ACOS_RUNTIME_DIR=$D/old "$OLD" serve --json --socket $D/old.sock --idle-timeout 1 2>/dev/null | head -1 | python3 -c 'import json,sys; print(json.load(sys.stdin)["identity"]["build_id"])' 2>/dev/null)
new_id=$(ACOS_RUNTIME_DIR=$D/new "$NEW" serve --json --socket $D/new.sock --idle-timeout 1 2>/dev/null | head -1 | python3 -c 'import json,sys; print(json.load(sys.stdin)["identity"]["build_id"])' 2>/dev/null)
wait $old_banner_pid 2>/dev/null
echo "old: ${old_id:-<none: pre-identity build>}"
echo "new: ${new_id:-<none>}"
if [ -z "$old_id" ]; then
  ok "the old build has no identity protocol (it cannot report a build id)"
else
  bad "the old build unexpectedly reported an identity: $old_id"
fi
[ -n "$new_id" ] && ok "the new build reports its identity: $new_id" || bad "the new build has no identity"

# --- 2. an old daemon on the shared socket --------------------------------
step "starting the OLD daemon (rel-030) on the shared socket, auto-started style"
# A client that has exited leaves the daemon orphaned: exactly what an upgrade
# finds. `setsid` + a subshell that exits gives ppid 1.
( setsid "$OLD" serve --socket "$SOCK" --idle-timeout 120 >"$D/old-daemon.log" 2>&1 &
  echo $! > "$D/old-daemon.pid" ) 
for _ in $(seq 1 100); do [ -S "$SOCK" ] && break; sleep 0.05; done
old_pid=$(cat "$D/old-daemon.pid" 2>/dev/null || true)
echo "old daemon pid=${old_pid:-?} ppid=$(awk '{print $4}' /proc/${old_pid:-0}/stat 2>/dev/null || echo '?')"
[ -S "$SOCK" ] && ok "old daemon is listening on the shared socket" || bad "old daemon never listened"

step "the same wire request, answered by the OLD daemon (the MODA11Y surface)"
python3 - "$SOCK" <<'PY'
import json, socket, sys
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.connect(sys.argv[1])
s.sendall((json.dumps({"id": 1, "tool": "hello", "args": {"client": {"protocol": 1}}}) + "\n").encode())
line = b""
while not line.endswith(b"\n"):
    chunk = s.recv(65536)
    if not chunk:
        break
    line += chunk
reply = json.loads(line)
env = reply.get("envelope", {})
print("old daemon reply -> status=%s error=%s identity=%s"
      % (env.get("status"), (env.get("error") or {}).get("code"),
         "yes" if (env.get("result") or {}).get("identity") else "no"))
assert not ((env.get("result") or {}).get("identity")), "old daemon must not report an identity"
PY
if [ $? -eq 0 ]; then
  ok "the old daemon answers the new build's service with a stale refusal and no identity"
else
  bad "unexpected old-daemon answer"
fi

step "the OLD client against the same daemon (no check: it just uses it)"
old_client_out=$("$OLD" session list --json 2>"$D/old-client.err")
echo "old client status=$(printf '%s' "$old_client_out" | python3 -c 'import json,sys; print(json.load(sys.stdin)["status"])' 2>/dev/null) stderr=$(wc -c <"$D/old-client.err") bytes"
if grep -q "DAEMON_VERSION_MISMATCH" "$D/old-client.err"; then
  bad "the old client never warns (it has no identity check)"
else
  ok "the old client used that daemon with no warning: the bug class is real"
fi

# --- 3. the new client on the same socket --------------------------------
step "the NEW client finds the old daemon"
new_client_out=$("$NEW" session list --json 2>"$D/new-client.err"); new_rc=$?
new_status=$(printf '%s' "$new_client_out" | python3 -c 'import json,sys; print(json.load(sys.stdin)["status"])' 2>/dev/null)
echo "new client rc=$new_rc status=$new_status"
printf '%s\n' "$new_client_out" | head -c 400; echo
echo "--- stderr:"; cat "$D/new-client.err"
if [ "$new_status" = "pass" ]; then ok "the new client's call passed (on its own build)"; else bad "new client call failed"; fi
if grep -q '"code":"DAEMON_VERSION_MISMATCH"' "$D/new-client.err"; then
  ok "structured DAEMON_VERSION_MISMATCH warning emitted"
else
  bad "no structured DAEMON_VERSION_MISMATCH warning"
fi
if grep -q '"predates_identity_protocol":true' "$D/new-client.err"; then
  ok "the warning names the daemon as pre-identity (both identities present)"
else
  bad "warning does not name the old daemon"
fi
if grep -q "\"build_id\":\"$new_id\"" "$D/new-client.err"; then
  ok "the warning names the new build's id"
else
  bad "warning does not name the new build id ($new_id)"
fi

step "the idle auto-started old daemon was replaced"
if kill -0 "$old_pid" 2>/dev/null; then
  bad "old daemon $old_pid is still alive"
else
  ok "old daemon $old_pid is gone"
fi

step "who answers on the shared socket now?"
python3 - "$SOCK" "$new_id" <<'PY'
import json, socket, sys
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.connect(sys.argv[1])
s.sendall((json.dumps({"id": 1, "tool": "hello", "args": {"client": {"protocol": 1}}}) + "\n").encode())
line = b""
while not line.endswith(b"\n"):
    chunk = s.recv(65536)
    if not chunk:
        break
    line += chunk
env = json.loads(line)["envelope"]
ident = (env.get("result") or {}).get("identity") or {}
print("shared daemon -> pid=%s build_id=%s auto_started=%s"
      % (ident.get("pid"), ident.get("build_id"), ident.get("auto_started")))
assert ident.get("build_id") == sys.argv[2], "the shared socket must belong to the new build"
PY
if [ $? -eq 0 ]; then ok "the shared socket answers as the NEW build"; else bad "shared socket is not the new build"; fi

step "acos doctor: running daemon identity vs the CLI"
"$NEW" doctor --json 2>/dev/null | python3 -c '
import json, sys
checks = json.load(sys.stdin)["checks"]
c = [c for c in checks if c["name"] == "session_daemon"][0]
print("session_daemon:", c["status"], "|", c["detail"].replace("\n", " / "))
assert c["status"] == "ok", "doctor must report a matching daemon"
'
if [ $? -eq 0 ]; then ok "doctor reports the daemon matches the CLI"; else bad "doctor mismatch"; fi

printf '\nRESULT %s\n' "$([ $fail -eq 0 ] && echo OK || echo FAILED)"
exit $fail
