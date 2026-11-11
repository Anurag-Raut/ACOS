#!/usr/bin/env bash
# probes.sh -- S2 sandbox probe suite.
#
# Runs tiny commands inside the *real* acos-art bwrap argv shape (bwrap-shape.sh,
# generated from a live run) twice: once as `acos-art` builds it today (plain),
# once with the hardening flag set S1 proposed (hard).
#
# Everything goes through tools/guarded.sh --mem 2G --time 120. Scratch files
# live under .cache/s2probe/. No host file outside the repo is ever read; the
# `/proc/<pid>/root` probe uses stat/readlink only.
set -uo pipefail
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$DIR/../../.." && pwd)"
SHAPE="$DIR/bwrap-shape.sh"
W="$ROOT/.cache/s2probe"
LOGS="$DIR/logs"
mkdir -p "$W" "$LOGS"

HARD="--unshare-pid --unshare-net --unshare-ipc --unshare-uts --unshare-cgroup-try --die-with-parent --clearenv --cap-drop ALL"

mode_extra() { [ "$1" = hard ] && printf '%s' "$HARD" || true; }

run() { # run MODE GUEST...   (guest = /system/bin/sh -c SNIPPET)
  local mode=$1; shift
  S2_EXTRA="$(mode_extra "$mode")" S2_MEM=2G S2_TIME=120 \
    "$SHAPE" -- /system/bin/sh -c "export PATH=/system/bin; $*" 2>&1
}

hdr() { printf '\n===== %s =====\n' "$*"; }

# ---------------------------------------------------------------- p1: version
p1() {
  hdr "P1 bwrap --version / NoNewPrivs / CapEff / Seccomp inside"
  printf 'host: %s\n' "$(bwrap --version)"
  for m in plain hard; do
    printf -- '--- %s ---\n' "$m"
    run "$m" 'grep -E "^(Uid|Gid|CapInh|CapPrm|CapEff|CapBnd|CapAmb|NoNewPrivs|Seccomp|Seccomp_filters|NSpid):" /proc/self/status'
  done
}

# ------------------------------------------------------------------- p2: /proc
p2() {
  hdr "P2 host PIDs visible in guest /proc, and /proc/<pid>/root of a same-uid host process"
  sleep 300 &                                  # harmless same-uid host process
  local hp=$!
  local hostn; hostn=$(ls -d /proc/[0-9]* 2>/dev/null | wc -l)
  printf 'host: own pid=%s, same-uid sleep pid=%s, host numeric /proc entries=%s\n' "$$" "$hp" "$hostn"
  for m in plain hard; do
    printf -- '--- %s ---\n' "$m"
    run "$m" "
      echo -n 'guest numeric /proc entries: '; ls -d /proc/[0-9]* 2>/dev/null | wc -l
      echo -n 'host sleep visible (-d /proc/$hp): '; test -d /proc/$hp && echo YES || echo no
      echo -n 'readlink /proc/$hp/root: '; readlink /proc/$hp/root || echo '(none)'
      echo -n 'stat -L /proc/$hp/root: '; stat -L -c 'dev=%d ino=%i mode=%A' /proc/$hp/root 2>&1
      echo -n 'stat -L /proc/$hp/root/home/Qui (host-only path): '; stat -L -c 'dev=%d ino=%i mode=%A' /proc/$hp/root/home/Qui 2>&1
      echo -n 'stat -L /proc/1/root/etc/shadow: '; stat -L -c 'mode=%A uid=%u size=%s' /proc/1/root/etc/shadow 2>&1
      echo -n 'readlink /proc/self/root (own root): '; readlink /proc/self/root
      echo -n 'stat /proc/self/root/etc/hostname (host-only path): '; stat -c %s /proc/self/root/etc/hostname 2>&1
      echo -n 'stat /proc/$hp/cmdline: '; stat -c %s /proc/$hp/cmdline 2>&1 | head -1
      echo -n 'stat /proc/$hp/environ: '; stat -c %s /proc/$hp/environ 2>&1 | head -1
      echo -n 'opendir /proc/$hp/fd: '; ls /proc/$hp/fd >/dev/null 2>&1 && echo YES || echo denied
      echo -n 'stat -L /proc/$hp/cwd: '; stat -L -c %A /proc/$hp/cwd 2>&1
      echo -n 'kill -0 host same-uid pid: '; kill -0 $hp 2>/dev/null && echo SIGNALLABLE || echo denied
    "
  done
  kill "$hp" 2>/dev/null
  wait "$hp" 2>/dev/null
}

# ---------------------------------------------------------------- p3: network
p3() {
  hdr "P3 network: localhost listener, /proc/net/tcp, abstract unix sockets"
  local port=34567
  python3 - "$port" "$W/listener.log" <<'PY' &
import socket, sys, threading, time
port, log = int(sys.argv[1]), sys.argv[2]
s = socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("127.0.0.1", port)); s.listen(8); s.settimeout(20)
open(log, "w").write("listening\n")
try:
    c, a = s.accept(); open(log, "a").write("CONNECTED from %r\n" % (a,)); c.close()
except Exception as e:
    open(log, "a").write("no connect: %r\n" % (e,))
PY
  local lp=$!
  # an abstract AF_UNIX socket the guest should not be able to see or reach
  python3 - "$W/abs.log" <<'PY' &
import socket, sys, time
s = socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM)
s.bind("\0s2-abstract-canary")
open(sys.argv[1], "w").write("bound\n")
time.sleep(20)
PY
  local ap=$!
  sleep 1
  printf 'host: listener pid=%s port=%s ; abstract socket @s2-abstract-canary bound\n' "$lp" "$port"
  for m in plain hard; do
    printf -- '--- %s ---\n' "$m"
    run "$m" "
      echo -n 'tcp connect 127.0.0.1:$port: '
      if nc -w 2 127.0.0.1 $port </dev/null >/dev/null 2>&1; then echo CONNECTED; else echo 'blocked/failed'; fi
      echo -n '/proc/net/tcp readable: '; test -r /proc/net/tcp && echo YES || echo no
      echo -n '/proc/net/tcp entries: '; wc -l </proc/net/tcp 2>/dev/null || echo 0
      echo -n '/proc/net/unix readable: '; test -r /proc/net/unix && echo YES || echo no
      echo -n 'abstract sockets in /proc/net/unix: '; grep -c '@' /proc/net/unix 2>/dev/null || echo 0
      echo -n 'canary @s2-abstract-canary visible: '; grep -q 's2-abstract-canary' /proc/net/unix 2>/dev/null && echo YES || echo no
      echo -n 'abstract unix connect to @s2-abstract-canary: '
      if nc -U -w 2 '@/s2-abstract-canary' </dev/null >/dev/null 2>&1; then echo REACHED; else echo 'blocked/failed'; fi
    "
  done
  sleep 1; kill "$lp" "$ap" 2>/dev/null; wait 2>/dev/null
  printf 'host listener log: %s\n' "$(tr '\n' ' ' <"$W/listener.log" 2>/dev/null)"
}

# -------------------------------------------------------------------- p4: env
p4() {
  hdr "P4 environment leakage (NAMES ONLY)"
  export S2_CANARY_NAME_7f3=marker
  printf 'host: canary variable name S2_CANARY_NAME_7f3 exported\n'
  for m in plain hard; do
    printf -- '--- %s ---\n' "$m"
    run "$m" "
      echo -n 'env var names seen by guest: '
      env | sed 's/=.*//' | grep -v '^PATH\$' | sort | tr '\n' ' '; echo
      echo -n 'host canary NAME visible: '; env | sed 's/=.*//' | grep -qx S2_CANARY_NAME_7f3 && echo YES || echo no
      echo -n 'credential-ish names visible: '
      env | sed 's/=.*//' | grep -Eic 'token|secret|key|passw|ssh|api|anthropic|openai|github|aws|display|dbus' | tr -d '\n'; echo
    "
  done
}

# ------------------------------------------------------------------ p5: writes
p5() {
  hdr "P5 guest writes to the rw /data bind, and a canary standing in for boot.art"
  mkdir -p "$W/data-canary/img/x86_64"
  local canary="$ROOT/.cache/art/data/s2probe/img/x86_64/boot.art"
  mkdir -p "$(dirname "$canary")"
  printf 'ORIGINAL-CANARY-CONTENT\n' >"$canary"
  local before; before=$(sha256sum "$canary" | cut -d' ' -f1)
  printf 'host: canary %s sha256=%s\n' "${canary#"$ROOT"/}" "${before:0:16}"
  for m in plain hard; do
    printf -- '--- %s ---\n' "$m"
    run "$m" "
      echo -n 'new file in /data: '; (echo x > /data/s2probe/probe-new.txt) 2>&1 && echo WRITTEN || echo denied
      echo -n 'new file in /data/local/tmp: '; (echo x > /data/local/tmp/probe-new.txt) 2>&1 && echo WRITTEN || echo denied
      echo -n 'overwrite canary /data/s2probe/img/x86_64/boot.art: '
      if (echo PWNED > /data/s2probe/img/x86_64/boot.art) 2>/dev/null; then echo OVERWRITTEN; else echo denied; fi
      echo -n 'test -w /data/acos.properties (shim property table): '; test -w /data/acos.properties && echo WRITABLE || echo 'not writable'
      echo -n 'write /system/framework/framework.jar: '; (echo x >> /system/framework/framework.jar) 2>/dev/null && echo WRITTEN || echo 'denied (ro)'
      echo -n 'test -w /linkerconfig/ld.config.txt (test only, no write): '; test -w /linkerconfig/ld.config.txt && echo WRITABLE || echo 'not writable'
      echo -n 'test -w /data/img (boot-image dir): '; test -w /data/img && echo WRITABLE || echo 'not writable'
      echo -n 'test -w /data/shim/libartshim.so: '; test -w /data/shim/libartshim.so && echo WRITABLE || echo 'not writable'
      echo -n 'test -w /dev/socket/logdw: '; test -w /dev/socket/logdw && echo WRITABLE || echo 'not writable'
    "
    local after; after=$(sha256sum "$canary" | cut -d' ' -f1)
    printf 'host: canary sha256 after %s = %s -> %s\n' "$m" "${after:0:16}" \
      "$([ "$before" = "$after" ] && echo UNCHANGED || echo MODIFIED)"
    printf 'ORIGINAL-CANARY-CONTENT\n' >"$canary"; before=$(sha256sum "$canary" | cut -d' ' -f1)
  done
  rm -f "$canary"
}

"$@"
