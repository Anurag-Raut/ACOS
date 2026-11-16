#!/usr/bin/env bash
# Runs INSIDE the relay netns ("fabric"). Owns tap0 (from pasta) plus the
# per-session veth ends, and executes setup commands received from the
# supervisor on a fifo. Nothing here is product code: this is the NETSHARE
# prototype's fabric half (option B2).
set -u
STATE=$1
echo $$ >"$STATE/fabric.pid"
ip link set lo up
echo ready >"$STATE/fabric.ready"
while IFS= read -r line; do
  id=${line%%|*}
  cmd=${line#*|}
  case "$cmd" in
    quit) exit 0 ;;
  esac
  # One command per line; the supervisor waits for ack.<id>.
  printf '%s\n' "--- $id: $cmd" >>"$STATE/fabric.cmdlog"
  bash -c "$cmd" >>"$STATE/fabric.cmdlog" 2>&1
  echo "$?" >"$STATE/ack.$id"
done <"$STATE/fabric.fifo"
