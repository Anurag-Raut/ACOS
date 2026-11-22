#!/usr/bin/env bash
# RELGATE4 step 5: conformance kit + all 38 module proof scenarios on ART.
set -uo pipefail
WT="/home/Qui/.t3/worktrees/acos/t3-f4bcc4d4-5c49b0ef4f83b639"
cd "$WT" || exit 2
source "$WT/.cache/env.sh"
export CARGO_TARGET_DIR="$WT/.cache/rg4-target"
BIN_DIR="$CARGO_TARGET_DIR/release"
OUT="$WT/results/release/v0.4.0-gate/relgate4/05-modules"
RT="$WT/.cache/rt-mod"
mkdir -p "$OUT" "$RT" "$WT/.cache/wvrun"

unset ACOS_BOOT_DIR XDG_RUNTIME_DIR ACOS_HOME TMPDIR ACOS_NET_PASTA ACOS_SPIKE_NET_PASTA ACOS_NET_ARM
export ACOS_RUNTIME_DIR="$RT"
export ACOS_SESSION_SOCKET="$RT/session.sock"
export ACOS_ART_BIN="$BIN_DIR/acos-art"
export ACOS_WEBVIEW_BIN="$BIN_DIR/acos-webview"
export ACOS_WEBVIEW_CACHE="$WT/.cache/webview"
export ACOS_WEBVIEW_SOCKET_DIR="$WT/.cache/wvrun"
export PATH="$BIN_DIR:$PATH"

echo "=== 1. Conformance kit + modules_doc $(date -u +%FT%TZ) ===" | tee "$OUT/driver.log"
tools/art-runtime/run-exclusive.sh --task RELGATE4-kit -- \
  tools/guarded.sh --mem 14G --time 900 -- \
  cargo test --release --offline -p acos-services --test module_kit --test modules_doc -- --nocapture \
  > "$OUT/01-conformance-kit.log" 2>&1
KIT_RC=$?
echo "KIT_RC=$KIT_RC $(date -u +%FT%TZ)" | tee -a "$OUT/driver.log"
grep -E "^test result|FAILED" "$OUT/01-conformance-kit.log" | tee -a "$OUT/driver.log"

echo "=== 2. Module proof scenarios $(date -u +%FT%TZ) ===" | tee -a "$OUT/driver.log"
: > "$OUT/rows.jsonl"
pass_n=0
fail_n=0
while IFS= read -r s; do
  [ -n "$s" ] || continue
  name=$(basename "$s" .yaml)
  artdir="$WT/.cache/rg4-mod-art/$name"
  rm -rf "$artdir"
  t0=$(date +%s)
  tools/art-runtime/run-exclusive.sh --task RELGATE4-mod -- \
    tools/guarded.sh --mem 8G --time 600 -- \
    "$BIN_DIR/acos" verify "$s" --engine art --artifacts "$artdir" --json \
    > "$OUT/$name.json" 2> "$OUT/$name.err"
  rc=$?
  t1=$(date +%s)
  # Keep only verdict.json (not raw frames/trees) under results/ to stay < 15 MB
  mkdir -p "$OUT/verdicts"
  if [ -f "$artdir/verdict.json" ]; then
    cp "$artdir/verdict.json" "$OUT/verdicts/$name.verdict.json"
  fi
  rm -rf "$artdir"
  info=$(python3 -c "
import json, sys
try:
    d = json.load(open('$OUT/$name.json'))
    st = d.get('status')
    err = (d.get('error') or {}).get('code') if isinstance(d.get('error'), dict) else None
    vp = '$OUT/verdicts/$name.verdict.json'
    steps = None
    try:
        v = json.load(open(vp))
        steps = len(v.get('steps') or [])
    except Exception:
        pass
    print(f'status={st} steps={steps} err={err}')
except Exception as e:
    print(f'status=UNPARSED err={e}')
")
  printf '{"scenario":"%s","name":"%s","exit":%d,"secs":%d,"info":"%s"}\n' \
    "$s" "$name" "$rc" "$((t1-t0))" "$info" >> "$OUT/rows.jsonl"
  echo "done $name rc=$rc (${t1}-${t0}=$((t1-t0))s) $info" | tee -a "$OUT/driver.log"
  if [ "$rc" -eq 0 ]; then pass_n=$((pass_n+1)); else fail_n=$((fail_n+1)); fi
done < "$OUT/scenarios-list.txt"

echo "=== MODULES DONE pass=$pass_n fail=$fail_n $(date -u +%FT%TZ) ===" | tee -a "$OUT/driver.log"
df -h / | tail -1 | tee -a "$OUT/driver.log"
