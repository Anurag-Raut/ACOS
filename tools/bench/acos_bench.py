#!/usr/bin/env python3
"""acos_bench.py -- the measurement half of tools/bench/acos-bench.sh (H6).

One phase per invocation, always inside
`tools/art-runtime/run-exclusive.sh --task H6 -- tools/guarded.sh ...`:

  --phase latency     M4/M6 strict+fast, M5, M7 strict+fast, M8 counter+form
                      strict+fast, tap ack, acos_destroy teardown (n=20)
  --phase memory      M1 + M2 (cgroup anon+shmem AND Pss) at N=1/5/10, idle CPU
                      and wakeups/s over a 60 s window per N
  --phase loop1       full P5 agent loop at N=1 (reuses P5's harness)
  --phase loop5       full P5 agent loop at N=5
  --phase loop10      full P5 agent loop at N=10 (16G guarded arm)
  --phase loop20      full P5 agent loop at N=20 (24G guarded arm, S3)
  --phase loop30      full P5 agent loop at N=30 (24G guarded arm, S3 ceiling probe)
  --phase cold        M3 cold start (page cache evicted per rep; reuses P7)
  --phase p11         P11 settle-lab false passes (reuses P11's harness)
  --phase scenarios   scenarios/p11-*.yaml sweep through `acos verify`

Every launch carries the process-wide animation arm (`--animations on|off`,
default on = the product default; `off` is P12's agent-facing functional-test
mode). Strict replies are gated on `settled:true` (a P12 timeout is never a
pass) and every strict input/tree row records `pixels_changed`.

Each phase writes one JSON with raw sample arrays. `--merge` pools the reps into
results/bench/baseline-<UTC>.json (+ .md) and derives the per-metric noise
thresholds from the rep-to-rep spread. `--compare` prints the per-metric delta
against a stored baseline and exits non-zero on a regression.

Reused, not reimplemented: tools/benchlib.py (stats, host gate, provenance,
cgroup/memory/PSS helpers), tools/art-runtime/d2_warm.py (Serve, WarmSampler,
measure_screenshot, survivors), tools/art-runtime/agent_loop.py (Daemon),
results/art-runtime/evidence/P5/p5_loop.py (agent loop),
results/art-runtime/evidence/P7/p7.py (M3 + fadvise eviction),
results/art-runtime/evidence/P11/p11_measure.py (strict-settle false passes).
Scratch is under /home/Qui/Projects/acos/.cache/h6/ and removed after each phase.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import math
import os
import shutil
import statistics
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
sys.path.insert(0, str(REPO / "tools"))
sys.path.insert(0, str(REPO / "tools/art-runtime"))
sys.path.insert(0, str(HERE))

import benchlib  # noqa: E402
import bench_lattice  # noqa: E402
import d2_warm  # noqa: E402
import agent_loop  # noqa: E402

APPS = REPO / "fixtures/build/apps"
COUNTER = APPS / "counter.apk"
FORM = APPS / "form-validation.apk"
SETTLE_LAB = APPS / "settle-lab.apk"
P5_LOOP = REPO / "results/art-runtime/evidence/P5/p5_loop.py"
P7_HARNESS = REPO / "results/art-runtime/evidence/P7/p7.py"
P11_MEASURE = REPO / "results/art-runtime/evidence/P11/p11_measure.py"

MiB = 1024.0 * 1024.0
HZ = os.sysconf("SC_CLK_TCK")
FORM_ERROR = "Name is required"

# PERF-REL3: the metrics whose settle path waits for the compositor, so every
# sample sits on a frame-period lattice. They get raw samples, a pooled 5 %-trimmed
# mean and per-frame-slot shares in the merged baseline (additive fields; p50/p95
# are unchanged), and a larger default sample count (--fast-reps).
FAST_METRICS = ("m4_launch_fast_ms", "m6_reset_fast_ms",
                "m8_tap_counter_fast_ms", "m8_tap_form_fast_ms")
DEFAULT_FAST_REPS = 40

# --------------------------------------------------------------------------- #
# Metric registry. category drives the minimum noise band; direction says which
# way is better. Every method string is part of the delivered label.
# --------------------------------------------------------------------------- #
METRICS = {
    "m1_idle_mem_mib": {
        "metric": "M1", "label": "M1 idle memory, 1 app",
        "unit": "MiB", "category": "memory", "direction": "lower",
        "method": "cgroup v2 memory.stat anon+shmem (job scope + guest scopes, "
                  "sampler's own anon+shmem charged back), 60 s after first frame, N=1",
    },
    "m1_idle_pss_mib": {
        "metric": "M1", "label": "M1 idle memory, 1 app (Pss)",
        "unit": "MiB", "category": "memory", "direction": "lower",
        "method": "sum of Pss over the serve process tree (smaps_rollup), 60 s after "
                  "first frame, N=1; shared pages divided by mapping count",
    },
    "m2_per_session_anon_shmem_n1": {
        "metric": "M2", "label": "M2 memory per extra session, N=1",
        "unit": "MiB/session", "category": "memory", "direction": "lower",
        "method": "(cgroup anon+shmem N=1 - bare daemon) / 1, 60 s idle",
    },
    "m2_per_session_anon_shmem_n5": {
        "metric": "M2", "label": "M2 memory per extra session, N=5",
        "unit": "MiB/session", "category": "memory", "direction": "lower",
        "method": "(cgroup anon+shmem N=5 - bare daemon) / 5, 60 s idle",
    },
    "m2_per_session_anon_shmem_n10": {
        "metric": "M2", "label": "M2 memory per extra session, N=10",
        "unit": "MiB/session", "category": "memory", "direction": "lower",
        "method": "(cgroup anon+shmem N=10 - bare daemon) / 10, 60 s idle",
    },
    "m2_per_session_pss_n1": {
        "metric": "M2", "label": "M2 Pss per session, N=1",
        "unit": "MiB/session", "category": "memory", "direction": "lower",
        "method": "(system-wide sum Pss of the session tree - bare daemon Pss) / 1; "
                  "Pss divides a shared page by mapping count, so at N=1 the single "
                  "session carries the whole boot-image/framework file share",
    },
    "m2_per_session_pss_n5": {
        "metric": "M2", "label": "M2 Pss per session, N=5",
        "unit": "MiB/session", "category": "memory", "direction": "lower",
        "method": "(sum Pss of sessions - bare daemon Pss) / 5; see M2 Pss N=1",
    },
    "m2_per_session_pss_n10": {
        "metric": "M2", "label": "M2 Pss per session, N=10",
        "unit": "MiB/session", "category": "memory", "direction": "lower",
        "method": "(sum Pss of sessions - bare daemon Pss) / 10; see M2 Pss N=1",
    },
    "m3_cold_start_ms": {
        "metric": "M3", "label": "M3 cold start -> first correct screen",
        "unit": "ms", "category": "latency", "direction": "lower",
        "method": "nothing running, page cache evicted per rep (posix_fadvise, "
                  "fincore-proved) -> spawn acos serve -> acos_launch -> reply tree "
                  "is count=0 and a following strict tree still reads 0",
    },
    "m4_launch_strict_ms": {
        "metric": "M4", "label": "M4 warm launch (strict)",
        "unit": "ms", "category": "latency", "direction": "lower",
        "method": "warm daemon, app not running -> acos_launch settle=strict -> "
                  "reply tree count=0 + post-frame strict tree count=0; destroy "
                  "between reps",
    },
    "m4_launch_fast_ms": {
        "metric": "M4", "label": "M4 warm launch (fast)",
        "unit": "ms", "category": "latency", "direction": "lower",
        "method": "acos_launch settle=fast (probe-fresh label recorded) -> reply "
                  "tree count=0 + post-frame strict tree count=0",
    },
    "m5_screenshot_ms": {
        "metric": "M5", "label": "M5 fresh screenshot (PNG)",
        "unit": "ms", "category": "latency", "direction": "lower",
        "method": "acos_observe {png:true} on a held session; PNG magic + newer "
                  "frame_seq + meaningful frame required",
    },
    "m6_reset_strict_ms": {
        "metric": "M6", "label": "M6 reset -> first correct screen (strict)",
        "unit": "ms", "category": "latency", "direction": "lower",
        "method": "count=1 strictly read -> acos_reset settle=strict -> reply tree "
                  "count=0 + following strict tree count=0",
    },
    "m6_reset_fast_ms": {
        "metric": "M6", "label": "M6 reset -> first correct screen (fast)",
        "unit": "ms", "category": "latency", "direction": "lower",
        "method": "acos_reset settle=fast; state_reset + data_cleared + count=0 in "
                  "the reply and in a following strict tree",
    },
    "m7_tree_strict_ms": {
        "metric": "M7", "label": "M7 UI tree with text (strict)",
        "unit": "ms", "category": "latency", "direction": "lower",
        "method": "acos_observe_tree settle=strict on a held session; tree has "
                  "nodes and text, not truncated",
    },
    "m7_tree_fast_ms": {
        "metric": "M7", "label": "M7 UI tree with text (fast)",
        "unit": "ms", "category": "latency", "direction": "lower",
        "method": "acos_observe_tree settle=fast; the fast tree must show the same "
                  "count text as a strict read (no stale tree)",
    },
    "m8_tap_counter_strict_ms": {
        "metric": "M8", "label": "M8 tap -> settled, counter (strict)",
        "unit": "ms", "category": "latency", "direction": "lower",
        "method": "acos_tap {id:inc} settle=strict on counter, one session; every "
                  "tap must report state_changed and the final strict count must "
                  "equal the tap count",
    },
    "m8_tap_counter_fast_ms": {
        "metric": "M8", "label": "M8 tap -> settled, counter (fast)",
        "unit": "ms", "category": "latency", "direction": "lower",
        "method": "acos_tap settle=fast; same state_changed/final-count checks",
    },
    "m8_tap_form_strict_ms": {
        "metric": "M8", "label": "M8 tap -> settled, form (strict)",
        "unit": "ms", "category": "latency", "direction": "lower",
        "method": "empty form, tap submit settle=strict; following strict tree "
                  "must read error='Name is required'; fast reset (unmeasured) "
                  "before each rep",
    },
    "m8_tap_form_fast_ms": {
        "metric": "M8", "label": "M8 tap -> settled, form (fast)",
        "unit": "ms", "category": "latency", "direction": "lower",
        "method": "empty form, tap submit settle=fast; same strict post-check",
    },
    "tap_ack_ms": {
        "metric": "tap ack", "label": "tap acknowledgement (settle.ack_ms)",
        "unit": "ms", "category": "latency", "direction": "lower",
        "method": "input ack latency reported in the tap reply's settle block "
                  "(counter + form, strict and fast)",
    },
    "teardown_ms": {
        "metric": "teardown", "label": "acos_destroy teardown_ms (GL1)",
        "unit": "ms", "category": "latency", "direction": "lower",
        "method": "reply teardown.teardown_ms for a live counter session "
                  "(lifecycle label recorded); 20 samples",
    },
    "teardown_wall_ms": {
        "metric": "teardown", "label": "acos_destroy wall",
        "unit": "ms", "category": "latency", "direction": "lower",
        "method": "client wall of acos_destroy incl. teardown evidence",
    },
    "loop_n1_ms": {
        "metric": "agent loop", "label": "full agent loop latency, N=1",
        "unit": "ms/task", "category": "latency", "direction": "lower",
        "method": "P5 loop: launch->tree->find->tap->settle->screenshot->verify->"
                  "reset; sum of the 7 request walls per verified task",
    },
    "loop_n1_verified_rate": {
        "metric": "agent loop", "label": "verified task rate, N=1",
        "unit": "fraction", "category": "throughput", "direction": "higher",
        "method": "verified tasks / measured tasks in the P5 loop (1.0 required "
                  "for a fully reliable loop; a failure is not a false pass)",
    },
    "loop_n1_tasks_per_s": {
        "metric": "agent loop", "label": "verified tasks/s, N=1",
        "unit": "tasks/s", "category": "throughput", "direction": "higher",
        "method": "verified tasks / cohort wall (P5 definition)",
    },
    "loop_n1_tasks_per_gib_min": {
        "metric": "agent loop", "label": "verified tasks/GiB-min, N=1",
        "unit": "tasks/GiB-min", "category": "throughput", "direction": "higher",
        "method": "verified tasks / cgroup GiB-min integral (benchlib.sample_integral) "
                  "over the cohort",
    },
    "loop_n1_tasks_per_gib_min_net": {
        "metric": "agent loop", "label": "verified tasks/GiB-min, N=1 (anon+shmem)",
        "unit": "tasks/GiB-min", "category": "throughput", "direction": "higher",
        "method": "verified tasks / integral of the job's cgroup anon+shmem only "
                  "(label-matched with M1/M2; excludes page cache charged to the "
                  "cgroup, which is file cache read by whichever phase ran first)",
    },
    "loop_n5_ms": {
        "metric": "agent loop", "label": "full agent loop latency, N=5",
        "unit": "ms/task", "category": "latency", "direction": "lower",
        "method": "same P5 loop with 5 concurrent sessions in one serve",
    },
    "loop_n5_verified_rate": {
        "metric": "agent loop", "label": "verified task rate, N=5",
        "unit": "fraction", "category": "throughput", "direction": "higher",
        "method": "verified tasks / measured tasks, 5 concurrent sessions",
    },
    "loop_n5_tasks_per_s": {
        "metric": "agent loop", "label": "verified tasks/s, N=5",
        "unit": "tasks/s", "category": "throughput", "direction": "higher",
        "method": "verified tasks / cohort wall, 5 concurrent sessions",
    },
    "loop_n5_tasks_per_gib_min": {
        "metric": "agent loop", "label": "verified tasks/GiB-min, N=5",
        "unit": "tasks/GiB-min", "category": "throughput", "direction": "higher",
        "method": "verified tasks / cgroup GiB-min integral, 5 concurrent sessions",
    },
    "loop_n5_tasks_per_gib_min_net": {
        "metric": "agent loop", "label": "verified tasks/GiB-min, N=5 (anon+shmem)",
        "unit": "tasks/GiB-min", "category": "throughput", "direction": "higher",
        "method": "verified tasks / integral of the job's cgroup anon+shmem only "
                  "(label-matched with M1/M2; excludes page cache charged to the "
                  "cgroup, which is file cache read by whichever phase ran first)",
    },
    "loop_n10_ms": {
        "metric": "agent loop", "label": "full agent loop latency, N=10",
        "unit": "ms/task", "category": "latency", "direction": "lower",
        "method": "same P5 loop with 10 concurrent sessions in one serve",
    },
    "loop_n10_verified_rate": {
        "metric": "agent loop", "label": "verified task rate, N=10",
        "unit": "fraction", "category": "throughput", "direction": "higher",
        "method": "verified tasks / measured tasks, 10 concurrent sessions",
    },
    "loop_n10_tasks_per_s": {
        "metric": "agent loop", "label": "verified tasks/s, N=10",
        "unit": "tasks/s", "category": "throughput", "direction": "higher",
        "method": "verified tasks / cohort wall, 10 concurrent sessions",
    },
    "loop_n10_tasks_per_gib_min": {
        "metric": "agent loop", "label": "verified tasks/GiB-min, N=10",
        "unit": "tasks/GiB-min", "category": "throughput", "direction": "higher",
        "method": "verified tasks / cgroup GiB-min integral, 10 concurrent sessions",
    },
    "loop_n10_tasks_per_gib_min_net": {
        "metric": "agent loop", "label": "verified tasks/GiB-min, N=10 (anon+shmem)",
        "unit": "tasks/GiB-min", "category": "throughput", "direction": "higher",
        "method": "verified tasks / integral of the job's cgroup anon+shmem only "
                  "(label-matched with M1/M2; excludes page cache charged to the "
                  "cgroup, which is file cache read by whichever phase ran first)",
    },
    "loop_n20_ms": {
        "metric": "agent loop", "label": "full agent loop latency, N=20",
        "unit": "ms/task", "category": "latency", "direction": "lower",
        "method": "same P5 loop with 20 concurrent sessions in one serve",
    },
    "loop_n20_verified_rate": {
        "metric": "agent loop", "label": "verified task rate, N=20",
        "unit": "fraction", "category": "throughput", "direction": "higher",
        "method": "verified tasks / measured tasks, 20 concurrent sessions",
    },
    "loop_n20_tasks_per_s": {
        "metric": "agent loop", "label": "verified tasks/s, N=20",
        "unit": "tasks/s", "category": "throughput", "direction": "higher",
        "method": "verified tasks / cohort wall, 20 concurrent sessions",
    },
    "loop_n20_tasks_per_gib_min": {
        "metric": "agent loop", "label": "verified tasks/GiB-min, N=20",
        "unit": "tasks/GiB-min", "category": "throughput", "direction": "higher",
        "method": "verified tasks / cgroup GiB-min integral, 20 concurrent sessions",
    },
    "loop_n20_tasks_per_gib_min_net": {
        "metric": "agent loop", "label": "verified tasks/GiB-min, N=20 (anon+shmem)",
        "unit": "tasks/GiB-min", "category": "throughput", "direction": "higher",
        "method": "verified tasks / integral of the job's cgroup anon+shmem only "
                  "(label-matched with M1/M2; excludes page cache charged to the "
                  "cgroup, which is file cache read by whichever phase ran first)",
    },
    "loop_n30_ms": {
        "metric": "agent loop", "label": "full agent loop latency, N=30",
        "unit": "ms/task", "category": "latency", "direction": "lower",
        "method": "same P5 loop with 30 concurrent sessions in one serve",
    },
    "loop_n30_verified_rate": {
        "metric": "agent loop", "label": "verified task rate, N=30",
        "unit": "fraction", "category": "throughput", "direction": "higher",
        "method": "verified tasks / measured tasks, 30 concurrent sessions",
    },
    "loop_n30_tasks_per_s": {
        "metric": "agent loop", "label": "verified tasks/s, N=30",
        "unit": "tasks/s", "category": "throughput", "direction": "higher",
        "method": "verified tasks / cohort wall, 30 concurrent sessions",
    },
    "loop_n30_tasks_per_gib_min": {
        "metric": "agent loop", "label": "verified tasks/GiB-min, N=30",
        "unit": "tasks/GiB-min", "category": "throughput", "direction": "higher",
        "method": "verified tasks / cgroup GiB-min integral, 30 concurrent sessions",
    },
    "loop_n30_tasks_per_gib_min_net": {
        "metric": "agent loop", "label": "verified tasks/GiB-min, N=30 (anon+shmem)",
        "unit": "tasks/GiB-min", "category": "throughput", "direction": "higher",
        "method": "verified tasks / integral of the job's cgroup anon+shmem only "
                  "(label-matched with M1/M2; excludes page cache charged to the "
                  "cgroup, which is file cache read by whichever phase ran first)",
    },
    "cpu_idle_n1_pct": {
        "metric": "CPU", "label": "idle CPU, N=1",
        "unit": "% of one core", "category": "cpu", "direction": "lower",
        "method": "/proc/<pid>/stat utime+stime over a 60 s idle window, summed "
                  "over the serve tree (guests included)",
    },
    "cpu_idle_n5_pct": {
        "metric": "CPU", "label": "idle CPU, N=5",
        "unit": "% of one core", "category": "cpu", "direction": "lower",
        "method": "same 60 s utime+stime window at N=5",
    },
    "cpu_idle_n10_pct": {
        "metric": "CPU", "label": "idle CPU, N=10",
        "unit": "% of one core", "category": "cpu", "direction": "lower",
        "method": "same 60 s utime+stime window at N=10",
    },
    "cpu_idle_n1_per_session_pct": {
        "metric": "CPU", "label": "idle CPU per session, N=1",
        "unit": "% of one core/session", "category": "cpu", "direction": "lower",
        "method": "idle CPU N=1 / 1",
    },
    "cpu_idle_n5_per_session_pct": {
        "metric": "CPU", "label": "idle CPU per session, N=5",
        "unit": "% of one core/session", "category": "cpu", "direction": "lower",
        "method": "idle CPU N=5 / 5",
    },
    "cpu_idle_n10_per_session_pct": {
        "metric": "CPU", "label": "idle CPU per session, N=10",
        "unit": "% of one core/session", "category": "cpu", "direction": "lower",
        "method": "idle CPU N=10 / 10",
    },
    "wakeups_idle_n1_per_s": {
        "metric": "CPU", "label": "idle wakeups/s, N=1",
        "unit": "voluntary ctxt switches/s", "category": "cpu", "direction": "lower",
        "method": "voluntary_ctxt_switches delta over the 60 s idle window, summed "
                  "over the serve tree; proxy for wakeups (perf unavailable)",
    },
    "wakeups_idle_n5_per_s": {
        "metric": "CPU", "label": "idle wakeups/s, N=5",
        "unit": "voluntary ctxt switches/s", "category": "cpu", "direction": "lower",
        "method": "same voluntary_ctxt_switches window at N=5",
    },
    "wakeups_idle_n10_per_s": {
        "metric": "CPU", "label": "idle wakeups/s, N=10",
        "unit": "voluntary ctxt switches/s", "category": "cpu", "direction": "lower",
        "method": "same voluntary_ctxt_switches window at N=10",
    },
    "cpu_active_tap_strict_pct": {
        "metric": "CPU", "label": "active CPU, 20 strict counter taps",
        "unit": "% of one core", "category": "cpu", "direction": "lower",
        "method": "utime+stime delta over the serve tree during the M8 strict "
                  "counter cohort (20 taps, ~20 s)",
    },
    "p11_false_passes": {
        "metric": "false passes", "label": "P11 settle-lab false passes",
        "unit": "count", "category": "count", "direction": "lower",
        "method": "P11 method: strict reply followed immediately by a fast tree and "
                  "a frame-only capture; a settled reply whose state is not final "
                  "is a false pass (0 required)",
    },
    "p11_scenarios_failed": {
        "metric": "false passes", "label": "p11-*.yaml scenarios failed",
        "unit": "count", "category": "count", "direction": "lower",
        "method": "scenarios/p11-*.yaml through acos verify --engine art; every "
                  "expectation:pass scenario must pass (0 required)",
    },
}

# S3c: the corrected (churn-safe) denominator. Same metric shape as
# `_tasks_per_gib_min_net`, but the guest scopes are discovered on creation
# (benchlib.ChurnSampler) instead of once a second.
for _n in (1, 5, 10, 20, 30):
    METRICS["loop_n%d_tasks_per_gib_min_net_s3c" % _n] = {
        "metric": "agent loop",
        "label": "verified tasks/GiB-min, N=%d (anon+shmem, S3c sampler)" % _n,
        "unit": "tasks/GiB-min", "category": "throughput", "direction": "higher",
        "method": "verified tasks / anon+shmem GiB-min integral over the cohort, "
                  "sampled at 20 Hz with guest scopes discovered at creation "
                  "(benchlib.ChurnSampler); unlike the 1 s-discovery sampler this "
                  "one keeps every guest that lives < 1 s (S3c)",
    }

# Old leaderboard (EXPERIMENTS.md before H6) for the .md column. Values are the
# published best p50 and are NOT re-measured here.
PREVIOUS = {
    "m1_idle_mem_mib": "117.5 (D-M2)",
    "m3_cold_start_ms": "463 (P7 warm cache)",
    "m4_launch_strict_ms": "440 (P9)",
    "m4_launch_fast_ms": "184 (P9)",
    "m5_screenshot_ms": "24.4 (P2/W1)",
    "m6_reset_strict_ms": "463 (P9)",
    "m6_reset_fast_ms": "195 (P9)",
    "m7_tree_strict_ms": "151 (P10)",
    "m7_tree_fast_ms": "0.2 (P10)",
    "m8_tap_counter_strict_ms": "955 (P11 event)",
    "m8_tap_counter_fast_ms": "70.9 (P1)",
    "m2_per_session_anon_shmem_n1": "100.9 (O2 @N=5)",
    "m2_per_session_anon_shmem_n10": "101-107 (D-M2/O2)",
    "teardown_ms": "18-20 (GL1)",
    "tap_ack_ms": "5 (P1)",
    "loop_n1_ms": "2210.6 (P5)",
    "loop_n1_tasks_per_s": "0.4219 (P5)",
    "loop_n1_tasks_per_gib_min": "136.8 (P5)",
    "loop_n5_tasks_per_gib_min": "188.0 (P5)",
    "p11_false_passes": "0 (P11)",
    "p11_scenarios_failed": "0 (P11)",
}


# --------------------------------------------------------------------------- #
# Small helpers
# --------------------------------------------------------------------------- #
def utc():
    return benchlib.utc_now()


def load_module(name, path):
    spec = importlib.util.spec_from_file_location(name, str(path))
    mod = importlib.util.module_from_spec(spec)
    sys.modules[name] = mod
    spec.loader.exec_module(mod)
    return mod


def host_now():
    h = {**benchlib.read_meminfo(), **benchlib.read_loadavg(), "utc": utc()}
    h["other_acos"] = other_acos_processes()
    h["noisy"] = bool((h.get("loadavg1") or 0) > 4.0
                      or (h.get("mem_available_gib") or 0) < 20.0)
    return h


def other_acos_processes():
    """Processes that look like another ACOS run (never our own tree)."""
    mine = d2_warm.descendants(os.getpid()) | {os.getpid()}
    out = []
    for name in os.listdir("/proc"):
        if not name.isdigit():
            continue
        pid = int(name)
        if pid in mine:
            continue
        try:
            cmd = Path("/proc/%d/cmdline" % pid).read_bytes().replace(
                b"\0", b" ").decode("utf-8", "replace").strip()
            comm = Path("/proc/%d/comm" % pid).read_text().strip()
        except OSError:
            continue
        if "acos serve" in cmd or "acos-art" in cmd or comm.startswith("dalvikvm64"):
            if "acos-bench" in cmd or "h6" in cmd:
                continue
            out.append({"pid": pid, "comm": comm, "cmd": cmd[:160]})
    return out


def stats(values):
    return benchlib.stats(values)


def median(values):
    vals = [float(v) for v in values if v is not None]
    return statistics.median(vals) if vals else None


def proc_ticks(pid):
    try:
        data = Path("/proc/%d/stat" % int(pid)).read_text()
        rest = data.rsplit(")", 1)[1].split()
        return int(rest[11]) + int(rest[12])  # utime, stime
    except (OSError, ValueError, IndexError):
        return None


def proc_voluntary_ctxt(pid):
    try:
        for line in Path("/proc/%d/status" % int(pid)).read_text().splitlines():
            if line.startswith("voluntary_ctxt_switches:"):
                return int(line.split()[1])
    except (OSError, ValueError, IndexError):
        pass
    return None


def tree_snapshot(root):
    pids = d2_warm.descendants(root) | {int(root)}
    out = {}
    for pid in pids:
        ticks = proc_ticks(pid)
        if ticks is None:
            continue
        out[pid] = {"ticks": ticks, "ctxt": proc_voluntary_ctxt(pid)}
    return out


def tree_delta(before, after, seconds):
    ticks = 0
    wakes = 0
    wake_pids = 0
    for pid, first in before.items():
        second = after.get(pid)
        if second is None:
            continue
        ticks += max(0, second["ticks"] - first["ticks"])
        if first["ctxt"] is not None and second["ctxt"] is not None:
            wakes += max(0, second["ctxt"] - first["ctxt"])
            wake_pids += 1
    return {
        "cpu_s": round(ticks / HZ, 3),
        "cpu_pct": round(100.0 * ticks / HZ / seconds, 3) if seconds else None,
        "wakeups": wakes,
        "wakeups_per_s": round(wakes / seconds, 3) if seconds else None,
        "pids": len(before),
        "wake_pids": wake_pids,
        "seconds": round(seconds, 3),
    }


def recent_net_mib(sampler, seconds=3.0):
    now = time.time()
    vals = [s["net_anon_shmem_mib"] for s in sampler.samples
            if s["t"] >= now - seconds and s.get("net_anon_shmem_mib") is not None]
    return {
        "median_mib": round(median(vals), 3) if vals else None,
        "min_mib": round(min(vals), 3) if vals else None,
        "max_mib": round(max(vals), 3) if vals else None,
        "n": len(vals),
    }


def total_pss_mib(root_pid):
    """sum(Pss) over `root_pid` and every descendant (Pss = shared/mappers)."""
    pids = d2_warm.descendants(root_pid) | {int(root_pid)}
    total = 0.0
    seen = 0
    for pid in pids:
        row = benchlib.pss_of(pid)
        if row:
            total += row[1] / 1024.0
            seen += 1
    return round(total, 3), seen


def session_pss(serve_pid):
    """Per launcher child: sum Pss of that session's whole process tree."""
    parents = d2_warm.ppid_map()
    rows = []
    for child in parents.get(int(serve_pid), []):
        pss, seen = total_pss_mib(child)
        rows.append({"launcher_pid": child, "pss_mib": pss, "pids": seen})
    return rows


def memory_snapshot(serve_pid, sampler):
    net = recent_net_mib(sampler)
    sessions = session_pss(serve_pid)
    total, seen = total_pss_mib(serve_pid)
    return {
        "net_anon_shmem_mib": net["median_mib"],
        "net_min_mib": net["min_mib"],
        "net_max_mib": net["max_mib"],
        "net_samples": net["n"],
        "sum_pss_mib": total,
        "pss_pids": seen,
        "session_pss": sessions,
        "sessions": len(sessions),
        "session_pss_mib": round(sum(r["pss_mib"] for r in sessions), 3),
    }


def host_cpu_times(path="/proc/stat"):
    """Host-wide CPU counters (S3 cohort load record; any-users aggregate)."""
    try:
        parts = Path(path).read_text().splitlines()[0].split()[1:]
        vals = [int(x) for x in parts]
    except (OSError, ValueError, IndexError):
        return None
    idle = vals[3] + (vals[4] if len(vals) > 4 else 0)
    return {"total": sum(vals), "idle": idle}


def host_cpu_delta(before, after):
    if not before or not after:
        return None
    dt = after["total"] - before["total"]
    di = after["idle"] - before["idle"]
    if dt <= 0:
        return None
    return {"busy_pct": round(100.0 * (dt - di) / dt, 3), "ticks": dt}


def loop_cohort_stats(doc, captured, cpu0, cpu1, n):
    """S3: the per-cell block that the merged doc does not carry.

    p50/p90/p95 per task (from the P5 per-task loop_ms), per-session
    anon+shmem and sum Pss sampled during the cohort, and the host CPU%.
    """
    tasks = [t for t in (doc.get("tasks") or [])
             if t.get("verdict") != "warmup"]
    # A failed task is workload evidence, not a latency sample.
    verified_tasks = [t for t in tasks if t.get("verdict") == "pass"]
    vals = [t["loop_ms"] for t in verified_tasks if t.get("loop_ms") is not None]
    st = benchlib.stats(vals)
    out = {
        "n_tasks": len(vals),
        "n_failed_tasks": len(tasks) - len(verified_tasks),
        "ms_p50": st.get("p50"),
        "ms_p90": benchlib.percentile(vals, 90) if vals else None,
        "ms_p95": st.get("p95"),
        "ms_max": st.get("max"),
        "host_cpu_pct": host_cpu_delta(cpu0, cpu1),
        "job_cgroup": captured.get("job_cgroup"),
        "memory": doc.get("memory"),
        "throughput": doc.get("throughput"),
    }
    sampler = captured.get("sampler")
    rows = getattr(sampler, "pss_rows", []) if sampler is not None else []
    samples = getattr(sampler, "samples", []) if sampler is not None else []
    # Restrict the memory statistics to the measured cohort. Before the barrier
    # the sessions do not exist yet, and a phase-wide median would divide the
    # pre-cohort zero samples into the per-session number.
    steps = [st for t in tasks for st in (t.get("steps") or {}).values()
             if st.get("t0_us") and st.get("t1_us")]
    window = ((min(st["t0_us"] for st in steps) / 1e6,
               max(st["t1_us"] for st in steps) / 1e6) if steps else None)
    out["cohort_window_utc_s"] = window
    if window:
        rows = [r for r in rows if window[0] <= r.get("t", 0) <= window[1]]
        samples = [r for r in samples
                   if window[0] <= r.get("t", 0) <= window[1]]
    # S3c: the corrected (churn-safe) reading of the same cohort window.
    churn = captured.get("churn")
    if churn is not None and window:
        expected = sum(1 for name, st in
                       ((name, st) for t in tasks
                        for name, st in (t.get("steps") or {}).items())
                       if name in ("launch", "reset"))
        out["memory_s3c"] = churn.summary(window[0], window[1], sessions=n,
                                          expected_guests=expected)
        out["memory_s3c"]["guests_expected_rule"] = (
            "one guest scope per acos_launch and per acos_reset step in the window")
        if churn.errors:
            out["memory_s3c"]["errors"] = churn.errors[:5]

    def series(items, key):
        return [r[key] for r in items if r.get(key) is not None]

    for key, label, items in (
            ("sum_pss_mib", "sum_pss_mib", rows),
            ("session_pss_mib", "session_pss_mib", rows),
            ("net_anon_shmem_mib", "net_anon_shmem_mib", samples)):
        xs = series(items, key)
        if xs:
            out[label] = {
                "median_mib": round(statistics.median(xs), 3),
                "max_mib": round(max(xs), 3),
                "samples": len(xs),
            }
    seen = series(rows, "sessions")
    if seen:
        out["sessions_seen"] = {"median": statistics.median(seen),
                                "max": max(seen)}
    # The compact per-sample series: a later analysis never has to re-run ART.
    out["pss_series"] = [
        {"t": round(r["t"], 2),
         "sum_pss_mib": round(r["sum_pss_mib"], 1) if r.get("sum_pss_mib") is not None else None,
         "session_pss_mib": round(r["session_pss_mib"], 1) if r.get("session_pss_mib") is not None else None,
         "sessions": r.get("sessions")}
        for r in rows]
    out["anon_series"] = [
        {"t": round(r["t"], 2),
         "net_anon_shmem_mib": round(r["net_anon_shmem_mib"], 1)
         if r.get("net_anon_shmem_mib") is not None else None,
         "job_memory_current_mib": round(r["job_memory_current_mib"], 1)
         if r.get("job_memory_current_mib") is not None else None}
        for r in samples]
    if out.get("session_pss_mib"):
        out["session_pss_per_session_mib"] = round(
            out["session_pss_mib"]["median_mib"] / n, 3)
        out["session_pss_peak_per_session_mib"] = round(
            out["session_pss_mib"]["max_mib"] / n, 3)
    if out.get("net_anon_shmem_mib"):
        out["net_anon_shmem_per_session_mib"] = round(
            out["net_anon_shmem_mib"]["median_mib"] / n, 3)
    if doc.get("memory"):
        peak = doc["memory"].get("net_anon_shmem_peak_mib")
        out["anon_shmem_peak_per_session_mib"] = (
            round(peak / n, 3) if peak else None)
    return out


def tree_of(value):
    return (value or {}).get("tree") if isinstance(value, dict) else None


def nodes_of(tree):
    return list(agent_loop.walk((tree or {}).get("nodes")))


def node_text(tree, short_id):
    node = agent_loop.find_node(tree, short_id)
    return agent_loop.node_text(node).strip() if node else None


def count_text(tree):
    return node_text(tree, "count")


def destroy(d, base, rows, label):
    r = d.call("acos_destroy", {"base": base})
    env = r.get("envelope") or {}
    res = env.get("result") or {}
    teardown = res.get("teardown") or {}
    row = {
        "label": label,
        "ok": bool(env.get("ok")),
        "wall_ms": round(r["wall_ms"], 3),
        "teardown_ms": teardown.get("teardown_ms"),
        "lifecycle": teardown.get("lifecycle"),
        "verified": teardown.get("verified"),
        "survivors": len(teardown.get("survivors") or []),
        "session_leftovers": len(teardown.get("session_leftovers") or []),
        "code": (env.get("error") or {}).get("code") if env.get("error") else None,
    }
    if rows is not None:
        rows.append(row)
    return row


class StepFailures:
    """Every failed launch/reset/observe/step, with its error code.

    A harness-level failure is never allowed to become a latency sample or a
    silent ``ok``. The rows are capped for the JSON, the counters are not.
    """

    def __init__(self, cap=500):
        self.cap = cap
        self.rows = []
        self.total = 0
        self.by_code = {}

    def add(self, task, step, reply=None, code=None, detail=None):
        env = (reply or {}).get("envelope") or {}
        err = env.get("error") or {}
        if code is None:
            code = err.get("code")
        if code is None:
            code = "NO_ERROR_CODE" if not env.get("ok") else "CHECK_FAILED"
        self.total += 1
        self.by_code[code] = self.by_code.get(code, 0) + 1
        if len(self.rows) < self.cap:
            self.rows.append({
                "task": task,
                "step": step,
                "code": code,
                "message": (err.get("message") or "")[:300],
                "wall_ms": round((reply or {}).get("wall_ms") or 0.0, 3),
                "detail": detail,
            })

    def summary(self):
        return {"failed_steps": self.total,
                "failed_by_code": dict(sorted(self.by_code.items())),
                "rows": self.rows}


def add(samples, key, value, ok=True, extra=None):
    if value is None:
        return
    entry = samples.setdefault(key, {"unit": METRICS[key]["unit"], "values": [],
                                     "ok": 0, "bad": 0})
    # A failed task must never contribute a latency sample. For non-latency
    # metrics (rates, memory, failure counts) keep the observation but mark it
    # bad, so a failed run is still visible rather than silently empty.
    if ok or METRICS[key]["category"] != "latency":
        entry["values"].append(round(float(value), 3))
    if ok:
        entry["ok"] += 1
        if extra:
            entry.setdefault("extra", []).append(extra)
    else:
        entry["bad"] += 1
        if len(entry.setdefault("bad_values", [])) < 20:
            entry["bad_values"].append({"value": round(float(value), 3),
                                        "extra": extra})


def finish_samples(samples):
    for key, entry in samples.items():
        entry["stats"] = stats(entry["values"])
        entry["median"] = entry["stats"].get("p50")
        entry["n"] = len(entry["values"])
    return samples


# --------------------------------------------------------------------------- #
# Animation arm (H14). Every launch in this process carries the arm, so a
# measurement is never a mix of sessions started in different modes. The
# default is the product default ("on"); "off" is the agent-facing
# functional-test mode (P12), not a product default change.
# --------------------------------------------------------------------------- #
ARM = {"animations": "on"}


def set_arm(value):
    value = (value or "on").strip().lower()
    if value not in ("on", "off"):
        raise SystemExit("acos-bench: --animations must be on|off (got %r)" % value)
    ARM["animations"] = value
    os.environ["ACOS_BENCH_ANIMATIONS"] = value


def with_arm(tool, args):
    """Attach the animation arm to any launch-shaped request."""
    if tool == "acos_launch":
        args = dict(args)
        args.setdefault("animations", ARM["animations"])
    return args


def launch(d, base, settle, apk=COUNTER, **extra):
    args = {"base": base, "apk": str(apk), "settle": settle}
    args.setdefault("animations", ARM["animations"])
    args.update(extra)
    return d.call("acos_launch", args)


def launch_ok(reply, require_count=True):
    env = reply.get("envelope") or {}
    value = env.get("result") or {}
    tree = tree_of(value)
    good = bool(env.get("ok") and nodes_of(tree))
    if require_count:
        good = good and count_text(tree) == "0"
    return good, value


def new_base(tag, i):
    return {"request_id": "h6-%s-%d" % (tag, i),
            "session_id": "s_h6_%s_%d" % (tag.replace("-", "_"), i)}


# --------------------------------------------------------------------------- #
# Phase: latency (M4/M6/M5/M7/M8/ack/teardown)
# --------------------------------------------------------------------------- #
def phase_latency(args):
    set_arm(getattr(args, "animations", "on"))
    reps = args.reps
    # PERF-REL3: the fast-settle metrics are lattice-quantised, so a rep's p50
    # only says which frame slot holds the middle sample. More samples make the
    # pooled trimmed mean and the slot shares stable; the added wall time is
    # recorded per mode below.
    fast_reps = int(getattr(args, "fast_reps", 0) or 0) or reps
    mode_wall = {}
    scratch = Path(args.scratch)
    if scratch.exists():
        shutil.rmtree(scratch)
    scratch.mkdir(parents=True)
    (scratch / "screens").mkdir()
    samples = {}
    teardowns = []
    checks = {}
    errors = []
    failures = StepFailures()
    serve = None
    d = None
    try:
        serve = d2_warm.Serve(args.acos, scratch, max_sessions=8)
        d = serve.client()
        d.call("ping", {})

        # --- reference frame (P9/P7 rule), also used to prove launch frames ---
        ref = make_reference(d, scratch)
        checks["reference_md5"] = ref["md5"]

        # --- M4: launch -> first correct screen, rep = launch + destroy ------
        for mode in ("strict", "fast"):
            n_mode = fast_reps if mode == "fast" else reps
            t_mode = time.time()
            tag = "m4-%s" % mode
            warm = new_base(tag + "-warm", 0)
            wr = launch(d, warm, mode)
            wok, _wvalue = launch_ok(wr)
            if not wok:
                failures.add("m4-warm", "launch", wr)
            destroy(d, warm, None, "m4-warm")
            for i in range(n_mode):
                base = new_base(tag, i)
                r = launch(d, base, mode)
                ok, value = launch_ok(r)
                settle = (value.get("settle") or {})
                # P6 rule: a later strict read proves the reply's tree.
                vr = d.call("acos_observe_tree", {"base": base, "settle": "strict",
                                                  "max_nodes": 2000})
                vtree = tree_of((vr.get("envelope") or {}).get("result") or {})
                ok = ok and bool((vr.get("envelope") or {}).get("ok"))
                ok = ok and count_text(vtree) == "0"
                # P12: a timed-out settle is not a pass, even when the tree looks
                # right; the reply says settled:false / bound:timeout.
                ok = ok and settle.get("settled") is not False
                if not ok:
                    failures.add("%s-%d" % (tag, i), "launch", r,
                                 detail={"verify_ok": count_text(vtree) == "0",
                                         "settled": settle.get("settled")})
                add(samples, "m4_launch_%s_ms" % mode, r["wall_ms"], ok,
                    {"bound": settle.get("bound"), "settled": settle.get("settled"),
                     "pixels_changed": settle.get("pixels_changed"),
                     "animations": value.get("animations"),
                     "nodes": len(nodes_of(tree_of(value))),
                     "verify_ok": count_text(vtree) == "0"})
                destroy(d, base, teardowns, "m4-%s" % mode)
            mode_wall["m4-%s" % mode] = round(time.time() - t_mode, 3)
            checks["m4_%s" % mode] = sum(
                1 for e in samples.get("m4_launch_%s_ms" % mode, {}).get("extra", [])
                if e.get("verify_ok"))

        # --- M6: reset -> correct screen, strict and fast --------------------
        for mode in ("strict", "fast"):
            n_mode = fast_reps if mode == "fast" else reps
            t_mode = time.time()
            tag = "m6-%s" % mode
            base = new_base(tag, 0)
            # H17 follow-up: this launch used to be ignored, so a failed launch
            # surfaced as 20 fast reset refusals. Check it, and never let any
            # failed step below contribute a latency sample.
            lr = launch(d, base, "strict",
                        apk=Path(getattr(args, "m6_apk", "") or COUNTER))
            lok, _lvalue = launch_ok(lr)
            if not lok:
                failures.add("%s-launch" % tag, "launch", lr)
            for i in range(n_mode + 1):      # rep 0 is the discarded warm-up
                task = "%s-%d" % (tag, i)
                tr = d.call("acos_tap", {"base": base, "id": "inc",
                                         "settle": "fast"})
                tap_ok = bool((tr.get("envelope") or {}).get("ok"))
                if not tap_ok:
                    failures.add(task, "tap", tr)
                br = d.call("acos_observe_tree",
                            {"base": base, "settle": "strict"})
                before = ((br.get("envelope") or {}).get("result") or {}).get("tree")
                before_ok = bool((br.get("envelope") or {}).get("ok"))
                if not before_ok:
                    failures.add(task, "before_tree", br)
                r = d.call("acos_reset", {"base": base, "settle": mode})
                env = r.get("envelope") or {}
                value = env.get("result") or {}
                tree = tree_of(value)
                vr = d.call("acos_observe_tree", {"base": base, "settle": "strict"})
                vtree = tree_of((vr.get("envelope") or {}).get("result") or {})
                verify_ok = bool((vr.get("envelope") or {}).get("ok"))
                if not verify_ok:
                    failures.add(task, "verify_tree", vr)
                settle = value.get("settle") or {}
                ok = bool(env.get("ok") and value.get("state_reset")
                          and value.get("data_cleared") and count_text(tree) == "0"
                          and verify_ok and count_text(vtree) == "0"
                          and count_text(before) not in (None, "0")
                          and settle.get("settled") is not False)
                if not ok:
                    failures.add(task, "reset", r,
                                 detail={"before": count_text(before),
                                         "after": count_text(tree),
                                         "verify_count": count_text(vtree),
                                         "state_reset": value.get("state_reset"),
                                         "data_cleared": value.get("data_cleared"),
                                         "settled": settle.get("settled")})
                if i == 0:
                    continue
                add(samples, "m6_reset_%s_ms" % mode, r["wall_ms"], ok,
                    {"bound": settle.get("bound"),
                     "settled": settle.get("settled"),
                     "pixels_changed": settle.get("pixels_changed"),
                     "animations": value.get("animations"),
                     "before": count_text(before), "after": count_text(tree)})
            mode_wall["m6-%s" % mode] = round(time.time() - t_mode, 3)
            destroy(d, base, teardowns, "m6-%s" % mode)

        # --- M5 / M7 / M8 on one held counter session -----------------------
        base = new_base("counter", 0)
        r = launch(d, base, "strict")
        if not launch_ok(r)[0]:
            raise RuntimeError("counter session did not launch: %s" % (r["envelope"],))

        shots = d2_warm.measure_screenshot(d, base, scratch / "screens", reps, warmup=1)
        for row in shots:
            if row.get("warmup"):
                continue
            if not row.get("good"):
                failures.add("m5-%s" % row.get("tag"), "screenshot",
                             code=row.get("error") or "SCREENSHOT_NOT_GOOD",
                             detail={"fresh": row.get("fresh"),
                                     "meaningful": row.get("meaningful"),
                                     "png_bytes": row.get("png_bytes"),
                                     "png_magic": row.get("png_magic")})
            add(samples, "m5_screenshot_ms", row["ms"], row.get("good"),
                {"png_bytes": row.get("png_bytes")})
        checks["m5_good"] = sum(1 for row in shots if not row.get("warmup")
                                and row.get("good"))

        # M7 strict and fast, with a strict read to expose a stale fast tree.
        for mode in ("strict", "fast"):
            for i in range(reps + 1):
                r = d.call("acos_observe_tree", {"base": base, "settle": mode,
                                                 "max_nodes": 2000})
                env = r.get("envelope") or {}
                value = env.get("result") or {}
                tree = tree_of(value)
                settle = value.get("settle") or {}
                nodes = len(nodes_of(tree))
                own_count = count_text(tree)
                ok = bool(env.get("ok") and nodes > 0
                          and not value.get("truncated")
                          and settle.get("settled") is not False)
                if mode == "fast":
                    vr = d.call("acos_observe_tree", {"base": base,
                                                      "settle": "strict",
                                                      "max_nodes": 2000})
                    vtree = tree_of((vr.get("envelope") or {}).get("result") or {})
                    ok = ok and count_text(vtree) == own_count
                if not ok:
                    failures.add("m7-%s-%d" % (mode, i), "tree", r,
                                 detail={"nodes": nodes, "count": own_count,
                                         "truncated": value.get("truncated"),
                                         "settled": settle.get("settled")})
                if i == 0:
                    continue
                add(samples, "m7_tree_%s_ms" % mode, r["wall_ms"], ok,
                    {"nodes": nodes, "count": own_count,
                     "bound": settle.get("bound"),
                     "settled": settle.get("settled"),
                     "pixels_changed": settle.get("pixels_changed"),
                     "truncated": value.get("truncated")})
            checks["m7_%s_good" % mode] = sum(
                1 for e in samples.get("m7_tree_%s_ms" % mode, {}).get("extra", [])
                if e.get("count") is not None)

        # M8 counter: repeated taps; strict/fast in the same session.
        taps_expected = 0
        for mode in ("strict", "fast"):
            n_mode = fast_reps if mode == "fast" else reps
            t_mode = time.time()
            cpu0 = tree_snapshot(serve.proc.pid) if mode == "strict" else None
            t_cpu0 = time.time() if cpu0 is not None else None
            for i in range(n_mode + 1):
                r = d.call("acos_tap", {"base": base, "id": "inc", "settle": mode})
                env = r.get("envelope") or {}
                settle = (env.get("result") or {}).get("settle") or {}
                # strict replies name the settled frame, fast replies name the
                # state commit; state_changed is only present on the fast path.
                ok = bool(env.get("ok") and settle.get("state_changed") is not False
                          and settle.get("settled") is not False
                          and (settle.get("frame_ms") is not None
                               or settle.get("first_change_ms") is not None))
                taps_expected += 1
                if not ok:
                    failures.add("m8-counter-%s-%d" % (mode, i), "tap", r,
                                 detail={"settled": settle.get("settled"),
                                         "state_changed": settle.get("state_changed"),
                                         "ack_ms": settle.get("ack_ms")})
                if i == 0:
                    continue
                add(samples, "m8_tap_counter_%s_ms" % mode, r["wall_ms"], ok,
                    {"ack_ms": settle.get("ack_ms"),
                     "first_change_ms": settle.get("first_change_ms"),
                     "frame_ms": settle.get("frame_ms"),
                     "bound": settle.get("bound"),
                     "settled": settle.get("settled"),
                     "pixels_changed": settle.get("pixels_changed"),
                     "animations": (env.get("result") or {}).get("animations")})
                if settle.get("ack_ms") is not None:
                    add(samples, "tap_ack_ms", settle.get("ack_ms"), ok)
            mode_wall["m8-counter-%s" % mode] = round(time.time() - t_mode, 3)
            if cpu0 is not None:
                cpu1 = tree_snapshot(serve.proc.pid)
                delta = tree_delta(cpu0, cpu1, time.time() - t_cpu0)
                add(samples, "cpu_active_tap_strict_pct", delta["cpu_pct"],
                    True, delta)

        final = ((d.call("acos_observe_tree",
                         {"base": base, "settle": "strict"})[
            "envelope"].get("result") or {}).get("tree"))
        checks["m8_counter_final_count"] = count_text(final)
        checks["m8_counter_expected_count"] = str(taps_expected)
        destroy(d, base, teardowns, "m8-counter")

        # M8 form: reset (fast, unmeasured) then tap submit; strict post-check.
        base = new_base("form", 0)
        r = launch(d, base, "strict", apk=FORM)
        fok, fvalue = launch_ok(r, require_count=False)
        if not (fok and node_text(tree_of(fvalue), "submit") == "Submit"):
            raise RuntimeError("form session did not launch: %s" % (r["envelope"],))
        for mode in ("strict", "fast"):
            n_mode = fast_reps if mode == "fast" else reps
            t_mode = time.time()
            for i in range(n_mode + 1):
                rr = d.call("acos_reset", {"base": base, "settle": "fast"})
                reset_ok = bool((rr.get("envelope") or {}).get("ok"))
                if not reset_ok:
                    failures.add("m8-form-%s-%d" % (mode, i), "reset", rr)
                r = d.call("acos_tap", {"base": base, "id": "submit",
                                        "settle": mode})
                env = r.get("envelope") or {}
                settle = (env.get("result") or {}).get("settle") or {}
                vr = d.call("acos_observe_tree", {"base": base, "settle": "strict"})
                vtree = tree_of((vr.get("envelope") or {}).get("result") or {})
                ok = bool(reset_ok and env.get("ok")
                          and bool((vr.get("envelope") or {}).get("ok"))
                          and node_text(vtree, "error") == FORM_ERROR
                          and settle.get("settled") is not False)
                if not ok:
                    failures.add("m8-form-%s-%d" % (mode, i), "tap", r,
                                 detail={"reset_ok": reset_ok,
                                         "error": node_text(vtree, "error"),
                                         "settled": settle.get("settled")})
                if i == 0:
                    continue
                add(samples, "m8_tap_form_%s_ms" % mode, r["wall_ms"], ok,
                    {"ack_ms": settle.get("ack_ms"),
                     "first_change_ms": settle.get("first_change_ms"),
                     "frame_ms": settle.get("frame_ms"),
                     "bound": settle.get("bound"),
                     "settled": settle.get("settled"),
                     "pixels_changed": settle.get("pixels_changed"),
                     "error": node_text(vtree, "error")})
                if settle.get("ack_ms") is not None:
                    add(samples, "tap_ack_ms", settle.get("ack_ms"), ok)
            mode_wall["m8-form-%s" % mode] = round(time.time() - t_mode, 3)
        checks["m8_form_error_text"] = node_text(vtree, "error")
        destroy(d, base, teardowns, "m8-form")

        checks["mode_wall_s"] = mode_wall
        checks["reps"] = {"other": reps, "fast": fast_reps}
        checks["teardown_verified"] = sum(1 for row in teardowns if row.get("verified"))
        # A failed destroy reply can carry no lifecycle label; sorting a set that
        # mixes str and None raised TypeError and truncated the phase record.
        checks["teardown_lifecycles"] = sorted(
            {row.get("lifecycle") for row in teardowns if row.get("lifecycle")})
    except Exception as exc:  # noqa: BLE001 -- a phase fault is recorded, never faked
        errors.append("%s: %s" % (type(exc).__name__, exc))
    finally:
        if d is not None:
            try:
                d.close()
            except Exception:  # noqa: BLE001
                pass
        if serve is not None:
            try:
                serve.close()
            except Exception:  # noqa: BLE001
                pass
    for row in teardowns:
        if not row.get("ok"):
            failures.add(row.get("label") or "destroy", "destroy",
                         code=row.get("code") or "DESTROY_FAILED",
                         detail={"lifecycle": row.get("lifecycle"),
                                 "survivors": row.get("survivors"),
                                 "session_leftovers": row.get("session_leftovers")})
    for key in ("teardown_ms", "teardown_wall_ms"):
        values = [row.get("teardown_ms") if key == "teardown_ms"
                  else row.get("wall_ms") for row in teardowns]
        for value in values:
            if value is not None:
                add(samples, key, value, True)
    finish_samples(samples)
    if failures.total:
        errors.append("%d failed launch/reset/step(s) with codes %s"
                      % (failures.total, failures.by_code))
    ok = not errors and not failures.total
    checks["failed_steps"] = failures.total
    checks["failed_by_code"] = dict(failures.by_code)
    return {
        "schema": "acos-bench-phase/2", "phase": "latency", "utc": utc(),
        "acos": args.acos, "binary_sha256": benchlib.sha256_file(args.acos),
        "host_before": host_now(), "host_after": host_now(),
        "env": {"ACOS_STRICT_SETTLE": os.environ.get("ACOS_STRICT_SETTLE", "default"),
                "animations": ARM["animations"]},
        "samples": samples, "teardowns": teardowns, "checks": checks,
        "failures": failures.summary(),
        "ok": ok, "errors": errors,
        "survivors": d2_warm.survivors(scratch) if Path(scratch).exists() else [],
    }


def make_reference(d, scratch):
    """P9/P7 rule: count 0, two identical settled captures, launch frame same."""
    base = new_base("ref", 0)
    r = launch(d, base, "strict", pixels=True)
    ok, value = launch_ok(r)
    if not ok:
        raise RuntimeError("reference launch tree is not count=0: %r" % (r["envelope"],))
    shots = []
    for i in (1, 2):
        path = Path(scratch) / ("ref-%d.png" % i)
        rr = d.call("acos_observe", {"base": base, "tree": False, "png": True,
                                     "screenshot": str(path)})
        shots.append(((rr.get("envelope") or {}).get("result") or {}).get("frame") or {})
        if path.exists():
            path.unlink()
    md5s = [s.get("md5") for s in shots]
    if not md5s[0] or md5s[0] != md5s[1]:
        raise RuntimeError("two consecutive settled captures differ: %s" % md5s)
    launch_md5 = (value.get("frame") or {}).get("md5")
    if launch_md5 != md5s[0]:
        raise RuntimeError("launch frame %s != settled capture %s"
                           % (launch_md5, md5s[0]))
    destroy(d, base, None, "reference")
    return {"md5": md5s[0], "nodes": len(nodes_of(tree_of(value)))}


# --------------------------------------------------------------------------- #
# Phase: memory (M1/M2 + idle CPU/wakeups N=1/5/10)
# --------------------------------------------------------------------------- #
def phase_memory(args):
    set_arm(getattr(args, "animations", "on"))
    scratch = Path(args.scratch)
    if scratch.exists():
        shutil.rmtree(scratch)
    scratch.mkdir(parents=True)
    samples = {}
    teardowns = []
    checks = {}
    errors = []
    serve = None
    d = None
    sampler = None
    try:
        serve = d2_warm.Serve(args.acos, scratch, max_sessions=12)
        d = serve.client()
        d.call("ping", {})
        job = benchlib.cgroup_path(serve.proc.pid)
        if job is None:
            raise RuntimeError("serve is not in a cgroup; cannot measure M1/M2")
        checks["job_cgroup"] = str(job)
        sampler = d2_warm.WarmSampler(job, os.getpid(), interval=0.1)
        sampler.start()
        time.sleep(3.0)
        bare = memory_snapshot(serve.proc.pid, sampler)
        checks["bare"] = bare

        sessions = []
        for n in (1, 5, 10):
            while len(sessions) < n:
                i = len(sessions)
                base = new_base("mem", i)
                r = launch(d, base, "strict")
                ok, _value = launch_ok(r)
                if not ok:
                    raise RuntimeError("session %d did not launch: %s"
                                       % (i, r["envelope"]))
                sessions.append(base)
                time.sleep(0.05)
            time.sleep(10.0)            # first frame + probe settle
            cpu0 = tree_snapshot(serve.proc.pid)
            mems = []
            t0 = time.time()
            while time.time() - t0 < 60.0:
                time.sleep(5.0)
                mems.append(memory_snapshot(serve.proc.pid, sampler))
            cpu1 = tree_snapshot(serve.proc.pid)
            delta = tree_delta(cpu0, cpu1, time.time() - t0)
            net = median([m["net_anon_shmem_mib"] for m in mems])
            pss = median([m["sum_pss_mib"] for m in mems])
            sess_pss = median([m["session_pss_mib"] for m in mems])
            record = {
                "n": n, "net_anon_shmem_mib": net, "sum_pss_mib": pss,
                "session_pss_mib": sess_pss,
                "per_session_anon_shmem_mib": (
                    round((net - bare["net_anon_shmem_mib"]) / n, 3)
                    if net is not None and bare["net_anon_shmem_mib"] is not None
                    else None),
                "per_session_pss_mib": (
                    round(sess_pss / n, 3) if sess_pss is not None else None),
                "bare_pss_mib": bare["sum_pss_mib"],
                "idle": delta,
                "mem_samples": mems,
            }
            checks["n%d" % n] = record
            if n == 1:
                add(samples, "m1_idle_mem_mib", record["net_anon_shmem_mib"],
                    True, {"n": n})
                add(samples, "m1_idle_pss_mib", pss, True)
            add(samples, "m2_per_session_anon_shmem_n%d" % n,
                record["per_session_anon_shmem_mib"], True, {"n": n})
            per_session_pss = (
                round((sess_pss - bare["sum_pss_mib"]) / n, 3)
                if sess_pss is not None and bare["sum_pss_mib"] is not None else None)
            record["per_session_pss_incremental_mib"] = per_session_pss
            add(samples, "m2_per_session_pss_n%d" % n, per_session_pss, True,
                {"n": n, "session_pss_mib": sess_pss})
            add(samples, "cpu_idle_n%d_pct" % n, delta["cpu_pct"], True, delta)
            add(samples, "cpu_idle_n%d_per_session_pct" % n,
                (delta["cpu_pct"] / n) if delta["cpu_pct"] is not None else None,
                True)
            add(samples, "wakeups_idle_n%d_per_s" % n, delta["wakeups_per_s"], True)
        for base in sessions:
            destroy(d, base, teardowns, "memory-n10")
        checks["mode_wall_s"] = mode_wall
        checks["reps"] = {"other": reps, "fast": fast_reps}
        checks["teardown_verified"] = sum(1 for row in teardowns if row.get("verified"))
    except Exception as exc:  # noqa: BLE001
        errors.append("%s: %s" % (type(exc).__name__, exc))
    finally:
        if sampler is not None:
            try:
                sampler.stop()
            except Exception:  # noqa: BLE001
                pass
        if d is not None:
            try:
                d.close()
            except Exception:  # noqa: BLE001
                pass
        if serve is not None:
            try:
                serve.close()
            except Exception:  # noqa: BLE001
                pass
    for row in teardowns:
        if row.get("teardown_ms") is not None:
            add(samples, "teardown_ms", row["teardown_ms"], True)
            add(samples, "teardown_wall_ms", row["wall_ms"], True)
    finish_samples(samples)
    return {
        "schema": "acos-bench-phase/2", "phase": "memory", "utc": utc(),
        "acos": args.acos, "binary_sha256": benchlib.sha256_file(args.acos),
        "host_before": host_now(), "host_after": host_now(),
        "samples": samples, "teardowns": teardowns, "checks": checks,
        "sampler_errors": sampler.errors if sampler else [],
        "ok": not errors, "errors": errors,
        "survivors": d2_warm.survivors(scratch) if Path(scratch).exists() else [],
    }


# --------------------------------------------------------------------------- #
# Phase: P5 agent loop (reuses results/.../P5/p5_loop.py)
# --------------------------------------------------------------------------- #
def phase_loop(args):
    set_arm(getattr(args, "animations", "on"))
    scratch = Path(args.scratch)
    if scratch.exists():
        shutil.rmtree(scratch)
    scratch.mkdir(parents=True)
    out_json = Path(args.scratch) / "loop.json"
    samples = {}
    errors = []
    doc = {}
    # Defined before the `try`: the handler below and the returned document both
    # read it, so a failure inside the bundle must report *that* error instead of
    # dying with `UnboundLocalError: cohort` (found running the SPIKE-NET A/B).
    cohort = {}
    try:
        p5 = load_module("h6_p5_loop", P5_LOOP)
        p5.APK = COUNTER
        # S3: keep a handle on the serve daemon so the per-cell record can carry
        # per-session sum-Pss and anon+shmem (the P5 sampler alone has no Pss).
        captured = {}
        base_serve = p5.Serve

        class CaptureServe(base_serve):
            def __init__(self, *a, **kw):
                super().__init__(*a, **kw)
                captured["serve_pid"] = int(self.proc.pid)
                cg = benchlib.cgroup_path(self.proc.pid)
                captured["job_cgroup"] = str(cg) if cg else None
                # S3c: the corrected sampler is owned by the P5 loop (it now
                # uses ChurnSampler directly); reuse that instance instead of
                # starting a second 10 Hz sampler over the same cohort.
                captured["job_cgroup"] = str(cg) if cg else None

        class PssSampler(p5.WarmSampler):
            def __init__(self, *a, **kw):
                super().__init__(*a, **kw)
                self.pss_rows = []
                captured["sampler"] = self

            def sample_once(self):
                row = super().sample_once()
                # 1 Hz: each snapshot walks smaps_rollup of the serve tree.
                if self.ticks % 10 == 1 and captured.get("serve_pid"):
                    try:
                        snap = memory_snapshot(captured["serve_pid"], self)
                        snap["t"] = row["t"]
                        self.pss_rows.append(snap)
                    except Exception as exc:  # noqa: BLE001
                        self.errors.append("pss: %s: %s"
                                           % (type(exc).__name__, exc))
                return row

        p5.Serve = CaptureServe
        p5.WarmSampler = PssSampler
        cpu0 = host_cpu_times()
        # The P5 harness builds its own launch arguments; give every launch in
        # the loop the bench's animation arm (H14). Wrapping the runner keeps the
        # harness untouched and leaves the arm visible in every phase doc.
        original_call = p5.TaskRunner.call

        def call_with_arm(self, step, tool, args, pngs_before):
            return original_call(self, step, tool, with_arm(tool, args), pngs_before)

        p5.TaskRunner.call = call_with_arm
        loops = args.loops or (22 if args.n == 1 else 6)
        argv = ["--acos", args.acos, "--out", str(out_json),
                "--scratch", str(scratch), "--n", str(args.n),
                "--loops", str(loops), "--warmup", "1",
                "--keep-screens", str(getattr(args, "keep_screens", 20))]
        rc = p5.main(argv)
        captured["churn"] = getattr(p5, "LAST_SAMPLER", None)
        doc = json.loads(out_json.read_text())
        cpu1 = host_cpu_times()
        cohort = loop_cohort_stats(doc, captured, cpu0, cpu1, args.n)
        measured = doc.get("measured_tasks") or 0
        verified = doc.get("verified") or 0
        if rc != 0:
            errors.append("P5 loop exited non-zero (rc=%s); it is fail-closed "
                          "and its failed_tasks carry error codes" % rc)
        if doc.get("false_pass"):
            errors.append("P5 loop reported %s false pass(es)"
                          % doc.get("false_pass"))
        if measured and verified < measured:
            errors.append("verified %d/%d tasks; every failed task is recorded "
                          "in p5.failed_tasks with its error codes"
                          % (verified, measured))
        if doc.get("sampler_errors"):
            errors.append("P5 sampler errors: %s" % (doc.get("sampler_errors")[:3],))
        loop_stats = doc.get("loop_total") or {}
        thr = doc.get("throughput") or {}
        add(samples, "loop_n%d_verified_rate" % args.n,
            (verified / measured) if measured else None,
            bool(measured and verified == measured), {"verified": verified,
                                                      "measured": measured})
        add(samples, "loop_n%d_ms" % args.n, loop_stats.get("p50"), bool(doc.get("ok")),
            {"p90": cohort.get("ms_p90"), "p95": loop_stats.get("p95"),
             "n": loop_stats.get("n"),
             "verified": doc.get("verified"),
             "measured": doc.get("measured_tasks"),
             "false_pass": doc.get("false_pass")})
        add(samples, "loop_n%d_tasks_per_s" % args.n, thr.get("verified_tasks_per_s"),
            bool(doc.get("ok")), thr)
        add(samples, "loop_n%d_tasks_per_gib_min" % args.n,
            thr.get("verified_tasks_per_gib_min"), bool(doc.get("ok")), thr)
        add(samples, "loop_n%d_tasks_per_gib_min_net" % args.n,
            thr.get("verified_tasks_per_gib_min_net"), bool(doc.get("ok")),
            {k: thr.get(k) for k in ("verified_tasks_per_gib_min",
                                     "verified_tasks_per_gib_min_net")})
        # S3c: the corrected denominator (churn-safe sampling). Recorded beside
        # the legacy one, never instead of it.
        s3c = cohort.get("memory_s3c") or {}
        if s3c.get("net_anon_shmem_gib_min"):
            add(samples, "loop_n%d_tasks_per_gib_min_net_s3c" % args.n,
                (verified / s3c["net_anon_shmem_gib_min"] if measured else None),
                bool(doc.get("ok")),
                {k: s3c.get(k) for k in ("net_anon_shmem_gib_min", "coverage",
                                         "guests_observed", "guests_expected",
                                         "samples_per_guest", "sampler_cpu_pct")})
    except Exception as exc:  # noqa: BLE001
        errors.append("%s: %s" % (type(exc).__name__, exc))
    finally:
        if scratch.exists():
            shutil.rmtree(scratch, ignore_errors=True)
    finish_samples(samples)
    return {
        "schema": "acos-bench-phase/2", "phase": "loop%d" % args.n, "utc": utc(),
        "acos": args.acos, "binary_sha256": benchlib.sha256_file(args.acos),
        "host_before": host_now(), "host_after": host_now(),
        "samples": samples, "cohort": cohort,
        "p5": {k: doc.get(k) for k in
                                   ("verified", "measured_tasks", "false_pass",
                                    "throughput", "memory", "loop_total",
                                    "loop_wall_total", "loop", "failed_tasks")},
        "env": {"animations": ARM["animations"], "loops": args.loops or
                (22 if args.n == 1 else 6)},
        "failures": [
            {"agent": task.get("agent"), "index": task.get("index"),
             "pre_text": task.get("pre_text"), "post_text": task.get("post_text"),
             "checks": task.get("checks"),
             "steps": {name: {"ms": step.get("ms"), "ok": step.get("ok"),
                              "status": step.get("status"),
                              "error": step.get("error"),
                              "tree_bytes": step.get("tree_bytes")}
                       for name, step in (task.get("steps") or {}).items()}}
            for task in (doc.get("tasks") or []) if task.get("verdict") == "fail"],
        "failures": doc.get("failed_tasks") or [],
        "ok": bool(not errors and not doc.get("false_pass")
                   and measured > 0 and verified == measured),
        "errors": errors,
    }


# --------------------------------------------------------------------------- #
# Phase: M3 cold start (reuses P7's fadvise eviction and M3 cohort)
# --------------------------------------------------------------------------- #
def phase_cold(args):
    set_arm(getattr(args, "animations", "on"))
    scratch = Path(args.scratch)
    if scratch.exists():
        shutil.rmtree(scratch)
    scratch.mkdir(parents=True)
    samples = {}
    errors = []
    checks = {}
    try:
        p7 = load_module("h6_p7", P7_HARNESS)
        p7.BIN = Path(args.acos)
        p7.ART_BIN = Path(args.acos).with_name("acos-art")
        p7.APK = COUNTER

        class PixelDaemon(p7.Daemon):
            """P7 predates P9's pixel-free launch default: its M3 rule needs the
            launch reply's own frame md5, so ask for pixels explicitly."""

            def call(self, tool, args):
                if tool == "acos_launch":
                    args = dict(args, pixels=True, animations=ARM["animations"])
                return super().call(tool, args)

        p7.Daemon = PixelDaemon
        ref = p7.make_reference(scratch)
        checks["reference_md5"] = ref.get("md5")
        out_json = scratch / "m3-cold.json"
        payload = p7.phase_m3(scratch, args.reps, ref, out_json, drop=True)
        rows = payload.get("rows") or []
        checks["page_cache_dropped"] = payload.get("page_cache_dropped")
        resident_before = []
        resident_after = []
        for row in rows:
            for path, ev in (row.get("fadvise") or {}).items():
                if isinstance(ev, dict) and ev.get("before") is not None:
                    resident_before.append(ev["before"])
                    resident_after.append(ev["after"])
            add(samples, "m3_cold_start_ms", row.get("m3_ms"),
                bool(row.get("ok")), {"daemon_start_ms": row.get("daemon_start_ms")})
        checks["cache_bytes_before"] = sum(resident_before)
        checks["cache_bytes_after"] = sum(resident_after)
        checks["cache_evicted"] = (sum(resident_after) < 0.1 * max(
            1, sum(resident_before))) if resident_before else None
        checks["verified"] = sum(1 for row in rows if row.get("ok"))
    except Exception as exc:  # noqa: BLE001
        errors.append("%s: %s" % (type(exc).__name__, exc))
    finally:
        if scratch.exists():
            shutil.rmtree(scratch, ignore_errors=True)
    finish_samples(samples)
    return {
        "schema": "acos-bench-phase/2", "phase": "cold", "utc": utc(),
        "acos": args.acos, "binary_sha256": benchlib.sha256_file(args.acos),
        "host_before": host_now(), "host_after": host_now(),
        "samples": samples, "checks": checks,
        "ok": not errors, "errors": errors,
    }


# --------------------------------------------------------------------------- #
# Phase: P11 settle-lab false passes (reuses P11's harness)
# --------------------------------------------------------------------------- #
def phase_p11(args):
    set_arm(getattr(args, "animations", "on"))
    scratch = Path(args.scratch)
    if scratch.exists():
        shutil.rmtree(scratch)
    scratch.mkdir(parents=True)
    out_json = scratch / "p11.json"
    samples = {}
    errors = []
    checks = {}
    cases = []
    mode = os.environ.get("ACOS_STRICT_SETTLE") or "event"
    try:
        proc = subprocess.run(
            [sys.executable, str(P11_MEASURE), "--acos", args.acos,
             "--out", str(out_json), "--scratch", str(scratch / "p11-scratch"),
             "--reps", str(max(2, args.p11_reps or args.reps)), "--mode", mode],
            capture_output=True, text=True, timeout=600)
        if not out_json.exists():
            raise RuntimeError("p11 harness wrote no JSON (rc=%d): %s"
                               % (proc.returncode, proc.stderr[-400:]))
        doc = json.loads(out_json.read_text())
        false_passes = 0
        early_renders = 0
        timeouts = 0
        for case in doc.get("cases") or []:
            summary = case.get("summary") or {}
            name = (case.get("case") or {}).get("name")
            row = {"case": name, "summary": summary}
            cases.append(row)
            if "false_passes" in summary:
                false_passes += summary["false_passes"]
                early_renders += summary.get("early_renders") or 0
            if summary.get("timeouts") is not None:
                timeouts += summary.get("timeouts") or 0
        checks["cases"] = cases
        checks["mode"] = mode
        checks["animations"] = "on (P11 harness launches its own sessions; the "
        "animations:off corpus is an open follow-up, see EXPERIMENTS.md E2.9)"
        add(samples, "p11_false_passes", false_passes, false_passes == 0,
            {"early_renders": early_renders, "cases": len(cases)})
        checks["early_renders"] = early_renders
        checks["honest_timeouts"] = timeouts
        checks["harness_rc"] = proc.returncode
        if false_passes:
            errors.append("P11 false passes: %d" % false_passes)
    except Exception as exc:  # noqa: BLE001
        errors.append("%s: %s" % (type(exc).__name__, exc))
    finally:
        if scratch.exists():
            shutil.rmtree(scratch, ignore_errors=True)
    finish_samples(samples)
    return {
        "schema": "acos-bench-phase/2", "phase": "p11", "utc": utc(),
        "acos": args.acos, "binary_sha256": benchlib.sha256_file(args.acos),
        "host_before": host_now(), "host_after": host_now(),
        "samples": samples, "checks": checks,
        "ok": not errors, "errors": errors,
    }


# --------------------------------------------------------------------------- #
# Phase: scenarios/p11-*.yaml sweep through `acos verify`
# --------------------------------------------------------------------------- #
def phase_scenarios(args):
    set_arm(getattr(args, "animations", "on"))
    scratch = Path(args.scratch)
    if scratch.exists():
        shutil.rmtree(scratch)
    scratch.mkdir(parents=True)
    samples = {}
    errors = []
    rows = []
    failed = 0
    try:
        scenarios = sorted((REPO / "scenarios").glob("p11-*.yaml"))
        for scenario in scenarios:
            name = scenario.stem
            text = scenario.read_text()
            expect = "pass"
            for line in text.splitlines():
                if line.startswith("expectation:"):
                    expect = line.split(":", 1)[1].strip()
                    break
            apk = None
            for line in text.splitlines():
                if line.startswith("app:"):
                    apk = (REPO / line.split(":", 1)[1].strip())
                    break
            out_dir = scratch / name
            out_dir.mkdir(parents=True)
            out_json = out_dir / "out.json"
            proc = subprocess.run(
                [args.acos, "verify", "--scenario", str(scenario),
                 "--apk", str(apk), "--engine", "art",
                 "--animations", ARM["animations"],
                 "--artifacts", str(out_dir), "--json"],
                capture_output=True, text=True, timeout=400)
            out_json.write_text(proc.stdout)
            verdict = None
            steps = []
            try:
                payload = json.loads(out_json.read_text())
                block = ((payload.get("result") or {}).get("verdict") or {})
                verdict = block.get("verdict")
                steps = block.get("steps") or []
            except (OSError, ValueError):
                verdict = None
            passed = sum(1 for s in steps if s.get("status") == "pass")
            bad = sum(1 for s in steps if s.get("status") != "pass")
            ok = (proc.returncode == 0 and verdict == "pass" and bad == 0
                  and passed > 0)
            if expect == "non_pass":
                ok = verdict != "pass"
            if not ok:
                failed += 1
            rows.append({"scenario": name, "expectation": expect, "ok": ok,
                         "verdict": verdict, "rc": proc.returncode,
                         "passed": passed, "failed_steps": bad,
                         "total": len(steps)})
        # A scenario failure is a correctness failure, not a false pass.
        add(samples, "p11_scenarios_failed", failed, failed == 0,
            {"scenarios": len(rows)})
    except Exception as exc:  # noqa: BLE001
        errors.append("%s: %s" % (type(exc).__name__, exc))
    finally:
        if scratch.exists():
            shutil.rmtree(scratch, ignore_errors=True)
    finish_samples(samples)
    return {
        "schema": "acos-bench-phase/2", "phase": "scenarios", "utc": utc(),
        "acos": args.acos, "binary_sha256": benchlib.sha256_file(args.acos),
        "host_before": host_now(), "host_after": host_now(),
        "env": {"animations": ARM["animations"]},
        "samples": samples, "scenarios": rows,
        "ok": not errors, "errors": errors,
    }


# --------------------------------------------------------------------------- #
# Merge + thresholds
# --------------------------------------------------------------------------- #
MIN_PCT = {"latency": 5.0, "memory": 3.0, "cpu": 10.0,
           "throughput": 8.0, "count": 0.0}
ABS_FLOOR = {"latency": 2.0, "memory": 0.5, "cpu": 0.2,
             "throughput": 0.0, "count": 0.0}
MAX_PCT = 40.0


def derive_threshold(key, rep_p50s):
    meta = METRICS[key]
    vals = [float(v) for v in rep_p50s if v is not None]
    med = statistics.median(vals) if vals else None
    worst = 0.0
    if med and len(vals) >= 2:
        worst = max(abs(v - med) / med for v in vals)
    pct = max(MIN_PCT[meta["category"]], math.ceil(2.0 * worst * 100.0))
    capped = False
    if pct > MAX_PCT:
        pct = MAX_PCT
        capped = True
    abs_floor = ABS_FLOOR[meta["category"]]
    return {
        "threshold_pct": pct,
        "threshold_abs": abs_floor,
        "rep_deviation_pct": round(100.0 * worst, 2),
        "min_pct": MIN_PCT[meta["category"]],
        "capped": capped,
        "rule": "max(min_pct, 2 x worst rep-to-rep deviation), capped at 40 %; a "
                "regression must exceed both threshold_pct and threshold_abs",
    }


def compare_rows(base, cur):
    rows = []
    for key, meta in METRICS.items():
        b = (base.get("metrics") or {}).get(key)
        c = (cur.get("metrics") or {}).get(key)
        if not b or not c:
            continue
        bv, cv = b.get("p50"), c.get("p50")
        if bv is None or cv is None:
            rows.append({"id": key, "label": meta["label"], "verdict": "missing",
                         "baseline": bv, "current": cv})
            continue
        delta = cv - bv
        pct = (100.0 * delta / bv) if bv else None
        thr_pct = c.get("threshold_pct")
        thr_abs = c.get("threshold_abs")
        worse = (delta > 0) if meta["direction"] == "lower" else (delta < 0)
        beyond = (abs(delta) > max(thr_abs or 0.0,
                                   (thr_pct or 0.0) / 100.0 * abs(bv)))
        if meta["category"] == "count":
            beyond = cv > bv
        if worse and beyond:
            verdict = "REGRESSION"
        elif (not worse) and beyond:
            verdict = "improved"
        else:
            verdict = "within-noise"
        rows.append({
            "id": key, "label": meta["label"], "unit": meta["unit"],
            "baseline": bv, "current": cv, "delta": round(delta, 3),
            "delta_pct": round(pct, 2) if pct is not None else None,
            "threshold_pct": thr_pct, "threshold_abs": thr_abs,
            "verdict": verdict,
        })
    return rows


def fast_lattice(target):
    """PERF-REL3 lattice record for one fast-settle metric (pooled + per rep).

    All reps share one lattice: the period and origin are measured on the pooled
    samples and reused for every rep, so per-rep shares are comparable and the
    A/B tool can compare two runs on the same slots.
    """
    pooled = bench_lattice.analyse(target["values"])
    per_rep = []
    for rep_no, vals in target.get("rep_values") or []:
        row = bench_lattice.analyse(vals, period=pooled["frame_period_ms"],
                                    origin=pooled["origin_ms"])
        st = stats(vals)
        per_rep.append({
            "rep": rep_no, "n": row["n"], "p50": st.get("p50"),
            "trimmed_mean": row["trimmed_mean"], "median": row["median"],
            "first_slot_share": row["first_slot_share"],
            "slot_shares": row["slot_shares"],
        })
    pooled["per_rep"] = per_rep
    return {"lattice": pooled}


def merge(args):
    run_dir = Path(args.run_dir)
    reps = []
    for rep_dir in sorted(run_dir.glob("rep-*")):
        phases = {}
        for path in sorted(rep_dir.glob("*.json")):
            if path.stem in ("meta", "bundle"):
                continue
            try:
                phases[path.stem] = json.loads(path.read_text())
            except (OSError, ValueError):
                continue
        meta_path = rep_dir / "meta.json"
        meta = {}
        if meta_path.exists():
            try:
                meta = json.loads(meta_path.read_text())
            except ValueError:
                meta = {}
        reps.append({"rep": len(reps) + 1, "dir": str(rep_dir),
                     "phases": phases, "meta": meta})
    # Pool the raw values per metric; keep each rep's p50 for the threshold.
    pooled = {}
    LOOP_NET = {"loop1": 1, "loop5": 5, "loop10": 10, "loop20": 20,
                "loop30": 30}
    for rep in reps:
        for phase in rep["phases"].values():
            # The anon+shmem tasks/GiB-min variant lives in the P5 document's
            # throughput block (the harness has always computed it); lift it into
            # the merged metrics when the phase predates the explicit sample.
            n = LOOP_NET.get(phase.get("phase"))
            if n is not None:
                net = ((phase.get("p5") or {}).get("throughput") or {}).get(
                    "verified_tasks_per_gib_min_net")
                key = "loop_n%d_tasks_per_gib_min_net" % n
                if net is not None and key not in (phase.get("samples") or {}):
                    target = pooled.setdefault(key, {"values": [], "rep_p50": [],
                                                     "ok": 0, "bad": 0, "extra": []})
                    target["values"].append(round(float(net), 3))
                    target["rep_p50"].append(round(float(net), 3))
                    target["ok"] += 1
            for key, block in (phase.get("samples") or {}).items():
                if key not in METRICS:
                    continue
                target = pooled.setdefault(key, {"values": [], "rep_p50": [],
                                                 "rep_values": [],
                                                 "ok": 0, "bad": 0, "extra": []})
                raw = block.get("values") or []
                target["values"].extend(raw)
                target["ok"] += block.get("ok") or 0
                target["bad"] += block.get("bad") or 0
                target["extra"].extend(block.get("extra") or [])
                st = stats(raw)
                target["rep_p50"].append(st.get("p50"))
                target["rep_values"].append((rep["rep"], list(raw)))
    metrics = {}
    for key, target in pooled.items():
        meta = METRICS[key]
        st = stats(target["values"])
        thr = derive_threshold(key, target["rep_p50"])
        metrics[key] = {
            **{k: meta[k] for k in ("metric", "label", "unit", "category",
                                    "direction", "method")},
            "p50": st.get("p50"), "p95": st.get("p95"), "mean": st.get("mean"),
            "min": st.get("min"), "max": st.get("max"),
            "n": st.get("n"), "spread_pct": st.get("spread_pct"),
            "ok": target["ok"], "bad": target["bad"],
            "rep_p50": target["rep_p50"],
            # PERF-REL3: raw samples are always kept now, so a stored run can be
            # re-analysed (trimmed mean, lattice) without re-running the bench.
            "samples": [round(float(v), 3) for v in target["values"]],
            **thr,
        }
        if key in FAST_METRICS:
            metrics[key] = {**metrics[key], **fast_lattice(target)}
    # Phase status + host/queue record.
    phase_rows = []
    for rep in reps:
        meta = rep.get("meta") or {}
        groups = meta.get("groups") or {}
        group_of = {}
        for gname, group in groups.items():
            for ph in group.get("phases") or []:
                group_of[ph] = group
        for name, phase in rep["phases"].items():
            group = group_of.get(name, {})
            ok_flag = bool(phase.get("ok"))
            if name in ("loop1", "loop5", "loop10", "loop20", "loop30"):
                p5 = phase.get("p5") or {}
                ok_flag = (not p5.get("false_pass")) and bool(p5.get("measured_tasks"))
            phase_rows.append({
                "rep": rep["rep"], "phase": name,
                "ok": ok_flag,
                "errors": phase.get("errors") or [],
                "bundle": group.get("name"),
                "host_before_loadavg1": (phase.get("host_before") or {}).get("loadavg1"),
                "host_after_loadavg1": (phase.get("host_after") or {}).get("loadavg1"),
                "host_before_mem_avail_gib": (phase.get("host_before") or {}).get(
                    "mem_available_gib"),
                "noisy": bool((phase.get("host_before") or {}).get("noisy")
                              or (phase.get("host_after") or {}).get("noisy")),
                "other_acos": (phase.get("host_before") or {}).get("other_acos") or [],
                "queue_wait_s": (group.get("queue_wait_s")
                                 if group else (meta.get("queue_wait_s") or {}).get(name)),
                "wall_s": (phase.get("bundle_wall_s")
                           if phase.get("bundle_wall_s") is not None
                           else group.get("wall_s")),
            })
    ok = all(row["ok"] for row in phase_rows) if phase_rows else False
    noisy = [row for row in phase_rows if row["noisy"]]
    doc = {
        "schema": "acos-bench-baseline/2",
        "utc": utc(),
        "tool": "tools/bench/acos-bench.sh",
        "commit": benchlib._git(REPO, "rev-parse", "HEAD"),
        "commit_subject": benchlib._git(REPO, "log", "-1", "--format=%s"),
        "branch": benchlib._git(REPO, "rev-parse", "--abbrev-ref", "HEAD"),
        "acos": args.acos,
        "binary_sha256": benchlib.sha256_file(args.acos),
        "profile": benchlib.detect_profile(args.acos),
        "mode": args.mode,
        "reps": len(reps),
        "env": {"ACOS_STRICT_SETTLE": os.environ.get("ACOS_STRICT_SETTLE", "default"),
                "animations": ARM["animations"],
                "ACOS_FONT_DIR": os.environ.get("ACOS_FONT_DIR"),
                "host": "AMD Ryzen 9 9950X, 32 threads, 61908 MiB"},
        "metrics": metrics,
        "phases": phase_rows,
        "noisy_phases": [{"rep": r["rep"], "phase": r["phase"]} for r in noisy],
        "ok": ok,
        "notes": [
            "value = p50 pooled over every sample in every rep; p95 pooled the "
            "same way; per-rep p50s are kept for the threshold derivation.",
            "threshold = max(category floor, 2 x worst rep-to-rep deviation), "
            "capped at 40 %; a delta must exceed both the relative and the "
            "absolute threshold to be called a regression.",
            "memory rows are label-matched (cgroup anon+shmem vs Pss); they are "
            "never mixed.",
            "M3 is measured with the launch working set evicted per rep; the "
            "warm-cache variant is M4 + daemon spawn (P7).",
            "PERF-REL3: every metric now carries its raw `samples`; the "
            "fast-settle metrics (M4 launch fast, M6 reset fast, M8 tap fast) "
            "also carry `lattice`: a 5 %-trimmed mean, the frame period measured "
            "from the samples (phase coherence), and the share of samples per "
            "frame slot (slot = round((x - origin) / period), origin = the "
            "fastest observed sample). p50/p95 and the p50 compare gate are "
            "unchanged; the lattice fields are additive.",
        ],
    }
    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    benchlib.write_json(str(out), doc)
    md = render_markdown(doc)
    if args.markdown:
        Path(args.markdown).write_text(md)
    if args.accept_baseline:
        accepted = Path(args.accept_baseline)
        benchlib.write_json(str(accepted), doc)
    print(md)
    return 0 if ok else 1


def render_markdown(doc):
    lines = []
    lines.append("# ACOS bench baseline -- %s" % doc["utc"])
    lines.append("")
    lines.append("Command: `tools/bench/acos-bench.sh --reps %d` (branch `%s`, commit "
                 "`%s`)." % (doc["reps"], doc.get("branch"), (doc.get("commit") or "")[:12]))
    lines.append("")
    lines.append("Mode `%s`; `ACOS_STRICT_SETTLE=%s`; `animations=%s`; binary "
                 "sha256 `%s`."
                 % (doc["mode"], doc["env"]["ACOS_STRICT_SETTLE"],
                    doc["env"].get("animations", "on"),
                    (doc.get("binary_sha256") or "")[:16]))
    lines.append("")
    lines.append("Host: %s. Noisy phases: %d/%d (load1 > 4 or MemAvailable < 20 GiB; "
                 "recorded, not deleted)." % (doc["env"]["host"],
                                              len(doc["noisy_phases"]),
                                              len(doc["phases"])))
    lines.append("")
    lines.append("## Metrics (p50 / p95, pooled across reps)")
    lines.append("")
    lines.append("| metric | label | p50 | p95 | n | threshold | prev best | method |")
    lines.append("|---|---|---:|---:|---:|---:|---|---|")
    for row in ordered_metrics(doc):
        prev = PREVIOUS.get(row, "-")
        lines.append("| `%s` | %s | %s %s | %s | %d | ±%s%% / %s | %s | %s |" % (
            row, doc["metrics"][row]["label"], fmt(doc["metrics"][row]["p50"]),
            doc["metrics"][row]["unit"], fmt(doc["metrics"][row]["p95"]),
            doc["metrics"][row]["n"], doc["metrics"][row]["threshold_pct"],
            fmt(doc["metrics"][row]["threshold_abs"]), prev,
            doc["metrics"][row]["method"][:110]))
    lines.append("")
    fast = [row for row in ordered_metrics(doc)
            if (doc["metrics"][row].get("lattice") or {}).get("n")]
    if fast:
        lines.append("## Fast-settle lattice (PERF-REL3)")
        lines.append("")
        lines.append("Every fast-settle sample waits for the compositor, so it "
                     "lands on a frame-period lattice: a rep's p50 is whichever "
                     "slot holds the middle sample. These numbers do not flip.")
        lines.append("")
        lines.append("| metric | n | frame ms (source) | median | 5%-trimmed mean "
                     "| first-slot share | slot shares |")
        lines.append("|---|---:|---:|---:|---:|---:|---|")
        for row in fast:
            lat = doc["metrics"][row]["lattice"]
            shares = ", ".join("%s:%.0f%%" % (k, 100.0 * v)
                               for k, v in sorted(lat["slot_shares"].items(),
                                                  key=lambda kv: int(kv[0])))
            lines.append("| `%s` | %d | %.2f (%s) | %s | %s | %s | %s |" % (
                row, lat["n"], lat["frame_period_ms"], lat["frame_period_source"],
                fmt(lat["median"]), fmt(lat["trimmed_mean"]),
                ("%.0f%%" % (100.0 * lat["first_slot_share"])
                 if lat["first_slot_share"] is not None else "-"), shares))
        lines.append("")
    lines.append("## Phases and host load")
    lines.append("")
    lines.append("| rep | phase | ok | load1 before -> after | MemAvailable | "
                 "queue wait s | wall s | noisy |")
    lines.append("|---:|---|---|---:|---:|---:|---:|---|")
    for row in doc["phases"]:
        lines.append("| %d | %s | %s | %s -> %s | %s GiB | %s | %s | %s |" % (
            row["rep"], row["phase"], "yes" if row["ok"] else "**no**",
            fmt(row["host_before_loadavg1"]), fmt(row["host_after_loadavg1"]),
            fmt(row["host_before_mem_avail_gib"]), fmt(row["queue_wait_s"]),
            fmt(row["wall_s"]), "yes" if row["noisy"] else "no"))
    if doc["noisy_phases"]:
        lines.append("")
        lines.append("Noisy phases (recorded): " + ", ".join(
            "%s/rep%d" % (r["phase"], r["rep"]) for r in doc["noisy_phases"]))
    lines.append("")
    lines.append("## Notes")
    lines.append("")
    for note in doc["notes"]:
        lines.append("* %s" % note)
    lines.append("")
    return "\n".join(lines)


def ordered_metrics(doc):
    order = [k for k in METRICS if k in doc["metrics"]]
    return order


def fmt(value):
    if value is None:
        return "-"
    if isinstance(value, float):
        if abs(value) >= 100:
            return "%.1f" % value
        if abs(value) >= 10:
            return "%.2f" % value
        return "%.3f" % value
    return str(value)


def compare(args):
    try:
        base = json.loads(Path(args.compare).read_text())
    except (OSError, ValueError) as exc:
        print("acos-bench --compare: cannot read %s: %s" % (args.compare, exc))
        return 2
    if args.from_run:
        cur = json.loads(Path(args.from_run).read_text())
    else:
        try:
            cur = json.loads(Path(args.out).read_text())
        except (OSError, ValueError) as exc:
            print("acos-bench --compare: no merged run at %s: %s" % (args.out, exc))
            return 2
    rows = compare_rows(base, cur)
    base_arm = (base.get("env") or {}).get("animations", "on")
    cur_arm = (cur.get("env") or {}).get("animations", "on")
    if base_arm != cur_arm:
        print("acos-bench --compare: animation arms differ (baseline %s, current "
              "%s); deltas are an A/B result, not a regression gate."
              % (base_arm, cur_arm))
    print("baseline %s (%s) vs current %s (%s)"
          % (base.get("utc"), (base.get("commit") or "")[:12],
             cur.get("utc"), (cur.get("commit") or "")[:12]))
    print("| metric | baseline | current | delta | delta %% | threshold | verdict |")
    print("|---|---:|---:|---:|---:|---:|---|")
    regressions = []
    for row in rows:
        if row["verdict"] == "REGRESSION":
            regressions.append(row)
        print("| %s | %s | %s | %s | %s | ±%s%% / %s | %s |" % (
            row["label"], fmt(row.get("baseline")), fmt(row.get("current")),
            fmt(row.get("delta")), fmt(row.get("delta_pct")),
            fmt(row.get("threshold_pct")), fmt(row.get("threshold_abs")),
            row["verdict"]))
    missing = [row for row in rows if row["verdict"] == "missing"]
    if missing:
        print("\nnot compared (missing in one run): " + ", ".join(
            row["label"] for row in missing))
    if regressions:
        print("\nREGRESSIONS: %d" % len(regressions))
        for row in regressions:
            print("  - %s: %s -> %s (%s %%)" % (
                row["label"], fmt(row["baseline"]), fmt(row["current"]),
                fmt(row["delta_pct"])))
        return 1
    print("\nno regression beyond the per-metric noise thresholds")
    return 0


# --------------------------------------------------------------------------- #
# One lock acquisition for several phases (keeps the host-wide ART lock short
# while cutting the number of queue waits).
# --------------------------------------------------------------------------- #
def run_bundle(args):
    phases = [p.strip() for p in args.bundle.split(",") if p.strip()]
    if not args.out_dir or not args.scratch_root:
        raise SystemExit("acos-bench: --bundle needs --out-dir and --scratch-root")
    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    handlers = {
        "latency": phase_latency, "memory": phase_memory,
        "loop1": phase_loop, "loop5": phase_loop, "loop10": phase_loop,
        "loop20": phase_loop, "loop30": phase_loop,
        "cold": phase_cold, "p11": phase_p11,
        "scenarios": phase_scenarios,
    }
    bundle = {"schema": "acos-bench-bundle/1", "utc": utc(), "phases": {},
              "host_before": host_now()}
    rc = 0
    for phase in phases:
        if phase not in handlers:
            bundle["phases"][phase] = {"ok": False,
                                       "errors": ["unknown phase %s" % phase]}
            rc = 1
            continue
        sub = argparse.Namespace(**vars(args))
        sub.phase = phase
        sub.scratch = str(Path(args.scratch_root) / phase)
        sub.out = str(out_dir / (phase + ".json"))
        sub.n = 1
        if phase == "loop1":
            sub.n = 1
        elif phase == "loop5":
            sub.n = 5
        elif phase == "loop10":
            sub.n = 10
        elif phase == "loop20":
            sub.n = 20
        elif phase == "loop30":
            sub.n = 30
        t0 = time.time()
        try:
            doc = handlers[phase](sub)
        except Exception as exc:  # noqa: BLE001 -- one phase must not kill the bundle
            doc = {"ok": False, "errors": ["%s: %s" % (type(exc).__name__, exc)]}
        doc.setdefault("schema", "acos-bench-phase/2")
        doc["phase"] = phase
        doc["bundle_wall_s"] = round(time.time() - t0, 3)
        benchlib.write_json(sub.out, doc)
        bundle["phases"][phase] = {"ok": bool(doc.get("ok")),
                                   "errors": doc.get("errors") or [],
                                   "wall_s": doc["bundle_wall_s"]}
        print("acos-bench: bundle phase %s ok=%s wall=%.1fs -> %s"
              % (phase, doc.get("ok"), doc["bundle_wall_s"], sub.out))
        if not doc.get("ok"):
            rc = 1
    bundle["ok"] = rc == 0
    bundle["host_after"] = host_now()
    bundle["wall_s"] = round(sum(p["wall_s"] for p in bundle["phases"].values()), 3)
    benchlib.write_json(str(out_dir / "bundle.json"), bundle)
    return rc


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--phase", choices=["latency", "memory", "loop1", "loop5",
                                        "loop10", "loop20", "loop30", "cold",
                                        "p11", "scenarios"])
    ap.add_argument("--bundle", default="",
                    help="comma-separated phases run in one lock acquisition")
    ap.add_argument("--acos", default=str(REPO / "target/release/acos"))
    ap.add_argument("--scratch", default="")
    ap.add_argument("--out", default="")
    ap.add_argument("--out-dir", default="")
    ap.add_argument("--scratch-root", default="")
    ap.add_argument("--reps", type=int, default=20)
    ap.add_argument("--fast-reps", type=int, default=DEFAULT_FAST_REPS,
                    help="samples per rep for the fast-settle metrics (M4 launch "
                         "fast, M6 reset fast, M8 tap fast; default %d, PERF-REL3)"
                         % DEFAULT_FAST_REPS)
    ap.add_argument("--p11-reps", type=int, default=3)
    ap.add_argument("--keep-screens", type=int, default=20,
                    help="max per-task PNG samples retained by the agent loop "
                         "(default 20; 0 deletes all)")
    ap.add_argument("--m6-apk", default="",
                    help="diagnostic: APK path for the M6 launch only (empty = "
                         "counter); used for fail-closed negative controls")
    ap.add_argument("--loops", type=int, default=0,
                    help="agent-loop iterations (0 = 22 at N=1, 6 at N=5)")
    ap.add_argument("--n", type=int, default=1)
    ap.add_argument("--merge", action="store_true")
    ap.add_argument("--run-dir", default="")
    ap.add_argument("--markdown", default="")
    ap.add_argument("--accept-baseline", default="")
    ap.add_argument("--mode", default="full")
    ap.add_argument("--compare", default="")
    ap.add_argument("--from-run", default="")
    ap.add_argument("--animations", default=os.environ.get("ACOS_BENCH_ANIMATIONS", "on"),
                    choices=["on", "off"],
                    help="animation arm for every launch (H14); default = product default on")
    args = ap.parse_args(argv)
    set_arm(args.animations)
    if args.merge:
        return merge(args)
    if args.compare and not args.phase and not args.bundle:
        return compare(args)
    if args.bundle:
        return run_bundle(args)
    if not args.phase:
        ap.error("either --phase, --bundle or --merge is required")
    if not args.out or not args.scratch:
        ap.error("--out and --scratch are required for a phase")
    if not Path(args.acos).exists():
        raise SystemExit("acos-bench: missing binary %s" % args.acos)
    handler = {
        "latency": phase_latency, "memory": phase_memory,
        "loop1": phase_loop, "loop5": phase_loop, "loop10": phase_loop,
        "loop20": phase_loop, "loop30": phase_loop,
        "cold": phase_cold, "p11": phase_p11,
        "scenarios": phase_scenarios,
    }[args.phase]
    if args.phase in ("loop1", "loop5", "loop10", "loop20", "loop30"):
        args.n = {"loop1": 1, "loop5": 5, "loop10": 10, "loop20": 20,
                  "loop30": 30}[args.phase]
    doc = handler(args)
    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    benchlib.write_json(str(out), doc)
    print("acos-bench: phase %s ok=%s errors=%s -> %s"
          % (args.phase, doc.get("ok"), doc.get("errors"), out))
    return 0 if doc.get("ok") else 1


if __name__ == "__main__":
    sys.exit(main())
