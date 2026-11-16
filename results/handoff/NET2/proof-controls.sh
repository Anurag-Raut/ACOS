#!/usr/bin/env bash
# NET2 control proofs on the *wired* build: the same `acos-netd run` proxy the
# ART launcher spawns (fd-passing stdio, owner-bound lifetime), driven directly so
# the controls can be timestamped against a running guest.
#
#   offline isolation   one session dropped, the other keeps its HTTP 200
#   hostport opt-in     a session that opted in reaches a host listener on
#                       127.0.0.1:18081; a session that did not is refused; a
#                       host loopback port nobody opted into stays unreachable
#   fail closed/restart relay killed -> the session reports the network
#                       unavailable -> restart -> HTTP 200 again
#
# Usage: results/handoff/NET2/proof-controls.sh [OUTDIR]
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
WT=$(cd "$HERE/../../.." && pwd)
OUT=${1:-$HERE/run/controls}
mkdir -p "$OUT"
BIN=${BIN:-$WT/.cache/net2-target/release/acos-netd}
PASTA=${ACOS_NET_PASTA:?set ACOS_NET_PASTA to the pinned static pasta}
export ACOS_NET_PASTA=$PASTA
LOG=$OUT/proof.log
: >"$LOG"
say() { printf '%s\n' "$*" | tee -a "$LOG"; }

# A guest stand-in with the guest's privilege (a child of the session netns; no
# capability over the relay netns), fetching what the fixture fetches.
FETCH='curl -sS -m 4 -o /dev/null -w "%{http_code}" https://example.com || echo 000'

# Start clean: a supervisor left over from an earlier build would answer `up`
# with a fabric it can no longer exec (its binary was replaced by a rebuild).
"$BIN" shutdown >/dev/null 2>&1
ps -eo pid,args | awk '/http\.server (18081|18080)/ && !/awk/ {print $1}' | xargs -r kill 2>/dev/null
sleep 1

# A host listener the opt-in proof needs. 127.0.0.1 only: the boundary proof is
# that the *same* loopback is unreachable without the opt-in.
python3 -m http.server 18081 --bind 127.0.0.1 >"$OUT/host-listener.log" 2>&1 &
LISTENER=$!
trap 'kill $LISTENER 2>/dev/null; "$BIN" shutdown >/dev/null 2>&1' EXIT
sleep 1

"$BIN" up >"$OUT/up.json" 2>&1 || { say "relay did not start"; exit 1; }
say "check=relay_up PASS $(grep -o '"pasta_version": "[^"]*"' "$OUT/up.json")"

# --- offline isolation ---------------------------------------------------------
# Two guests, each fetching three times with a gap, so the control lands between
# two fetches of the same session (the app's own retry, not a re-launch).
GUEST='for i in 1 2 3; do printf "fetch$i=%s\n" "$('"$FETCH"')"; sleep 2; done'
"$BIN" spawn s-a --stdout "$OUT/s-a.log" --stderr "$OUT/s-a.err" -- /bin/sh -c "$GUEST" >/dev/null 2>&1
"$BIN" spawn s-b --stdout "$OUT/s-b.log" --stderr "$OUT/s-b.err" -- /bin/sh -c "$GUEST" >/dev/null 2>&1
sleep 1
"$BIN" offline s-a >"$OUT/s-a-offline.json" 2>&1
sleep 16
"$BIN" detach s-a >/dev/null 2>&1; "$BIN" detach s-b >/dev/null 2>&1
a_ok=$(grep -c 'fetch[0-9]=200' "$OUT/s-a.log" 2>/dev/null || true)
a_fail=$(grep -c 'fetch[0-9]=000' "$OUT/s-a.log" 2>/dev/null || true)
b_ok=$(grep -c 'fetch[0-9]=200' "$OUT/s-b.log" 2>/dev/null || true)
if [ "${a_fail:-0}" -ge 1 ] && [ "${b_ok:-0}" -ge 3 ]; then
  say "check=offline_isolation PASS offline_session_failed=$a_fail offline_session_200=$a_ok peer_200=$b_ok"
else
  say "check=offline_isolation FAIL offline_session_failed=$a_fail offline_session_200=$a_ok peer_200=$b_ok"
fi

# --- host-port opt-in ----------------------------------------------------------
# Only `18081` is named, and only for `opt`. `nope` has no opt-in; a session with
# no opt-in must also not reach any *other* host loopback port (18080) nor the
# same port on the host's loopback by address.
python3 -m http.server 18080 --bind 127.0.0.1 >"$OUT/host-listener-18080.log" 2>&1 &
LISTENER2=$!
sleep 1
"$BIN" spawn opt --stdout "$OUT/opt.log" --stderr "$OUT/opt.err" -- /bin/sh -c 'sleep 3; printf "gw=%s\n" "$(curl -sS -m 5 -o /dev/null -w "%{http_code}" http://10.163.1.1:18081/)"; printf "other=%s\n" "$(curl -sS -m 5 -o /dev/null -w "%{http_code}" http://10.163.1.1:18080/)"; printf "lo=%s\n" "$(curl -sS -m 5 -o /dev/null -w "%{http_code}" http://127.0.0.1:18081/)"; sleep 6' >/dev/null 2>&1
"$BIN" spawn nope --stdout "$OUT/nope.log" --stderr "$OUT/nope.err" -- /bin/sh -c 'sleep 3; printf "gw=%s\n" "$(curl -sS -m 5 -o /dev/null -w "%{http_code}" http://10.163.2.1:18081/)"; sleep 6' >/dev/null 2>&1
sleep 1
"$BIN" hostport add 18081 --session opt >"$OUT/hostport.json" 2>&1
sleep 10
"$BIN" detach opt >/dev/null 2>&1; "$BIN" detach nope >/dev/null 2>&1
kill $LISTENER2 2>/dev/null
one() { grep -o "$2=[0-9]*" "$1" 2>/dev/null | head -1 | cut -d= -f2; }
opt_gw=$(one "$OUT/opt.log" gw)
opt_other=$(one "$OUT/opt.log" other)
opt_lo=$(one "$OUT/opt.log" lo)
nope_gw=$(one "$OUT/nope.log" gw)
# Boundary half that must hold regardless of the forwarding mechanism: a host
# loopback address is never reachable from the guest.
if [ "${opt_lo:-1}" != 200 ] && [ "${opt_other:-1}" != 200 ]; then
  say "check=host_loopback_blocked PASS host_loopback=$opt_lo unopted_port=$opt_other"
else
  say "check=host_loopback_blocked FAIL host_loopback=$opt_lo unopted_port=$opt_other"
fi
if [ "${opt_gw:-0}" = 200 ] && [ "${nope_gw:-1}" != 200 ]; then
  say "check=hostport_optin PASS opted_in=$opt_gw not_opted_in=$nope_gw"
else
  say "check=hostport_optin FAIL opted_in=$opt_gw not_opted_in=$nope_gw (see REPORT.md §5: pasta -t/-T direction)"
fi

# --- fail closed, then restart -------------------------------------------------
# The guest fetches before and after the relay is killed mid-session. While the
# relay is held down the session's network is gone (the supervisor answers
# `network-unavailable` to every networked request); after `up` it returns.
"$BIN" spawn fc --stdout "$OUT/fc.log" --stderr "$OUT/fc.err" -- /bin/sh -c 'printf "before=%s\n" "$('"$FETCH"')"; sleep 5; printf "after=%s\n" "$('"$FETCH"')"; sleep 5; printf "recovered=%s\n" "$('"$FETCH"')"' >/dev/null 2>&1
sleep 3
"$BIN" kill-pasta --hold >"$OUT/kill.json" 2>&1
"$BIN" status >"$OUT/status-down.json" 2>&1
"$BIN" spawn refused -- /bin/true >"$OUT/refused.json" 2>&1
sleep 5
"$BIN" up >"$OUT/up2.json" 2>&1
sleep 6
"$BIN" detach fc >/dev/null 2>&1
before=$(one "$OUT/fc.log" before)
after=$(one "$OUT/fc.log" after)
recovered=$(one "$OUT/fc.log" recovered)
up_down=$(grep -c '"up": false' "$OUT/status-down.json" 2>/dev/null || true)
code=$(grep -c 'network-unavailable' "$OUT/refused.json" 2>/dev/null || true)
if [ "${before:-0}" = 200 ] && [ "${after:-1}" != 200 ] && [ "${recovered:-0}" = 200 ] && [ "${up_down:-0}" -ge 1 ] && [ "${code:-0}" -ge 1 ]; then
  say "check=fail_closed_restart PASS before=$before during=$after recovered=$recovered relay_down=yes spawn=network-unavailable"
else
  say "check=fail_closed_restart FAIL before=$before during=$after recovered=$recovered relay_down=${up_down:-0} spawn_code=${code:-0}"
fi

# --- no leak -------------------------------------------------------------------
"$BIN" detach refused >/dev/null 2>&1
"$BIN" status >"$OUT/status-final.json" 2>&1
say "check=no_leak $(grep -o '"relay_veths": \[[^]]*\]' "$OUT/status-final.json") $(grep -o '"session_rules": [0-9]*' "$OUT/status-final.json")"
"$BIN" shutdown >/dev/null 2>&1
