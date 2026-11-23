#!/usr/bin/env bash
# Lean, interleaved A/B for the SPIKE-NET flag on the launch path.
#
# NOT the bench's M3/M4 (tools/bench is broken at this commit, see REPORT.md):
# this measures `acos verify --engine art` on hello-views, whose verdict records
# the ART session boot (`session_start_ms`), the launch step's wall
# (`steps[0].wall_ms`) and the session cgroup's memory peak. Same binary, same
# scenario, alternating arms, one run per sample.
set -uo pipefail
WT=/home/Qui/.t3/worktrees/acos/t3-bd758e68-c5d2c8dcdcd2504d
cd "$WT"
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export ACOS_ART_BIN="$WT/.cache/sn3-target/release/acos-art"
unset ACOS_BOOT_DIR XDG_RUNTIME_DIR ACOS_SPIKE_NET_DNS_LOG
OUT="$WT/.cache/ab/lean.jsonl"
: > "$OUT"
N=${N:-10}
for i in $(seq 1 "$N"); do
  for ARM in off on; do
    if [ "$ARM" = on ]; then
      export ACOS_SPIKE_NET=1 ACOS_SPIKE_NET_PASTA="$WT/.cache/passt-build/src/pasta"
    else
      unset ACOS_SPIKE_NET ACOS_SPIKE_NET_PASTA
    fi
    DIR="$WT/.cache/sn3-artifacts/lean-$ARM"
    rm -rf "$DIR"
    tools/art-runtime/run-exclusive.sh --task SPIKE-NET -- \
      tools/guard/guarded.sh --mem 8G --time 300 -- \
      "$WT/.cache/sn3-target/release/acos" verify --engine art \
        --apk fixtures/build/apps/hello-views.apk \
        --scenario scenarios/hello-views.yaml \
        --artifacts "$DIR" --keep-artifacts none-on-pass >/dev/null 2>&1
    rc=$?
    python3 - "$ARM" "$i" "$rc" "$DIR" >> "$OUT" <<'PY'
import json, sys, pathlib
arm, rep, rc, d = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), pathlib.Path(sys.argv[4])
row = {"arm": arm, "rep": rep, "verify_rc": rc}
try:
    e = json.loads((d / "engine.json").read_text())["engine"]
    t = e.get("timing") or {}
    row["session_start_ms"] = t.get("session_start_ms")
    row["verdict_wall_ms"] = t.get("verdict_wall_ms")
    steps = t.get("steps") or []
    row["launch_step_ms"] = steps[0]["wall_ms"] if steps else None
    row["launch_status"] = steps[0]["status"] if steps else None
    row["status"] = e.get("session_exit", {}) and json.loads((d/"verdict.json").read_text())["status"]
    cg = (e.get("session_exit") or {}).get("cgroup") or ""
    import re
    m = re.search(r"peak=(\d+)MiB", cg)
    row["cgroup_peak_mib"] = int(m.group(1)) if m else None
except Exception as exc:
    row["error"] = "%s: %s" % (type(exc).__name__, exc)
print(json.dumps(row))
PY
    rm -rf "$DIR"
  done
done
