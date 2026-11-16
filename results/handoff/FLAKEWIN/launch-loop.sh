#!/usr/bin/env bash
# FLAKEWIN launch loop: N launches of counter + hello-views at a chosen
# concurrency. Every launch is a fresh ART guest (the relaunch path), so the
# `addToDisplayAsUser` reply and the `IWindow.dispatchAppVisibility` delivery
# that follows it are produced for every launch -- the exact pair the flake
# corrupted. One JSON line per launch in <out>/launches.jsonl.
#
# Each worker slot gets its own ACOS_RUNTIME_DIR/daemon socket, so concurrency 4
# is four independent sessions (the "shared slot 4" shape) and never trips the
# "session already runs <apk>" guard.
#
# usage: launch-loop.sh --out DIR --launches N --concurrency C [--engine art]
set -uo pipefail
cd "${WT:?WT must point at the worktree}" || exit 2

out=""; launches=30; conc=1; engine=art
while [ $# -gt 0 ]; do
  case "$1" in
    --out) out=$2; shift 2;;
    --launches) launches=$2; shift 2;;
    --concurrency) conc=$2; shift 2;;
    --engine) engine=$2; shift 2;;
    *) echo "unknown arg $1" >&2; exit 2;;
  esac
done
[ -n "$out" ] || { echo "--out is required" >&2; exit 2; }
mkdir -p "$out"

export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
unset ACOS_HOME
apps=(fixtures/build/apps/counter.apk fixtures/build/apps/hello-views.apk)
jsonl="$out/launches.jsonl"
: > "$jsonl"

acos="$WT/.cache/flakewin-target/release/acos"
[ -x "$acos" ] || { echo "missing $acos" >&2; exit 2; }

run_one() {
  local i=$1 slot=$2
  # Blocks of five per app: a session may not switch APK, and a relaunch of the
  # same APK is exactly the reset path the flake lived in.
  local ai=$(( ((i - 1) / 5) % ${#apps[@]} ))
  local apk=${apps[$ai]}
  # One daemon per (slot, app): a session may not switch APK, and a relaunch of
  # the same APK is exactly the reset path the flake lived in.
  local rt="$out/rt-$slot-$ai"
  mkdir -p "$rt"; chmod 0700 "$rt"
  local started=$SECONDS
  local json
  json=$(ACOS_RUNTIME_DIR="$rt" ACOS_SESSION_SOCKET="$rt/session.sock" \
         ACOS_ARTIFACTS_DIR="$out/artifacts-$i" \
         "$acos" launch --engine "$engine" --timeout 120 "$apk" --json \
         2>"$out/launch-$i.stderr")
  local rc=$?
  local wall=$(( (SECONDS - started) * 1000 ))
  python3 - "$i" "$apk" "$rc" "$wall" "$json" "$jsonl" <<'PY'
import json, sys
i, apk, rc, wall, out, jsonl = sys.argv[1:7]
try:
    d = json.loads(out)
except Exception:
    d = {"_unparsed": out[:600]}
rec = {
    "i": int(i),
    "apk": apk.rsplit("/", 1)[-1],
    "rc": int(rc),
    "wall_ms": int(wall),
    "ok": d.get("ok"),
    "status": d.get("status"),
    "stage": d.get("stage_reached") or d.get("stage"),
    "code": (d.get("error") or {}).get("code"),
    "message": (d.get("error") or {}).get("message"),
    "guest_crash": d.get("guest_crash"),
    "unparsed": d.get("_unparsed"),
}
with open(jsonl, "a") as f:
    f.write(json.dumps(rec) + "\n")
sys.exit(0 if rec["ok"] is True else 1)
PY
}

fail=0
slot=0
if [ "$conc" -le 1 ]; then
  for i in $(seq 1 "$launches"); do
    run_one "$i" 0 || fail=$((fail + 1))
    echo "launch $i/$launches failed=$fail"
  done
else
  pids=()
  idx=0
  while [ "$idx" -lt "$launches" ]; do
    batch=()
    for _ in $(seq 1 "$conc"); do
      idx=$((idx + 1))
      [ "$idx" -le "$launches" ] || break
      slot=$(( (idx - 1) % conc ))
      run_one "$idx" "$slot" & pids+=($!); batch+=($slot)
    done
    for p in "${pids[@]}"; do wait "$p" || fail=$((fail + 1)); done
    pids=()
    echo "launched $idx/$launches failed=$fail"
  done
fi

total=$(wc -l < "$jsonl")
ok=$(python3 - "$jsonl" <<'PY'
import json, sys
print(sum(1 for line in open(sys.argv[1]) if json.loads(line).get("ok") is True))
PY
)
bad=$(python3 - "$jsonl" <<'PY'
import json, sys
for line in open(sys.argv[1]):
    r = json.loads(line)
    if r.get("ok") is not True:
        print("  FAIL", json.dumps(r)[:400])
PY
)
[ -n "$bad" ] && echo "$bad"
echo "SUMMARY launches=$total ok=$ok failed=$((total - ok)) concurrency=$conc engine=$engine"
[ "$ok" -eq "$total" ]
