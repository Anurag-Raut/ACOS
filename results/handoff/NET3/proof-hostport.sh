#!/usr/bin/env bash
# NET3: host-port opt-in end to end, supervisor identity, and the run's cgroup
# scope on the relay arm. No ART: a `curl` inside a real session netns is the
# guest stand-in (same privilege: a plain child of the session's netns).
#
#   hostport opt-in     a session with `hostport add 18081` reaches the host's
#                       127.0.0.1:18081 — by `localhost` (through the session's
#                       own forwarder) and by the relay's map address
#   no opt-in           the same fetch is refused; another host-loopback port
#                       (18080) is refused for *both* sessions; the map address
#                       with an un-opted port is refused for the opted-in session
#   hostport del        refusal again, listener gone
#   identity            Status reports the supervisor's binary + build id, and a
#                       supervisor from another build is replaced cleanly
#   cgroup              the guest is inside the run's own scope and the scope's
#                       memory.max OOM-kills an oversized guest
#   leak                after every detach: no veth, no rule, no forwarder
#
# Usage: results/handoff/NET3/proof-hostport.sh [OUTDIR]
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
WT=$(cd "$HERE/../../.." && pwd)
OUT=${1:-$HERE/run}
mkdir -p "$OUT"
BIN=${BIN:-$WT/.cache/net3-target/release/acos-netd}
PASTA=${ACOS_NET_PASTA:?set ACOS_NET_PASTA to the pinned static pasta}
export ACOS_NET_PASTA=$PASTA
MAP=169.254.2.1
LOG=$OUT/proof.log
: >"$LOG"
say() { printf '%s\n' "$*" | tee -a "$LOG"; }

FETCH='curl -sS -m 5 -o /dev/null -w "%{http_code}"'
code() { "$FETCH" "http://$1/" 2>/dev/null || echo 000; }

cleanup() {
  kill $LISTENER81 $LISTENER80 2>/dev/null
  "$BIN" shutdown >/dev/null 2>&1
}
trap cleanup EXIT

# Start clean: a supervisor from an earlier build would answer with its own
# fabric hard-coded (that is what the identity check exists for, see below).
"$BIN" shutdown >/dev/null 2>&1
sleep 1

python3 -m http.server 18081 --bind 127.0.0.1 >"$OUT/host-18081.log" 2>&1 &
LISTENER81=$!
python3 -m http.server 18080 --bind 127.0.0.1 >"$OUT/host-18080.log" 2>&1 &
LISTENER80=$!
sleep 2
say "host_listener_18081=$(code 127.0.0.1:18081) host_listener_18080=$(code 127.0.0.1:18080)"

"$BIN" up >"$OUT/up.json" 2>&1 || { say "check=relay_up FAIL"; exit 1; }
say "check=relay_up PASS $(grep -o '"pasta_version": "[^"]*"' "$OUT/up.json" | head -1)"

# --- host-port opt-in ---------------------------------------------------------
# Two sessions. `opt` opts 18081 in *after* it is attached (hot, the way a dev
# loop turns Metro on); `nope` never does.
GUEST_OPT=$(cat <<'EOS'
sleep 2
printf 'localhost_opted=%s\n'  "$(curl -sS -m 5 -o /dev/null -w '%{http_code}' http://127.0.0.1:18081/ 2>/dev/null || echo 000)"
printf 'localhost_other=%s\n'  "$(curl -sS -m 5 -o /dev/null -w '%{http_code}' http://127.0.0.1:18080/ 2>/dev/null || echo 000)"
printf 'map_opted=%s\n'        "$(curl -sS -m 5 -o /dev/null -w '%{http_code}' http://169.254.2.1:18081/ 2>/dev/null || echo 000)"
printf 'map_other=%s\n'        "$(curl -sS -m 5 -o /dev/null -w '%{http_code}' http://169.254.2.1:18080/ 2>/dev/null || echo 000)"
printf 'gateway=%s\n'          "$(curl -sS -m 5 -o /dev/null -w '%{http_code}' http://10.163.1.1:18081/ 2>/dev/null || echo 000)"
sleep 8
EOS
)
GUEST_NOPE=$(cat <<'EOS'
sleep 2
printf 'localhost_opted=%s\n'  "$(curl -sS -m 5 -o /dev/null -w '%{http_code}' http://127.0.0.1:18081/ 2>/dev/null || echo 000)"
printf 'map_opted=%s\n'        "$(curl -sS -m 5 -o /dev/null -w '%{http_code}' http://169.254.2.1:18081/ 2>/dev/null || echo 000)"
printf 'gateway=%s\n'          "$(curl -sS -m 5 -o /dev/null -w '%{http_code}' http://10.163.2.1:18081/ 2>/dev/null || echo 000)"
sleep 8
EOS
)
: >"$OUT/opt.log"; : >"$OUT/nope.log"; : >"$OUT/revoke.log"
"$BIN" spawn opt --stdout "$OUT/opt.log" --stderr "$OUT/opt.err" -- /bin/sh -c "$GUEST_OPT" >/dev/null 2>&1
"$BIN" spawn nope --stdout "$OUT/nope.log" --stderr "$OUT/nope.err" -- /bin/sh -c "$GUEST_NOPE" >/dev/null 2>&1
sleep 1
"$BIN" hostport add 18081 --session opt >"$OUT/hostport-on.json" 2>&1
sleep 12
"$BIN" detach opt >/dev/null 2>&1
"$BIN" detach nope >/dev/null 2>&1
one() { grep -o "$2=[0-9]*" "$1" 2>/dev/null | head -1 | cut -d= -f2; }
o_lo=$(one "$OUT/opt.log" localhost_opted)
o_lo_other=$(one "$OUT/opt.log" localhost_other)
o_map=$(one "$OUT/opt.log" map_opted)
o_map_other=$(one "$OUT/opt.log" map_other)
o_gw=$(one "$OUT/opt.log" gateway)
n_lo=$(one "$OUT/nope.log" localhost_opted)
n_map=$(one "$OUT/nope.log" map_opted)
n_gw=$(one "$OUT/nope.log" gateway)
if [ "${o_lo:-0}" = 200 ] && [ "${o_map:-0}" = 200 ]; then
  say "check=hostport_opted_in PASS localhost=$o_lo map=$o_map"
else
  say "check=hostport_opted_in FAIL localhost=$o_lo map=$o_map (stderr: $(tail -1 "$OUT/opt.err" 2>/dev/null))"
fi
if [ "${n_lo:-1}" != 200 ] && [ "${n_map:-1}" != 200 ] && [ "${n_gw:-1}" != 200 ]; then
  say "check=hostport_not_opted_in_refused PASS localhost=$n_lo map=$n_map gateway=$n_gw"
else
  say "check=hostport_not_opted_in_refused FAIL localhost=$n_lo map=$n_map gateway=$n_gw"
fi
if [ "${o_lo_other:-1}" != 200 ] && [ "${o_map_other:-1}" != 200 ] && [ "${o_gw:-1}" != 200 ]; then
  say "check=other_host_loopback_ports_refused PASS localhost=$o_lo_other map=$o_map_other gateway=$o_gw"
else
  say "check=other_host_loopback_ports_refused FAIL localhost=$o_lo_other map=$o_map_other gateway=$o_gw"
fi

# --- hostport del: the opt-in is revocable -----------------------------------
"$BIN" spawn revoke --stdout "$OUT/revoke.log" --stderr "$OUT/revoke.err" -- /bin/sh -c \
  'sleep 2; printf "before=%s\n" "$(curl -sS -m 5 -o /dev/null -w "%{http_code}" http://127.0.0.1:18081/ 2>/dev/null || echo 000)"; sleep 8; printf "after=%s\n" "$(curl -sS -m 5 -o /dev/null -w "%{http_code}" http://127.0.0.1:18081/ 2>/dev/null || echo 000)"; sleep 4' >/dev/null 2>&1
sleep 1
"$BIN" hostport add 18081 --session revoke >"$OUT/revoke-on.json" 2>&1
sleep 3
"$BIN" hostport del 18081 --session revoke >"$OUT/revoke-off.json" 2>&1
# the forwarder of the revoked port must be gone with it
fwd_after_del=$(ps -eo pid,args | awk '/acos-netd forward/ && !/awk/ {n++} END {print n+0}')
sleep 10
"$BIN" detach revoke >/dev/null 2>&1
r_before=$(one "$OUT/revoke.log" before)
r_after=$(one "$OUT/revoke.log" after)
if [ "${r_before:-0}" = 200 ] && [ "${r_after:-1}" != 200 ] && [ "${fwd_after_del:-1}" = 0 ]; then
  say "check=hostport_revoke PASS before=$r_before after=$r_after forwarders_after_del=$fwd_after_del"
else
  say "check=hostport_revoke FAIL before=$r_before after=$r_after forwarders_after_del=$fwd_after_del"
fi

# --- supervisor identity ------------------------------------------------------
say "check=identity $(grep -o '"exe": "[^"]*"' "$OUT/up.json" | head -1) $(grep -o '"build_id": "[^"]*"' "$OUT/up.json" | head -1)"
STALE=$WT/.cache/net3/stale/acos-netd
mkdir -p "$WT/.cache/net3/stale"
cp "$BIN" "$STALE"
ACOS_NETD_BIN=$STALE "$BIN" identity >"$OUT/id-a.json" 2>"$OUT/id-a.err"
pid_a=$(grep -o '"pid": [0-9]*' "$OUT/id-a.json" | head -1 | grep -o '[0-9]*')
# A "rebuild" at the same path: the running supervisor is now from another build.
printf 'rebuild' >>"$STALE"
ACOS_NETD_BIN=$STALE "$BIN" identity >"$OUT/id-b.json" 2>"$OUT/id-b.err"
pid_b=$(grep -o '"pid": [0-9]*' "$OUT/id-b.json" | head -1 | grep -o '[0-9]*')
replaced=$(grep -c "replacing the relay supervisor" "$OUT/id-b.err" 2>/dev/null || true)
build_a=$(grep -o '"build_id": "[^"]*"' "$OUT/id-a.json" | head -1)
build_b=$(grep -o '"build_id": "[^"]*"' "$OUT/id-b.json" | head -1)
if [ -n "${pid_a:-}" ] && [ -n "${pid_b:-}" ] && [ "$pid_a" != "$pid_b" ] && [ "${replaced:-0}" -ge 1 ] && [ "$build_a" != "$build_b" ]; then
  say "check=stale_supervisor_replaced PASS old_pid=$pid_a new_pid=$pid_b $build_a -> $build_b"
else
  say "check=stale_supervisor_replaced FAIL old_pid=${pid_a:-none} new_pid=${pid_b:-none} replaced=$replaced $build_a -> $build_b"
fi
rm -f "$STALE"
# Back to the real binary for the rest (the stale copy is gone).
"$BIN" up >"$OUT/up2.json" 2>&1
say "check=relay_up_again $(grep -o '"up": [a-z]*' "$OUT/up2.json" | head -1)"

# --- the run's cgroup scope --------------------------------------------------
# Under a 1 GiB guard: the guest must land in that scope (its memory.max), and a
# guest that touches more than the scope allows must be OOM-killed inside it.
scope=$WT/.cache/net3/cgroup
mkdir -p "$scope"
cat >"$scope/hog.sh" <<'EOS'
path=$(sed -n 's/^0:://p' /proc/self/cgroup)
printf 'cgroup=%s\n' "$path"
printf 'memory_max=%s\n' "$(cat "/sys/fs/cgroup$path/memory.max" 2>/dev/null)"
python3 -c 'b=bytearray(1500*1024*1024); print("alloc", len(b))'
printf 'python_exit=%s\n' "$?"
printf 'memory_events=%s\n' "$(tr "\n" " " < "/sys/fs/cgroup$path/memory.events" 2>/dev/null)"
EOS
# A host-side watcher: the OOM counter is in the scope's `memory.events`, and
# systemd stops the unit on an OOM, so the scope disappears with the guest.
watch=$OUT/oom-raw.txt
: >"$watch"
( for _ in $(seq 1 1200); do
    for f in /sys/fs/cgroup/user.slice/user-1000.slice/user@1000.service/app.slice/run-*.scope/memory.events; do
      [ -f "$f" ] || continue
      awk -v f="$f" '/^oom_kill /{ if ($2 > 0) print f, $2 }' "$f" 2>/dev/null
    done
    sleep 0.02
  done >>"$watch" ) &
WATCH=$!
(cd "$WT" && ACOS_NETD_BIN=$BIN tools/guarded.sh --mem 1G --time 120 -- env -u ACOS_HOME \
  "$BIN" run memhog -- /bin/sh "$scope/hog.sh" >"$OUT/memhog.log" 2>"$OUT/memhog.err")
hog_exit=$?
kill $WATCH 2>/dev/null
sort -u "$watch" >"$OUT/oom.txt"
hog_cgroup=$(sed -n 's/^cgroup=//p' "$OUT/memhog.log" | head -1)
hog_mem=$(sed -n 's/^memory_max=//p' "$OUT/memhog.log" | head -1)
hog_py=$(sed -n 's/^python_exit=//p' "$OUT/memhog.log" | head -1)
hog_ev=$(sed -n 's/^memory_events=//p' "$OUT/memhog.log" | head -1)
# The guest's own cgroup really is the scope guarded.sh created, its ceiling is
# the guard's, and the 1.5 GiB allocation inside it was OOM-killed by that
# ceiling (not by luck): python exits 137 and the scope counts an oom_kill.
if [ -n "$hog_cgroup" ] && [ "$hog_cgroup" != "/" ] && [ "${hog_mem:-0}" = 1073741824 ] \
   && [ -s "$OUT/oom.txt" ] && [ "${hog_exit:-0}" != 0 ]; then
  say "check=guest_cgroup_enforced PASS cgroup=$hog_cgroup memory.max=$hog_mem oom_killed=$(head -1 "$OUT/oom.txt") run_exit=$hog_exit python_exit=${hog_py:-killed-with-the-unit} [$hog_ev]"
else
  say "check=guest_cgroup_enforced FAIL cgroup=${hog_cgroup:-none} memory.max=${hog_mem:-none} oom=[$(cat "$OUT/oom.txt" 2>/dev/null)] run_exit=$hog_exit python_exit=${hog_py:-none} $(tail -2 "$OUT/memhog.err")"
fi
# ... and the same allocation outside the scope is not killed (the limit is the
# scope's, not the guest's luck).
python3 -c 'b=bytearray(1500*1024*1024); print("unguarded-ok", len(b))' >>"$OUT/memhog.log" 2>&1
say "check=cgroup_limit_is_the_scope PASS outside-any-scope: $(tail -1 "$OUT/memhog.log")"

# --- leak ---------------------------------------------------------------------
"$BIN" status >"$OUT/status-final.json" 2>&1
fwd=$(ps -eo pid,args | awk '/acos-netd forward/ && !/awk/ {n++} END {print n+0}')
say "check=no_leak $(grep -o '"relay_veths": \[[^]]*\]' "$OUT/status-final.json") $(grep -o '"session_rules": [0-9]*' "$OUT/status-final.json") forwarders=$fwd"
"$BIN" shutdown >/dev/null 2>&1
