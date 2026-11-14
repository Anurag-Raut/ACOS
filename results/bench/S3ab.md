# S3ab — the two reset/relaunch defects from S3 (S3a, S3b): final report

**Branch:** `fix-s3ab-reset` (off `main` `6301c06`)
**Date:** 2026-11-14
**Final binary:** `.cache/s3ab-target/release/acos`
sha256 `6a758637203350e9b981aa8c0caf6fb9145193a8b9c4ceac59917b05471dfd98`
(`acos-art` sha256 `e7d09554e64285b4258eda3958ebc1d7bb77cfcf135f8de1305d3a950e6057a7`)
**Status:** S3b class A root-caused and fixed; S3b class B **mitigated** (bounded host-transport
retry, first failure preserved, retry path now proven to fire) with the **root cause still open**;
S3a is a **reason-based hardening with no reproduced occurrence** (the pixel gate that rejected the
earlier naive fix passes). Nothing here is a speculative fix: every artefact below is measured.

This file replaces checkpoint 1, which described an `ADD_FLAG_APP_VISIBLE` experiment that was
**reverted** (it regressed `suite/T03-render-good`). Do not re-apply it (see §5).

---

## 1. The defects

S3 (`results/bench/S3.md`, E3.4) found two stochastic reset/relaunch failures at N=10/20:

* **S3a** — `acos_reset` relaunches, the Activity resumes, but no live frame exists for 60 s
  (`no live frame within 60s: activity_resumed=true probe=true c1_generation=0`).
* **S3b** — the relaunched guest exits 1 in 133–301 ms before its first live frame
  (`GUEST_EXITED: the guest ended before its first live frame (status exit status: 1)`).

S3b turned out to be **two unrelated classes**:

* **class A** — a *host task-count limit*: the guarded scope's `TasksMax=512` was exhausted by a
  N=20/40 cohort, the vsync writer thread could not spawn (`EAGAIN`), the guest's
  `DisplayEventReceiver` init failed with `-ENODEV`, `Choreographer` threw and the guest exited 1.
* **class B** — a *host transport* defect: the guest could not parse the `addToDisplayAsUser`
  reply (`E/Parcel: ipcSetDataReference: unsupported type object (7209057) at offset 16`,
  `RemoteException ... Unknown exception code: -1040187388`, `Adding window failed`) and exited 1
  after `activity_resumed=true`.

## 2. What changed

| File | Change | Class |
|---|---|---|
| `tools/guarded.sh` | `TasksMax` is configurable, default **8192** (was hard-coded 512) | S3b-A (root fix in the harness) |
| `crates/acos-services/src/display_events.rs` | vsync-thread spawn retries `WouldBlock`/`OutOfMemory` (20 × 10 ms), logs the attempt and names it in the final error | S3b-A (defence in depth, product side) |
| `crates/acos-cli/src/engine/art.rs` | reset retries **once** only for named host-transport strings; the first attempt's `session-exit.json`/`live-session.json`/`session-timing.json`/… are copied to `<artifacts>/relaunch-retry-1/` before the retry; `relaunch_retry` carried on the reply | S3b-B (mitigation) |
| `crates/acos-cli/src/engine/serve.rs` | `acos_reset` reply states `relaunch_retry` when a retry happened | S3b-B (never a silent pass) |
| `crates/acos-art/guest/java/dev/acos/art/LaunchMain.java` | `awaitFirstTraversal` requires **draw evidence** (framework skip-reason fields); on a skipped draw it records the reason, replays `dispatchAppVisibility(true)` only when the reason is `not_visible`, forces one redraw and waits for `OnDrawListener`; a draw that never comes is `POST_RESUME_NOT_READY` with `draw_skip=…` instead of a 60 s hang | S3a (hardening) |
| `crates/acos-art/src/launch.rs` | `NO_LIVE_FRAME` carries a C1 snapshot (`c1_layers/transactions/retained/destroyed`, per-layer `tx/gen/buf/hidden/placed`) plus probe age/seq/gen | S3a (evidence for the next occurrence) |
| `tools/bench/s3ab_repro.py` | targeted reproducer: P5 loop in short reps, keeps the failing session's small evidence, deletes PNGs after each rep; `relaunch_retries` now counts retry **events** (one entry per event, not two) | harness |

## 3. Does the mitigation actually fire? (the brief's first requirement)

Two independent proofs, both committed:

1. **Synthetic, deterministic, committed test** —
   `crates/acos-cli/src/engine/art.rs::reset_retries_a_host_transport_relaunch_once_and_preserves_the_first_failure`.
   A fake `acos-art` whose **second** spawn (the first reset attempt) writes the N=20 class-B
   `session-exit.json` and exits 1 before its ready line; the third spawn is the retry.
   Asserts: the reset succeeds **and** three real spawns happened, `relaunch_retry.attempts == 2`,
   the reason names the binders/window-add class, `first_failure` still says
   `LIVE_FRAME_UNAVAILABLE`, `relaunch-retry-1/session-exit.json` is **byte-identical** to the
   injected first failure, and the retried guest serves a newer frame. A second test
   (`reset_does_not_retry_an_app_startup_wall`) proves an app wall (`Application onCreate threw`)
   gets **no** second spawn, no retry record, and a visible `LIVE_FRAME_UNAVAILABLE` error.
   Output: `results/bench/S3ab-evidence/retry-path-test.log` (`2 passed; 0 failed`).
2. **Natural, in the matrix** — the retry fired **twice in 3 000 tasks**, once at N=20 and once at
   N=10, with the same class-B signature as the original incident, and both resets recovered:
   * `results/bench/S3ab-evidence/repro-n20-binder-parcel-retry-2026-11-14/`
     (`relaunch-retry.json` reason `host transport: malformed binder reply parcel`;
     `relaunch-retry-1/session-exit.json` = guard OK, `GUEST_EXIT_AFTER_RESUME`,
     `activity_resumed=true`, `Adding window failed`, `Unknown exception code: -1040187388`)
   * `results/bench/S3ab-evidence/repro-n10-binder-parcel-retry-2026-11-14/` (same shape)
   Both were counted as **verified only because the retried guest produced a live frame**; the
   first failure is preserved verbatim in every case.

## 4. Measured results (final binary, full gates, full A/B matrix)

### 4.1 A/B reliability matrix — `tools/bench/s3ab_repro.py`, `--tasks 1` (a failing reset is the last action)

All under `run-exclusive.sh --task S3AB` + `guarded.sh --mem 24G --time 590`.
Summaries: `results/bench/S3ab-evidence/matrix/<arm>.json`.

| arm | resets attempted | verified | false passes | failure classes | relaunch retries |
|---|---:|---:|---:|---|---:|
| N=20, animations on, 30 reps | 600 | **600** | 0 | none | **1** (recovered; evidence above) |
| N=10, animations on, 60 reps | 600 | **600** | 0 | none | **1** (recovered; evidence above) |
| N=20, animations off, 30 reps | 600 | **600** | 0 | none | 0 |
| N=10, animations off, 60 reps | 600 | **600** | 0 | none | 0 |
| N=40, animations on, 15 reps (class-A stress, `TasksMax=8192`) | 600 | **600** | 0 | none | 0 |
| **total** | **3 000** | **3 000 (100 %)** | **0** | — | **2 (both recovered)** |

Host: MemAvailable 45–46 GiB (host used 24 %, never near the 90 % ceiling), load1 1.9 → 8.2.
No reset that failed was counted as verified: the harness counts `verified` only from a live-frame
assertion on the post-reset task, and `false_pass` stayed 0 in every rep.

**S3a did not occur in any of these 3 000 resets** (nor in >3 000 earlier tasks, §7 of the
handoff): 0 `NO_LIVE_FRAME`. It is not claimed as reproduced or as proven fixed.

### 4.2 Crate gates (final binary, `guarded.sh --mem 16G --time 900`, `cargo test --release --offline`)

| crate | result | log |
|---|---|---|
| `acos-art` | rc 0, all suites `0 failed` (98 + 0 + 8 + 4 + 6 + 1 + 2 + 7 + 5 + 3 + 5 + 5 + 4 + 0) | `.cache/s3ab/gates-acos-art.final2.log` |
| `acos-cli` | rc 0, all suites `0 failed` (89 + 6 + 0 + 4 + 1 + 2 + 7 + 1 + 11 + 2 + 4 + 4 + 7 + 3 + 5 + 3) | `.cache/s3ab/gates-acos-cli.final.log` |
| `acos-verify` | rc 0, 43 + 23 `0 failed` | `.cache/s3ab/gates-acos-verify.final.log` |

A first `acos-art` attempt failed one test (`allocator_bomb_is_oom_killed_inside_the_scope_and_the_host_is_untouched`)
with `systemd-run --user --scope ... Failed to connect to bus` — a transient host user-bus flake
(the same command passes directly and in the rerun). Rerun rc 0; the failing log is kept as
`.cache/s3ab/gates-acos-art.final.log` for honesty.

### 4.3 The 15 gate scenarios (final binary, `--engine art`, under lock+guard)

Results in `.cache/s3ab/gates-scenarios-final/`.

* pass, as expected: `v2-art-counter`, `w22-counter-reset`, `w54-dialog-pixels`, `w42-back-frame`,
  `w15-dialog-menu`, `form-validation`, **`suite/T03-render-good`** (the pixel gate that rejected
  the reverted add-flag experiment), `v1-offscreen-pager`, `p11-background`, `p11-delay1000`,
  `p12-progress-anim-off`.
* fail as designed (never a pass): `suite/T03-render-wrong` → `ASSERTION_FAILED`;
  `negative/offscreen-pager-visible` → `ELEMENT_OFFSCREEN`; `negative/offscreen-pager-wait` →
  `TIMEOUT`; `p11-progress-timeout` → `TIMEOUT` (non-pass).

## 5. Dead ends — do not repeat

* **`ADD_FLAG_APP_VISIBLE` in the add reply.** Made the first traversal draw, but before
  focus/visibility settled; `suite/T03-render-good` regressed by 945 compressed PNG bytes
  (~198k pixels; missing EditText caret, different hint colour). Reverted. The guest-side
  replay is done **after** the first traversal for exactly this reason.
* `tools/bench/acos-bench.sh` loop cells delete the failing session's evidence in `finally`
  (even with `--keep-scratch`) — use `tools/bench/s3ab_repro.py`.
* A 3 000-task single-serve P5 loop writes ~27 GiB of PNGs and never writes its JSON.
* N=40 (even with the fixed limit) is **not** a substitute for the N=20 cohort: class B was
  1/600 at N=20 and 0/600 at N=40 in this matrix.
* A longer 60 s startup budget only hides S3a; the fix must be draw evidence + recovery.

## 6. What is still open

1. **S3b class B root cause.** The leading hypothesis is unchanged: `0x6e0061` (7209057) looks like
   two UTF-16 characters of the `InputChannel` name read where a binder object type belongs, i.e.
   the object-table/data alignment of the `addToDisplayAsUser` reply is wrong
   (`crates/acos-binder-peer/src/peer.rs`, `frame_from_parcel`). Not investigated further here:
   shipping a speculative binder/parcel fix is out of scope for this task. The new evidence
   directories above are the best reproduction material (they are a *natural* occurrence with the
   full guest stack).
2. **S3a has no end-to-end reproduction.** The hardening is reason-based (AOSP
   `ViewRootImpl.performTraversals` sets `mFirst=false` before the draw decision). The next
   occurrence will carry a C1 snapshot and a `draw_skip=` reason. A reviewer may want an
   off-by-default guest test seam that forces a draw-skipping first traversal; not built here.
3. **The retry classification is string-based and broad** (`adding window failed`,
   `display event receiver`, `ipcsetdatareference`, …). An app that prints one of those strings to
   its stderr could be retried once. The unit tests pin "app wall is not retried", but tightening
   it (e.g. require `guest_exit==1`, `guest_signal==None`, and the string in the guest logd only)
   is a reasonable review follow-up.
4. **`TasksMax=8192` is a harness/environment fix.** A product running under a 512-task cgroup
   would still hit class A; `display_events.rs` now retries the thread spawn, but a graceful
   host-thread-limit story (`HOST_THREAD_LIMIT`) is not implemented.
