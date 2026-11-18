#!/bin/bash
# SCENHYG: run one list of scenarios on the current tree and print one line per
# run (name, rc, status, code, reason, steps). Call under run-exclusive.sh.
#
#   run-probe.sh <out-dir> <scenario> [scenario...]
set -u
cd "$1" || exit 2
out=$2; shift 2
mkdir -p "$out"
acos=$PWD/.cache/scenhyg-target/release/acos
fail=0
for f in "$@"; do
  name=$(basename "$f" .yaml)
  dir="$out/$name"
  env -u ACOS_HOME -u ACOS_BOOT_DIR "$acos" verify "$f" --engine art --json \
      --artifacts "$dir" > "$dir.json" 2> "$dir.err"
  rc=$?
  line=$(python3 - "$dir.json" "$rc" "$name" <<'PY'
import json, sys
p, rc, name = sys.argv[1], sys.argv[2], sys.argv[3]
try:
    d = json.load(open(p))
except Exception as e:
    print("PROBE %s rc=%s unparseable=%s" % (name, rc, e)); raise SystemExit(0)
v = (d.get("result") or {}).get("verdict") or d.get("verdict") or {}
f = v.get("failure") or {}
e = d.get("error") or f.get("error") or {}
det = e.get("details") or {}
print("PROBE %s rc=%s status=%s code=%s reason=%s step=%s steps=%d" % (
    name, rc, d.get("status"), e.get("code"), det.get("reason"),
    f.get("step_index"), len(v.get("steps") or [])))
PY
)
  echo "$line"
  [ "$rc" -ne 0 ] && fail=1
done
echo "PROBE_FAILURES $fail"
exit 0
