#!/usr/bin/env bash
# p6-orphan.sh -- does the guest survive a SIGKILL of its launcher?
#
# Run the whole script under tools/guarded.sh (see S2.md). Inside, bwrap is
# spawned directly (bwrap-raw.sh) because guarded.sh's process-group cleanup
# would otherwise mask the answer.
#
# Chain, mirroring the product (launcher.rs pre_exec -> bwrap -> dalvikvm64):
#
#   level 1  "launcher": a bash subshell whose only job is to hold level 2.
#            It reproduces launcher.rs:216-233 exactly: prctl(PR_SET_PDEATHSIG,
#            SIGKILL) in the to-be-exec'd child, then exec bwrap.
#   level 2  bwrap (the argv from bwrap-raw.sh)
#   level 3  the guest: /system/bin/sh -> toybox sleep <unique>
#
# Guest identity is found on the host by (comm == sleep, cmdline == "sleep N")
# with a per-trial unique N. With --unshare-pid the guest's own $$ is a
# namespace pid (2), so a pid file written by the guest is useless.
set -uo pipefail
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RAW="$DIR/bwrap-raw.sh"
HARD="--unshare-pid --unshare-net --unshare-ipc --unshare-uts --unshare-cgroup-try --die-with-parent --clearenv --cap-drop ALL"
PDEATHSIG='import ctypes,os,sys
ctypes.CDLL("libc.so.6", use_errno=True).prctl(1, 9, 0, 0, 0)   # PR_SET_PDEATHSIG, SIGKILL
os.execv(sys.argv[1], sys.argv[1:])'

ppid_of() { awk '{print $4}' "/proc/$1/stat" 2>/dev/null; }
alive()   { kill -0 "$1" 2>/dev/null && echo yes || echo no; }
is_sleep() { [ "$(cat /proc/$1/comm 2>/dev/null)" = sleep ] &&
             [ "$(tr -d '\0' <"/proc/$1/cmdline" 2>/dev/null)" = "sleep$2" ]; }
find_guest() { for p in /proc/[0-9]*; do p=${p#/proc/}; is_sleep "$p" "$1" && { echo "$p"; return; }; done; }

trial() { # trial MODE TARGET(A|B) SLEEPSECS
  local mode=$1 target=$2 n=$3
  local extra=""; [ "$mode" = hard ] && extra="$HARD"
  ( S2_EXTRA="$extra" python3 -c "$PDEATHSIG" "$RAW" -- \
      /system/bin/sh -c "export PATH=/system/bin; sleep $n" >/dev/null 2>&1 ) &
  local launcher=$!
  local gp=""
  for _ in $(seq 1 50); do gp=$(find_guest "$n"); [ -n "$gp" ] && break; sleep 0.1; done
  if [ -z "$gp" ]; then
    printf '%-5s kill=%s  guest never started (launcher=%s)\n' "$mode" "$target" "$launcher"
    kill -KILL "$launcher" 2>/dev/null; wait "$launcher" 2>/dev/null; return
  fi
  local bw; bw=$(ppid_of "$gp")
  local victim; [ "$target" = A ] && victim=$bw || victim=$launcher
  printf '%-5s kill=%-1s launcher=%s bwrap=%s guest=%s  ' "$mode" "$target" "$launcher" "$bw" "$gp"
  kill -KILL "$victim" 2>/dev/null
  sleep 1.5
  printf 'after: bwrap=%s guest=%s(ppid=%s) => %s\n' \
    "$(alive "$bw")" "$(alive "$gp")" "$(ppid_of "$gp")" \
    "$([ "$(alive "$gp")" = yes ] && echo 'guest SURVIVES (orphan)' || echo 'guest DIES')"
  kill -KILL "$gp" "$bw" "$launcher" 2>/dev/null
  wait "$launcher" 2>/dev/null
}

echo "=== P6 orphan survival: SIGKILL the launcher -- does the guest live on? ==="
echo "A = SIGKILL bwrap itself; B = SIGKILL bwrap's parent (the launcher, PDEATHSIG chain)"
echo "--- plain (acos-art argv as of today) ---"
trial plain A 5561
trial plain B 5562
echo "--- hard (S1 flag set added) ---"
trial hard A 5563
trial hard B 5564
