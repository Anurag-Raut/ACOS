#!/usr/bin/env python3
"""benchlib -- the one place ACOS benchmark statistics, noise rules, host gates,
memory-type comparison, provenance and result validation live (W29, 2026-11-12).

Pure standard library. Importable with ``python3 -I`` (callers add the repo's
``tools/`` directory to ``sys.path`` explicitly). Nothing in this module starts
an ART guest, builds anything or touches the network.

The rules encoded here are the decided ones in
``results/art-runtime/ADVISOR-benchmarks-top10.md`` (global decisions table) and
BENCHMARKS.md section 11:

* **spread** = ``(p95 - p5) / p50``; a row with spread > 15 % is ``noisy`` and
  can never PASS.
* **noise** = ``max(spread(acos), spread(legacy), 5 %)``.
* **PASS** iff the two rows are the same metric type *and* state, both have
  enough samples, neither is noisy, and ``acos_p50 <= legacy_p50 * (1 - noise)``.
  A **TIE** (within noise) is **not a pass**. Worse than ``legacy * (1 + noise)``
  is FAIL. Missing data is NOT_MEASURED. A metric-type/state mismatch is
  NOT_COMPARABLE. None of NON-PASS verdicts may be rendered as a win.
* **A/B** is interleaved ABBA, at least 3 pairs, in one host window.
* **host gate**: ``loadavg1 <= 4.0``, ``MemAvailable >= 30 GiB``, and no other
  holder of the host-wide ``art.lock``.
* **memory**: cgroup ``memory.current`` sampled every 100 ms gives the peak and
  the GiB-minute integral; PSS is a secondary, share-aware account; a settle
  time is part of a row's identity. Never add cgroup and PSS. Rows whose types
  or states differ are NOT_COMPARABLE (``compare_memory``).

CLI (convenience only; the test suite is ``tools/tests/test-benchlib.py``):

    python3 tools/bench/benchlib.py stats 1.0 2.0 3.0 ...
    python3 tools/bench/benchlib.py host-gate
    python3 tools/bench/benchlib.py provenance --profile release target/release/acos
"""

from __future__ import annotations

import datetime
import hashlib
import json
import math
import os
import random
import re
import shutil
import statistics
import subprocess
import sys
import threading
import time
from pathlib import Path

# --------------------------------------------------------------------------- #
# Decided constants (do not tune per run; override only with a recorded reason)
# --------------------------------------------------------------------------- #

MAX_SPREAD = 0.15          # spread above this => noisy => cannot PASS
MIN_NOISE = 0.05           # noise floor is never smaller than 5 %
MIN_LATENCY_SAMPLES = 20   # BENCHMARKS.md section 2 rule 2
MIN_MEMORY_REPS = 5        # ADVISOR global decisions: memory reps
MIN_AB_PAIRS = 3           # ADVISOR global decisions: ABBA pairs
BOOTSTRAP_RESAMPLES = 2000
BOOTSTRAP_SEED = 20261112
DEFAULT_LOADAVG1_MAX = 4.0
DEFAULT_MIN_AVAIL_GIB = 30.0
MEMORY_SAMPLE_MS = 100
LEGACY_ANON_SHMEM_SETTLE_S = 60

# Metric types that are allowed to be compared with each other. The string is
# the canonical name; aliases are accepted on input so old JSON keeps working.
METRIC_TYPES = {
    "pss": "PSS",
    "uss": "USS",
    "rss": "RSS",
    "cgroup_anon_shmem": "CGROUP_ANON_SHMEM",
    "cgroup-memory.stat-anon+shmem": "CGROUP_ANON_SHMEM",
    "anon_shmem": "CGROUP_ANON_SHMEM",
    "cgroup_memory_current": "CGROUP_MEMORY_CURRENT",
    "cgroup_peak": "CGROUP_MEMORY_CURRENT",
    "host_memavailable_delta": "HOST_MEMAVAILABLE_DELTA",
    "zram": "ZRAM",
    "swap": "SWAP",
}

STATES = {"first-run", "cold-process", "cold", "warm", "unknown"}


def canonical_metric_type(value):
    """Normalise a metric-type string; unknown values are upper-cased verbatim."""
    if value is None:
        return None
    key = str(value).strip().lower().replace(" ", "")
    return METRIC_TYPES.get(key, str(value).strip().upper())


def canonical_state(value):
    if value is None:
        return None
    key = str(value).strip().lower().replace("_", "-")
    if key in ("firstrun", "first-run"):
        return "first-run"
    if key in ("coldprocess", "cold-process"):
        return "cold-process"
    return key


# --------------------------------------------------------------------------- #
# Statistics
# --------------------------------------------------------------------------- #

def percentile(values, q):
    """Linear-interpolated percentile (same convention as numpy's default).

    ``q`` is 0..100. Returns None for an empty sample. A single sample returns
    itself for every q (the caller must still treat n<2 as "no spread info").
    """
    xs = sorted(float(v) for v in values)
    if not xs:
        return None
    if len(xs) == 1:
        return xs[0]
    if q <= 0:
        return xs[0]
    if q >= 100:
        return xs[-1]
    pos = (len(xs) - 1) * (q / 100.0)
    lo = int(math.floor(pos))
    hi = int(math.ceil(pos))
    if lo == hi:
        return xs[lo]
    frac = pos - lo
    return xs[lo] * (1.0 - frac) + xs[hi] * frac


def _clean(values):
    out = []
    for v in values:
        if v is None:
            continue
        try:
            f = float(v)
        except (TypeError, ValueError):
            continue
        if math.isnan(f) or math.isinf(f):
            continue
        out.append(f)
    return out


def stats(values, resamples=BOOTSTRAP_RESAMPLES, seed=BOOTSTRAP_SEED, ci=0.95):
    """Distribution summary used by every ACOS benchmark row.

    Keys: ``n, min, max, mean, median (== p50), p5, p50, p95, spread, spread_pct,
    stdev, cv, ci95_low, ci95_high, resamples, method``.

    ``spread`` is ``(p95 - p5) / p50`` as a fraction. It is None when n < 2 or
    p50 == 0: with one sample there is no measured spread, and the verdict code
    then refuses to call the row stable. ``ci95`` is a percentile bootstrap of
    the median (2000 resamples, fixed seed => reproducible).
    """
    xs = _clean(values)
    n = len(xs)
    base = {
        "n": n,
        "min": None, "max": None, "mean": None, "median": None,
        "p5": None, "p50": None, "p95": None,
        "spread": None, "spread_pct": None,
        "stdev": None, "cv": None,
        "ci95_low": None, "ci95_high": None,
        "resamples": 0,
        "method": "linear-interpolated percentile; spread=(p95-p5)/p50; "
                  "bootstrap 95% CI of the median",
    }
    if n == 0:
        return base
    base["min"] = min(xs)
    base["max"] = max(xs)
    base["mean"] = sum(xs) / n
    base["p5"] = percentile(xs, 5)
    base["p50"] = base["median"] = percentile(xs, 50)
    base["p95"] = percentile(xs, 95)
    if n >= 2:
        var = sum((x - base["mean"]) ** 2 for x in xs) / (n - 1)
        base["stdev"] = math.sqrt(var)
        if base["mean"]:
            base["cv"] = base["stdev"] / base["mean"]
        if base["p50"]:
            base["spread"] = (base["p95"] - base["p5"]) / base["p50"]
            base["spread_pct"] = 100.0 * base["spread"]
    if n >= 2 and resamples > 0:
        rng = random.Random(seed)
        meds = []
        for _ in range(int(resamples)):
            sample = [xs[rng.randrange(n)] for _ in range(n)]
            sample.sort()
            meds.append(percentile(sample, 50))
        meds.sort()
        tail = (1.0 - ci) / 2.0
        base["ci95_low"] = percentile(meds, 100.0 * tail)
        base["ci95_high"] = percentile(meds, 100.0 * (1.0 - tail))
        base["resamples"] = int(resamples)
    return base


def is_noisy(st, max_spread=MAX_SPREAD):
    """True when the row's spread exceeds the gate (or is unmeasurable)."""
    if not st or st.get("n", 0) < 2:
        return True
    spread = st.get("spread")
    if spread is None:
        return True
    return spread > max_spread


def noise_floor(st_a, st_b, min_noise=MIN_NOISE):
    """``max(spread_a, spread_b, min_noise)`` plus the components for the record."""
    spreads = []
    for st in (st_a, st_b):
        if st and st.get("spread") is not None:
            spreads.append(float(st["spread"]))
    return {
        "noise": max(spreads + [float(min_noise)]),
        "min_noise": float(min_noise),
        "spread_a": spreads[0] if len(spreads) > 0 else None,
        "spread_b": spreads[1] if len(spreads) > 1 else None,
    }


def _as_stats(value):
    if isinstance(value, dict):
        if "p50" in value or "median" in value:
            st = dict(value)
            if st.get("p50") is None:
                st["p50"] = st.get("median")
            if st.get("median") is None:
                st["median"] = st.get("p50")
            if "spread" not in st:
                p5, p95, p50 = st.get("p5"), st.get("p95"), st.get("p50")
                st["spread"] = ((p95 - p5) / p50) if None not in (p5, p95) and p50 else None
            st.setdefault("n", 0)
            return st
        raise ValueError("stats dict has no p50/median")
    return stats(value)


def verdict(acos, legacy, *, metric_type=None, legacy_type=None, state=None,
            legacy_state=None, max_spread=MAX_SPREAD, min_noise=MIN_NOISE,
            min_samples=3, lower_is_better=True):
    """The decided D2 row rule: beat legacy by more than noise, a tie is not a pass.

    ``acos`` and ``legacy`` are either stats dicts (from :func:`stats`) or raw
    sample sequences. Returns a dict with ``verdict`` one of
    ``PASS | TIE | FAIL | NOISY | NOT_COMPARABLE | NOT_MEASURED | INSUFFICIENT``,
    a boolean ``pass`` (True only for PASS), ``ratio`` (acos/legacy), the noise
    used, the threshold and human-readable ``reasons``.

    Order of checks matters and is deliberate: missing data first, then
    comparability, then sample sufficiency, then noise, then the ratio.
    """
    reasons = []
    a = _as_stats(acos) if acos not in (None, [], ()) else None
    b = _as_stats(legacy) if legacy not in (None, [], ()) else None
    out = {
        "verdict": "NOT_MEASURED",
        "pass": False,
        "ratio": None,
        "noise": None,
        "threshold": None,
        "acos_p50": None,
        "legacy_p50": None,
        "reasons": reasons,
        "metric_type": canonical_metric_type(metric_type),
        "legacy_type": canonical_metric_type(legacy_type if legacy_type is not None else metric_type),
        "state": canonical_state(state),
        "legacy_state": canonical_state(legacy_state if legacy_state is not None else state),
    }
    if a is None or a.get("p50") is None:
        reasons.append("ACOS row missing (NOT_MEASURED = not pass)")
    if b is None or b.get("p50") is None:
        reasons.append("legacy row missing (NOT_MEASURED = not pass)")
    if a is not None:
        out["acos_p50"] = a.get("p50")
    if b is not None:
        out["legacy_p50"] = b.get("p50")
    if a is None or b is None or a.get("p50") is None or b.get("p50") is None:
        return out

    if out["metric_type"] != out["legacy_type"]:
        out["verdict"] = "NOT_COMPARABLE"
        reasons.append("metric type mismatch: %s vs %s"
                       % (out["metric_type"], out["legacy_type"]))
        return out
    if out["state"] != out["legacy_state"]:
        out["verdict"] = "NOT_COMPARABLE"
        reasons.append("state mismatch: %s vs %s" % (out["state"], out["legacy_state"]))
        return out
    if a.get("n", 0) < min_samples or b.get("n", 0) < min_samples:
        out["verdict"] = "INSUFFICIENT"
        reasons.append("samples below %d (acos n=%s, legacy n=%s); not a pass"
                       % (min_samples, a.get("n"), b.get("n")))
        return out

    nf = noise_floor(a, b, min_noise)
    noise = float(nf["noise"])
    out["noise"] = noise
    out["noise_components"] = nf
    if is_noisy(a, max_spread):
        out["verdict"] = "NOISY"
        reasons.append("ACOS row noisy: spread %s > %.0f%% gate"
                       % (_fmt_pct(a.get("spread")), 100 * max_spread))
        return out
    if is_noisy(b, max_spread):
        out["verdict"] = "NOISY"
        reasons.append("legacy row noisy: spread %s > %.0f%% gate"
                       % (_fmt_pct(b.get("spread")), 100 * max_spread))
        return out

    p50a, p50b = float(a["p50"]), float(b["p50"])
    if p50b == 0:
        out["verdict"] = "NOT_COMPARABLE"
        reasons.append("legacy p50 is zero")
        return out
    out["ratio"] = p50a / p50b
    if lower_is_better:
        pass_line = p50b * (1.0 - noise)
        tie_hi = p50b * (1.0 + noise)
        out["threshold"] = pass_line
        if p50a <= pass_line:
            out["verdict"] = "PASS"
            out["pass"] = True
            reasons.append("acos p50 %.6g <= legacy %.6g * (1 - noise %.3f) = %.6g"
                           % (p50a, p50b, noise, pass_line))
        elif p50a <= tie_hi:
            out["verdict"] = "TIE"
            reasons.append("acos p50 %.6g is within noise of legacy %.6g (tie = not pass)"
                           % (p50a, p50b))
        else:
            out["verdict"] = "FAIL"
            reasons.append("acos p50 %.6g > legacy %.6g * (1 + noise %.3f) = %.6g"
                           % (p50a, p50b, noise, tie_hi))
    else:  # higher-is-better (throughput)
        pass_line = p50b * (1.0 + noise)
        tie_lo = p50b * (1.0 - noise)
        out["threshold"] = pass_line
        if p50a >= pass_line:
            out["verdict"] = "PASS"
            out["pass"] = True
            reasons.append("acos %.6g >= legacy %.6g * (1 + noise %.3f) = %.6g"
                           % (p50a, p50b, noise, pass_line))
        elif p50a >= tie_lo:
            out["verdict"] = "TIE"
            reasons.append("acos %.6g is within noise of legacy %.6g (tie = not pass)"
                           % (p50a, p50b))
        else:
            out["verdict"] = "FAIL"
            reasons.append("acos %.6g < legacy %.6g * (1 - noise %.3f) = %.6g"
                           % (p50a, p50b, noise, tie_lo))
    return out


def _fmt_pct(value):
    return "n/a" if value is None else "%.1f%%" % (100.0 * float(value))


# --------------------------------------------------------------------------- #
# Interleaved A/B (ABBA)
# --------------------------------------------------------------------------- #

def abba(fn_a, fn_b, pairs=MIN_AB_PAIRS, timer=None, alternate_pairs=False):
    """Run an interleaved A/B experiment: ``A B B A`` per pair.

    Each callable returns its sample (a number, ms by convention) or ``None`` to
    have the helper time the call itself with ``timer`` (default
    ``time.perf_counter``). Returns ``{"rows": [...], "a": stats, "b": stats,
    "pairs": n, "pairs_ok": bool, "order": [...]}``.

    ``pairs < 3`` is allowed for tooling smoke tests but is flagged
    ``pairs_ok: False`` and must not be cited as a verdict.
    With ``alternate_pairs=True`` even pairs run ``B A A B`` so slow drift does
    not favour arm A. The decided default is plain ABBA.
    """
    timer = timer or time.perf_counter
    rows = []
    order = []
    for p in range(1, int(pairs) + 1):
        seq = ["A", "B", "B", "A"]
        if alternate_pairs and p % 2 == 0:
            seq = ["B", "A", "A", "B"]
        order.append("".join(seq))
        for arm in seq:
            fn = fn_a if arm == "A" else fn_b
            t0 = timer()
            value = fn()
            dt = timer() - t0
            if value is None:
                value = dt
            rows.append({"pair": p, "arm": arm, "value": float(value)})
    a_vals = [r["value"] for r in rows if r["arm"] == "A"]
    b_vals = [r["value"] for r in rows if r["arm"] == "B"]
    return {
        "pairs": int(pairs),
        "pairs_ok": int(pairs) >= MIN_AB_PAIRS,
        "order": order,
        "rows": rows,
        "a": stats(a_vals),
        "b": stats(b_vals),
        "note": "ABBA interleaved in one host window; >=3 pairs is required for a verdict",
    }


# --------------------------------------------------------------------------- #
# Host gate
# --------------------------------------------------------------------------- #

def read_loadavg(path="/proc/loadavg"):
    with open(path) as f:
        parts = f.read().split()
    return {"loadavg1": float(parts[0]), "loadavg5": float(parts[1]),
            "loadavg15": float(parts[2]),
            "runnable": parts[3] if len(parts) > 3 else None}


def read_meminfo(path="/proc/meminfo"):
    info = {}
    with open(path) as f:
        for line in f:
            key, _, rest = line.partition(":")
            fields = rest.split()
            if fields:
                info[key] = float(fields[0])  # kB
    total = info.get("MemTotal", 0.0) / 1024.0 / 1024.0
    avail = info.get("MemAvailable", 0.0) / 1024.0 / 1024.0
    used = total - avail
    return {
        "mem_total_mib": round(total * 1024.0, 1),
        "mem_available_mib": round(avail * 1024.0, 1),
        "mem_total_gib": round(total, 2),
        "mem_available_gib": round(avail, 2),
        "used_mib": round(used * 1024.0, 1),
        "used_pct": round(100.0 * used / total, 2) if total else None,
    }


def art_lock_held(lock_path):
    """Best-effort true/false: can we take ``art.lock`` non-blocking right now?

    Uses ``fcntl.flock(LOCK_EX | LOCK_NB)`` on the same file ``run-exclusive.sh``
    flocks. Returns None when the lock file or ``fcntl`` is unavailable (the gate
    then records "unknown" instead of pretending the host is free).
    """
    if not lock_path:
        return None
    path = Path(lock_path)
    if not path.exists():
        return None
    try:
        import fcntl
    except ImportError:  # non-POSIX host
        return None
    try:
        with open(path, "a") as f:
            try:
                fcntl.flock(f.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
            except OSError:
                return True
            else:
                fcntl.flock(f.fileno(), fcntl.LOCK_UN)
                return False
    except OSError:
        return None


def host_gate(repo_root=None, *, loadavg1_max=DEFAULT_LOADAVG1_MAX,
              min_avail_gib=DEFAULT_MIN_AVAIL_GIB, meminfo_path="/proc/meminfo",
              loadavg_path="/proc/loadavg", lock_path=None, check_lock=True):
    """Pre-flight host conditions. Refuses (``ok: False``) on load, RAM or lock.

    The lock check only *probes* the host-wide ``art.lock``; a benchmark that
    wraps itself in ``run-exclusive.sh`` already owns it, so callers inside the
    lock pass ``check_lock=False`` (and then the probe would report "held" by
    itself). Returns the measured values either way so a contaminated run can be
    labelled rather than silently discarded.
    """
    reasons = []
    load = {}
    mem = {}
    try:
        load = read_loadavg(loadavg_path)
    except OSError as exc:
        reasons.append("loadavg unreadable: %s" % exc)
    try:
        mem = read_meminfo(meminfo_path)
    except OSError as exc:
        reasons.append("meminfo unreadable: %s" % exc)
    if load and load["loadavg1"] > float(loadavg1_max):
        reasons.append("loadavg1 %.2f > %.2f" % (load["loadavg1"], loadavg1_max))
    if mem and mem["mem_available_gib"] < float(min_avail_gib):
        reasons.append("MemAvailable %.1f GiB < %.1f GiB"
                       % (mem["mem_available_gib"], min_avail_gib))
    lock = None
    last_run = None
    if repo_root:
        candidate = Path(repo_root) / ".cache/art-locks/art.lock"
        if lock_path is None:
            lock_path = str(candidate)
        log = Path(repo_root) / ".cache/art-locks/runs.log"
        if log.exists():
            try:
                last_run = log.read_text(errors="replace").strip().splitlines()[-1]
            except OSError:
                last_run = None
    if check_lock and lock_path:
        lock = art_lock_held(lock_path)
        if lock:
            reasons.append("another process holds %s" % lock_path)
    return {
        "ok": not reasons,
        "reasons": reasons,
        "loadavg1": load.get("loadavg1"),
        "loadavg5": load.get("loadavg5"),
        "loadavg15": load.get("loadavg15"),
        "mem_available_gib": mem.get("mem_available_gib"),
        "mem_total_gib": mem.get("mem_total_gib"),
        "mem_available_mib": mem.get("mem_available_mib"),
        "art_lock_held": lock,
        "art_lock_checked": bool(check_lock and lock_path),
        "last_art_run": last_run,
        "limits": {"loadavg1_max": float(loadavg1_max),
                   "min_avail_gib": float(min_avail_gib)},
        "utc": utc_now(),
    }


# --------------------------------------------------------------------------- #
# Provenance (build profile, sha256, dirty tree, host)
# --------------------------------------------------------------------------- #

def utc_now():
    return datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds")


def sha256_file(path, chunk=1 << 20):
    try:
        h = hashlib.sha256()
        with open(path, "rb") as f:
            for block in iter(lambda: f.read(chunk), b""):
                h.update(block)
        return h.hexdigest()
    except OSError:
        return None


def detect_profile(binary_path):
    """Declared build profile from the cargo target directory.

    Cargo writes release artefacts under ``target/release`` and debug artefacts
    under ``target/debug`` (custom ``CARGO_TARGET_DIR`` keeps the leaf). The ELF
    debug-info sections are deliberately *not* used: this workspace sets
    ``[profile.release] debug = 1``, so a release binary containing
    ``.debug_info`` is normal and is not evidence of a debug build.
    """
    if not binary_path:
        return "unknown"
    parts = Path(str(binary_path)).parts
    if "release" in parts:
        return "release"
    if "debug" in parts:
        return "debug"
    env = os.environ.get("ACOS_BENCH_PROFILE")
    if env:
        return env.strip().lower()
    return "unknown"


def binary_provenance(path, profile=None):
    """One binary's identity and whether rows measured with it are gateable."""
    p = str(path)
    prof = (profile or detect_profile(p) or "unknown").strip().lower()
    digest = sha256_file(p)
    eligible = prof == "release"
    if prof == "debug":
        why = "debug build: NOT_VERDICT_ELIGIBLE (debug inflates wall time and anon memory)"
    elif prof == "release":
        why = None
    else:
        why = "unknown build profile: NOT_VERDICT_ELIGIBLE"
    return {
        "path": p,
        "exists": os.path.exists(p),
        "profile": prof,
        "sha256": digest,
        "sha256_16": digest[:16] if digest else None,
        "size_bytes": os.path.getsize(p) if os.path.exists(p) else None,
        "mtime_utc": (datetime.datetime.fromtimestamp(
            os.path.getmtime(p), datetime.timezone.utc).isoformat(timespec="seconds")
            if os.path.exists(p) else None),
        "verdict_eligible": eligible,
        "not_eligible_reason": why,
        "profile_source": "explicit" if profile else "path heuristic (%s)" % prof,
    }


def _git(repo_root, *args):
    try:
        r = subprocess.run(["git", *args], cwd=str(repo_root), capture_output=True,
                           text=True, timeout=30)
        return r.stdout.strip()
    except (OSError, subprocess.SubprocessError):
        return ""


def repo_dirty_hash(repo_root):
    """sha256 of ``git status --porcelain`` -- a stable dirty-tree fingerprint."""
    porcelain = _git(repo_root, "status", "--porcelain")
    return hashlib.sha256(porcelain.encode()).hexdigest(), porcelain


def rustc_version():
    try:
        r = subprocess.run(["rustc", "-V"], capture_output=True, text=True, timeout=15)
        return r.stdout.strip() or None
    except (OSError, subprocess.SubprocessError):
        return None


def provenance(repo_root, binaries=(), profile=None, *, gate=None, engine=None,
               extra=None):
    """The provenance block stamped into every result JSON.

    ``profile`` is the build profile the run *claims*; each binary's own
    :func:`binary_provenance` records its detected profile and
    ``verdict_eligible``. If any binary is not release, the whole stamp is
    ``verdict_eligible: false`` and carries a reason (rows are still written, but
    labelled NOT_VERDICT_ELIGIBLE instead of refused, so a smoke test on debug
    binaries stays possible).
    """
    repo_root = Path(repo_root)
    dirty_hash, porcelain = repo_dirty_hash(repo_root)
    bins = [binary_provenance(b, profile=None) for b in binaries]
    if profile is not None:
        for b in bins:
            if b["profile"] == "unknown":
                b["profile"] = str(profile).lower()
                b["verdict_eligible"] = b["profile"] == "release"
                b["not_eligible_reason"] = (None if b["verdict_eligible"]
                                            else "profile %s is not release" % b["profile"])
                b["profile_source"] = "run argument"
    declared = (str(profile).lower() if profile else
                (bins[0]["profile"] if bins else "unknown"))
    blockers = [b for b in bins if not b["verdict_eligible"]]
    if declared != "release":
        eligible = False
        reason = "declared build profile is %s, not release" % declared
    elif blockers:
        eligible = False
        reason = "binary not release: " + ", ".join(b["path"] for b in blockers)
    elif isinstance(gate, dict) and gate.get("ok") is False:
        # A run on a host that failed its own gate is a correctness run: the gate
        # block was *recorded* but used to be ignored here, so a contaminated
        # matrix could still claim `verdict_eligible: true` in its provenance
        # while its summary said otherwise (GATEFIX, 2026-11-17).
        eligible = False
        reason = "host gate refused: " + "; ".join(gate.get("reasons") or ["failed"])
    else:
        eligible = True
        reason = None
    out = {
        "schema": "acos-bench-provenance/1",
        "utc": utc_now(),
        "host": {**read_meminfo(), **read_loadavg(), "nproc": os.cpu_count()},
        "git_head": _git(repo_root, "rev-parse", "HEAD"),
        "git_head_subject": _git(repo_root, "log", "-1", "--format=%s"),
        "git_status_porcelain_sha256": dirty_hash,
        "git_dirty": bool(porcelain.strip()),
        "git_dirty_files": len([l for l in porcelain.splitlines() if l.strip()]),
        "rustc": rustc_version(),
        "profile": declared,
        "verdict_eligible": eligible,
        "not_verdict_eligible_reason": reason,
        "binaries": bins,
        "engine": engine,
        "host_gate": gate,
        "method": "BENCHMARKS.md section 11 (benchlib.py)",
    }
    if extra:
        out.update(extra)
    return out


# --------------------------------------------------------------------------- #
# Memory: cgroup samples, integrals, same-type comparison
# --------------------------------------------------------------------------- #

def sample_integral(samples, value_key="bytes"):
    """Trapezoid integral of a sampled quantity, in GiB-minutes.

    ``samples`` is a list of dicts with ``t`` (epoch seconds) and ``value_key``
    (bytes by default). Non-monotonic timestamps are ignored rather than
    producing a negative integral.
    """
    rows = []
    for s in samples or []:
        t = s.get("t")
        v = s.get(value_key)
        if t is None or v is None:
            continue
        rows.append((float(t), float(v)))
    rows.sort(key=lambda r: r[0])
    gib_min = 0.0
    for (t0, v0), (t1, v1) in zip(rows, rows[1:]):
        dt = t1 - t0
        if dt <= 0:
            continue
        gib_min += ((v0 + v1) / 2.0) / (1024.0 ** 3) * (dt / 60.0)
    return gib_min


def sample_peak(samples, value_key="bytes"):
    vals = [float(s[value_key]) for s in samples or []
            if s.get(value_key) is not None]
    return max(vals) if vals else None


def memory_row(row):
    """Extract the canonical fields of one memory measurement row."""
    row = row or {}
    return {
        "value_mib": row.get("value_mib", row.get("mib")),
        "metric_type": canonical_metric_type(row.get("metric_type", row.get("type"))),
        "state": canonical_state(row.get("state")),
        "settle_s": row.get("settle_s"),
        "source": row.get("source"),
        "n": row.get("n"),
    }


def compare_memory(acos_row, legacy_row):
    """Same-type memory comparison; a mismatch is NOT_COMPARABLE, never a pass.

    Rules (ADVISOR global decisions, item 6): the only column compared with the
    legacy ACVM section 3 values is per-session cgroup ``memory.stat`` anon+shmem
    settled 60 s after first frame. cgroup peak, PSS and GiB-min are separate
    columns and are never compared across types. A row whose ``metric_type`` or
    ``state`` differs renders NOT_COMPARABLE (not-pass).
    """
    a = memory_row(acos_row)
    b = memory_row(legacy_row)
    reasons = []
    comparable = True
    if a["value_mib"] is None or b["value_mib"] is None:
        comparable = False
        reasons.append("value missing")
    if a["metric_type"] != b["metric_type"]:
        comparable = False
        reasons.append("metric type %s != %s" % (a["metric_type"], b["metric_type"]))
    if a["state"] != b["state"]:
        comparable = False
        reasons.append("state %s != %s" % (a["state"], b["state"]))
    if (a["metric_type"] == "CGROUP_ANON_SHMEM"
            and (a["settle_s"] != b["settle_s"])):
        comparable = False
        reasons.append("settle time %s s != %s s" % (a["settle_s"], b["settle_s"]))
    ratio = (float(a["value_mib"]) / float(b["value_mib"])
             if comparable and float(b["value_mib"]) else None)
    return {
        "verdict": "COMPARABLE" if comparable else "NOT_COMPARABLE",
        "comparable": comparable,
        "ratio": ratio,
        "acos": a,
        "legacy": b,
        "reasons": reasons,
        "note": "never sum cgroup and PSS; cgroup anon+shmem is the legacy-comparable column",
    }


def legacy_memory_reference(mib=130.3):
    """The frozen legacy ACVM per-session reference (BENCHMARKS.md section 3)."""
    return {"value_mib": mib, "metric_type": "CGROUP_ANON_SHMEM",
            "state": "warm", "settle_s": LEGACY_ANON_SHMEM_SETTLE_S,
            "source": "BENCHMARKS.md section 3 (legacy ACVM N=1), reference only"}


# --------------------------------------------------------------------------- #
# Failure taxonomy (item 10 groundwork; matrix rows carry class/retryable/hint)
# --------------------------------------------------------------------------- #

_CLASS_BY_CODE = {
    # app / assertion behaviour.
    #
    # This block is `acos_protocol::ErrorCode::default_status() == Status::Fail`
    # verbatim: an assertion outcome is the app's or the expectation's answer, so
    # an expected negative that fails with one of them is a `correct_negative`,
    # never an unclassified row. `ELEMENT_OFFSCREEN` was missing here, which made
    # two correct matrix negatives (`form-wrong-error`, `offscreen-pager-visible`)
    # `unclassified` and the whole release-gate matrix INVALID at
    # `false_pass = 0` (v0.3.0 gate F2). `tools/tests/test-benchlib.py`
    # cross-checks this block against the protocol, so a new assertion code cannot
    # silently become `unknown` again.
    "ASSERTION_FAILED": "assertion",
    "ELEMENT_NOT_FOUND": "assertion",
    "ELEMENT_OFFSCREEN": "assertion",
    "AMBIGUOUS_SELECTOR": "assertion",
    "TREE_TRUNCATED": "assertion",
    "END_OF_LIST": "assertion",
    "INPUT_NOT_HANDLED": "assertion",
    "STALE_REF": "assertion",
    "WRONG_TEXT": "assertion",
    "WRONG_BOUNDS": "assertion",
    "APP_CRASHED": "app_crash",
    "APP_NOT_RESPONDING": "app_crash",
    "JAVA_EXCEPTION": "app_crash",
    "INSUFFICIENT_HOST_EVIDENCE": "assertion",
    # capability gaps (never a pass)
    "UNSUPPORTED_ASSERTION": "unsupported",
    "UNSUPPORTED_BACKEND": "unsupported",
    "UNSUPPORTED_ANDROID_API": "unsupported",
    "UNSUPPORTED_JAVA_API": "unsupported",
    "UNSUPPORTED_BYTECODE": "unsupported",
    "UNSUPPORTED_NATIVE_SYMBOL": "unsupported",
    "UNSUPPORTED_ABI": "unsupported",
    "UNSUPPORTED_RELOCATION": "unsupported",
    "UNSUPPORTED_RN": "unsupported",
    "UNSUPPORTED_RN_SETUP": "unsupported",
    "UNSUPPORTED_RN_DRIVER": "unsupported",
    "UNSUPPORTED_RN_NO_RENDERER": "unsupported",
    "UNSUPPORTED_RN_KEY": "unsupported",
    "UNSUPPORTED_RN_TEXT": "unsupported",
    "UNSUPPORTED_RN_NO_TARGET": "unsupported",
    "NO_DISPLAY": "unsupported",
    "NO_TEXT": "unsupported",
    "NEEDS_REAL_DEVICE": "unsupported",
    "REQUIRES_GMS": "unsupported",
    "WRONG_ABI": "unsupported",
    "UNSUPPORTED_ASSERTION_KIND": "unsupported",
    # timeouts / resources.
    #
    # `TIMEOUT` is deliberately *not* in the static table: a deadline that a
    # scenario's own step hit (a `wait_for`/settle deadline, reported by the
    # engine while the run was still alive) is a real non-pass of that scenario,
    # while the harness's own `--timeout-s` kill is an infrastructure timeout.
    # `classify_error(..., deadline=...)` is the one place that decides, and the
    # matrix runner passes `deadline="harness"` only for its own rc=124 kill
    # (SCENHYG: three designed TIMEOUT negatives were counted `infra_timeout`,
    # so `negative_all_correct` could never be true and the matrix carried a
    # permanent infrastructure ambiguity).
    "RESOURCE_LIMIT": "infra_timeout",
    "RAM_GUARD": "host_guard",
    "RAM_ABORT": "host_guard",
    "CONFINEMENT_UNAVAILABLE": "host_guard",
    "LANDLOCK_UNAVAILABLE": "host_guard",
    # harness (ACOS bug or bad request/scenario/artifact)
    "INTERNAL": "harness",
    "VM_INTERNAL": "harness",
    "INVALID_SCENARIO": "harness",
    "INVALID_ARGUMENT": "harness",
    "UNSAFE_PATH": "harness",
    "ZIP_SLIP": "harness",
    "ARTIFACTS_INVALID": "harness",
    "UNVERIFIED_FIDELITY": "harness",
    "NO_FRESH_FRAME": "harness",
    "SESSION_NOT_FOUND": "harness",
    "SESSION_BUSY": "harness",
    # W55: the guest process ended (launcher death / reset failure). The
    # protocol's own `ErrorCode::GuestExited.class()` is `Harness`; without this
    # entry benchlib counted the same row as `unknown` and failed the matrix
    # gate, which is a taxonomy gap, not an app verdict.
    "GUEST_EXITED": "harness",
    "ISOLATION_UNAVAILABLE": "harness",
    "ISOLATION_UNSATISFIED": "harness",
    "UNTRUSTED_APK_NOT_CERTIFIED": "harness",
}

# A step deadline is retryable for the same reason an infrastructure timeout is:
# one retry separates "the host was slow" from "the expectation is wrong". A
# negative that *declares* the timeout is a correct negative, not a retry.
_RETRYABLE = {"infra_timeout", "step_timeout", "host_guard"}


def classify_error(code, message=None, retryable=None, hint=None, deadline="step"):
    """Map an ``acos_protocol::ErrorCode`` string to the matrix failure taxonomy.

    Returns ``{code, class, retryable, hint}`` with ``class`` one of
    ``assertion | app_crash | unsupported | harness | infra_timeout |
    step_timeout | host_guard`` and ``unknown`` when the code is not in the table
    (the matrix gate requires ``unclassified == 0``).

    ``deadline`` says whose deadline a ``TIMEOUT`` was:

    * ``"step"`` (default) — the scenario's own step/wait/settle deadline, which
      is a real non-pass of the run; class ``step_timeout``;
    * ``"harness"`` — the runner killed the run at its own wall-clock limit;
      class ``infra_timeout`` (never a verdict about the app).
    """
    c = (code or "UNKNOWN").strip().upper()
    if c == "TIMEOUT":
        cls = "infra_timeout" if deadline == "harness" else "step_timeout"
    else:
        cls = _CLASS_BY_CODE.get(c, "unknown")
    if retryable is None:
        retryable = cls in _RETRYABLE
    if hint is None:
        hint = _default_hint(c, cls)
    return {"code": c, "class": cls, "retryable": bool(retryable),
            "hint": hint, "message": message}


def _default_hint(code, cls):
    if cls == "assertion":
        return ("The app ran but the asserted state differs; re-observe the tree and fix "
                "the expectation or the app. Not retryable.")
    if cls == "app_crash":
        return ("The app crashed; capture the guest stderr tail and the frame/tree at "
                "failure. Not retryable without an app fix.")
    if cls == "unsupported":
        return ("The engine lacks this capability; report it as unsupported, never as a "
                "pass. Not retryable on this engine.")
    if cls == "step_timeout":
        return ("The scenario's own step deadline elapsed (a wait/settle the run declared). "
                "That is a non-pass of the scenario, not a harness failure; retry once to "
                "separate a slow host from a wrong expectation.")
    if cls == "infra_timeout":
        return "The harness wall clock or a resource budget killed the run; retry once and record the retry cost."
    if cls == "host_guard":
        return "The host guard fired (RAM/confinement); free memory and retry."
    if cls == "harness":
        return ("Harness/ACOS-side failure; keep the evidence bundle and report it as a "
                "harness item (counts against the <=2% harness gate).")
    return ("Unclassified error code %s: add it to tools/bench/benchlib.py classify_error; the "
            "matrix gate requires unclassified == 0." % code)


# --------------------------------------------------------------------------- #
# Result-schema validation (results/bench/schema/matrix.schema.json)
# --------------------------------------------------------------------------- #

MATRIX_SCHEMA = "acos-bench-matrix/1"

_REQUIRED_RUN_FIELDS = {
    "run_id": str,
    "utc": str,
    "scenario": str,
    "variant": str,
    "expected": str,
    "verdict": str,
    "false_pass": bool,
    "state": str,
    "n_concurrent": int,
    "wall_ms": (int, float),
    "mem": dict,
    "host": dict,
    "profile": str,
    "engine": str,
    "verdict_eligible": bool,
    "error": (dict, type(None)),
}


def validate_matrix(obj):
    """Validate a matrix result object against the section-11 schema.

    Returns a list of human-readable errors (empty == valid). Deliberately a
    small hand-written validator: no third-party dependency, and it can be
    called by the runner before it writes the file. The JSON Schema in
    ``results/bench/schema/matrix.schema.json`` is the documentation twin.
    """
    errors = []
    if not isinstance(obj, dict):
        return ["result is not a JSON object"]
    if obj.get("schema") != MATRIX_SCHEMA:
        errors.append("schema must be %r (got %r)" % (MATRIX_SCHEMA, obj.get("schema")))
    prov = obj.get("provenance")
    if not isinstance(prov, dict):
        errors.append("provenance missing")
    else:
        for key in ("utc", "git_head", "profile", "verdict_eligible", "binaries"):
            if key not in prov:
                errors.append("provenance.%s missing" % key)
    rows = obj.get("runs")
    if not isinstance(rows, list) or not rows:
        errors.append("runs must be a non-empty list")
        return errors
    seen = set()
    for i, row in enumerate(rows):
        where = "runs[%d]" % i
        if not isinstance(row, dict):
            errors.append("%s is not an object" % where)
            continue
        for key, typ in _REQUIRED_RUN_FIELDS.items():
            if key not in row:
                errors.append("%s.%s missing" % (where, key))
                continue
            if not isinstance(row[key], typ):
                errors.append("%s.%s has type %s (want %s)"
                              % (where, key, type(row[key]).__name__, typ))
        if row.get("run_id") in seen:
            errors.append("%s.run_id duplicated: %s" % (where, row.get("run_id")))
        seen.add(row.get("run_id"))
        if row.get("variant") not in ("good", "wrong", "negative"):
            errors.append("%s.variant must be good|wrong|negative" % where)
        if row.get("expected") not in ("pass", "fail"):
            errors.append("%s.expected must be pass|fail" % where)
        if row.get("verdict") not in ("pass", "fail", "error"):
            errors.append("%s.verdict must be pass|fail|error" % where)
        if row.get("state") not in STATES:
            errors.append("%s.state not one of %s" % (where, sorted(STATES)))
        mem = row.get("mem")
        if isinstance(mem, dict):
            for key in ("cgroup_peak_mib", "cgroup_gib_min", "pss_mib",
                        "anon_shmem_settled_mib"):
                if key not in mem:
                    errors.append("%s.mem.%s missing" % (where, key))
            if mem.get("metric_type") is None:
                errors.append("%s.mem.metric_type missing" % where)
        host = row.get("host")
        if isinstance(host, dict):
            for key in ("loadavg1", "mem_available_mib"):
                if key not in host:
                    errors.append("%s.host.%s missing" % (where, key))
        err = row.get("error")
        if isinstance(err, dict):
            for key in ("code", "class", "retryable", "hint"):
                if key not in err:
                    errors.append("%s.error.%s missing" % (where, key))
            if err.get("class") not in ("assertion", "app_crash", "unsupported",
                                        "harness", "infra_timeout", "step_timeout",
                                        "host_guard", "unknown"):
                errors.append("%s.error.class invalid: %r" % (where, err.get("class")))
        # negative-scenario column must be present on every row
        if "negative" not in row:
            errors.append("%s.negative missing (negative-scenario column)" % where)
    summary = obj.get("summary")
    if not isinstance(summary, dict):
        errors.append("summary missing")
    else:
        for key in ("total_runs", "passed", "failed", "false_pass",
                    "negative_runs", "unclassified"):
            if key not in summary:
                errors.append("summary.%s missing" % key)
        if isinstance(summary.get("unclassified"), int) and summary["unclassified"] != 0:
            errors.append("summary.unclassified=%d (gate requires 0)"
                          % summary["unclassified"])
        if isinstance(summary.get("false_pass"), int) and summary["false_pass"] > 0:
            errors.append("summary.false_pass=%d (gate requires 0)"
                          % summary["false_pass"])
    return errors


def write_json(path, obj, indent=2):
    """Write JSON via a temporary file then rename (no half-written results)."""
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + ".tmp")
    with open(tmp, "w") as f:
        json.dump(obj, f, indent=indent, sort_keys=True)
        f.write("\n")
    os.replace(tmp, path)
    return str(path)


# --------------------------------------------------------------------------- #
# Cgroup scope sampling for one variant (W45: the trustworthy M11 denominator)
# --------------------------------------------------------------------------- #
# W41 found three measurement defects in the suite denominator: (1) `acos-art`
# creates the guest in a *sibling* systemd scope, so the runner's cgroup does not
# contain it at all; (2) shared read-only page cache is charged to whichever
# cgroup faults it in first, so the same binary swings >1.7x; (3) the suite used
# one scope for all 22 variants, so every variant's integral was charged the
# suite's cache high-water mark. The functions below implement the decided fix:
#
#   * one fresh scope per variant run (`guarded.sh` per run), the sampler runs
#     *inside* that scope and resolves its own cgroup;
#   * the job cgroup AND every guest cgroup the launcher creates are sampled
#     (evidence file `<run>/cgroup` plus a ppid-chain descendant scan, so other
#     bwrap users on the host are never charged);
#   * 100 ms samples with an explicit closing sample after the child exits;
#   * the sampler's own process is charged out (`sampler.excluded: true`) with
#     its RSS and the window recorded;
#   * the primary denominator (the only one that gates M11) is same-type with
#     the legacy numbers the 24.4 bar was derived from: cgroup memory.stat
#     anon+shmem. `resident` (anon+shmem+kernel), `job_all` (memory.current,
#     includes file) and the old runner formula are reported alongside, each
#     labelled, and the shared read-only page cache is reported separately as
#     `infra_file_cache` (never charged to a task).

CGROUP_ROOT = Path("/sys/fs/cgroup")
PROC_ROOT = Path("/proc")

MEMSTAT_KEYS = ("anon", "file", "shmem", "kernel", "kernel_stack", "sock",
                "slab", "pagetables", "pgfault", "pgmajfault",
                "workingset_refault_file")
PRIMARY_KEYS = ("anon", "shmem")          # same type as the legacy numbers
RESIDENT_KEYS = ("anon", "shmem", "kernel")
CACHE_KEYS = ("file",)

PRIMARY_METRIC_TYPE = "CGROUP_ANON_SHMEM"
JOB_ALL_METRIC_TYPE = "CGROUP_MEMORY_CURRENT"
LEGACY_DERIVED_BAR = 24.4                   # tasks/GiB.min (ADVISOR item 3)
MIB = 1 << 20

def cgroup_path(pid=None, proc_root=None, cgroup_root=None):
    """Resolve a process's cgroup v2 path; None when it is not in a leaf scope."""
    proc_root = Path(proc_root or PROC_ROOT)
    cgroup_root = Path(cgroup_root or CGROUP_ROOT)
    try:
        lines = (proc_root / str(pid or "self") / "cgroup").read_text().splitlines()
    except OSError:
        return None
    for line in lines:
        if line.startswith("0::"):
            rel = line[3:].strip()
            if not rel or rel == "/":
                return None
            return cgroup_root / rel.lstrip("/")
    return None

def cgroup_has_limit(cg):
    """True when the cgroup has a finite memory.max (i.e. a guarded scope)."""
    try:
        mx = (Path(cg) / "memory.max").read_text().strip()
    except OSError:
        return False
    return bool(mx) and mx != "max"

def read_cgroup_int(cg, name):
    try:
        return int((Path(cg) / name).read_text().strip())
    except (OSError, ValueError):
        return None

def read_memstat(cg, keys=MEMSTAT_KEYS):
    out = {}
    try:
        for line in (Path(cg) / "memory.stat").read_text().splitlines():
            k, _, v = line.partition(" ")
            if k in keys:
                try:
                    out[k] = int(v)
                except ValueError:
                    pass
    except OSError:
        return None
    return out

def cgroup_from_info_file(path, cgroup_root=None):
    """Resolve the launcher's `<run>/cgroup` evidence file to a cgroup path."""
    cgroup_root = Path(cgroup_root or CGROUP_ROOT)
    try:
        for line in Path(path).read_text().splitlines():
            if line.startswith("0::"):
                rel = line[3:].strip()
                if rel and rel != "/":
                    return cgroup_root / rel.lstrip("/")
    except OSError:
        pass
    return None

def procs_in_cgroup(cg, proc_root=None):
    """(pid, comm) for the processes directly in a cgroup."""
    proc_root = Path(proc_root or PROC_ROOT)
    out = []
    try:
        pids = [int(x) for x in (Path(cg) / "cgroup.procs").read_text().split()]
    except (OSError, ValueError):
        return out
    for pid in pids:
        try:
            comm = (proc_root / str(pid) / "comm").read_text().strip()
        except OSError:
            comm = "?"
        out.append((pid, comm))
    return out

def pss_of(pid, proc_root=None):
    """(comm, Pss_kb, Rss_kb) from smaps_rollup (fallback /proc/<pid>/status)."""
    proc_root = Path(proc_root or PROC_ROOT)
    try:
        comm = (proc_root / str(pid) / "comm").read_text().strip()
    except OSError:
        return None
    pss = rss = None
    try:
        for line in (proc_root / str(pid) / "smaps_rollup").read_text().splitlines():
            if line.startswith("Pss:"):
                pss = int(line.split()[1])
            elif line.startswith("Rss:"):
                rss = int(line.split()[1])
            if pss is not None and rss is not None:
                break
    except (OSError, ValueError, IndexError):
        pass
    if pss is None:
        try:
            pss = int(next(l for l in (proc_root / str(pid) / "status").read_text().splitlines()
                           if l.startswith("VmRSS:")).split()[1])
            rss = pss
        except (OSError, StopIteration, ValueError):
            return None
    return comm, pss, rss

def classify_comm(comm, tier="job"):
    """Process class for the PSS table (never used for the denominator)."""
    if tier == "guest":
        return "guest-dalvikvm" if comm.startswith("dalvikvm") else "guest-app"
    if comm.startswith("dalvikvm"):
        return "guest-dalvikvm"
    if comm.startswith("acos-art"):
        return "acos-art-launcher"
    if comm.startswith("acos"):
        return "acos-host"
    if comm == "bwrap":
        return "bwrap"
    if comm.startswith("python"):
        return "sampler-python"
    if comm in ("sh", "timeout", "systemd-run", "cat", "sed"):
        return "harness-shell"
    return "other"

def _ppid_map(proc_root):
    ppid = {}
    try:
        entries = list(Path(proc_root).iterdir())
    except OSError:
        return ppid
    for p in entries:
        if not p.name.isdigit():
            continue
        try:
            raw = (p / "stat").read_text()
            tail = raw[raw.rfind(")") + 1:].split()
            ppid[int(p.name)] = int(tail[1])
        except (OSError, ValueError, IndexError):
            continue
    return ppid

def _is_descendant(pid, root_pid, ppid):
    """True when ``pid``'s ppid chain reaches ``root_pid``."""
    hops = 0
    while pid and hops < 64:
        if pid == root_pid:
            return True
        pid = ppid.get(pid)
        hops += 1
    return False


def _run_owner_pid(evidence_path):
    """Owner pid of a launcher run dir (`<data>/runs/<pid>-<seq>/cgroup`).

    Under concurrency every run shares one `ACOS_ART_DATA`, so one sampler's
    evidence scan sees every run's `<pid>-<seq>/cgroup`. The directory prefix is
    the launcher pid; a run whose launcher is not a descendant of this sampler
    belongs to another concurrent job and must not be charged here. A path that
    does not follow the launcher naming scheme (unit-test fixtures) is accepted.
    """
    name = Path(evidence_path).parent.name
    head, sep, _ = name.partition("-")
    if sep and head.isdigit():
        return int(head)
    return None


def descendant_cgroups(root_pid, job, proc_root=None, cgroup_root=None,
                       ppid_map=None):
    """Cgroups of our descendants that live outside the job cgroup.

    This is the fallback for a launcher that has not written its `<run>/cgroup`
    evidence file yet. Descendant-ness is proved by the ppid chain, so other
    users of bwrap on the host (flatpak apps) are never charged to this job.
    """
    proc_root = Path(proc_root or PROC_ROOT)
    cgroup_root = Path(cgroup_root or CGROUP_ROOT)
    ppid = ppid_map if ppid_map is not None else _ppid_map(proc_root)
    job = Path(job) if job is not None else None

    def is_descendant(pid):
        return _is_descendant(pid, root_pid, ppid)

    found = set()
    for p in Path(proc_root).iterdir():
        if not p.name.isdigit() or not is_descendant(int(p.name)):
            continue
        try:
            for line in (p / "cgroup").read_text().splitlines():
                if line.startswith("0::"):
                    rel = line[3:].strip()
                    cg = cgroup_root / rel.lstrip("/")
                    if rel and cg != job and cg.exists():
                        found.add(cg)
        except OSError:
            continue
    return found


def _rss_mib(pid=None, proc_root=None):
    proc_root = Path(proc_root or PROC_ROOT)
    try:
        for line in (proc_root / str(pid or "self") / "status").read_text().splitlines():
            if line.startswith("VmRSS:"):
                return round(int(line.split()[1]) / 1024.0, 3)
    except (OSError, ValueError, IndexError):
        pass
    return None


def _anon_shmem_mib(pid=None, proc_root=None):
    """RssAnon + RssShmem of a process -- the SAME TYPE as the primary denominator.

    A sampler process's VmRSS also contains file-backed pages (the Python
    interpreter and this module); subtracting VmRSS from an anon+shmem integral
    over-charges the sampler and inflates M11. Fall back to VmRSS only when the
    split is unavailable (recorded as ``split_available: false``).
    """
    proc_root = Path(proc_root or PROC_ROOT)
    anon = shmem = None
    try:
        for line in (proc_root / str(pid or "self") / "status").read_text().splitlines():
            if line.startswith("RssAnon:"):
                anon = int(line.split()[1]) / 1024.0
            elif line.startswith("RssShmem:"):
                shmem = int(line.split()[1]) / 1024.0
    except (OSError, ValueError, IndexError):
        pass
    if anon is None:
        return _rss_mib(pid, proc_root)
    return round(anon + (shmem or 0.0), 3)


class ScopeSampler(threading.Thread):
    """100 ms sampler of one job scope plus every guest scope the run creates.

    Every path root is injectable so the unit tests can drive a fake cgroup tree
    and a fake /proc (`tools/tests/test-benchlib.py`). `root_pid` is the process
    whose ppid chain is trusted as "ours" (default: this process).
    """

    def __init__(self, job, *, interval=0.1, info_dirs=(), pss_every=3,
                 proc_root=None, cgroup_root=None, root_pid=None,
                 discover_every=2, stop_event=None):
        super().__init__(daemon=True)
        self.job = Path(job) if job is not None else None
        self.interval = float(interval)
        self.info_dirs = [Path(d) for d in info_dirs if d]
        self.pss_every = max(1, int(pss_every))
        self.proc_root = Path(proc_root or PROC_ROOT)
        self.cgroup_root = Path(cgroup_root or CGROUP_ROOT)
        self.root_pid = int(root_pid or os.getpid())
        self.discover_every = max(1, int(discover_every))
        self.stop_event = stop_event or threading.Event()
        self.samples = []          # job scope rows
        self.guest = {}            # str(cgroup) -> rows
        self.pss = []              # [{"t", "procs": [...]}]
        self.discovered = set()
        self.misses = 0
        self.ticks = 0

    # -- discovery -------------------------------------------------------- #
    def discover(self):
        found = set()
        ppid = _ppid_map(self.proc_root)
        for d in self.info_dirs:
            if not d.exists():
                continue
            try:
                for p in d.rglob("cgroup"):
                    owner = _run_owner_pid(p)
                    if owner is not None and not _is_descendant(owner, self.root_pid,
                                                                 ppid):
                        # Another concurrent run's evidence file under the shared
                        # ACOS_ART_DATA: charging it here would double count the
                        # guest (W52 concurrency defect).
                        continue
                    cg = cgroup_from_info_file(p, self.cgroup_root)
                    if cg is not None and cg != self.job and cg.exists():
                        found.add(cg)
            except OSError:
                continue
        found |= descendant_cgroups(self.root_pid, self.job,
                                    proc_root=self.proc_root,
                                    cgroup_root=self.cgroup_root,
                                    ppid_map=ppid)
        self.discovered |= found
        return found

    def guest_cgroups(self):
        return sorted(str(c) for c in self.discovered)

    # -- sampling --------------------------------------------------------- #
    def sample_once(self):
        t = time.time()
        self.ticks += 1
        if self.job is not None:
            cur = read_cgroup_int(self.job, "memory.current")
            if cur is None:
                self.misses += 1
            else:
                self.samples.append({"t": t, "bytes": cur,
                                     "stat": read_memstat(self.job)})
        for cg in self.discover():
            c = read_cgroup_int(cg, "memory.current")
            if c is not None:
                self.guest.setdefault(str(cg), []).append(
                    {"t": t, "bytes": c, "stat": read_memstat(cg)})
        if self.pss_every and self.ticks % self.pss_every == 0:
            procs = []
            seen = set()
            for cg, tier in [(self.job, "job")] + [(Path(x), "guest") for x in self.guest]:
                if cg is None:
                    continue
                for pid, comm in procs_in_cgroup(cg, self.proc_root):
                    if pid in seen:
                        continue
                    seen.add(pid)
                    r = pss_of(pid, self.proc_root)
                    if r:
                        comm, pss, rss = r
                        procs.append({"class": classify_comm(comm, tier), "comm": comm,
                                      "pid": pid, "pss_kb": pss, "rss_kb": rss})
            self.pss.append({"t": t, "procs": procs})
        return t

    def run(self):
        while not self.stop_event.is_set():
            t0 = time.time()
            self.sample_once()
            left = self.interval - (time.time() - t0)
            if left > 0:
                self.stop_event.wait(left)

    def closing_sample(self):
        """One sample after the child exits, so teardown has two points."""
        return self.sample_once()


def thread_cpu_seconds(tid, proc_root=None):
    """utime+stime of one thread (``/proc/self/task/<tid>/stat``), or None.

    Used to price the sampler itself: ``ChurnSampler`` runs in its own thread, so
    its CPU cost is separable from the harness threads that drive the loop.
    """
    proc_root = Path(proc_root or PROC_ROOT)
    try:
        # comm may contain spaces/parens: split on the LAST ')'.
        fields = (proc_root / "self" / "task" / str(int(tid)) / "stat").read_text()
        rest = fields.rsplit(")", 1)[1].split()
        return (int(rest[11]) + int(rest[12])) / float(os.sysconf("SC_CLK_TCK"))
    except (OSError, ValueError, IndexError, AttributeError):
        return None


# --------------------------------------------------------------------------- #
# S3c: churn-safe accounting (guest scopes discovered on creation, not at 1 Hz)
# --------------------------------------------------------------------------- #

MiB = 1024.0 * 1024.0          # bytes per MiB (the S3c sampler's unit)
CHURN_INTERVAL_S = 0.1          # 10 Hz: a 0.5 s guest is still seen ~5 times
CHURN_PSS_EVERY = 20            # Pss (smaps_rollup) every 1 s (kept cheap)
CHURN_SLOW_EVERY = 20           # full /proc walk once a second, as a backstop
CHURN_MAX_PENDING = 8           # a scope that never proves ours is not ours


def _pid_chain_hits(pid, root_pid, proc_root=None):
    """True when ``pid``'s ppid chain reaches ``root_pid`` (no /proc-wide walk).

    The S3c discovery path must not read all of /proc for every tick (that is
    3000+ small reads): one scope arrives with one live pid, so the chain is
    walked from *that* pid only.
    """
    proc_root = Path(proc_root or PROC_ROOT)
    cur, hops = int(pid), 0
    while cur and cur > 1 and hops < 64:
        if cur == int(root_pid):
            return True
        try:
            fields = (proc_root / str(cur) / "stat").read_text().rsplit(")", 1)[1].split()
            cur = int(fields[1])
        except (OSError, ValueError, IndexError):
            return False
        hops += 1
    return False


def cgroup_procs(cg):
    """Pids in one cgroup's `cgroup.procs` (empty list on any error)."""
    try:
        text = (Path(cg) / "cgroup.procs").read_text()
    except OSError:
        return []
    return [int(line.strip()) for line in text.splitlines() if line.strip().isdigit()]


class ChurnSampler(threading.Thread):
    """20 Hz job+guest sampler whose discovery keeps up with short-lived guests.

    The S3c defect: ``WarmSampler`` re-walked /proc once a second, so a guest
    that lives < 1 s (every animations-off task) was absent from most samples and
    the anon+shmem integral missed it. This sampler discovers on *creation*: the
    guests are transient systemd scopes created as siblings of the job scope, so
    one ``listdir`` of that slice (a handful of names, no /proc walk) finds a new
    scope within one tick; ownership is then proved by walking that scope's own
    ppid chain to ``root_pid`` (a few reads, never a host-wide scan). The /proc
    walk is kept as a 1 Hz backstop for scopes created outside that slice.

    Everything is per-tick and continuous, so the integral closes a guest that
    dies between two ticks by itself: the tick after its death already reads the
    job-only value, i.e. a trapezoid down to zero. Per-scope records also keep
    the kernel's own ``memory.peak``, which does not depend on the tick grid.
    """

    def __init__(self, job, *, interval=CHURN_INTERVAL_S, pss_every=CHURN_PSS_EVERY,
                 slow_every=CHURN_SLOW_EVERY, scope_parent=None, artifacts=(),
                 sampler_pid=None, proc_root=None, cgroup_root=None,
                 root_pid=None, stop_event=None, max_pending=CHURN_MAX_PENDING):
        super().__init__(daemon=True)
        self.job = Path(job) if job is not None else None
        self.interval = float(interval)
        self.pss_every = max(1, int(pss_every))
        self.slow_every = max(1, int(slow_every))
        self.max_pending = max(1, int(max_pending))
        self.proc_root = Path(proc_root or PROC_ROOT)
        self.cgroup_root = Path(cgroup_root or CGROUP_ROOT)
        self.artifacts = [str(Path(a)) for a in artifacts if a]
        self.scope_parent = (Path(scope_parent) if scope_parent is not None
                             else (self.job.parent if self.job else None))
        self.sampler_pid = int(sampler_pid or os.getpid())
        self.root_pid = int(root_pid or os.getpid())
        self.stop_event = stop_event or threading.Event()
        self.samples = []          # one row per tick (job + guests, same types)
        self.scopes = {}           # str(cg) -> lifetime record
        self.live = {}             # str(cg) -> Path
        self.dead = set()
        self.pending = {}          # str(cg) -> failed ownership proofs
        self.blacklisted = set()   # scope dirs that never proved ours
        self.errors = []
        self.ticks = 0
        self.births = 0
        self.found_by = {"slice": 0, "proc_walk": 0}
        self.tid = None
        self.cpu_s = None
        self._slow_ppid = None

    # -- discovery -------------------------------------------------------- #
    def _owned(self, cg):
        """True when a pid in this scope is a ppid-descendant of root_pid."""
        for pid in cgroup_procs(cg)[:8]:
            if _pid_chain_hits(pid, self.root_pid, self.proc_root):
                return True
        return False

    def admit(self, cg, how):
        key = str(cg)
        if key in self.live or key in self.dead:
            return False
        self.live[key] = Path(cg)
        self.scopes[key] = {
            "cgroup": key, "first_t": None, "last_t": None, "gone_t": None,
            "ticks": 0, "anon_shmem_peak_mib": None, "memory_peak_mib": None,
            "pss_peak_mib": None, "session": self._session_of(cg), "found_by": how,
        }
        self.births += 1
        self.found_by[how] = self.found_by.get(how, 0) + 1
        return True

    def _session_of(self, cg):
        """Session id of a guest scope, from a run tree path in its cmdline.

        ``acos serve`` puts every session's run tree under
        ``<artifacts>/<session_id>/`` (engine/serve.rs), so one cmdline read
        attributes the scope to a session id -- which is what makes the Pss
        figure per *session* instead of per current launcher child.
        """
        if not self.artifacts:
            return None
        for pid in cgroup_procs(cg)[:4]:
            try:
                cmd = (self.proc_root / str(pid) / "cmdline").read_bytes()
            except OSError:
                continue
            text = cmd.replace(b"\0", b" ").decode("utf-8", "replace")
            for root in self.artifacts:
                idx = text.find(root)
                if idx < 0:
                    continue
                rest = text[idx + len(root):].lstrip("/")
                if rest:
                    return rest.split("/", 1)[0]
        return None

    def discover_slice(self):
        """One listdir of the slice the guest scopes are created in."""
        if self.scope_parent is None:
            return 0
        try:
            names = sorted(os.listdir(self.scope_parent))
        except OSError as exc:
            self.errors.append("listdir %s: %s" % (self.scope_parent, exc))
            return 0
        added = 0
        for name in names:
            key = str(self.scope_parent / name)
            if key in self.live or key in self.dead or key in self.blacklisted:
                continue
            tries = self.pending.get(key, 0)
            if tries >= self.max_pending:
                self.blacklisted.add(key)
                self.pending.pop(key, None)
                continue
            if self._owned(key):
                self.pending.pop(key, None)
                added += 1 if self.admit(key, "slice") else 0
            else:
                self.pending[key] = tries + 1
        return added

    def discover_walk(self):
        """Backstop: the old 1 Hz /proc walk (scopes outside the slice)."""
        try:
            if self._slow_ppid is None:
                self._slow_ppid = _ppid_map(self.proc_root)
            found = descendant_cgroups(self.root_pid, self.job,
                                       proc_root=self.proc_root,
                                       cgroup_root=self.cgroup_root,
                                       ppid_map=self._slow_ppid)
            self._slow_ppid = None
        except (OSError, ValueError) as exc:
            self.errors.append("walk: %s" % exc)
            return 0
        added = 0
        for cg in found:
            added += 1 if self.admit(cg, "proc_walk") else 0
        return added

    # -- sampling --------------------------------------------------------- #
    def _read_scope(self, cg, peak=True):
        """(anon+shmem MiB, memory.current MiB, memory.peak MiB) or None.

        ``peak=False`` skips the two extra reads: the 10 Hz integral only needs
        memory.stat, and the kernel's monotone ``memory.peak`` is read 2 Hz.
        """
        st = read_memstat(cg, PRIMARY_KEYS)
        if st is None:
            return None
        cur = read_cgroup_int(cg, "memory.current") if peak else None
        pk = read_cgroup_int(cg, "memory.peak") if peak else None
        peak = pk
        return (round((st.get("anon", 0) + st.get("shmem", 0)) / MiB, 3),
                round(cur / MiB, 3) if cur is not None else None,
                round(peak / MiB, 3) if peak is not None else None)

    def sample_once(self):
        t = time.time()
        self.ticks += 1
        if self.ticks == 1:
            self.tid = threading.get_native_id()
        if self.ticks == 1 or self.ticks % self.slow_every == 0:
            self.discover_walk()
        self.discover_slice()
        job_mib = job_cur = None
        if self.job is not None:
            st = read_memstat(self.job, PRIMARY_KEYS)
            if st is not None:
                job_mib = round((st.get("anon", 0) + st.get("shmem", 0)) / MiB, 3)
            cur = read_cgroup_int(self.job, "memory.current")
            job_cur = round(cur / MiB, 3) if cur is not None else None
        guests = {}
        guest_pss = 0.0
        pss_seen = 0
        do_pss = bool(self.pss_every) and self.ticks % self.pss_every == 0
        peak_due = self.ticks % 5 == 0
        for key in list(self.live):
            rec = self.scopes[key]
            got = self._read_scope(key, peak=peak_due)
            if got is None:
                rec["gone_t"] = t
                self.dead.add(key)
                del self.live[key]
                continue
            mib, cur, peak = got
            guests[key] = mib
            rec["ticks"] += 1
            if rec["session"] is None and rec["ticks"] <= 12:
                # The run tree path only appears in a cmdline once the guest
                # (not just the scope shell) is exec'd: retry briefly.
                rec["session"] = self._session_of(key)
            if rec["first_t"] is None:
                rec["first_t"] = t
            rec["last_t"] = t
            if peak is not None:
                rec["memory_peak_mib"] = max(rec["memory_peak_mib"] or 0.0, peak)
            # anon+shmem only: memory.current also carries page cache, which is
            # charged to whichever scope read the image first (E3.2 4b) and is
            # not this metric's type.
            rec["anon_shmem_peak_mib"] = max(rec["anon_shmem_peak_mib"] or 0.0, mib)
            if do_pss:
                total = 0.0
                seen = 0
                for pid in cgroup_procs(key):
                    row = pss_of(pid, self.proc_root)
                    if row:
                        total += row[1] / 1024.0
                        seen += 1
                if seen:
                    guest_pss += total
                    pss_seen += seen
                    rec["pss_peak_mib"] = max(rec["pss_peak_mib"] or 0.0,
                                              round(total, 3))
        sampler = _anon_shmem_mib(self.sampler_pid, self.proc_root) or 0.0
        guest_total = round(sum(guests.values()), 3)
        net = round((job_mib or 0.0) + guest_total - sampler, 3)
        row = {
            "t": t, "job_anon_shmem_mib": job_mib,
            "job_memory_current_mib": job_cur, "guests": guests,
            "guest_anon_shmem_mib": guest_total, "guest_scopes": len(guests),
            "guest_pss_mib": round(guest_pss, 3) if pss_seen else None,
            "guest_pss_pids": pss_seen, "sampler_mib": sampler,
            "net_anon_shmem_mib": net,
        }
        self.samples.append(row)
        return row

    def run(self):
        cpu0 = thread_cpu_seconds(threading.get_native_id(), self.proc_root)
        try:
            while not self.stop_event.is_set():
                t0 = time.time()
                try:
                    self.sample_once()
                except Exception as exc:  # noqa: BLE001 -- never fake a row
                    self.errors.append("%s: %s" % (type(exc).__name__, exc))
                left = self.interval - (time.time() - t0)
                if left > 0:
                    self.stop_event.wait(left)
                else:
                    self.overruns = getattr(self, "overruns", 0) + 1
        finally:
            cpu1 = thread_cpu_seconds(threading.get_native_id(), self.proc_root)
            if cpu0 is not None and cpu1 is not None:
                self.cpu_s = round(cpu1 - cpu0, 4)

    def stop(self):
        self.stop_event.set()
        self.join(timeout=5)

    # -- derived readings -------------------------------------------------- #
    @property
    def extras(self):
        """Live guest scope paths (WarmSampler's ``extras`` compatibility)."""
        return sorted(self.live)

    def series(self, t_from=None, t_to=None):
        return [r for r in self.samples
                if (t_from is None or r["t"] >= t_from)
                and (t_to is None or r["t"] <= t_to)]

    def window(self, t_from, t_to=None):
        """WarmSampler-compatible window summary (legacy callers)."""
        rows = self.series(t_from, t_to)
        vals = [s.get("net_anon_shmem_mib") for s in rows
                if s.get("net_anon_shmem_mib") is not None]
        if not rows:
            return None
        return {
            "samples": len(rows),
            "net_anon_shmem_mib": stats(vals) if vals else None,
            "last": rows[-1],
            "first": rows[0],
        }

    def summary(self, t_from=None, t_to=None, sessions=None, expected_guests=None):
        """Integrals + coverage for one window (all from this sampler's ticks)."""
        rows = self.series(t_from, t_to)
        out = {"samples": len(rows), "interval_s": self.interval}
        if len(rows) >= 2:
            out["net_anon_shmem_gib_min"] = round(sample_integral(
                [{"t": r["t"], "bytes": r["net_anon_shmem_mib"] * MiB} for r in rows]), 6)
            out["job_gib_min"] = round(sample_integral(
                [{"t": r["t"], "bytes": (r["job_memory_current_mib"] or 0) * MiB}
                 for r in rows]), 6)
            # Per scope: the guest ramps from 0 at birth and back to 0 at death,
            # so the integral of a boxcar seen at two ticks is the full interval,
            # not the 2-tick trapezoid. Both ends are closed with a zero sample.
            guest_int = 0.0
            for cg in {cg for s in rows for cg in s["guests"]}:
                series = [{"t": s["t"], "bytes": s["guests"][cg] * MiB}
                          for s in rows if cg in s["guests"]]
                rec = self.scopes.get(cg) or {}
                if series:
                    series.insert(0, {"t": series[0]["t"] - self.interval, "bytes": 0.0})
                    if rec.get("gone_t"):
                        series.append({"t": rec["gone_t"], "bytes": 0.0})
                guest_int += sample_integral(series)
            out["guest_anon_shmem_gib_min"] = round(guest_int, 6)
        net = [r["net_anon_shmem_mib"] for r in rows]
        job_only = [r["job_anon_shmem_mib"] for r in rows
                    if r["job_anon_shmem_mib"] is not None]
        guests = [r["guest_anon_shmem_mib"] for r in rows]
        live = [r["guest_scopes"] for r in rows]
        pss = [r["guest_pss_mib"] for r in rows if r["guest_pss_mib"] is not None]
        if net:
            out["net_anon_shmem_mib"] = {"median": round(statistics.median(net), 3),
                                         "max": round(max(net), 3),
                                         "min": round(min(net), 3)}
        if job_only:
            out["job_only_anon_shmem_mib"] = {"median": round(statistics.median(job_only), 3)}
        if guests:
            out["guest_anon_shmem_mib"] = {"median": round(statistics.median(guests), 3),
                                           "max": round(max(guests), 3)}
        if live:
            out["guest_scopes_live"] = {"median": statistics.median(live),
                                        "max": max(live),
                                        "zero_ticks": sum(1 for v in live if v == 0)}
            live_n = statistics.median([v for v in live if v]) if any(live) else None
            if live_n and guests:
                out["guest_anon_shmem_per_scope_mib"] = round(
                    statistics.median(guests) / live_n, 3)
        if pss:
            out["guest_pss_mib"] = {"median": round(statistics.median(pss), 3),
                                    "max": round(max(pss), 3), "samples": len(pss)}
            if sessions:
                out["per_session_pss_mib"] = round(statistics.median(pss) / sessions, 3)
                out["per_session_pss_min_mib"] = round(min(pss) / sessions, 3)
        if sessions and net:
            out["net_anon_shmem_per_session_mib"] = round(
                statistics.median(net) / sessions, 3)
        # coverage: every scope admitted in the window, and how much of its life we saw
        scopes = [r for r in self.scopes.values()
                  if r["first_t"] is not None
                  and (t_to is None or r["first_t"] <= t_to)]
        ticks = [r["ticks"] for r in scopes]
        out["guests_observed"] = len(scopes)
        out["guests_with_peak"] = sum(1 for r in scopes if r["memory_peak_mib"])
        if expected_guests:
            out["guests_expected"] = int(expected_guests)
            out["coverage"] = round(min(1.0, len(scopes) / float(expected_guests)), 4)
        if ticks:
            out["samples_per_guest"] = {"min": min(ticks),
                                        "median": statistics.median(ticks),
                                        "max": max(ticks)}
            out["guests_sampled_once"] = sum(1 for v in ticks if v <= 1)
        per_session = {}
        for rec in scopes:
            sid = rec.get("session")
            if sid and rec.get("anon_shmem_peak_mib"):
                per_session[sid] = max(per_session.get(sid, 0.0),
                                       rec["anon_shmem_peak_mib"])
        if per_session:
            out["session_anon_shmem_peak_mib"] = {k: round(v, 3)
                                                  for k, v in sorted(per_session.items())}
        out["found_by"] = dict(self.found_by)
        out["births"] = self.births
        out["blacklisted_scope_dirs"] = len(self.blacklisted)
        if self.cpu_s is not None:
            wall = (rows[-1]["t"] - rows[0]["t"]) if len(rows) >= 2 else None
            out["sampler_cpu_s"] = self.cpu_s
            if wall:
                out["sampler_cpu_pct"] = round(100.0 * self.cpu_s / wall, 4)
        return out


# --------------------------------------------------------------------------- #
# Host-level cross-check (W52): the union of every concurrent job + guest scope
# --------------------------------------------------------------------------- #

class HostScopeMonitor(threading.Thread):
    """Samples the union of all job and guest scopes of a concurrent window.

    It runs in the suite process (outside every job scope) and discovers the
    scopes independently of the per-run samplers: a descendant-cgroup scan from
    the suite pid finds the guarded job scopes and the launcher's sibling guest
    scopes. That independence is what makes it a real cross-check of the per-run
    denominators -- a run whose sampler drops a guest scope still shows up here.

    The suite's own cgroup (``self_job``) is excluded, so the monitor charges
    only the jobs under test. Its own sampling cost lives in the suite cgroup,
    outside the measured set.
    """

    def __init__(self, *, self_job=None, interval=0.1, discover_every=2,
                 proc_root=None, cgroup_root=None, root_pid=None,
                 registry_dirs=(), stop_event=None):
        super().__init__(daemon=True)
        self.self_job = Path(self_job) if self_job is not None else None
        self.interval = float(interval)
        self.discover_every = max(1, int(discover_every))
        self.proc_root = Path(proc_root or PROC_ROOT)
        self.cgroup_root = Path(cgroup_root or CGROUP_ROOT)
        self.root_pid = int(root_pid or os.getpid())
        self.stop_event = stop_event or threading.Event()
        self.registry_dirs = [Path(d) for d in registry_dirs if d]
        self.known = set()
        self.samples = []          # [{"t", "bytes", "stat", "cgroups"}]
        self.ticks = 0

    def _registry_jobs(self):
        """Job scopes announced by the per-run samplers before they spawn.

        This only closes the discovery gap for the JOB cgroup (the sampler
        writes it at t=0). Guest scopes are still found independently through
        the process tree, so a mutated sampler that drops a guest is caught.
        """
        found = set()
        for d in self.registry_dirs:
            try:
                for p in Path(d).glob("*.job"):
                    try:
                        rel = p.read_text().strip()
                    except OSError:
                        continue
                    if not rel:
                        continue
                    cg = Path(rel)
                    if cg.exists() and cg != self.self_job:
                        found.add(cg)
            except OSError:
                continue
        return found

    def discover(self):
        found = descendant_cgroups(self.root_pid, self.self_job,
                                   proc_root=self.proc_root,
                                   cgroup_root=self.cgroup_root)
        found |= self._registry_jobs()
        found = {c for c in found if c.exists() and c != self.self_job}
        self.known |= found
        self.known = {c for c in self.known if c.exists()}
        return found

    def sample_once(self):
        t = time.time()
        self.ticks += 1
        if self.ticks == 1 or self.ticks % self.discover_every == 0:
            self.discover()
        anon = shmem = 0
        active = []
        for cg in sorted(self.known):
            st = read_memstat(cg, PRIMARY_KEYS)
            if st is None:
                continue
            anon += st.get("anon", 0)
            shmem += st.get("shmem", 0)
            active.append(str(cg))
        self.samples.append({"t": t, "bytes": anon + shmem,
                             "stat": {"anon": anon, "shmem": shmem},
                             "cgroups": active})
        return t

    def run(self):
        while not self.stop_event.is_set():
            t0 = time.time()
            self.sample_once()
            left = self.interval - (time.time() - t0)
            if left > 0:
                self.stop_event.wait(left)

    def closing_sample(self):
        return self.sample_once()

    def integral(self):
        """Integral of the summed anon+shmem over the sampled window, GiB.min."""
        return stat_key_integral(self.samples, PRIMARY_KEYS)

    def peak(self):
        vals = [s["bytes"] for s in self.samples]
        return max(vals) if vals else None


def scope_agreement(per_run_sum, host_gib_min, *, sampler_overhead=0.0,
                    tolerance=MIN_NOISE):
    """Compare the sum of per-run integrals with the host-level integral.

    Both cover the same wall window; ``host_gib_min`` is the raw host figure and
    ``sampler_overhead`` is the sum of the per-run sampler charges (which the
    per-run denominators already subtract, so the fair comparison is
    host - overhead). A relative difference above the noise tolerance means the
    two do not measure the same set of scopes (e.g. a dropped guest scope) and
    the row must not be accepted as a measurement.
    """
    host = None if host_gib_min is None else max(0.0, float(host_gib_min) -
                                                 float(sampler_overhead or 0.0))
    rel = None
    if per_run_sum and host is not None:
        rel = abs(host - float(per_run_sum)) / float(per_run_sum)
    return {
        "per_run_sum_gib_min": _round(per_run_sum),
        "host_gib_min_raw": _round(host_gib_min),
        "sampler_overhead_gib_min": _round(sampler_overhead),
        "host_gib_min": _round(host),
        "rel_diff_pct": None if rel is None else round(100.0 * rel, 3),
        "tolerance": float(tolerance),
        "within_noise": bool(rel is not None and rel <= float(tolerance)),
        "note": "per-run sum = Σ primary (anon+shmem, sampler excluded) of every "
                "run in the window; host = an independent union of all job+guest "
                "scopes over the same window, minus the same sampler charge",
    }


# --------------------------------------------------------------------------- #
# Denominators (W45)
# --------------------------------------------------------------------------- #

def combine_series(job_rows, guest_series):
    """Per-timestamp job+guest sum, forward-filling each guest series.

    A guest scope is created mid-run (it is discovered after the launcher starts
    it), so aligning by index (W41 did) mixes timestamps. Forward fill is the
    correct reading: before the guest exists its charge is zero.
    """
    out = []
    for s in job_rows or []:
        t = s.get("t")
        st = dict(s.get("stat") or {})
        total = s.get("bytes") or 0
        for cg, rows in (guest_series or {}).items():
            pick = None
            for r in rows:
                if r.get("t") is None:
                    continue
                if r["t"] <= t:
                    pick = r
                else:
                    break
            if pick is None:
                continue
            total += pick.get("bytes") or 0
            for k, v in (pick.get("stat") or {}).items():
                st[k] = st.get(k, 0) + v
        out.append({"t": t, "bytes": total, "stat": st})
    return out

def stat_key_integral(rows, keys):
    """Integral (GiB.min) of the sum of the named memory.stat keys."""
    sel = [{"t": r["t"], "bytes": sum((r.get("stat") or {}).get(k, 0) for k in keys)}
           for r in rows or [] if r.get("stat") is not None]
    return sample_integral(sel) if len(sel) >= 2 else None

def stat_key_peak(rows, keys):
    vals = [sum((r.get("stat") or {}).get(k, 0) for k in keys)
            for r in rows or [] if r.get("stat") is not None]
    return max(vals) if vals else None

def _round(v):
    return None if v is None else round(float(v), 9)

def memory_denominators(job_samples, guest_series, *, sampler_mib=None,
                        sampler_rss_mib=None, window_s=None,
                        interval_s=MEMORY_SAMPLE_MS / 1000.0,
                        job_cgroup=None, closing_sample=True):
    """The labelled denominators for one variant run.

    ``primary`` (anon+shmem, sampler excluded) is the ONLY denominator that may
    be compared with the legacy-derived 24.4 tasks/GiB.min bar. The others are
    recorded for continuity and debugging and are marked ``gate_uses: false``.
    """
    total_rows = combine_series(job_samples, guest_series)
    if window_s is None and len(job_samples) >= 2:
        window_s = job_samples[-1]["t"] - job_samples[0]["t"]
    window_s = float(window_s or 0.0)
    sampler_mib = sampler_mib or 0.0
    overhead = (float(sampler_mib) / 1024.0) * (window_s / 60.0)
    overhead_full_rss = (((float(sampler_rss_mib) if sampler_rss_mib is not None
                           else float(sampler_mib)) / 1024.0) * (window_s / 60.0))

    def series(rows):
        return sample_integral([{"t": r["t"], "bytes": r["bytes"]} for r in rows])

    scope_gib = series(job_samples) if job_samples else None
    job_all = series(total_rows) if total_rows else None
    primary_raw = stat_key_integral(total_rows, PRIMARY_KEYS)
    resident_raw = stat_key_integral(total_rows, RESIDENT_KEYS)
    file_incl = stat_key_integral(total_rows, ("file",))
    shmem = stat_key_integral(total_rows, ("shmem",))
    anon = stat_key_integral(total_rows, ("anon",))
    kernel = stat_key_integral(total_rows, ("kernel",))
    sock = stat_key_integral(total_rows, ("sock",))

    def excl(v):
        return None if v is None else max(0.0, v - overhead)

    def entry(name, value, metric_type, gate, includes, excl_value=None, note=None):
        return {
            "name": name,
            "gib_min": _round(value),
            "gib_min_excl_sampler": _round(excl_value if excl_value is not None else value),
            "metric_type": metric_type,
            "gate_uses": bool(gate),
            "includes": includes,
            "note": note,
        }

    primary = excl(primary_raw)
    resident = excl(resident_raw)
    infra = None
    if file_incl is not None:
        infra = max(0.0, file_incl - (shmem or 0.0))

    return {
        "schema": "acos-scope-denominators/1",
        "primary": entry("anon+shmem", primary_raw, PRIMARY_METRIC_TYPE, True,
                         "job cgroup + every guest cgroup (memory.stat anon+shmem)",
                         excl_value=primary,
                         note="same type as the legacy 206 MiB core + 112 MiB/session "
                              "numbers the 24.4 bar was derived from; gib_min is the raw "
                              "sample, gib_min_excl_sampler is the gate value"),
        "resident": entry("anon+shmem+kernel", resident_raw, PRIMARY_METRIC_TYPE, False,
                          "job + guest cgroups, plus kernel overhead",
                          excl_value=resident),
        "job_all_incl_file": entry("memory.current (anon+file+shmem+kernel)", job_all,
                                   JOB_ALL_METRIC_TYPE, False,
                                   "job + guest cgroups; includes page cache",
                                   note="affected by shared page cache: not a gate value"),
        "scope_runner_formula": entry("job scope only (old runner formula)", scope_gib,
                                      JOB_ALL_METRIC_TYPE, False,
                                      "job cgroup only; reproduces the pre-W45 run-suite "
                                      "number and excludes the guest",
                                      note="kept for continuity; never gate on it"),
        "infra_file_cache": {
            "name": "file - shmem (shared read-only page cache)",
            "gib_min": _round(infra),
            "metric_type": "CGROUP_FILE_CACHE",
            "charged_to_task": False,
            "reported_separately": True,
            "note": "ART sysroot/boot image/binary pages are read-only infra, not task "
                    "cost (W41 4.3/1.2); reported here and never added to primary",
        },
        "sampler": {
            "excluded": True,
            "subtracted_metric": "sampler RssAnon+RssShmem (same type as the primary "
                                 "anon+shmem denominator)",
            "anon_shmem_mib": round(float(sampler_mib), 3) if sampler_mib else None,
            "rss_mib": (round(float(sampler_rss_mib), 3)
                        if sampler_rss_mib is not None else
                        (round(float(sampler_mib), 3) if sampler_mib else None)),
            "window_s": round(window_s, 3),
            "overhead_gib_min": _round(overhead),
            "overhead_if_full_rss_gib_min": _round(overhead_full_rss),
            "note": "this sampler process runs inside the job scope; only its "
                    "anon+shmem share is subtracted from the anon+shmem denominators "
                    "(its file-backed pages are not part of that type). The full-RSS "
                    "figure is reported for sensitivity only.",
        },
        "components": {
            "anon_gib_min": _round(anon),
            "shmem_gib_min": _round(shmem),
            "kernel_gib_min": _round(kernel),
            "sock_gib_min": _round(sock),
            "file_incl_shmem_gib_min": _round(file_incl),
            "file_excl_shmem_gib_min": _round(infra),
            "peak_mib_all_cgroups": (round(max((r["bytes"] or 0) for r in total_rows) / MIB, 3)
                                     if total_rows else None),
            "peak_mib_primary": (round((stat_key_peak(total_rows, PRIMARY_KEYS) or 0) / MIB, 3)
                                 if total_rows else None),
        },
        "scope": {
            "job_cgroup": str(job_cgroup) if job_cgroup else None,
            "guest_cgroups": sorted(guest_series or {}),
            "scope_count": 1 + len(guest_series or {}),
            "job_samples": len(job_samples or []),
            "guest_samples": {k: len(v) for k, v in (guest_series or {}).items()},
            "interval_s": interval_s,
            "closing_sample": bool(closing_sample),
        },
    }


def m11_denominator(task_costs, verified, *, bar=LEGACY_DERIVED_BAR,
                    min_noise=MIN_NOISE, resamples=BOOTSTRAP_RESAMPLES,
                    seed=BOOTSTRAP_SEED, ci=0.95):
    """M11 = verified tasks / sum(task_costs), with a bootstrap CI over tasks.

    ``task_costs`` maps task id -> GiB.min for that task (good + wrong twin, as
    the suite runs both). ``verified`` maps task id -> bool. A task with a null
    cost is excluded (and reported) rather than estimated. The CI resamples
    tasks (the unit of work) and recomputes both numerator and denominator.
    """
    keys = [k for k in sorted(task_costs) if task_costs.get(k) is not None]
    missing = [k for k in sorted(task_costs) if task_costs.get(k) is None]
    total = sum(float(task_costs[k]) for k in keys) if keys else None
    nver = sum(1 for k in keys if verified.get(k))
    m11 = (nver / total) if total else None
    out = {
        "m11_tasks_per_gib_min": m11,
        "gib_min": total,
        "verified_tasks": nver,
        "tasks": len(keys),
        "missing_tasks": missing,
        "bar_tasks_per_gib_min": float(bar),
        "noise": float(min_noise),
        "required_tasks_per_gib_min": float(bar) * (1.0 + float(min_noise)),
        "passes_bar": (m11 is not None and m11 >= float(bar) * (1.0 + float(min_noise))),
        "ci95_low": None, "ci95_high": None,
        "ci95_width_pct": None,
        "resamples": 0,
        "method": "M11 = verified / sum(task anon+shmem GiB.min, good+wrong); "
                  "percentile bootstrap resampling tasks (both numerator and "
                  "denominator recomputed); passes bar only if M11 >= bar*(1+noise)",
    }
    if m11 is not None and len(keys) >= 2 and resamples:
        rng = random.Random(seed)
        vals = []
        n = len(keys)
        for _ in range(int(resamples)):
            pick = [keys[rng.randrange(n)] for _ in range(n)]
            c = sum(float(task_costs[k]) for k in pick)
            v = sum(1 for k in pick if verified.get(k))
            if c > 0:
                vals.append(v / c)
        vals.sort()
        tail = (1.0 - ci) / 2.0
        out["ci95_low"] = percentile(vals, 100.0 * tail)
        out["ci95_high"] = percentile(vals, 100.0 * (1.0 - tail))
        if m11:
            out["ci95_width_pct"] = round(
                100.0 * (out["ci95_high"] - out["ci95_low"]) / m11, 3)
        out["resamples"] = int(resamples)
    return out


def check_variant_scopes(rows):
    """Reject a suite-lifetime scope: every variant run must own a fresh cgroup.

    ``rows`` is [{"variant", "job_cgroup", "sampler_pid"}]. A duplicated job
    cgroup means two variants were measured in one scope (the W41 defect 1.3),
    which charges shared page cache to every later variant. Returns
    ``{ok, errors, distinct_scopes, scopes, variants}``.
    """
    errors = []
    by = {}
    pids = {}
    for r in rows or []:
        variant = r.get("variant")
        cg = (r.get("job_cgroup") or "").strip()
        if not cg:
            errors.append("%s: no job cgroup resolved (no fresh guarded scope)" % variant)
            continue
        by.setdefault(cg, []).append(variant)
        if r.get("sampler_pid") is not None:
            pids.setdefault(r["sampler_pid"], []).append(variant)
    for cg, names in sorted(by.items()):
        if len(names) > 1:
            errors.append("scope reused across %d variant runs (suite-lifetime scope): "
                          "%s at %s" % (len(names), ", ".join(map(str, names)), cg))
    for pid, names in sorted(pids.items()):
        if len(names) > 1:
            errors.append("sampler pid reused across variant runs: %s (pid %s)"
                          % (", ".join(map(str, names)), pid))
    return {
        "ok": not errors,
        "errors": errors,
        "distinct_scopes": len(by),
        "scopes": sorted(by),
        "variants": len(rows or []),
        "rule": "one fresh systemd scope per variant run (guarded.sh per run); the "
                "suite-lifetime scope is a measurement defect and nulls M11",
    }


# --------------------------------------------------------------------------- #
# Artifact pruning (W45 decision e: the host disk filled during W41)
# --------------------------------------------------------------------------- #

ALWAYS_KEEP_EVIDENCE = ("verdict.json", "engine.json", "mem.json",
                        "art-frames/session-timing.json")
SMALL_EVIDENCE_BYTES = 32 * 1024


def prune_screens(root, keep=20):
    """Keep at most ``keep`` screenshots under ``root`` (newest by name/mtime).

    Per-task harnesses used to write one PNG per task and never remove them;
    S3ab filled 28 GB that way. This is a hard cap, not an evidence policy:
    the retained files are the newest ``keep`` files matching ``*.png`` (the
    failure evidence for a run is the JSON record, not 10 000 frames).
    """
    root = Path(root)
    out = {"dir": str(root), "keep": max(0, int(keep)), "kept": [], "deleted": [],
           "freed_bytes": 0}
    if not root.exists():
        return out
    files = [p for p in root.rglob("*.png") if p.is_file()]
    files.sort(key=lambda p: (p.stat().st_mtime, str(p)), reverse=True)
    for p in files[:out["keep"]]:
        out["kept"].append(str(p.relative_to(root)))
    for p in files[out["keep"]:]:
        try:
            size = p.stat().st_size
            p.unlink()
        except OSError:
            out["kept"].append(str(p.relative_to(root)))
            continue
        out["deleted"].append(str(p.relative_to(root)))
        out["freed_bytes"] += size
    return out


def prune_artifacts(art_dir, *, keep=False, passed=True, max_failure_mib=4.0,
                    always_keep=ALWAYS_KEEP_EVIDENCE,
                    small_bytes=SMALL_EVIDENCE_BYTES):
    """Prune one per-run artifact dir after the summary JSON has been written.

    * ``keep=True`` (``--keep-artifacts``): delete nothing.
    * passing run: keep the declared evidence files plus any file <= 32 KiB
      (verdict, step timing, trees); delete frames/PNGs/screenshots.
    * failing/unsupported run: additionally keep up to ``max_failure_mib`` of
      the remaining failure artifacts (name order), so the failure evidence
      survives without letting one run fill the disk.
    """
    root = Path(art_dir)
    out = {"dir": str(root), "pruned": False, "kept": [], "deleted": [],
           "freed_bytes": 0, "kept_bytes": 0, "reason": None}
    if not root.exists():
        out["reason"] = "missing"
        return out
    files = [p for p in sorted(root.rglob("*")) if p.is_file()]
    keep_set = set()
    for p in files:
        rel = str(p.relative_to(root))
        try:
            size = p.stat().st_size
        except OSError:
            continue
        if rel in always_keep or size <= small_bytes:
            keep_set.add(p)
    if keep:
        out["reason"] = "--keep-artifacts"
        out["kept"] = [str(p.relative_to(root)) for p in files]
        out["kept_bytes"] = sum(p.stat().st_size for p in files)
        return out
    if not passed:
        cap = int(max_failure_mib * MIB)
        used = sum(p.stat().st_size for p in keep_set)
        for p in files:
            if p in keep_set:
                continue
            size = p.stat().st_size
            if used + size <= cap:
                keep_set.add(p)
                used += size
    for p in files:
        rel = str(p.relative_to(root))
        size = p.stat().st_size
        if p in keep_set:
            out["kept"].append(rel)
            out["kept_bytes"] += size
        else:
            try:
                p.unlink()
            except OSError:
                out["kept"].append(rel)
                out["kept_bytes"] += size
                continue
            out["deleted"].append(rel)
            out["freed_bytes"] += size
    # remove directories left empty
    for d in sorted((p for p in root.rglob("*") if p.is_dir()),
                    key=lambda p: len(p.parts), reverse=True):
        try:
            d.rmdir()
        except OSError:
            pass
    out["pruned"] = bool(out["deleted"])
    out["reason"] = "pass: evidence subset" if passed else "fail: evidence + capped failure artifacts"
    return out


# --------------------------------------------------------------------------- #
# CLI: sample-scope (used by tools/verify/run-suite.sh once per variant run)
# --------------------------------------------------------------------------- #

def sample_scope(argv):
    """Run one command inside the current (fresh) scope and write its memory JSON.

    Usage: benchlib.py sample-scope --out FILE [--interval S] [--label L]
                                    [--info-dir D] -- CMD [ARGS...]

    The sampler samples its own cgroup plus every guest cgroup the command
    creates, takes a closing sample, subtracts its own cost and exits with the
    command's return code. It writes the command's stdout/stderr next to FILE so
    the caller can read the verdict envelope without interleaving.
    """
    opts = {"out": None, "interval": MEMORY_SAMPLE_MS / 1000.0, "label": None,
            "info_dirs": [], "register_dir": None}
    cmd = []
    i = 0
    while i < len(argv):
        a = argv[i]
        if a == "--":
            cmd = argv[i + 1:]
            break
        if a in ("--out", "--interval", "--label", "--info-dir", "--register-dir"):
            if i + 1 >= len(argv):
                print("benchlib sample-scope: %s needs a value" % a, file=sys.stderr)
                return 2
            v = argv[i + 1]
            if a == "--info-dir":
                opts["info_dirs"].append(v)
            elif a == "--interval":
                opts["interval"] = float(v)
            else:
                opts[a[2:]] = v
            i += 2
            continue
        print("benchlib sample-scope: unknown argument %r" % a, file=sys.stderr)
        return 2
    if not cmd:
        print("benchlib sample-scope: no command after --", file=sys.stderr)
        return 2
    out = Path(opts["out"]) if opts["out"] else None
    if out is None:
        print("benchlib sample-scope: --out is required", file=sys.stderr)
        return 2
    out.parent.mkdir(parents=True, exist_ok=True)
    job = cgroup_path()
    if job is not None and opts.get("register_dir"):
        reg = Path(opts["register_dir"])
        try:
            reg.mkdir(parents=True, exist_ok=True)
            (reg / ("%s-%d.job" % ((opts.get("label") or "run"), os.getpid()))).write_text(
                str(job))
        except OSError:
            pass
    stop = threading.Event()
    sampler = ScopeSampler(job, interval=opts["interval"],
                           info_dirs=opts["info_dirs"], stop_event=stop)
    t0 = time.time()
    sampler.start()
    rc = None
    try:
        proc = subprocess.run(cmd, capture_output=True, text=True)
        rc = proc.returncode
        stdout, stderr = proc.stdout or "", proc.stderr or ""
    except OSError as exc:
        stdout, stderr = "", "benchlib sample-scope: exec failed: %s" % exc
        rc = 127
    t1 = time.time()
    sampler.closing_sample()
    stop.set()
    sampler.join(timeout=5)
    self_rss_mib = _rss_mib()
    self_mib = _anon_shmem_mib()
    den = memory_denominators(sampler.samples, sampler.guest, sampler_mib=self_mib,
                              sampler_rss_mib=self_rss_mib,
                              window_s=t1 - t0, interval_s=opts["interval"],
                              job_cgroup=job, closing_sample=True)
    result = {
        "schema": "acos-scope-sample/1",
        "utc": utc_now(),
        "label": opts["label"],
        "command": cmd,
        "rc": rc,
        "wall_s": round(t1 - t0, 3),
        "scope_available": job is not None,
        "scope_limit": cgroup_has_limit(job) if job is not None else False,
        "job_cgroup": str(job) if job is not None else None,
        "sampler_pid": os.getpid(),
        "sampler_rss_mib": self_rss_mib,
        "sampler_anon_shmem_mib": self_mib,
        "samples": {"ticks": sampler.ticks, "job": len(sampler.samples),
                    "misses": sampler.misses, "pss_ticks": len(sampler.pss),
                    "interval_s": opts["interval"]},
        "denominators": den,
        "pss_peak_mib_by_class": _pss_by_class(sampler.pss),
        "stdout_path": str(out) + ".stdout",
        "stderr_path": str(out) + ".stderr",
    }
    Path(str(out) + ".stdout").write_text(stdout)
    Path(str(out) + ".stderr").write_text(stderr)
    write_json(out, result)
    p = den["primary"]
    print("sample-scope: %s rc=%s scope=%s primary_anon_shmem=%s GiB.min "
          "(guest scopes %d)" % (opts["label"] or "run", rc, job,
                                  p["gib_min_excl_sampler"],
                                  len(den["scope"]["guest_cgroups"])))
    return int(rc)


def _pss_by_class(pss_rows):
    peak = {}
    for p in pss_rows or []:
        for row in p.get("procs") or []:
            cur = peak.setdefault(row["class"], {"pss_mib": 0.0, "rss_mib": 0.0,
                                                 "comm": row.get("comm")})
            cur["pss_mib"] = max(cur["pss_mib"], round((row.get("pss_kb") or 0) / 1024.0, 3))
            cur["rss_mib"] = max(cur["rss_mib"], round((row.get("rss_kb") or 0) / 1024.0, 3))
    return peak


# --------------------------------------------------------------------------- #
# CLI
# --------------------------------------------------------------------------- #

def _cli(argv):
    if argv and argv[0] == "sample-scope":
        return sample_scope(argv[1:])
    if not argv or argv[0] in ("-h", "--help"):
        print(__doc__.strip())
        return 0
    cmd = argv[0]
    if cmd == "stats":
        st = stats([float(x) for x in argv[1:]])
        print(json.dumps(st, indent=2, sort_keys=True))
        return 0
    if cmd == "host-gate":
        repo = None
        args = [a for a in argv[1:] if not a.startswith("--")]
        if args:
            repo = args[0]
        print(json.dumps(host_gate(repo), indent=2, sort_keys=True))
        return 0
    if cmd == "provenance":
        args = argv[1:]
        profile = None
        rest = []
        i = 0
        while i < len(args):
            if args[i] == "--profile":
                profile = args[i + 1]
                i += 2
            else:
                rest.append(args[i])
                i += 1
        root = os.environ.get("ACOS_REPO_ROOT", ".")
        print(json.dumps(provenance(root, rest, profile), indent=2, sort_keys=True))
        return 0
    if cmd == "classify":
        print(json.dumps([classify_error(c) for c in argv[1:]], indent=2))
        return 0
    if cmd == "m11":
        # m11 COST VERIFIED [COST VERIFIED ...] -- quick sanity from the CLI
        pairs = []
        for arg in argv[1:]:
            cost, _, ver = arg.partition(":")
            pairs.append((cost, float(cost), ver == "1"))
        costs = {c: v for c, v, _ in pairs}
        verified = {c: ok for c, _, ok in pairs}
        print(json.dumps(m11_denominator(costs, verified), indent=2, sort_keys=True))
        return 0
    print("benchlib: unknown command %r" % cmd, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(_cli(sys.argv[1:]))
