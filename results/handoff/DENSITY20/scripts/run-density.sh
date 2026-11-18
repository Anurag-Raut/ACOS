#!/usr/bin/env bash
# run-density.sh -- DENSITY20 cohort runner.
#
#   tools/... no: results/handoff/DENSITY20/scripts/run-density.sh --n 10 --reps 3 \
#       [--loops 3] [--zygote off|on] [--arm-tag exec] [--mem 16G] [--out DIR]
#
# Each rep is ONE `run-exclusive.sh` acquisition (shared slot by default: the
# brief allows indicative numbers and says so) wrapping `tools/guarded.sh`, so
# every cohort is bound by cgroup MemoryMax and a wall timeout and can never
# overlap an exclusive benchmark.
set -uo pipefail
WT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../.." && pwd)"
cd "$WT" || exit 1
source "$WT/tools/agent-env.sh" >/dev/null 2>&1 || true

N=1; REPS=1; LOOPS=3; ZYGOTE=off; MEM=12G; TIME=600; OUT=""; SLOT="shared"; ACOS=""
while [ $# -gt 0 ]; do
  case "$1" in
    --n) N=$2; shift 2 ;;
    --reps) REPS=$2; shift 2 ;;
    --loops) LOOPS=$2; shift 2 ;;
    --zygote) ZYGOTE=$2; shift 2 ;;
    --mem) MEM=$2; shift 2 ;;
    --time) TIME=$2; shift 2 ;;
    --out) OUT=$2; shift 2 ;;
    --exclusive) SLOT="exclusive"; shift ;;
    --acos) ACOS=$2; shift 2 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done
ACOS="${ACOS:-$WT/.cache/density20-target/release/acos}"
[ -x "$ACOS" ] || { echo "no binary: $ACOS" >&2; exit 2; }
OUT="${OUT:-$WT/results/handoff/DENSITY20/raw}"
mkdir -p "$OUT" || exit 2
arm="exec"; [ "$ZYGOTE" = "on" ] && arm="zygote-on"

df -h / | tail -1
free -g | head -2

for rep in $(seq 1 "$REPS"); do
  out="$OUT/n${N}-${arm}-r${rep}.json"
  lock_args=(--task DENSITY20)
  [ "$SLOT" = "exclusive" ] && lock_args+=(--exclusive)
  echo "== n=$N arm=$arm rep=$rep -> $out"
  tools/art-runtime/run-exclusive.sh "${lock_args[@]}" -- \
    tools/guarded.sh --mem "$MEM" --time "$TIME" -- \
    python3 -I "$WT/results/handoff/DENSITY20/scripts/density20.py" \
      --acos "$ACOS" --n "$N" --loops "$LOOPS" --zygote "$ZYGOTE" \
      --out "$out" --lock "$SLOT" --label "dens20-n${N}-${arm}-r${rep}" \
    || echo "rep $rep exit $? (recorded in $out if written)"
  sleep 3
done
