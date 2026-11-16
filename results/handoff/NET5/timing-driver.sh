#!/usr/bin/env bash
# NET5: the `network` module's attach budget, measured (INDICATIVE).
#
# Two arms, N=1 and N=5, 5 reps each, real ART, the same app and scenario:
#
#   off     no `services:`          -> the session boot without the module
#   module  `network:real` attached -> the same boot inside the relay's netns
#
# Per session it prints the fact that decides boot latency —
# `engine.json.timing.session_start_ms` (launcher spawn to the guest's ready
# line) — plus the boot image's cache outcome, and per rep the median and the
# arm's delta. Run on the SHARED slot (no run-exclusive): the numbers are
# indicative, not a benchmark claim, and they are labelled as such in TIMING.md.
#
#   bash results/handoff/NET5/timing-driver.sh <outdir> [reps]
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
WT=$(cd "$HERE/../../.." && pwd)
OUT=${1:?outdir}
REPS=${2:-5}
BIN=${ACOS:-$WT/.cache/net5-target/release/acos}
export ACOS_FONT_DIR=${ACOS_FONT_DIR:-/home/Qui/Projects/acos/.cache/boot/android-34/fonts}
export ACOS_NETD_BIN=${ACOS_NETD_BIN:-$WT/.cache/net5-target/release/acos-netd}
export ACOS_NET_PASTA=${ACOS_NET_PASTA:-$WT/.cache/net5/passt-src/usr/bin/pasta}
export ACOS_SPIKE_NET_PASTA=$ACOS_NET_PASTA
export ACOS_NETD_STATE=${ACOS_NETD_STATE:-$WT/.cache/net5/netd-state}
export ACOS_RUNTIME_DIR=${ACOS_RUNTIME_DIR:-$WT/.cache/net5/rt}
export ACOS_SESSION_SOCKET=${ACOS_SESSION_SOCKET:-$ACOS_RUNTIME_DIR/session.sock}
mkdir -p "$OUT" "$ACOS_RUNTIME_DIR"
: >"$OUT/timing.log"
"$ACOS_NETD_BIN" up >/dev/null 2>&1

for n in 1 5; do
  for arm in off module; do
    case $arm in
      off)    scenario=scenarios/net-network-off.yaml ;;
      module) scenario=scenarios/net-network-real.yaml ;;
    esac
    for rep in $(seq 1 "$REPS"); do
      rdir=$OUT/n$n-$arm-rep$rep
      rm -rf "$rdir"; mkdir -p "$rdir"
      for i in $(seq 1 "$n"); do
        ( cd "$WT" && env -u ACOS_HOME "$BIN" verify --engine art \
            --apk fixtures/build/apps/net-check.apk --scenario "$scenario" \
            --artifacts "$rdir/$i" >"$rdir/$i.out" 2>"$rdir/$i.err"; \
          echo "exit=$?" >>"$rdir/$i.out" ) &
      done
      wait
      for i in $(seq 1 "$n"); do
        python3 - "$rdir/$i" "$arm" "$n" "$rep" "$i" <<'PY' >>"$OUT/timing.log"
import json, sys
d, arm, n, rep, i = sys.argv[1:6]
try:
    e = json.load(open(f"{d}/engine.json"))["engine"]
    t = e.get("timing", {})
    bi = t.get("boot_image") or {}
    print(f"arm={arm} n={n} rep={rep} session={i} "
          f"session_start_ms={t.get('session_start_ms')} "
          f"attach_total_ms={t.get('module_attach_ms')} "
          f"cache={bi.get('cache')} prep_ms={bi.get('prep_ms')}")
except Exception as exc:  # noqa: BLE001
    print(f"arm={arm} n={n} rep={rep} session={i} NO_ENGINE_JSON={exc}")
PY
      done
      pass=$(grep -l 'verify: pass' "$rdir"/*.out 2>/dev/null | wc -l)
      echo "arm=$arm n=$n rep=$rep pass=$pass/$n" | tee -a "$OUT/timing.log"
    done
  done
done
python3 - "$OUT/timing.log" <<'PY'
import statistics, sys, collections
rows = collections.defaultdict(list)
for line in open(sys.argv[1]):
    if "session_start_ms=" not in line: continue
    kv = dict(p.split("=", 1) for p in line.split() if "=" in p)
    if kv.get("session_start_ms") in (None, "None"): continue
    rows[(kv["arm"], kv["n"])].append(float(kv["session_start_ms"]))
print("\n| arm | N | sessions | median session_start_ms |")
print("|---|---|---|---|")
med = {}
for (arm, n), vals in sorted(rows.items()):
    med[(arm, n)] = statistics.median(vals)
    print(f"| {arm} | {n} | {len(vals)} | {statistics.median(vals):.0f} |")
for n in ("1", "5"):
    if ("off", n) in med and ("module", n) in med:
        print(f"\ndelta N={n}: {med[('module', n)] - med[('off', n)]:+.0f} ms "
              f"(module {med[('module', n)]:.0f} vs off {med[('off', n)]:.0f})")
PY
