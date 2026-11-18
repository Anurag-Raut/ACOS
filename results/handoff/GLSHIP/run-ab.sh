#!/usr/bin/env bash
# GLSHIP cost A/B (indicative, shared slots): default provider ON vs
# `ACOS_GUEST_GLES=off`, 5 interleaved reps each, for counter and form.
#
# Measures, per rep, from the run's own artifacts:
#   * M8 fast: the sum of the tap/type step durations (fast settle) — the
#     action→state latency;
#   * total verify wall (`verdict.resources.wall_ms`);
#   * per-session peak RSS (`verdict.resources.peak_rss_bytes`, the session's own
#     high-water mark, sampled by the launcher);
#   * launch step duration (`steps[0].duration_ms`).
#
# Interleaved (on, off, on, off, …) so host drift hits both arms, and each rep
# gets its own private runtime dir (run-arm.sh), so no state is shared.
#
#   tools/art-runtime/run-exclusive.sh --task GLSHIP -- \
#       tools/guarded.sh --mem 12G --time 900 -- results/handoff/GLSHIP/run-ab.sh
set -uo pipefail
ROOT="$PWD"
R=results/handoff/GLSHIP/run-arm.sh
OUT=results/handoff/GLSHIP/ab
mkdir -p "$OUT"

# One rep: $1 = arm (on|off), $2 = app tag, $3 = scenario, $4 = rep
rep() {
  local arm="$1"
  local app="$2"
  local scen="$3"
  local n="$4"
  local tag="ab-$app-$arm-$n"
  if [ "$arm" = off ]; then
    $R "$tag" "$scen" ACOS_GUEST_GLES=off >/dev/null
  else
    # The trace line reports the provider verify cost of this launch (cache
    # hit vs first digest), so the launch cost is measured, not asserted.
    $R "$tag" "$scen" ACOS_GUEST_GLES_TRACE=1 >/dev/null
  fi
}

for n in 1 2 3 4 5; do
  rep on  counter results/handoff/GLSHIP/scenarios/counter-fast.yaml "$n"
  rep off counter results/handoff/GLSHIP/scenarios/counter-fast.yaml "$n"
  rep on  form    results/handoff/GLSHIP/scenarios/form-fast.yaml "$n"
  rep off form    results/handoff/GLSHIP/scenarios/form-fast.yaml "$n"
done

python3 results/handoff/GLSHIP/summarize-ab.py > "$OUT/summary.md"
echo "--- summary"
cat "$OUT/summary.md"
