# S3PERF handoff — the readiness gate is off the ready path (S3PERF2)

**Status: PARTIAL at the 60-minute checkpoint.** Implementation committed (`2c6c64f`), unit tests
green, per-launch probe clean (ready path main-identical, `draw_skip` evidence present). The
interleaved A/B (3 reps/arm), the crate-test sweep, the gate sweep and the N=10 reliability smoke
were still running / not started when the time box expired: the host had 5–7 ART-lock waiters and
every acquisition queued 4–8 min. Nothing below this line is a claim about the fix's latency: it is
the E2.16 root-cause record this attempt built on (see `EXPERIMENTS.md` E2.17 for the new state).

## S3PERF2 — what changed (commit `2c6c64f`, branch `fix-s3ab-perf`)

Binaries: fix `acos` sha256 `f53c87712fc9efeafb3b3db6e7a50e7caad24d1bde459e84b503b97a9572a769`,
`acos-art` `bf59ef19015cbca534e1627c0a08d7fcb91067d26485225b33751a06dc4e2cd5`; main arm `acos`
`5a2ea97d95d16dd173915fb858a26a3d35e01cb5ce0dd1d89d8fe48110d95850` (the E2.16 main build).

1. **Guest ready path = main, byte for byte.** `LaunchMain.awaitFirstTraversal` again reports
   ready as soon as a first traversal exists (else waits for the next `OnDraw`); the forced
   `requestLayout()`/`invalidate()` and the `dispatchAppVisibility(true)` replay are deleted, not
   disabled. The skip is published as evidence in the `activity-resumed.marker`
   (`draw_skip=<reason>`, `first_traversal=traversed=true app_visible=… skip=…`) and in the
   `ACOSLAUNCH MARK activity-resumed` line.
2. **The S3a protection moved to the no-frame path.** `acos-art`'s interactive startup loop reads
   `draw_skip=` from the marker. If a skip was reported and no live C1 frame
   (`c1_generation > 0`) arrives within `SKIPPED_TRAVERSAL_NO_FRAME_BUDGET` (2000 ms, named and
   documented as two 60 Hz frames and `<= sResumeReadyMs`), it kills the guest and answers
   `{"ok":false,"code":"POST_RESUME_NOT_READY","draw_skip":"…","first_traversal":"…"}` instead of
   waiting out the 60 s `NO_LIVE_FRAME` budget. A launch still requires a live frame, so a skipped
   draw is never certified. `POST_RESUME_NOT_READY` now maps to `NoFreshFrame`
   (`acos-cli/src/failure.rs`); it was previously an unregistered code.
3. **Parity bar for the A/B:** M4 strict/fast, M6 strict/fast within ±5 % of main (target equal);
   M5/M7/M8 unchanged. No `ADD_FLAG_APP_VISIBLE` retry, no redraw on the skip path.

**Probe evidence** (`results/bench/S3PERF-evidence/probe-{main,fix,fix2}.json`, 10/8/8 sequential
single-session counter launches): fix `forced_redraws=0`, `ready_path=main-identical`,
`draw_skip` evidence 10/10 markers, p50 **315.5 ms** vs main **315.5 ms** (+0.01 %) vs `int-s3ab`
**325.0 ms** (+3.0 %). 5/10 launches printed `first-traversal-skipped traversal:view_not_visible`
with `app_visible=true` at the check (the host callback had landed); the rest took the
`OnDraw`/`first-traversal-drew` branch.

**Unit tests** (`cargo test -p acos-art --lib skipped_traversal`, 6/6): budget fires at 2000 ms
(not 1999 ms), a live frame or a no-skip marker never expires, budget is >= 2 frames and <= 2 s,
marker fields parse, the failure reply carries `POST_RESUME_NOT_READY` + `draw_skip=`, and a source
guard forbids `redeliverAppVisibility` / `requestLayout()` / `invalidate()` / a
`dispatchAppVisibility` call / "requesting one redraw" in the guest.

**Running / remaining:** `.cache/s3perf2/run_ab2.sh` (setsid) alternates main and fix,
`acos-bench.sh --reps 1 --groups "a:latency:8G:300" --latency-reps 12 --no-build`, three reps per
arm, into `results/bench/S3PERF-evidence/ab/`; summarize with `ab/extract.py`. Then: crate tests
(`acos-cli`, `acos-art`, `acos-services`, `acos-verify`; run acos-art with `--test-threads=1
--skip refuses_to_run_when_no_cgroup_can_be_created` then that test alone with `--exact`), the
11-gate sweep (`results/bench/S3PERF-evidence/gates/`), Gaten scenarios/negatives, and the N=10
smoke `python3 tools/bench/s3ab_repro.py --acos <WT>/.cache/gr4-target/release/acos --out
.cache/s3perf2/n10.json --scratch .cache/s3perf2/n10 --n 10 --tasks 1 --reps 10 --animations on`
under `--mem 20G --time 590`.

---

# S3PERF (E2.16, previous attempt) — why the S3AB readiness hardening costs a frame (root cause + measured candidate)

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
