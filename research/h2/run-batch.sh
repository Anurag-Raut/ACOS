#!/usr/bin/env bash
# H2 batched A/B runner: one host ART lock acquisition per batch (<10 min).
#   run-batch.sh first-form          # form-validation off+on, 20 launches each
#   run-batch.sh memory-rep N        # both arms, N=1/5/10, one interleaved rep
#   run-batch.sh loop N              # both arms, 3 reps x 1 min at one N
#   run-batch.sh validation          # cargo test + 7 gates + p11 scenarios
#   run-batch.sh k1                  # the 5 K1 first-frame apps
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
CACHE=/home/Qui/Projects/acos/.cache
export ACOS_FONT_DIR=$CACHE/boot/android-34/fonts ACOS_CACHE=$CACHE
export ACOS_ART_BIN=$CACHE/h2-target/release/acos-art ACOS_Z7_RUNS=$CACHE/h2/runs
unset ACOS_BOOT_DIR ACOS_ART_GC_DELAY_MS ACOS_ZYGOTE_APP ACOS_ZYGOTE_NO_STUB
OUT=$CACHE/h2/runs; LOG=$OUT/run-batch.log; mkdir -p "$OUT"
export ROOT CACHE OUT LOG
host() { { date -u +%FT%TZ; uptime; free -m | sed -n '1,2p'; } >> "$OUT/host.log"; }
lockrun() { # TASK TIME MEM CMD...
  local task=$1 time=$2 mem=$3; shift 3
  local rc=0
  tools/art-runtime/run-exclusive.sh --task "$task" -- \
    tools/guard/guarded.sh --mem "$mem" --time "$time" -- "$@" >> "$LOG" 2>&1 || rc=$?
  echo "=== $task rc=$rc $(date -u +%FT%TZ)" >> "$LOG"
  return $rc
}
lifetime() { # arm early n rep
  local arm=$1 v=$2 n=$3 rep=$4 dir=$OUT/memory/rep$4/$1-n$3
  mkdir -p "$dir"
  ACOS_ART_EARLY_GC=$v python3 "$ROOT/research/zygote-proto/z7/lifetime.py" \
    --arm "$arm" --n "$n" --out "$dir" --json "$dir/result.json" \
    --ages 0.5,2,5,15 --label "rep$rep $arm n$n"
}
first_form() {
  for arm in off on; do
    local v=0; [ "$arm" = on ] && v=1
    local dir=$OUT/first-action/form-validation-$arm; mkdir -p "$dir"
    H2_SCRATCH=$dir ACOS_ART_EARLY_GC=$v \
      python3 "$ROOT/research/h2/first_action.py" --app form-validation --arm "$arm" \
        --n 20 --out "$dir" --json "$dir/result.json"
  done
}
memory_rep() {
  local rep=$1; local order="a:0 ap:1"; [ $((rep % 2)) -eq 0 ] && order="ap:1 a:0"
  for pair in $order; do
    local arm=${pair%%:*} v=${pair##*:}
    for n in 1 5 10; do lifetime "$arm" "$v" "$n" "$rep"; done
  done
}
loopn() {
  local n=$1 mem=8G; [ "$n" = 10 ] && mem=24G
  local order="a:0 ap:1"
  for rep in 1 2 3; do
    [ $((rep % 2)) -eq 0 ] && order="ap:1 a:0"
    for pair in $order; do
      local arm=${pair%%:*} v=${pair##*:}
      local dir=$OUT/loop/n$n/rep$rep/$arm; mkdir -p "$dir"
      ACOS_ART_EARLY_GC=$v python3 "$ROOT/research/zygote-proto/z7/loop.py" \
        --arm "$arm" --n "$n" --minutes 1.0 --app counter \
        --out "$dir" --json "$dir/result.json"
    done
  done
}
form_n5() {
  for arm in a ap; do
    local v=0; [ "$arm" = ap ] && v=1
    for run in 1 2; do
      local dir=$OUT/form-n5/$arm-run$run; mkdir -p "$dir"
      ACOS_ART_EARLY_GC=$v python3 "$ROOT/research/zygote-proto/z7/loop.py" \
        --arm "$arm" --n 5 --cycles 1 --app form-validation --close-bound 5 \
        --out "$dir" --json "$dir/result.json"
    done
  done
}
validation() {
  local dir=$OUT/validation; mkdir -p "$dir"; : > "$dir/gates.txt"; : > "$dir/p11.txt"
  CARGO_TARGET_DIR=$CACHE/h2-target cargo test --release --offline \
    -p acos-cli -p acos-art -p acos-services >> "$dir/cargo-test.log" 2>&1
  echo "cargo-test rc=$?" >> "$dir/gates.txt"
  for g in v2-art-counter w22-counter-reset w54-dialog-pixels w42-back-frame w15-dialog-menu suite/T03-render-good suite/T03-render-wrong; do
    target/release/acos verify "scenarios/$g.yaml" --engine art >> "$dir/gates.log" 2>&1
    echo "$g rc=$?" >> "$dir/gates.txt"
  done
  for s in scenarios/p11-*.yaml; do
    local name=$(basename "$s" .yaml)
    target/release/acos verify "$s" >> "$dir/p11.log" 2>&1
    echo "$name rc=$?" >> "$dir/p11.txt"
  done
}
case "${1:?first-form|memory-rep|loop|validation|form-n5|k1}" in
  first-form)
    lockrun H2-first-form 540 8G bash -c "$(declare -f first_form); first_form" ;;
  memory-rep)
    host
    lockrun "H2-memory-r$2" 600 24G bash -c "first_form() { :; }; $(declare -f lifetime memory_rep); memory_rep $2" ;;
  loop)
    host
    lockrun "H2-loop-$2" 540 24G bash -c "$(declare -f loopn); loopn $2" ;;
  validation)
    host
    lockrun H2-validation 600 8G bash -c "$(declare -f validation); validation" ;;
  form-n5)
    host
    lockrun H2-form-n5 600 8G bash -c "$(declare -f form_n5); form_n5" ;;
  k1)
    export ACOS_BIN=$CACHE/h2-target/release/acos ACOS_ART_BIN=$CACHE/h2-target/release/acos-art
    export ACOS_K1_RUN=$CACHE/h2/k1; mkdir -p "$ACOS_K1_RUN"
    bash "$ROOT/tools/compat/run-k1.sh" --batch 5 \
      --only com.beemdevelopment.aegis,org.jellyfin.mobile,com.nonsenseapps.feeder,net.cozic.joplin,org.secuso.privacyfriendlynotes
    ;;
  *) echo "unknown batch $1" >&2; exit 2 ;;
esac
host
