#!/usr/bin/env bash
# NET6 diagnosis: reproduce the "second fetch never returns after offline" hang
# and inspect the *live* guest while the fetch is stuck: every thread's kernel
# state, the pollfd the fetch thread is blocked on, its sockets, an ART thread
# dump (SIGQUIT) and the guest's own stderr (which receives that dump).
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
    --scenario "${SCENARIO:-$WT/results/handoff/NET5/scenarios/open-finding-offline.yaml}" \
    --artifacts "$OUT/art" >"$OUT/verify.out" 2>"$OUT/verify.err" ) &
RUN=$!

# The guest's process (real ART) is the one whose argv[0] is dalvikvm64. The
# scenario reaches its Retry tap ~1-2 s after the guest boots, so sample the
# still-pending fetch a few seconds later.
# The app process is the one with the fixture's own threads: the zygote keeper
# and the template process are dalvikvm64 too, so argv[0] alone is not enough.
find_guest() {
  for q in /proc/[0-9]*/task/*; do
    case "$(cat "$q/comm" 2>/dev/null)" in
      net-check-fetch) echo "$q" | cut -d/ -f3; return 0 ;;
    esac
  done
  return 1
}

pid=""
for _ in $(seq 1 30); do
  sleep 1
  pid=$(find_guest) && break
done
echo "guest_pid=$pid" | tee "$OUT/live.txt"
[ -z "$pid" ] && { wait $RUN; echo "no guest pid"; exit 1; }
sleep 10

ftid=$(for t in /proc/"$pid"/task/*; do
  [ "$(cat "$t"/comm 2>/dev/null)" = "net-check-fetch" ] && basename "$t"
done | head -1)

{
  echo "--- ps (session process group) ---"
  pgid=$(ps -o pgid= -p "$pid" | tr -d ' ')
  ps -eo pid,ppid,pgid,stat,args | awk -v g="$pgid" 'NR==1 || $3==g' | cut -c1-200
  echo
  echo "--- threads (state / wchan / syscall) ---"
  for t in /proc/"$pid"/task/*; do
    tid=$(basename "$t")
    state=$(awk '{print $3}' "$t"/stat 2>/dev/null)
    name=$(cat "$t"/comm 2>/dev/null)
    echo "tid=$tid state=${state:-?} comm=$name wchan=$(cat "$t"/wchan 2>/dev/null) syscall=$(cat "$t"/syscall 2>/dev/null)"
  done
  echo
  echo "--- guest tcp sockets (ss -tnp) ---"
  ss -tnp 2>/dev/null | awk -v p="$pid" 'NR<=1 || $0 ~ ("pid=" p ",")'
  echo
  echo "--- fetch thread: the pollfd it is blocked on ---"
  echo "fetch_tid=$ftid"
  [ -n "$ftid" ] && python3 "$HERE/ppollfd.py" "$pid" "$ftid" 2>&1 | head -60
  echo
  echo "--- the guest's own netns socket tables (inode of the polled fd) ---"
  pfd=$(python3 "$HERE/ppollfd.py" "$pid" "$ftid" 2>/dev/null | sed -n 's/^RESULT fd=[0-9]* inode=\([0-9]*\)$/\1/p' | head -1)
  echo "polled_socket_inode=${pfd:-?}"
  if [ -n "${pfd:-}" ]; then
    for t in unix tcp tcp6 udp udp6; do
      echo "net/$t:"; grep -n "$pfd" /proc/"$pid"/net/$t 2>/dev/null | head -3
    done
    echo "nsenter unix:"; nsenter -t "$pid" -n -- ss -xap 2>&1 | grep -F "$pfd" | head -3
    echo "nsenter tcp:"; nsenter -t "$pid" -n -- ss -tanp 2>&1 | grep -F "$pfd" | head -3
  fi
  echo
  echo "--- SIGQUIT (ART thread dump -> the guest's stderr fd) ---"
} >"$OUT/threads.txt" 2>&1

kill -QUIT "$pid" 2>>"$OUT/threads.txt"
sleep 4
{
  echo "guest_stderr_fd=$(readlink /proc/"$pid"/fd/2)"
  if cp "/proc/$pid/fd/2" "$OUT/guest-stderr.txt" 2>/dev/null; then
    echo "copied guest stderr: $(wc -c <"$OUT/guest-stderr.txt") bytes"
  fi
  echo "guest_stdout_fd=$(readlink /proc/"$pid"/fd/1)"
  echo "guest_root=$(readlink /proc/"$pid"/root)"
  if [ -f /proc/"$pid"/root/data/app-data/dev.acos.fixtures.netcheck/diag.txt ] &&
     cp /proc/"$pid"/root/data/app-data/dev.acos.fixtures.netcheck/diag.txt \
        "$OUT/app-diag.txt" 2>/dev/null; then
    echo "copied app diag: $(wc -l <"$OUT/app-diag.txt") lines"
  else
    f=$(find /home/Qui/Projects/acos/.cache/art-locks/data-NET6-*/runs -name diag.txt 2>/dev/null | head -1)
    if [ -n "$f" ]; then cp "$f" "$OUT/app-diag.txt"; echo "copied app diag from $f"; fi
  fi
} >>"$OUT/threads.txt" 2>&1

wait $RUN
echo "verify_exit=$?" | tee -a "$OUT/live.txt"
"$ACOS_NETD_BIN" down >"$OUT/netd-down.log" 2>&1
exit 0
