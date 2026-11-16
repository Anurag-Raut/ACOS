#!/usr/bin/env bash
# NET6 diagnosis: reproduce the "second fetch never returns after offline" hang
# and take an ART thread dump (SIGQUIT) from the *live* guest process while the
# fetch is stuck, plus every thread's kernel state. One ART run.
#
#   bash results/handoff/NET6/diag-stacks.sh <outdir>
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
WT=$(cd "$HERE/../../.." && pwd)
OUT=${1:?outdir}
BIN=${ACOS:-$WT/.cache/net6-target/release/acos}
export ACOS_FONT_DIR=${ACOS_FONT_DIR:-/home/Qui/Projects/acos/.cache/boot/android-34/fonts}
export ACOS_NET_PASTA=${ACOS_NET_PASTA:-$WT/.cache/net6/passt-src/pasta}
export ACOS_SPIKE_NET_PASTA=$ACOS_NET_PASTA
export ACOS_NETD_BIN=${ACOS_NETD_BIN:-$WT/.cache/net6-target/release/acos-netd}
export ACOS_NETD_STATE=${ACOS_NETD_STATE:-$WT/.cache/net6/netd-state}
export ACOS_RUNTIME_DIR=${ACOS_RUNTIME_DIR:-$WT/.cache/net6/rt}
export ACOS_SESSION_SOCKET=${ACOS_SESSION_SOCKET:-$ACOS_RUNTIME_DIR/session.sock}
export ACOS_ART_KEEP_RUN=1
export ACOS_SPIKE_NET_DNS_LOG=${ACOS_SPIKE_NET_DNS_LOG:-$OUT/dns.log}
mkdir -p "$OUT" "$ACOS_RUNTIME_DIR"
: >"$OUT/dns.log"
rm -rf "$OUT/art"

"$ACOS_NETD_BIN" up >"$OUT/netd-up.log" 2>&1 || { echo "relay failed"; exit 1; }

( cd "$WT" && tools/guarded.sh --mem 12G --time 300 -- env -u ACOS_HOME "$BIN" verify --engine art \
    --apk fixtures/build/apps/net-check.apk \
    --scenario "$WT/results/handoff/NET5/scenarios/open-finding-offline.yaml" \
    --artifacts "$OUT/art" >"$OUT/verify.out" 2>"$OUT/verify.err" ) &
RUN=$!

# Find the guest's main process (the ART LaunchMain of this session), then wait
# until the scenario has tapped Retry, then dump.
pid=""
for i in $(seq 1 60); do
  sleep 1
  pid=""
  for q in /proc/[0-9]*; do
    first=$(tr '\0' '\n' <"$q/cmdline" 2>/dev/null | head -1)
    case "$first" in
      /system/apex/*/dalvikvm64|*/bin/dalvikvm64) pid=${q#/proc/}; break ;;
    esac
  done
  [ -n "$pid" ] && break
done
echo "guest_pid=$pid (after ${i}s)" | tee "$OUT/live.txt"
if [ -z "$pid" ]; then
  wait $RUN; echo "no guest pid"; exit 1
fi

# The scenario reaches the retry tap ~1-3 s after the guest is up. Wait until
# the fetch is pending, then record states and dump.
sleep 12
pgid=$(ps -o pgid= -p "$pid" | tr -d ' ')
{
  echo "--- ps (guest process group $pgid) ---"
  ps -eo pid,ppid,pgid,stat,args | awk -v g="$pgid" 'NR==1 || $3==g'
  echo "--- threads (state / wchan / syscall) ---"
  for t in /proc/"$pid"/task/*; do
    tid=$(basename "$t")
    state=$(awk '{print $3}' "$t"/stat 2>/dev/null)
    name=$(cat "$t"/comm 2>/dev/null)
    echo "tid=$tid state=${state:-?} comm=$name wchan=$(cat "$t"/wchan 2>/dev/null) syscall=$(cat "$t"/syscall 2>/dev/null)"
  done
  echo "--- guest tcp sockets (ss -tnp) ---"
  ss -tnp 2>/dev/null | awk -v p="$pid" 'NR==1 || $0 ~ ("pid=" p ",")'
  echo "--- guest unix sockets ---"
  ss -xnp 2>/dev/null | awk -v p="$pid" 'NR==1 || $0 ~ ("pid=" p ",")'
  echo "--- fetch thread syscall, two samples 5 s apart ---"
  ftid=$(for t in /proc/"$pid"/task/*; do [ "$(cat "$t"/comm 2>/dev/null)" = "net-check-fetch" ] && basename "$t"; done | head -1)
  echo "fetch_tid=$ftid"
  cat /proc/"$pid"/task/"$ftid"/syscall 2>/dev/null
  sleep 5
  cat /proc/"$pid"/task/"$ftid"/syscall 2>/dev/null
  echo "--- sending SIGQUIT ---"
} >"$OUT/threads.txt" 2>&1
kill -QUIT "$pid" 2>>"$OUT/threads.txt"
sleep 4
i=0
for f in $(find /home/Qui/Projects/acos/.cache/art-locks/data-NET6-* /home/Qui/.cache/acos/art /run/user/1000 /dev/shm -maxdepth 4 -name 'keeper.log' -newermt '-10 minutes' 2>/dev/null); do
  i=$((i+1))
  cp "$f" "$OUT/keeper-$i.log"
  echo "keeper_log=$f -> keeper-$i.log ($(wc -c <"$f") bytes)" >>"$OUT/threads.txt"
done

wait $RUN
rc=$?
echo "verify_exit=$rc" | tee -a "$OUT/live.txt"
# Leave the relay sessions cleaned up.
"$ACOS_NETD_BIN" down >"$OUT/netd-down.log" 2>&1
exit 0
