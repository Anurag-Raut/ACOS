# PERF4 — v0.3.0 M8 tap→settled regression: cause, fix, evidence

Branch `perf4` from `rel-030` (`5a912d4b`, the RELGATE2 tree), merged with
`rel-030` at `a46fcbae` (LOOPERWAKE `381ec7ad`, GATEFIX `8f8919f4`) before the
final numbers. Worktree
`/home/Qui/.t3/worktrees/acos/t3-5cf7912d-cec9a432baa8a011`.

**Status: the two M8 regressions are gone — both fast metrics are now better
than v0.2.2.** Indicative shared-slot A/B (one shared slot per acquisition, no
`--exclusive`; the release-gate A/B is the bench owner's), 2 reps × 20 strict / 40
fast samples per rep, `05-ab-strict-cadence/ab-compare.txt`:

| metric | v0.2.2 | perf4 | delta | ADR-0028 verdict |
|---|---:|---:|---:|---|
| M8 tap→settled, **counter (fast)** | 37.11 | **24.85** | −12.26 (−33.0 %) | improved (first slot 21 %→50 %) |
| M8 tap→settled, **form (fast)** | 32.39 | **29.69** | −2.71 (−8.4 %) | improved (first slot 100 %→100 %) |
| M8 counter (strict) | 946.19 | 951.81 | +0.6 % | within-noise |
| M8 form (strict) | 927.58 | 927.89 | +0.0 % | within-noise |
| tap acknowledgement | 5.00 | 5.00 | 0 | within-noise |

The 5-rep run (`03-ab/`, same driver, 200 samples/arm) was taken on the earlier
variant that also made the *strict* settle loop eager, and shows the same M8
picture (counter 45.96 → 24.85, −45.9 %; form 32.25 → 29.72, −7.9 %; M4 −10.9 %,
M6 −10.2 %, M7 strict −22.9 %) plus the one row that made me revert that part:
`cpu_active_tap_strict_pct` +46.3 % there vs +10.6 % now (see §2.2).

## 1. What was measured, not guessed (`diagnosis/README.md`)

The RELGATE2 numbers (`results/release/v0.3.0-gate/timing/02-ab/`) split into
fields that all three candidate areas can be judged on. M8 form fast, n = 120 per
arm, from the raw `samples.extra`:

| field | v0.2.2 | v0.3.0 RC | delta |
|---|---:|---:|---:|
| `ack_ms` (guest's own publish→FINISHED) | 5 | 5 | 0 |
| `first_change_ms` (publish→changed probe sample) | 15.79 | 14.77 | −1.0 |
| `frame_ms` (publish→the **bound** C1 store) | 20.16 | 29.23 | **+9.07** |

So neither the ack bookkeeping nor the probe was the cost: the reply waited for a
later C1 store. Two independent measurements explain it.

**(a) A full-display composite before every tap: 8.7–9.2 ms.** Instrumenting the
action path (`diagnosis/tap-split-rel030.txt`) puts the whole delta between
`publish_begin` and the acks:

```
PERF4 stamp removal_mark_us=0
tap publish_begin at (157,616) ack_wait_ms=2500
PERF4 stamp entry_fnv_us=8989 none=true        <-- REL3H's pre-publish composite
tap acks_returned total_ms=14 ...
```

`entry_fnv` (REL3H's fallback pre-action pixels) called
`live.compositor.capture_latest()` — every visible layer's buffer copied and
fnv1a-hashed, 1080×2400 — **before every input whose caller had no capture of its
own**, i.e. every tap after a `pixels:false` reply. The bench's M8 form loop
(`acos_reset{fast}` → `acos_tap{fast}` → `observe{strict}`) is exactly that
shape, and so is any agent that does not ask for pixels. Five consecutive taps:
8989, 9141, 9045, 9203, 8741 µs.

**(b) The settle loops re-read state on a fixed sleep.** 15 ms in the fast loop,
10 ms in the strict one, 2 ms in the fast-tree wait. A store that landed just
after a tick waited a whole quantum, and `frame_ms`/wall were quantized to it: the
*same build* resolved `frame_ms` 16 or 29 depending on the phase
(`diagnosis/tr-fix2` vs `tr-fix3`). With (a) fixed, the ack came back at 5 ms and
the shipped rule (`a C1 store observed after the sample`) then waited for the
*following* frame, because a draw's own buffer is stored before the probe sample
that describes it is parsed.

Suspects ruled out by measurement: the ack bookkeeping (`ack_ms` 5 in both arms,
and `InputHost::collect` finishes in 5.07 ms in both, `tap-split-v022.txt` vs
`tap-split-rel030.txt`); the hit-test / `input.target` and the MERGE3I3
precedence code (all host-side after the bind, and the diff at `e4fb2424` is at
parity with v0.2.2 in the same slot); the delivery wake (REL3I3) — the peer loop
wakes on the frame fd, and the guest's own FINISHED latency is identical in both
arms; `UnhandledSettleGate` (strict-only, never on a fast tap).

## 2. Fix (no reply field dropped, no settle rule weakened)

1. **`entry_fnv` is now taken after the acks, and only when it can be read at
   all.** The unhandled-input branch is the only consumer, and it can bind only
   when *no* C1 store landed after the publish — so with `latest_generation() ==
   g0` the composite still is the pre-action screen, byte for byte. When a store
   did land, the pre-action screen is gone and the old comparison would by
   construction have reported a change; the branch reports that conservative
   `true`. Fast taps, handled gestures and callers with their own capture take no
   capture at all.
2. **The fast settle waits on the session's own change signal** (`ProbeWake`,
   already signalled by the guest-line reader for a probe sample; the peer loop
   now also signals it after applying a guest transaction — the C1 store). A
   10 ms liveness bound replaces the poll for a silent guest.

   The **strict** loop keeps its 10 ms cadence, on measurement: making it eager
   too cost 20 points of active CPU for nothing on the clock — that rule binds on
   the 150 ms quiet and the 24 ms confirmation windows, not on observation order,
   and every binder transaction of an animating screen re-reads the tree.
   `cpu_active_tap_strict_pct` per rep p50, 5 reps: v0.2.2 45.5/45.0/44.0/43.9/45.0
   vs eager 65.0/65.9/65.7/65.8/66.5. With the cadence kept it is 43.7/45.4 vs
   v0.2.2's 37.3/43.2 (2 reps) — not separable from the run-to-run spread of the
   same arm at that sample size, so the final exclusive A/B decides. If it lands
   above the ±10 % row, the smallest next action is to signal the change only when
   the C1 store counter advanced (pass the compositor handle into the peer loop
   the way the wake already is) instead of after every applied transaction.
3. **The fast rule accepts the sample's *own* draw** as the bound store:
   `generation >= draw_seq` (every draw stored — the sample reports C1's store
   counter and the guest's `OnDraw` counter), `last_change_ms >= draw_ms` (the
   store arrived after that draw's own clock) and `draw_seq > baseline.draw_seq`
   (the draw is newer than the pre-publish one). The tree and the bound frame then
   describe one UI state, which is what the observation-order rule approximated;
   anything the equality cannot prove falls back to the shipped rule unchanged, so
   it can never bind a sample taken before the action (test:
   `a_fast_commit_binds_its_own_draw_only_with_proof`).

## 3. Memory, N = 10 (item 3)

Not a leak and not an N = 10 phenomenon. From the release-gate memory samples
(3 reps, 60 s windows, `run-*/rep-*/memory.json`), the RC's per-session anon+shmem
is higher at **every** N; N = 10 only trips the *relative* ±3 % gate because
v0.2.2's per-session value is smallest there:

| N | v0.2.2 per-session (Med of 3 reps) | perf4-RC | delta | gate ±3 % |
|---|---:|---:|---:|---|
| 1 | 53.81 | 55.19 | +1.38 (+2.5 %) | inside |
| 5 | 54.14 | 55.35 | +1.21 (+2.2 %) | inside |
| 10 | 54.01 | 55.77 | +1.76 (+3.3 %) | **outside** |

The same +1.5 MiB/session shows in the guest PSS (`session_pss_mib`: 538.74 →
554.10 at N = 10, i.e. +1.54 MiB/session) while the host's share (`sum_pss_mib` −
`session_pss_mib`) is flat, so the extra memory is **inside each guest process**,
uniform per session — the v0.3.0 feature work touching more framework code per
guest (IME `InputEventSender` + the real touch chain), not a per-session buffer,
ring or fd in the host and not a growth over the 60 s sample window beyond what
v0.2.2 shows (both arms drift ~+4–6 MiB at N = 10; the reported value is the
median). It is recorded here as a ≤2 % absolute cost of the new features; the
smallest next action, if the owner wants it gone, is a per-guest `smaps_rollup`
diff to name the mapping (one ART run, ~1 min), not a code hunt.

## 4. Proof

* `cargo test --release -p acos-protocol -p acos-verify -p acos-art -p acos-cli`
  — 50 suites, **792 passed, 0 failed** (`02-tests/cargo-test.log`), including
  the new `a_fast_commit_binds_its_own_draw_only_with_proof` and
  `the_change_wake_is_event_driven_and_loses_no_edge`.
* ART scenarios on the release binary (`04-scenarios/`, `run-scenarios.sh`,
  driver log `04-scenarios-driver.log`):

  | scenario | result |
  |---|---|
  | `rel3h-unhandled-input` (tap nothing consumes) | **pass** 4/4 |
  | `rel3h-unhandled-input-expect-change` (twin: must fail) | **fail `ASSERTION_FAILED` / `reason: expect_change`**, `settle.bound: unhandled-input`, `pixels_changed: false` — a *measured* false, so `expect_change: true` still cannot pass on a tap nothing consumed (this is the negative that would break first if the lazy `entry_fnv` had turned into a hard-coded `true`) |
  | `rel3h2-expect-handled-false` | pass 4/4 |
  | `rel3h2-expect-handled-true` (twin: must fail) | **fail `INPUT_NOT_HANDLED`** |
  | `touchdisp-posted-side-effect` | pass 5/5 |
  | `touchdisp-gesture` | pass 3/3 |
  | `form-validation` | pass 28/28 |
  | `back-layers` | pass **22/22** |
  | `back-layers-ime` | pass **22/22** |

  The three fixture APKs (`tap-targets`, `touch-probe`, `back-layers`) are not in
  the main checkout's `fixtures/build` (RELGATE2 F3); they were built with
  `ACOS_APP_ONLY="tap-targets touch-probe back-layers" tools/build-apps.sh`
  (GATEFIX's path) before the run.
* Indicative A/B: `05-ab-strict-cadence/` (2 reps, the shipped variant) and
  `03-ab/` (5 reps, the eager-strict variant), both ADR-0028 sample counts, one
  shared slot per acquisition.
* `rel-030` was merged twice before the final numbers: `381ec7ad` (LOOPERWAKE) and
  `8f8919f4` (GATEFIX). GATEFIX's settle change is the launch first-frame wait
  (`crates/acos-cli/src/engine/art.rs`: `first_frame_placeholder_verdict` +
  `window_spent`), which no action path reaches: `acos_tap` goes
  `ArtSession` → `wait_for_settled_observation` and never runs a first-frame
  wait. The M8 numbers above were re-measured on the merged tree
  (`diag-form-merged.json` 29.01 vs 32.52, `diag-counter-merged.json` 24.90 vs
  30.76, 90 taps/arm interleaved) and by the A/B in `05-ab-strict-cadence/`.

## 5. Files

* `crates/acos-art/src/launch.rs` — the lazy `entry_fnv`, the fast settle's
  change-signal wait, `sample_own_draw_is_stored` + its test, `ProbeWake` test,
  the peer loop's `change.notify()`.
* `p4probe.py`, `p4trace.py`, `run-ab-shared.sh`, `run-scenarios.sh` — the
  harnesses (PERF3's `perf3_diag.py` extended with the reply's own
  `timing.input` split).
