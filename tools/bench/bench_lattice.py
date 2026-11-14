"""Frame-lattice statistics for the fast-settle bench metrics (PERF-REL3).

Every fast-settle sample (M4 launch fast, M6 reset fast, M8 tap fast) lands on
a frame-period lattice: the settle path waits for the compositor, so a value is
`input + k * frame_period` plus a small amount of noise. A rep's p50 is
whichever lattice mode holds the middle sample, so medians of per-rep p50s flip
by a whole frame between reps (REL3-AB.md). This module turns raw samples into
numbers that do not flip:

* `trimmed_mean` -- the 5 %-trimmed mean, the pooled central value;
* `estimate_frame_period` -- the lattice period measured from the samples
  (phase-coherence search), used only when the compositor's vsync period is not
  supplied;
* `slot_shares` -- the share of samples per frame slot,
  `slot = round((x - origin) / frame_period)` with `origin` the fastest observed
  mode (or an explicit shared origin so two arms are comparable);
* `analyse` -- all of the above in one record for the baseline JSON.

Pure stdlib, no bench imports, so the unit tests run anywhere.
"""

import math
import statistics

# Pinned fallback: 60 Hz. Only used when the data has no detectable lattice
# (fewer than 3 distinct values, or a spread under one frame) and no vsync
# period was supplied. The record always says which source was used.
DEFAULT_FRAME_PERIOD_MS = 1000.0 / 60.0

# The old gate floors (formerly in acos_bench.py). Kept here so acos-bench.sh's
# compare and tools/bench/ab-compare.py cannot drift apart.
MIN_PCT = {"latency": 5.0, "memory": 3.0, "cpu": 10.0,
           "throughput": 8.0, "count": 0.0}
ABS_FLOOR = {"latency": 2.0, "memory": 0.5, "cpu": 0.2,
             "throughput": 0.0, "count": 0.0}

# PERF-REL3 lattice gate proposal (see results/handoff/PERF-REL3.md). The
# orchestrator decides adoption; the old p50 gate output is always kept.
LATTICE_GATE_PCT = 5.0
LATTICE_GATE_ABS_MS = 2.0
SLOT_SHIFT_PP = 10.0


def _clean(values):
    out = []
    for value in values or []:
        if value is None:
            continue
        try:
            f = float(value)
        except (TypeError, ValueError):
            continue
        if math.isfinite(f):
            out.append(f)
    return out


def trimmed_mean(values, fraction=0.05):
    """Symmetric trimmed mean; the count removed is floor(n * fraction) per side.

    With n small enough that no sample is removed it equals the plain mean, and
    the caller can see that from the returned `trimmed` count. Returns None for
    an empty input.
    """
    vals = sorted(_clean(values))
    n = len(vals)
    if n == 0:
        return None
    k = int(math.floor(n * float(fraction)))
    if k <= 0:
        return sum(vals) / n
    if 2 * k >= n:                      # never trim everything
        k = max(0, (n - 1) // 2)
    kept = vals[k:n - k] if k else vals
    return sum(kept) / len(kept)


def trimmed_count(values, fraction=0.05):
    n = len(_clean(values))
    k = int(math.floor(n * float(fraction)))
    if 2 * k >= n:
        k = max(0, (n - 1) // 2)
    return k


def estimate_frame_period(values, lo=7.0, hi=40.0, tolerance_ms=1.5,
                          min_share=0.20, min_pairs=5, max_spread_ms=4.0):
    """Measure the lattice spacing (ms) from samples, or None if none is visible.

    Fast-settle values form clusters one frame apart, so the spacing shows up as
    a dominant distance between two samples: take every pairwise difference in
    `[lo, hi]` (within-cluster jitter is far below one frame, so it is excluded),
    histogram them at 1 ms, and keep the busiest bin. The period is the mean of
    the differences within `tolerance_ms` of that bin; the bin's share of the
    candidate differences is the confidence.

    `None` (caller falls back to the pinned 60 Hz period) when the samples do
    not show a lattice: one cluster, a spread under one frame (a rep that landed
    in a single slot -- any period fits), fewer than `min_pairs` candidates, or
    a bin share under `min_share` (a continuum such as M8 form fast, whose
    samples spread over ~5 ms with no gap).

    This deliberately measures only the spacing. It cannot separate one frame
    from two when a sample set covers two clusters two frames apart and nothing
    in between; the record says the value was measured, and the slot shares stay
    comparable as long as both arms use the same measured period.
    """
    vals = _clean(values)
    if len(vals) < 2 or len(set(round(v, 3) for v in vals)) < 3:
        return None
    # A hung launch between two fast ones is a real sample (it lands in a far
    # slot) but must not distort the spacing, so measure on the central 96 %.
    if len(vals) >= 25:
        ordered = sorted(vals)
        k = max(1, int(0.02 * len(ordered)))
        vals = ordered[k:len(ordered) - k]
    if max(vals) - min(vals) < max_spread_ms:
        return None
    diffs = []
    ordered = sorted(vals)
    for i in range(len(ordered)):
        for j in range(i + 1, len(ordered)):
            d = ordered[j] - ordered[i]
            if d < lo:
                continue
            if d > hi:
                break
            diffs.append(d)
    if len(diffs) < min_pairs:
        return None
    bins = {}
    for d in diffs:
        bins[int(math.floor(d))] = bins.get(int(math.floor(d)), 0) + 1
    bin_ms, count = max(bins.items(), key=lambda kv: (kv[1], kv[0]))
    share = count / float(len(diffs))
    if share < min_share:
        return None
    near = [d for d in diffs if abs(d - (bin_ms + 0.5)) <= tolerance_ms]
    if not near:
        return None
    return sum(near) / len(near)


def _slot_index(value, origin, period):
    return int(round((float(value) - float(origin)) / float(period)))


def slot_shares(values, period=None, origin=None):
    """Share of samples per frame slot.

    `slot = round((x - origin) / period)`; slot 0 is the fastest observed mode.
    An explicit `origin` (and `period`) lets the A/B tool score two arms on one
    lattice, which is what makes the shares comparable across arms.
    """
    vals = _clean(values)
    if not vals:
        return {}
    if period is None:
        period = estimate_frame_period(vals) or DEFAULT_FRAME_PERIOD_MS
    if origin is None:
        origin = min(vals)
    counts = {}
    for v in vals:
        k = _slot_index(v, origin, period)
        counts[k] = counts.get(k, 0) + 1
    total = float(len(vals))
    return {k: counts[k] / total for k in sorted(counts)}


def slot_detail(values, period=None, origin=None):
    """Per-slot count/share/min/max plus the first-slot share."""
    vals = _clean(values)
    if not vals:
        return {"origin_ms": None, "frame_period_ms": None, "slots": [],
                "first_slot_share": None}
    if period is None:
        period = estimate_frame_period(vals) or DEFAULT_FRAME_PERIOD_MS
    if origin is None:
        origin = min(vals)
    buckets = {}
    for v in vals:
        buckets.setdefault(_slot_index(v, origin, period), []).append(v)
    total = float(len(vals))
    rows = []
    for k in sorted(buckets):
        group = buckets[k]
        rows.append({
            "slot": k,
            "n": len(group),
            "share": round(len(group) / total, 4),
            "center_ms": round(origin + k * period, 3),
            "min_ms": round(min(group), 3),
            "max_ms": round(max(group), 3),
        })
    return {
        "origin_ms": round(origin, 3),
        "frame_period_ms": round(period, 3),
        "slots": rows,
        "first_slot_share": round(len(buckets.get(0, [])) / total, 4),
    }


def analyse(values, fraction=0.05, vsync_period_ms=None, period=None,
            origin=None):
    """Full lattice record for one metric (additive; never replaces p50/p95)."""
    vals = _clean(values)
    measured = None
    source = "fallback"
    if vsync_period_ms:
        period_used = float(vsync_period_ms)
        source = "compositor"
    elif period:
        period_used = float(period)
        source = "explicit"
    else:
        measured = estimate_frame_period(vals)
        period_used = measured if measured else DEFAULT_FRAME_PERIOD_MS
        source = "measured" if measured else "fallback"
    detail = slot_detail(vals, period=period_used, origin=origin)
    return {
        "n": len(vals),
        "samples": [round(v, 3) for v in vals],
        "frame_period_ms": round(period_used, 3),
        "frame_period_source": source,
        "frame_period_measured_ms": (round(measured, 3) if measured else None),
        "origin_ms": detail["origin_ms"],
        "trimmed_mean": (round(trimmed_mean(vals, fraction), 3)
                         if vals else None),
        "trimmed_mean_pct": round(100.0 * fraction, 2),
        "trimmed_count_per_side": trimmed_count(vals, fraction),
        "median": (round(statistics.median(vals), 3) if vals else None),
        "slots": detail["slots"],
        "slot_shares": {str(row["slot"]): row["share"]
                        for row in detail["slots"]},
        "first_slot_share": detail["first_slot_share"],
    }


def pool_trimmed_mean(values, fraction=0.05):
    """Trimmed mean over pooled samples (thin wrapper for readable call sites)."""
    return trimmed_mean(values, fraction)
