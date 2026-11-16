#!/usr/bin/env bash
# NET3 §host-port: can pasta's guest -> host forward (`-T`, `--tcp-ns`) open a
# host loopback port for a *relay-topology* session?
#
# Topology (design B2's, without the supervisor):
#
#   userns (this script re-execs itself as root inside one)
#    ├─ netns A  the "relay" netns, the one pasta joins with `--netns`
#    │            veth end 10.163.1.1/30
#    └─ netns B  a "session" netns, joined to A by the veth pair
#                 veth end 10.163.1.2/30, default route via 10.163.1.1
#
# The host listener is 127.0.0.1:18081 in the *init* netns. The probes ask the
# only question that matters for Metro: does B reach the host's loopback?
#
# Every `-T` form of the pinned tag (2026_10_02.cba3570) is measured:
#   -T 18081              pasta binds the target ns's own 127.0.0.1
#   -T 169.254.2.15/18081 an explicit address in the target namespace
#   -T 10.163.1.1/18081   the one address a session can always reach
#
# Usage: results/handoff/NET3/exp-pasta-T.sh [OUTDIR]
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
WT=$(cd "$HERE/../../.." && pwd)
OUT=${1:-$HERE/run/pasta-T}
PASTA=${ACOS_NET_PASTA:?set ACOS_NET_PASTA to the pinned static pasta}

# Everything below needs to be root in the user namespace that owns netns A
# (pasta has to join A, and `nsenter` has to enter it): re-exec once.
if [ -z "${ACOS_NS_ROOT:-}" ]; then
  mkdir -p "$OUT"
  export ACOS_NP_PASTA=$PASTA
  ACOS_NS_ROOT=1 exec unshare --user --map-root-user -- env \
    ACOS_NS_ROOT=1 ACOS_NET_PASTA="$ACOS_NP_PASTA" bash "$0" "$OUT"
fi
mkdir -p "$OUT"
LOG=$OUT/experiment.log
: >"$LOG"
say() { printf '%s\n' "$*" | tee -a "$LOG"; }

python3 -m http.server 18081 --bind 127.0.0.1 >"$OUT/host-listener.log" 2>&1 &
LISTENER=$!
unshare -n -- sleep 300 & A=$!
unshare -n -- sleep 300 & B=$!
trap 'kill $LISTENER $A $B ${P:-} 2>/dev/null' EXIT
sleep 0.5
ip link set lo up
# The pair is created *inside* A: a veth cannot be created in another user
# namespace's netns (EPERM), and A is where pasta will live.
nsenter -t "$A" -n -- ip link add vrel type veth peer name vsess
nsenter -t "$A" -n -- ip link set vsess netns "/proc/$B/ns/net"
nsenter -t "$A" -n -- ip addr add 10.163.1.1/30 dev vrel
nsenter -t "$A" -n -- ip link set vrel up
nsenter -t "$A" -n -- ip link set lo up
nsenter -t "$B" -n -- ip addr add 10.163.1.2/30 dev vsess
nsenter -t "$B" -n -- ip link set vsess up
nsenter -t "$B" -n -- ip link set lo up
nsenter -t "$B" -n -- ip route add default via 10.163.1.1
nsenter -t "$A" -n -- sysctl -qw net.ipv4.ip_forward=1
# B reaches A's pasta interface address through the relay.
nsenter -t "$B" -n -- ip route add 169.254.2.0/24 via 10.163.1.1

probe_a() { nsenter -t "$A" -n -- curl -sS -m 4 -o /dev/null -w '%{http_code}' "http://$1/" 2>/dev/null || echo -n 000; }
probe_b() { nsenter -t "$B" -n -- curl -sS -m 4 -o /dev/null -w '%{http_code}' "http://$1/" 2>/dev/null || echo -n 000; }
say "host_listener_127.0.0.1:18081=$(curl -sS -m 4 -o /dev/null -w '%{http_code}' http://127.0.0.1:18081/ 2>/dev/null)"
# Control: a plain listener inside A answers from A and not from B (the probe
# machinery works, and a session's `127.0.0.1` is its own).
nsenter -t "$A" -n -- python3 -m http.server 18082 --bind 127.0.0.1 >/dev/null 2>&1 &
CTRL=$!
sleep 1
say "control_listener_in_A from_A=$(probe_a 127.0.0.1:18082) from_B=$(probe_b 127.0.0.1:18082)"
kill $CTRL 2>/dev/null
sleep 0.3

try() {
  desc=$1; listen=$2; probe=$3
  "$PASTA" --netns "/proc/$A/ns/net" --config-net -a 169.254.2.15/24 -g 169.254.2.1 \
    --no-map-gw --map-host-loopback none --no-splice -t none -u none -T "$listen" -U none \
    -f -q -- >"$OUT/pasta-$desc.log" 2>&1 &
  P=$!
  sleep 0.8
  from_a=$(probe_a "$(echo "$probe" | cut -d'|' -f1)")
  from_b=$(probe_b "$(echo "$probe" | cut -d'|' -f2)")
  log=$(grep -o "Listening address not allowed[^,]*\|Couldn.t listen[^\\n]*\|No rules to forward[^\\n]*\|Non loopback address[^\\n]*" \
        "$OUT/pasta-$desc.log" 2>/dev/null | head -1)
  say "form=$desc listen=$listen from_target_ns=$from_a from_session_ns=$from_b ${log:+pasta=\"$log\"}"
  kill $P 2>/dev/null
  sleep 0.4
}

# 1. `-T 18081`: the form NET2 tried first, and pasta's documented use (it binds
#    the *target namespace's* loopback).
try T-port "18081" "127.0.0.1:18081|10.163.1.1:18081"
# 2. `-T <relay-address>/18081`: the explicit gateway address form.
try T-addr-relay "169.254.2.15/18081" "169.254.2.15:18081|169.254.2.15:18081"
# 3. `-T 10.163.1.1/18081`: the one address a session is guaranteed to reach
#    (the relay end of its own veth).
try T-addr-veth "10.163.1.1/18081" "10.163.1.1:18081|10.163.1.1:18081"
