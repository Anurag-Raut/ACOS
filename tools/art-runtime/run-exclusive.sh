#!/usr/bin/env bash
# Host-wide ART run lock. Usage:
#   run-exclusive.sh --task NAME [--exclusive] -- <command...>
#
# Gives each invocation unique scratch (ACOS_ART_DATA) and logs UTC start/finish
# + queue wait to .cache/art-locks/runs.log.
#
# Slots (2026-11-14). The host has room for more than one guarded ART run, and a
# single global lock made agents queue 4-8 min per acquisition. So there are
# ACOS_ART_SLOTS (default 2) slot files; slot 0 is the historical `art.lock`, so
# older copies of this script (which flock only art.lock) still interlock.
#
#   * default (shared): correctness runs (gates, scenarios, crate tests, corpus).
#     Takes slot 0 if free. Takes slot >=1 only when ALL of these hold:
#       - no exclusive acquisition is pending,
#       - the slot-0 holder is not a benchmark/probe (checked from /proc/locks +
#         the holder's cmdline, so legacy bench holders are respected too),
#       - MemAvailable >= the command's guarded.sh --mem + ACOS_ART_SLOT_HEADROOM_GIB
#         (default 12; a command without --mem is assumed to need 16 GiB).
#   * --exclusive (or ACOS_ART_EXCLUSIVE=1): takes every slot. Use it for any
#     timing/memory measurement (tools/bench/acos-bench.sh passes it), so
#     benchmark numbers never share the host with another ART run.
set -euo pipefail
task=adhoc
exclusive=${ACOS_ART_EXCLUSIVE:-0}
while [ $# -gt 0 ]; do
  case "$1" in
    --task) task=$2; shift 2 ;;
    --exclusive) exclusive=1; shift ;;
    --) shift; break ;;
    *) break ;;
  esac
done
[ $# -gt 0 ] || { echo "usage: $0 --task NAME [--exclusive] -- cmd..." >&2; exit 2; }
root=/home/Qui/Projects/acos
lockdir=${ACOS_ART_LOCKDIR:-$root/.cache/art-locks}; mkdir -p "$lockdir"
log=$lockdir/runs.log
slots=${ACOS_ART_SLOTS:-2}
headroom=${ACOS_ART_SLOT_HEADROOM_GIB:-12}
t0=$(date +%s); u0=$(date -u +%FT%TZ)

slot_file() { if [ "$1" = 0 ]; then echo "$lockdir/art.lock"; else echo "$lockdir/art.lock.$1"; fi; }

# GiB the wrapped command asks guarded.sh for (first --mem NG), else 16.
need_gib=16
prev=""
for a in "$@"; do
  if [ "$prev" = "--mem" ]; then n=${a%[Gg]}; [[ "$n" =~ ^[0-9]+$ ]] && need_gib=$n; break; fi
  prev=$a
done

mem_avail_gib() { awk '/^MemAvailable:/{print int($2/1048576)}' /proc/meminfo; }

# Is the current holder of slot 0 a benchmark/probe? (legacy holders included)
slot0_holder_is_bench() {
  local ino pid
  ino=$(stat -c %i "$(slot_file 0)" 2>/dev/null) || return 1
  # Held FLOCK entries on that inode (waiters are listed as "N: -> FLOCK ...").
  for pid in $(awk -v ino="$ino" '$2=="FLOCK" {split($6, a, ":"); if (a[3]==ino) print $5}' /proc/locks \
               | sed 's/[^0-9]//g' | sort -u); do
    [ -r "/proc/$pid/cmdline" ] || continue
    if tr '\0' ' ' < "/proc/$pid/cmdline" | grep -Eq 'bench|probe|--exclusive'; then return 0; fi
  done
  return 1
}

excl_pending() {
  local f pid
  for f in "$lockdir"/excl-pending.*; do
    [ -e "$f" ] || continue
    pid=${f##*.}
    if kill -0 "$pid" 2>/dev/null; then return 0; fi
    rm -f "$f"
  done
  return 1
}

slot=-1
if [ "$exclusive" = 1 ]; then
  marker="$lockdir/excl-pending.$$"
  : > "$marker"
  trap 'rm -f "$marker"' EXIT
  for ((i = 0; i < slots; i++)); do
    exec {fd}>"$(slot_file "$i")"
    flock "$fd"
  done
  rm -f "$marker"; trap - EXIT
  slot=all
else
  while :; do
    # A pending exclusive run goes first (no starvation by a stream of shared runs).
    if excl_pending; then sleep 2; continue; fi
    exec {fd}>"$(slot_file 0)"
    if flock -n "$fd"; then slot=0; break; fi
    exec {fd}>&-
    if [ "$slots" -gt 1 ] && ! slot0_holder_is_bench \
       && [ "$(mem_avail_gib)" -ge $((need_gib + headroom)) ]; then
      for ((i = 1; i < slots; i++)); do
        exec {fd}>"$(slot_file "$i")"
        if flock -n "$fd"; then slot=$i; break 2; fi
        exec {fd}>&-
      done
    fi
    sleep 2
  done
fi
t1=$(date +%s)
export ACOS_ART_DATA="$lockdir/data-$task-$$"; mkdir -p "$ACOS_ART_DATA"
export ACOS_ART_LOCK_SLOT=$slot
rc=0; "$@" || rc=$?
t2=$(date +%s)
echo "task=$task started_utc=$u0 finished_utc=$(date -u +%FT%TZ) elapsed_s=$((t2-t0)) queue_wait_s=$((t1-t0)) slot=$slot rc=$rc cmd=$*" >> "$log"
rm -rf "$ACOS_ART_DATA"
exit $rc
