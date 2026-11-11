#!/usr/bin/env bash
# p7-corpus.sh -- replay the 27-program DEX corpus through the real bwrap argv
# shape, plain vs hardened, and diff stdout against the committed fixtures.
#
# The only deviation from a real `acos-art run`: the DEX is passed as
# /data/s2probe/NN.dex instead of /data/task/00_classes.dex, because the argv
# binds one fixed per-run DEX. Same mount, same flags, same dalvikvm invocation.
set -uo pipefail
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$DIR/../../.." && pwd)"
SHAPE="$DIR/bwrap-shape.sh"
HARD="--unshare-pid --unshare-net --unshare-ipc --unshare-uts --unshare-cgroup-try --die-with-parent --clearenv --cap-drop ALL"
DEXROOT="$ROOT/fixtures/build/dexprogs"
EXPECT="$ROOT/fixtures/expected-art/dexprogs"
STAGE="$ROOT/.cache/art/data/s2probe"
mkdir -p "$STAGE"
pass=0; fail=0; failed=""
for d in "$DEXROOT"/*/; do
  name=$(basename "$d"); dex="$d/classes.dex"
  [ -f "$dex" ] || { printf '%-32s SKIP (no dex)\n' "$name"; continue; }
  cp "$dex" "$STAGE/$name.dex"
  code=0
  for m in plain hard; do
    EX=""; [ "$m" = hard ] && EX="$HARD"
    out=$(S2_EXTRA="$EX" S2_MEM=4G S2_TIME=120 \
      "$SHAPE" -- /system/apex/com.android.art/bin/dalvikvm64 -cp "/data/s2probe/$name.dex" Main 2>/dev/null)
    rc=$?
    exp="$EXPECT/$name.stdout"; expc=0
    [ -f "$EXPECT/$name.exit" ] && expc=$(cat "$EXPECT/$name.exit")
    if [ "$rc" = "$expc" ] && { [ ! -f "$exp" ] || [ "$out" = "$(cat "$exp")" ]; }; then
      printf '%-32s %-5s PASS rc=%s\n' "$name" "$m" "$rc"; pass=$((pass+1))
    else
      printf '%-32s %-5s FAIL rc=%s (expected %s)\n' "$name" "$m" "$rc" "$expc"; fail=$((fail+1)); failed="$failed $name/$m"
    fi
  done
done
rm -f "$STAGE"/*.dex; rmdir "$STAGE" 2>/dev/null
printf '\nTOTAL pass=%s fail=%s%s\n' "$pass" "$fail" "${failed:+ (failed:$failed)}"
