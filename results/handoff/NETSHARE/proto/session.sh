#!/usr/bin/env bash
# Runs INSIDE a per-session netns (created by `unshare -n`), waits for the
# fabric to move the session's veth end in, configures it, then execs the
# command. Prints the route-ready timestamp so the supervisor can measure the
# attach cost. Not product code.
set -u
STATE=$1
IFACE=$2
ADDR=$3
GW=$4
shift 4
t0=$(date +%s%N)
ip link set lo up
for _ in $(seq 1 3000); do
  ip link show "$IFACE" >/dev/null 2>&1 && break
  sleep 0.002
done
ip link set "$IFACE" up
ip addr add "$ADDR" dev "$IFACE" 2>/dev/null
ip route add default via "$GW"
t1=$(date +%s%N)
echo "$t0 $t1" >"$STATE/sess.$BASHPID.setup"
exec "$@"
