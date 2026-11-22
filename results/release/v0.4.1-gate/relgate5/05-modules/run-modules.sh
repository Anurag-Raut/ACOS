#!/usr/bin/env bash
# RELGATE5 step 5: conformance kit + 48 module proof scenarios + metro-reload (5/5)
# + 9 new negative scenarios (must FAIL) + metro-reload-full (3x best-effort).
set -uo pipefail
WT="/home/Qui/.t3/worktrees/acos/t3-8432ccd8-1d58d051095d020b"
cd "$WT" || exit 2
source "$WT/.cache/env.sh"
export CARGO_TARGET_DIR="$WT/.cache/rg5-target"
BIN_DIR="$CARGO_TARGET_DIR/release"
OUT="$WT/results/release/v0.4.1-gate/relgate5/05-modules"
RT="$WT/.cache/rt-mod"
mkdir -p "$OUT/verdicts" "$RT" "$WT/.cache/wvrun"

unset ACOS_BOOT_DIR XDG_RUNTIME_DIR ACOS_HOME TMPDIR ACOS_NET_PASTA ACOS_SPIKE_NET_PASTA ACOS_NET_ARM
export ACOS_RUNTIME_DIR="$RT"
export ACOS_SESSION_SOCKET="$RT/session.sock"
export ACOS_ART_BIN="$BIN_DIR/acos-art"
export ACOS_WEBVIEW_BIN="$BIN_DIR/acos-webview"
export ACOS_WEBVIEW_CACHE="$WT/.cache/webview"
export ACOS_WEBVIEW_SOCKET_DIR="$WT/.cache/wvrun"
export PATH="$BIN_DIR:$PATH"

run_one() {
  local s="$1" name="$2"
  local artdir="$WT/.cache/rg5-mod-art/$name"
  rm -rf "$artdir"
  local t0 t1 rc info
  t0=$(date +%s)
  tools/art-runtime/run-exclusive.sh --task RELGATE5-mod -- \
    tools/guarded.sh --mem 8G --time 600 -- \
    "$BIN_DIR/acos" verify "$s" --engine art --artifacts "$artdir" --json \
    > "$OUT/$name.json" 2> "$OUT/$name.err"
  rc=$?
  t1=$(date +%s)
  if [ -f "$artdir/verdict.json" ]; then
    cp "$artdir/verdict.json" "$OUT/verdicts/$name.verdict.json"
  fi
  rm -rf "$artdir"
  info=$(python3 -c "
import json
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
  echo "done $name rc=$rc ($((t1-t0))s) $info" | tee -a "$OUT/driver.log"
  return "$rc"
}

echo "=== 1. Conformance kit + modules_doc $(date -u +%FT%TZ) ===" | tee "$OUT/driver.log"
tools/art-runtime/run-exclusive.sh --task RELGATE5-kit -- \
  tools/guarded.sh --mem 14G --time 900 -- \
  cargo test --release --offline -p acos-services --test module_kit --test modules_doc -- --nocapture \
  > "$OUT/01-conformance-kit.log" 2>&1
KIT_RC=$?
echo "KIT_RC=$KIT_RC $(date -u +%FT%TZ)" | tee -a "$OUT/driver.log"
grep -E "^test result|FAILED" "$OUT/01-conformance-kit.log" | tee -a "$OUT/driver.log"

echo "=== 2. Positive module proof scenarios (48) $(date -u +%FT%TZ) ===" | tee -a "$OUT/driver.log"
: > "$OUT/rows.jsonl"
pass_n=0
fail_n=0
while IFS= read -r s; do
  [ -n "$s" ] || continue
  name=$(basename "$s" .yaml)
  if run_one "$s" "$name"; then pass_n=$((pass_n+1)); else fail_n=$((fail_n+1)); fi
done < "$OUT/scenarios-list.txt"
echo "=== POSITIVES DONE pass=$pass_n fail=$fail_n $(date -u +%FT%TZ) ===" | tee -a "$OUT/driver.log"

echo "=== 3. metro-reload 5/5 $(date -u +%FT%TZ) ===" | tee -a "$OUT/driver.log"
mr_pass=0
for i in 1 2 3 4 5; do
  if run_one "scenarios/metro-reload.yaml" "metro-reload-r$i"; then mr_pass=$((mr_pass+1)); fi
done
echo "METRO_RELOAD_PASS=$mr_pass/5 $(date -u +%FT%TZ)" | tee -a "$OUT/driver.log"

echo "=== 4. New v0.4.1 negative scenarios (9, must FAIL) $(date -u +%FT%TZ) ===" | tee -a "$OUT/driver.log"
neg_ok=0
neg_fp=0
while IFS= read -r s; do
  [ -n "$s" ] || continue
  name="neg__$(basename "$s" .yaml)"
  if run_one "$s" "$name"; then
    neg_fp=$((neg_fp+1))
    echo "FALSE_PASS on $s!" | tee -a "$OUT/driver.log"
  else
    neg_ok=$((neg_ok+1))
  fi
done < "$OUT/negatives-list.txt"
echo "NEW_NEGATIVES_OK=$neg_ok FALSE_PASS=$neg_fp $(date -u +%FT%TZ)" | tee -a "$OUT/driver.log"

echo "=== 5. metro-reload-full (3x best-effort) $(date -u +%FT%TZ) ===" | tee -a "$OUT/driver.log"
mrf_pass=0
for i in 1 2 3; do
  if run_one "scenarios/metro-reload-full.yaml" "metro-reload-full-r$i"; then mrf_pass=$((mrf_pass+1)); fi
done
echo "METRO_RELOAD_FULL_BEST_EFFORT=$mrf_pass/3 $(date -u +%FT%TZ)" | tee -a "$OUT/driver.log"

echo "=== Check orphan metro/node processes ===" | tee -a "$OUT/driver.log"
ps -eo pid,args | awk '/metro|react-native.*start/ && !/awk/' | tee -a "$OUT/driver.log" || true

rm -rf "$WT/.cache/rg5-mod-art" "$RT"
echo "=== MODULES DONE $(date -u +%FT%TZ) ===" | tee -a "$OUT/driver.log"
df -h / | tail -1 | tee -a "$OUT/driver.log"
free -m | head -2 | tee -a "$OUT/driver.log"
