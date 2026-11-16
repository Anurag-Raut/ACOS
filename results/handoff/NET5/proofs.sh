#!/usr/bin/env bash
# NET5: the three `network` module arms on real ART, plus the host-port check.
#
#   bash results/handoff/NET5/proofs.sh <outdir>
#
# One arm per scenario (the arm is a launch-time choice: a netns is joined at
# exec). Every run is a real `acos verify --engine art` of the NET3/NET4
# `net-check` fixture, driven only by the module (`services:` /
# `service_ctl:` in the scenario) — no ACOS_SPIKE_NET, no ACOS_NET_ARM.
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
WT=$(cd "$HERE/../../.." && pwd)
OUT=${1:?outdir}
BIN=${ACOS:-$WT/.cache/net5-target/release/acos}
PASTA=${ACOS_NET_PASTA:-$WT/.cache/net5/passt-src/usr/bin/pasta}
export ACOS_FONT_DIR=${ACOS_FONT_DIR:-/home/Qui/Projects/acos/.cache/boot/android-34/fonts}
# The supervisor reads ACOS_NET_PASTA; the launcher's spike arm reads
# ACOS_SPIKE_NET_PASTA. One binary, two names — set both to the pinned build.
export ACOS_NET_PASTA=$PASTA
export ACOS_SPIKE_NET_PASTA=$PASTA
export ACOS_NETD_BIN=${ACOS_NETD_BIN:-$WT/.cache/net5-target/release/acos-netd}
export ACOS_NETD_STATE=${ACOS_NETD_STATE:-$WT/.cache/net5/netd-state}
export ACOS_RUNTIME_DIR=${ACOS_RUNTIME_DIR:-$WT/.cache/net5/rt}
export ACOS_SESSION_SOCKET=${ACOS_SESSION_SOCKET:-$ACOS_RUNTIME_DIR/session.sock}
mkdir -p "$OUT" "$ACOS_RUNTIME_DIR"
: >"$OUT/proof.log"

note() { echo "$*" | tee -a "$OUT/proof.log"; }

# The relay supervisor, started by the module's own client (`ensure_up`).
"$ACOS_NETD_BIN" up >"$OUT/netd-up.log" 2>&1
rc=$?
note "relay_start rc=$rc $(grep -o '"pasta_version": *"[^"]*"' "$OUT/netd-up.log" | head -1)"

run_arm() {
  local arm=$1 scenario=$2
  local dir=$OUT/$arm
  rm -rf "$dir"
  ( cd "$WT" && env -u ACOS_HOME "$BIN" verify --engine art \
      --apk fixtures/build/apps/net-check.apk --scenario "$scenario" \
      --artifacts "$dir" >"$dir.out" 2>"$dir.err" )
  local exit=$?
  local verdict
  verdict=$(python3 - "$dir/verdict.json" <<'PY'
import json,sys
try:
    v=json.load(open(sys.argv[1]))
except Exception as exc:
    print(f"NO_VERDICT({exc})"); raise SystemExit
print(f"{v.get('verdict')} steps={sum(1 for s in v.get('steps',[]) if s.get('status')=='pass')}/{len(v.get('steps',[]))}")
PY
)
  note "arm=$arm exit=$exit verdict=$verdict scenario=$scenario"
}

run_arm off  scenarios/net-network-off.yaml
run_arm real scenarios/net-network-real.yaml
# The open finding (a bug, so it is not a gate): kept out of `scenarios/`, run
# here so the evidence stays reproducible. Its step 3 (the module control) and
# steps 0-5 pass; step 6 fails — `results/handoff/NET5/FINDING-offline.md`.
run_arm offline_open_finding "$WT/results/handoff/NET5/scenarios/open-finding-offline.yaml"

# NET3's host-port opt-in proof still passes against this build.
BIN="$ACOS_NETD_BIN" bash "$WT/results/handoff/NET3/proof-hostport.sh" "$OUT/hostport" >"$OUT/hostport.out" 2>&1
note "hostport_runs exit=$? $(tail -1 "$OUT/hostport.out")"

note "--- relay sessions left behind ---"
"$ACOS_NETD_BIN" status 2>&1 | tail -2 | tee -a "$OUT/proof.log"
