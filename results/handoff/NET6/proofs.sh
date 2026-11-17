#!/usr/bin/env bash
# NET6: the network module's app-level arms on real ART, plus NET3's host-port
# proof. Four arms, one build, driven only by the module (`services:` /
# `service_ctl:` in the scenario) — no ACOS_SPIKE_NET, no ACOS_NET_ARM.
#
#   bash results/handoff/NET6/proofs.sh <outdir>
#
# off     scenarios/net-network-off.yaml     no module: the honest no-network
# real    scenarios/net-network-real.yaml   network:real, one HTTPS GET = 200
# offline scenarios/net-network-offline.yaml network:real, offline -> onLost +
#                                            a bounded exception, online ->
#                                            onAvailable + 200 (ONE process)
# hostport NET3's own opt-in proof
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
WT=$(cd "$HERE/../../.." && pwd)
OUT=${1:?outdir}
BIN=${ACOS:-$WT/.cache/net6-target/release/acos}
PASTA=${ACOS_NET_PASTA:-$WT/.cache/net6/passt-src/pasta}
export ACOS_FONT_DIR=${ACOS_FONT_DIR:-/home/Qui/Projects/acos/.cache/boot/android-34/fonts}
export ACOS_NET_PASTA=$PASTA
export ACOS_SPIKE_NET_PASTA=$PASTA
export ACOS_NETD_BIN=${ACOS_NETD_BIN:-$WT/.cache/net6-target/release/acos-netd}
export ACOS_NETD_STATE=${ACOS_NETD_STATE:-$WT/.cache/net6/netd-state}
export ACOS_RUNTIME_DIR=${ACOS_RUNTIME_DIR:-$WT/.cache/net6/rt}
export ACOS_SESSION_SOCKET=${ACOS_SESSION_SOCKET:-$ACOS_RUNTIME_DIR/session.sock}
mkdir -p "$OUT" "$ACOS_RUNTIME_DIR"
: >"$OUT/proof.log"

note() { echo "$*" | tee -a "$OUT/proof.log"; }

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

run_arm off     scenarios/net-network-off.yaml
run_arm real    scenarios/net-network-real.yaml
run_arm offline scenarios/net-network-offline.yaml

BIN="$ACOS_NETD_BIN" bash "$WT/results/handoff/NET3/proof-hostport.sh" "$OUT/hostport" >"$OUT/hostport.out" 2>&1
note "hostport_runs exit=$? $(tail -1 "$OUT/hostport.out")"

note "--- relay sessions left behind ---"
"$ACOS_NETD_BIN" status 2>&1 | tail -2 | tee -a "$OUT/proof.log"
