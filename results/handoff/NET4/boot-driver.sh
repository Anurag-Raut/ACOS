#!/usr/bin/env bash
# NET4: the relay arm's session boot, measured — and the module-off control.
#
# For each arm (`relay` = ACOS_SPIKE_NET=1 ACOS_NET_ARM=relay, `off` = the flag
# unset) and each rep it starts N concurrent `acos verify` runs of the same real
# ART scenario and prints, per session, the two facts that decide boot latency:
#
#   session_start_ms  guest boot, `acos-art` spawn to its ready line
#                     (`engine.json.timing.session_start_ms`)
#   cache             the shared boot image: `hit`, or `built` + `prep_ms`
#                     (`engine.json.timing.boot_image`)
#
# NET3's "relay boot is 4.7–12.6 s" was `cache=built` / queueing behind a build:
# the entry a worktree had just built was pruned by another worktree's build (the
# key hashed the sysroot's *absolute path*), so nearly every launch paid a
# 4–12 s dex2oat rebuild inside the guest boot. Both arms pay it equally; the
# relay was never the cause. This driver is the A/B that shows it.
#
#   OUTDIR [reps] [n]        (inside `run-exclusive.sh -- tools/guarded.sh -- …`)
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
WT=$(cd "$HERE/../../.." && pwd)
OUT=${1:?outdir}
REPS=${2:-1}
N=${3:-1}
ARMS=${ARMS:-"relay off"}
BIN=${ACOS:-$WT/.cache/net4-target/release/acos}
export ACOS_FONT_DIR=${ACOS_FONT_DIR:-/home/Qui/Projects/acos/.cache/boot/android-34/fonts}
export ACOS_ART_STAGE_TIMING=1
export ACOS_NETD_BIN=${ACOS_NETD_BIN:-$WT/.cache/net4-target/release/acos-netd}
mkdir -p "$OUT"
: >"$OUT/boot.log"

for arm in $ARMS; do
  case $arm in
    relay) export ACOS_SPIKE_NET=1 ACOS_NET_ARM=relay ;;
    base)  export ACOS_SPIKE_NET=1 ACOS_NET_ARM=base ;;
    off)   unset ACOS_SPIKE_NET ACOS_NET_ARM ;;
    *) echo "arms: relay|base|off" >&2; exit 2 ;;
  esac
  for rep in $(seq 1 "$REPS"); do
    rdir=$OUT/$arm-rep$rep
    rm -rf "$rdir"; mkdir -p "$rdir"
    started=$(date +%s%3N)
    for i in $(seq 1 "$N"); do
      ( cd "$WT" && env -u ACOS_HOME "$BIN" verify --engine art \
          --apk fixtures/build/apps/net-check.apk --scenario scenarios/net-check.yaml \
          --artifacts "$rdir/$i" >"$rdir/$i.out" 2>"$rdir/$i.err"; \
        echo "exit=$?" >>"$rdir/$i.out" ) &
    done
    wait
    elapsed=$(( $(date +%s%3N) - started ))
    for i in $(seq 1 "$N"); do
      python3 - "$rdir/$i" "$arm" "$rep" "$i" "$elapsed" <<'PY' >>"$OUT/boot.log"
import json, sys
d, arm, rep, i, elapsed = sys.argv[1:6]
try:
    e = json.load(open(f"{d}/engine.json"))["engine"]
    t = e.get("timing", {})
    bi = t.get("boot_image") or {}
    print(f"arm={arm} rep={rep} n={i} session_start_ms={t.get('session_start_ms')} "
          f"cache={bi.get('cache')} prep_ms={bi.get('prep_ms')} "
          f"key={bi.get('input_hash')} session_id={e.get('session_id')} wall_ms={elapsed}")
except Exception as exc:  # noqa: BLE001 - a missing report is itself the datum
    print(f"arm={arm} rep={rep} n={i} NO_ENGINE_JSON={exc} wall_ms={elapsed}")
PY
    done
    pass=$(grep -l 'verify: pass' "$rdir"/*.out 2>/dev/null | wc -l)
    echo "arm=$arm rep=$rep pass=$pass/$N wall_ms=$elapsed" | tee -a "$OUT/boot.log"
  done
done
cat "$OUT/boot.log"
