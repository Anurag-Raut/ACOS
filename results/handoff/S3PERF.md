# S3PERF handoff — why the S3AB readiness hardening costs a frame (root cause + measured candidate)

**Date:** 2026-11-14 ~15:40 UTC. **Branch:** `fix-s3ab-perf` off `int-s3ab` (`d1541ea`).
**State:** **root cause named with evidence; two fix candidates measured; no product change
committed yet** (candidate A is preserved as a patch, not applied — it does not meet the ±5 %
bar). Gates/crate tests not run for a product change because there is none on the branch.
`int-s3ab`, `fix-s3ab-reset` and `settle-se1-expect-change` are untouched.

**Evidence:** `results/bench/S3PERF-evidence/` (`probe-*.json`, `ab-reps.json`,
`variant-a-no-forced-redraw.diff`), probe tool `tools/bench/s3perf_probe.py`.

---

## 1. Root cause (measured, not hypothesised)

`LaunchMain.awaitFirstTraversal`'s new draw-evidence gate fires on **every normal launch**, and
the reason it reads is not stale and not benign: it is the *normal* first traversal.

Per-launch probe (`tools/bench/s3perf_probe.py`, sequential single-session counter launches, the
guest's own `ACOSLAUNCH MARK` lines teed through `ACOSLAUNCH_GUEST_STDOUT_FILE`):

| binary | launches | took the skip path | reason | p50 launch |
|---|---:|---:|---|---:|
| main `3e11d9c` (`5a2ea97d…`) | 8 measured | **0** (no gate) | – | 315.5 ms |
| `int-s3ab` (`d54249d9…`) | 8 measured | **7** | `traversal:view_not_visible` | 325.0 ms |
| candidate A (§3) | 10 measured | **6** | `traversal:view_not_visible app_visible=true` | 317.7 ms |

`results/bench/S3PERF-evidence/probe-fix.json`, `probe-main.json`, `probe-B.json`.

Mechanism, all in AOSP (`.cache/aosp/base/core/java/android/view/ViewRootImpl.java`):

* `mAppVisible` is declared `= true` (:520) but `setView` overwrites it from the add reply's
  `ADD_FLAG_APP_VISIBLE` bit (:1606). ACOS deliberately does **not** set that bit (setting it
  regressed `T03-render-good`; E2.15 §2.4), so after add `mAppVisible == false`.
* The first traversal therefore computes `isViewVisible == false` (:3407) and skips the draw,
  recording `mLastPerformTraversalsSkipDrawReason = "view_not_visible"` (:4059) — while `mFirst`
  is already `false` (:4015 before the draw decision at :4093).
* The host's oneway `IWindow.dispatchAppVisibility(true)` arrives **after** that traversal (probe:
  `app_visible=true` already at the check, i.e. it lands between the traversal and the posted
  readiness check). It schedules the next traversal, which draws at frame F+1.

So `int-s3ab`'s ready path is: skip seen at F → redeliver + `requestLayout()`/`invalidate()` →
wait for the OnDraw listener → ready at the end of F+1. Main reported ready right after F and let
the launch's frame wait cover F+1, which is why main is exactly one frame faster: the gate now
waits for a draw main never waited for, and the forced `requestLayout()`/`invalidate()` can push
the traversal past the pending vsync (the probe shows F+2 tails).

## 2. Candidate A — keep the gate, drop the forced redraw/replay (measured, insufficient)

Patch: on a skipped traversal, record `draw_skip` and just wait for the next OnDraw (the host's
visibility callback already schedules it); no `dispatchAppVisibility` replay, no
`requestLayout()`/`invalidate()`. `POST_RESUME_NOT_READY` keeps `draw_skip=` and now also carries
`first_traversal="traversed=… app_visible=… skip=…"`.
`results/bench/S3PERF-evidence/variant-a-no-forced-redraw.diff`.

Interleaved A/B, 3 reps per arm, `tools/bench/acos-bench.sh --reps 1 --groups "a:latency:8G:300"`,
binary sha256 main `5a2ea97d95d16dd173915fb858a26a3d35e01cb5ce0dd1d89d8fe48110d95850`,
candidate A `d54249d94d7ffb60cb2fb4fd2ceadf068cec0e174b0aace4f7f43d6a1644c33a` (the guest Java is
read at run time, so the binary sha is unchanged by the patch; the dex cache keys on source bytes).

| metric | main p50 median (3 reps) | candidate A p50 median | Δ | verdict (±5 %) |
|---|---:|---:|---:|---|
| M4 warm launch strict | 309.5 ms (310.0 / 309.5 / 308.8) | 325.4 (341.3 / 325.0 / 325.4) | **+5.1 %** | borderline fail |
| M4 warm launch fast | 173.9 (159.4 / 174.1 / 173.9) | 189.9 (190.3 / 189.9 / 174.8) | **+9.2 %** | **fail** |
| M6 reset strict | 329.2 (320.0 / 333.3 / 329.2) | 336.1 (335.4 / 336.1 / 344.3) | +2.1 % | pass |
| M6 reset fast | 191.0 (177.5 / 191.0 / 196.8) | 204.7 (204.7 / 199.5 / 205.9) | **+7.2 %** | **fail** |
| M7 tree strict | 41.2 | 41.2 | 0.0 % | unchanged |
| M8 tap counter strict | 952.9 | 955.3 | +0.3 % | unchanged |
| M8 tap counter fast | 45.3 | 46.6 | +2.8 % | within noise |
| M5 screenshot | 24.9 | 24.7 | −0.8 % | unchanged |

Raw per-rep p50s: `results/bench/S3PERF-evidence/ab-reps.json` (host load1 0.9–2.2,
MemAvailable ≥ 50 GiB for every rep; one main rep lost one M4 sample to a pre-existing
`GUEST_EXITED` = S3b class B, which main has no retry for).

**Conclusion:** removing the forced redraw is not enough. Any readiness gate that *waits for the
draw* costs one frame on the fast path (the fast reply does wait for the guest's `activity-resumed`
marker) and 0.5–1 frame on the strict path (whose settle waits for the same frame the gate now
blocks on). The protection and the latency are in direct conflict at the *ready* level.

## 3. What the next attempt should do (smallest correct action)

Move the S3a gate from *readiness* to *the no-frame path*, which is where the 60 s hang actually
was:

1. Guest (`LaunchMain`): publish readiness as main does (no draw wait — zero latency delta), but
   put the skip evidence in the `activity-resumed` marker / `ACOSLAUNCH` line
   (`draw_skip=<reason>`, `first_traversal=…`). This is a diagnostic, not a gate.
2. Engine (`acos-art` launch `NO_LIVE_FRAME` loop): read that evidence. When the guest reported a
   skipped first traversal (or no draw at all) use a short no-frame budget (≥ 2 frames, ≤ `sResumeReadyMs`
   = 2 s) and fail with **`POST_RESUME_NOT_READY` + `draw_skip=`**, instead of the 60 s frame budget.
   A launch still requires a live frame (`c1_generation`), so a skipped traversal can never be
   certified ready.
3. Keep `NO_LIVE_FRAME`'s C1 snapshot (`c1_state_summary`) — it is diagnostic-only and free.

Expected: M4/M6 back to main parity (the ready path is byte-identical to main apart from one
marker field), the S3a failure is named and bounded at ≤2 s instead of 60 s, and `T03-render-good`
is untouched (no change to visibility/traversal order). This has **not** been implemented or
measured yet; it needs a new A/B plus the gate sweep.

Do **not** re-try: `ADD_FLAG_APP_VISIBLE` in the add reply (regresses T03), or
`requestLayout()`/`invalidate()` on the skip path (one frame, measured above).

## 4. Commands used (reproducible)

```
# root-cause probe (one lock acquisition, ~40 s)
ACOS_FONT_DIR=… ACOS_PROBE_REPO=$PWD tools/art-runtime/run-exclusive.sh --task S3PERF -- \
  tools/guarded.sh --mem 12G --time 300 -- python3 tools/bench/s3perf_probe.py \
  --acos $PWD/.cache/gr4-target/release/acos --out .cache/s3perf/probe-fix.json \
  --scratch .cache/s3perf/probe-fix --n 8

# interleaved A/B (3 reps/arm), scratch in this worktree only
.cache/s3perf/ab.sh      # main = /home/Qui/Projects/acos/.cache/wt-abmain (read-only arm)
```
