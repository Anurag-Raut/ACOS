#!/usr/bin/env bash
# NETREAL: every network arm, once, with the module's real profile and the
# *supported-way* pinned pasta (no ACOS_NET_PASTA / ACOS_SPIKE_NET_PASTA /
# ACOS_SPIKE_NET set: the binary is found at <checkout>/libexec/pasta by
# crates/acos-netd-ctl/src/pasta.rs, exactly as an install finds
# <prefix>/share/acos/<version>/libexec/pasta).
#
#   bash results/handoff/NETREAL/proofs.sh <outdir>
#
# Each ART run takes the shared lock for its own acquisition (never > 10 min).
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
WT=$(cd "$HERE/../../.." && pwd)
OUT=${1:?outdir}
TARGET=${CARGO_TARGET_DIR:-$WT/.cache/netrealc-target}
BIN=$TARGET/release/acos
NETD=$TARGET/release/acos-netd
unset ACOS_NET_PASTA ACOS_SPIKE_NET_PASTA ACOS_SPIKE_NET ACOS_NET_ARM ACOS_HOME
export ACOS_FONT_DIR=${ACOS_FONT_DIR:-/home/Qui/Projects/acos/.cache/boot/android-34/fonts}
export ACOS_VIEW_TEST_DIR=${ACOS_VIEW_TEST_DIR:-$WT/.cache/lv}
export ACOS_NETD_BIN=$NETD
export ACOS_NETD_STATE=${ACOS_NETD_STATE:-$WT/.cache/ns}
export ACOS_RUNTIME_DIR=${ACOS_RUNTIME_DIR:-$WT/.cache/rt}
export ACOS_SESSION_SOCKET=${ACOS_SESSION_SOCKET:-$ACOS_RUNTIME_DIR/session.sock}
mkdir -p "$OUT" "$ACOS_RUNTIME_DIR" "$ACOS_NETD_STATE"
: >"$OUT/proof.log"
note() { echo "$*" | tee -a "$OUT/proof.log"; }

cd "$WT" || exit 2
note "# pinned pasta, found by the product's own rule"
"$NETD" pasta --json >"$OUT/netd-pasta.json" 2>&1; note "netd_pasta rc=$? $(python3 -c "import json;d=json.load(open('$OUT/netd-pasta.json'));print(d['version'],d['source'],'pinned='+str(d['pinned']))" 2>&1)"
note "sha256 $(sha256sum "$("$NETD" pasta | sed 's/ (.*//')" | cut -d' ' -f1)"
"$BIN" doctor >"$OUT/doctor.txt" 2>&1; note "doctor: $(grep -E '^\[.*\] pasta' "$OUT/doctor.txt")"

"$NETD" up >"$OUT/netd-up.log" 2>&1
note "relay_start rc=$? $(grep -o '"pasta_version": *"[^"]*"' "$OUT/netd-up.log" | head -1) $(grep -o '"pasta_path": *"[^"]*"' "$OUT/netd-up.log" | head -1)"

run_arm() {
  local arm=$1 scenario=$2
  local dir=$OUT/$arm
  rm -rf "$dir"
  tools/art-runtime/run-exclusive.sh --task NETREALC -- tools/guarded.sh --mem 8G --time 600 -- \
    "$BIN" verify --engine art --apk fixtures/build/apps/net-check.apk \
      --scenario "$scenario" --artifacts "$dir" >"$dir.out" 2>"$dir.err"
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

run_arm off          scenarios/net-network-off.yaml
run_arm check-off    scenarios/net-check-offline.yaml
run_arm check        scenarios/net-check.yaml
run_arm real         scenarios/net-network-real.yaml
run_arm offline      scenarios/net-network-offline.yaml
run_arm https-proof  scenarios/net-https-proof.yaml

BIN="$NETD" ACOS_NET_PASTA= bash "$WT/results/handoff/NET3/proof-hostport.sh" "$OUT/hostport" >"$OUT/hostport.out" 2>&1
note "hostport exit=$? $(tail -1 "$OUT/hostport.out")"

note "--- relay left behind ---"
"$NETD" status 2>&1 | tail -3 | tee -a "$OUT/proof.log"
"$NETD" shutdown >/dev/null 2>&1; note "netd_shutdown rc=$?"
