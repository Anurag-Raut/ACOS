#!/usr/bin/env bash
# NET1 A/B driver — PREPARED, NOT RUN. Wired since NET2: `acos-art`'s
# `spike_net::relay_wrap` calls `acos_netd::client` (through the `acos-netd run`
# stdio proxy), so the `relay` arm is a live arm and the three arms below are
# three different code paths. Still the bench owner's run (one exclusive A/B per
# release candidate); a coding turn must not claim its numbers.
#
# Arms (same build, same host, interleaved):
#   off    ACOS_SPIKE_NET unset                      today's default: no network
#   relay  ACOS_SPIKE_NET=1 ACOS_NET_ARM=relay       design B2: one relay + veth
#   base   ACOS_SPIKE_NET=1 ACOS_NET_ARM=base        spike fallback: pasta/session
#
# What the gate is about (ADR-0030 item 6): cold launch, warm launch, light
# memory and N=20 density must not regress beyond noise. The memory axis is no
# longer in question for `relay` (26 MiB once per host, ~0 per session, measured
# in results/handoff/NET1/), so the decision is cold/warm launch.
#
# Usage (bench owner, under the shared lock, not from a coding turn):
#   NET1_AB_RUN=1 results/handoff/NET1/ab-driver.sh [--reps 3]
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
WT=$(cd "$HERE/../../.." && pwd)
REPS=3
[ "${1:-}" = "--reps" ] && REPS=${2:-3}

say() { printf '%s\n' "$*" >&2; }

# --- precondition: the arm must actually be live ---------------------------
# A bench that compares `off` with `off` is a false pass, so the driver refuses
# to run until the launcher calls the relay. This is the honest gate, not a
# comment: `acos_netd::client` must be reached from an ART session path.
wired=$(grep -rl "acos_netd::client" "$WT/crates/acos-art/src" 2>/dev/null | head -1)
if [ -z "$wired" ]; then
  say "ab-driver: REFUSING — the relay is not wired into the ART session launcher yet."
  say "  transport is measured (results/handoff/NET1/proof.sh), but no acos-art path"
  say "  calls acos_netd::client, so 'off' and 'relay' would run the same code."
  say "  Smallest next action: implement Request::Spawn in acos-art's session launch"
  say "  (spike_net::wrap's successor) behind ACOS_NET_ARM, then re-run this."
  exit 3
fi
if [ "${NET1_AB_RUN:-0}" != "1" ]; then
  say "ab-driver: prepared, not run. Set NET1_AB_RUN=1 to execute (bench owner only,"
  say "  under tools/art-runtime/run-exclusive.sh; one exclusive A/B per release candidate)."
  exit 0
fi

export ACOS_FONT_DIR=${ACOS_FONT_DIR:-/home/Qui/Projects/acos/.cache/boot/android-34/fonts}
OUT=$HERE/ab
mkdir -p "$OUT"

# One lock acquisition per arm; the bench harness re-acquires per phase itself.
run_arm() {
  arm=$1; shift
  say "== arm $arm reps=$REPS"
  ( cd "$WT" && "$@" tools/bench/acos-bench.sh --quick --reps "$REPS" --task NET1AB \
      >"$OUT/arm-$arm.log" 2>&1 ) || { say "arm $arm failed, see $OUT/arm-$arm.log"; return 1; }
  latest=$(ls -t "$WT"/results/bench/baseline-*.json 2>/dev/null | head -1)
  [ -n "$latest" ] && cp "$latest" "$OUT/run-$arm.json"
  return 0
}

run_arm off env -u ACOS_SPIKE_NET -u ACOS_NET_ARM || exit 1
run_arm relay env ACOS_SPIKE_NET=1 ACOS_NET_ARM=relay || exit 1
run_arm base env ACOS_SPIKE_NET=1 ACOS_NET_ARM=base || exit 1

for pair in "relay off" "relay base"; do
  set -- $pair
  say "== compare $1 vs $2"
  tools/bench/acos-bench.sh --from-run "$OUT/run-$1.json" --compare "$OUT/run-$2.json" \
    >"$OUT/compare-$1-vs-$2.txt" 2>&1 || true
  grep -E "^(metric|M[0-9]|GATE|REGRESS)" "$OUT/compare-$1-vs-$2.txt" | head -40 | tee -a "$OUT/summary.txt"
done
say "== done: $OUT/summary.txt"
