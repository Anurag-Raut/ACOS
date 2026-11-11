#!/usr/bin/env bash
# p6b-pdeathsig.sh -- sanity check for the mechanism used in p6 trial B: does
# prctl(PR_SET_PDEATHSIG, SIGKILL) + exec actually make the exec'd process die
# when its parent is SIGKILLed? (This is the launcher.rs:216-233 mechanism.)
set -uo pipefail
SPAWN='import ctypes,os,sys
ctypes.CDLL("libc.so.6", use_errno=True).prctl(1, 9, 0, 0, 0)
os.execv("/bin/sleep", ["sleep", sys.argv[1]])'
find_child() { for p in /proc/[0-9]*; do p=${p#/proc/}; [ "$(cat /proc/$p/comm 2>/dev/null)" = sleep ] &&
  [ "$(tr -d '\0' <"/proc/$p/cmdline" 2>/dev/null)" = "sleep$1" ] && { echo "$p"; return; }; done; }

for trial in with-pdeathsig without-pdeathsig; do
  N=$([ "$trial" = with-pdeathsig ] && echo 5771 || echo 5772)
  if [ "$trial" = with-pdeathsig ]; then
    bash -c "python3 -c '$SPAWN' $N & wait" &
  else
    bash -c "/bin/sleep $N & wait" &
  fi
  L=$!
  G=""
  for _ in $(seq 1 30); do G=$(find_child "$N"); [ -n "$G" ] && break; sleep 0.1; done
  printf '%-16s launcher=%s child=%s child_ppid=%s  ' "$trial" "$L" "${G:-none}" "$(awk '{print $4}' "/proc/$G/stat" 2>/dev/null)"
  kill -KILL "$L" 2>/dev/null
  sleep 0.7
  printf 'after SIGKILL(launcher): child alive=%s => %s\n' \
    "$(kill -0 "$G" 2>/dev/null && echo yes || echo no)" \
    "$(kill -0 "$G" 2>/dev/null && echo 'SURVIVES (no pdeathsig effect)' || echo 'DIED (pdeathsig fired)')"
  kill -KILL "$G" 2>/dev/null
  wait "$L" 2>/dev/null
done
