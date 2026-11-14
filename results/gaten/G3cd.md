# G3cd — Gaten launch SIGSEGV (G3d / R2a) and the thin first frame (G3c)

Debugger/implementer task. Worktree `/home/Qui/.paseo/worktrees/3oxa6xao/g3cd`, branch
`fix-g3cd-gaten-launch`, off `main` `11b917c`. Same read-only Gaten APK as GR1/GR2/GR3/R3
(`/home/Qui/Projects/acos/.cache/gaten/gaten-dev-f344272-x86_64-release-unsigned.apk`,
sha256 `e04c7ffc…`). Gaten source and APK untouched; every ART run through
`tools/art-runtime/run-exclusive.sh --task G3CD` + `tools/guarded.sh --mem 8G`, scratch in
`.cache/g3cd/`. No secrets read; the phone was not touched.

## Checkpoint 1 (2026-11-13 08:40, ~60 min in) — root cause of G3d named; fix in code; measurement queued

### What the R3 evidence actually says (symbolised)

The kept run tree R3 asked for already existed: `.cache/gr3/runs/tabsb-2/art-frames/`
(GR3's `gaten-tabs` loss). Its record, re-read here:

```
ACOSCRASH {"code":2,"fault_addr":"0x7256aecfb000","pid":2,"signal":11,
           "source":"acos-shim","thread":"com.gaten","tid":2}
Fatal signal 11 (SIGSEGV), code 2 (SEGV_ACCERR) fault addr 0x7256aecfb000
Thread: 2 "com.gaten"
    rax: 1   rbx: 0x80 (128)   rcx: 0x7256aecfaff8   r12: 0x7256aecfaff8
    r14: (buf)   r15: 0x7ffc58ad98f8 (end)   rsp: 0x7252483fe570
    rip: 0x7256aa6e7d45  ->  /apex/com.android.runtime/lib64/bionic/libc.so
Backtrace:
    #00 pc 000cfd45 libc.so (android_unsafe_frame_pointer_chase+117)
```

Symbolised against the image's own libc
(`.cache/art/sysroot/system/apex/com.android.runtime/lib64/bionic/libc.so`, BuildId
`fa337969c798946280caa45e2d71a2e7`, the same id the guest printed):

```
00000000000cfcd0 <android_unsafe_frame_pointer_chase>:
   cfd40: cmp  %rbx,%rax            ; num_frames vs num_entries (rbx = 0x80)
   cfd45: mov  0x8(%r12),%rcx       ; <-- fault: frame->return_addr, r12+8 == fault_addr
   cfd51: mov  (%r12),%rcx          ; frame->next_frame
```

So the faulting instruction is the frame-pointer chase reading `frame->return_addr`; `r12`
(the current frame record) is the **last 8 bytes of a mapped stack page** and `r12+8` is the
**guard page** below/above it: `SEGV_ACCERR` on a page-aligned address is what a `PROT_NONE`
VMA gives (a hole would be `SEGV_MAPERR`), i.e. the walk stepped onto a stack guard page.

Who calls that function at all? `android_unsafe_frame_pointer_chase` is `LIBC_PRIVATE` and
**the only reference in libc is one GOT slot**, loaded at `0x44900` inside
`MaybeInitGwpAsan` (`bionic/libc/bionic/gwp_asan_wrappers.cpp:258`,
`options->Backtrace = android_unsafe_frame_pointer_chase`). No library in the image or in the
Gaten APK imports it (`readelf -sW --dyn-syms` over the sysroot and all 20 Gaten `.so`s: only
libc defines it). **The crash is therefore inside GWP-ASan's sampled-allocation backtrace
capture.**

### Why the capture fails on this guest (mechanism)

`android_unsafe_frame_pointer_chase` (`bionic/libc/bionic/android_unsafe_frame_pointer_chase.cpp`)
walks `frame->next_frame` while validating it against `end = __get_thread()->stack_top`
(or the alternate-signal-stack top when `SS_ONSTACK`). It validates the *record* address only,
then dereferences `frame + 8`; a record in the last 8 bytes of the topmost stack page is
"in bounds" and the return-address read lands in the guard page. On x86_64 guest code built
without frame pointers (optimised libraries, JIT/AOT frames) the walk follows stale stack words,
so it does reach the top of the stack — and the end bound in this crash (`0x7ffc58ad98f8`) is not
this thread's stack at all (its `rsp` is `0x7252483fe570`), so the bound did not stop it either.
The function is documented as unsafe and "only for memory-safety tools".

### Reproduction (deterministic, on this branch)

GWP-ASan is enabled per process by libc's own lottery (`ShouldGwpAsanSampleProcess`,
`arc4random byte % 128 == 0`) unless someone sets `GWP_ASAN_*`. With process sampling forced on
(`ACOS_ART_GWP_ASAN=on`, forwarded to the guest as `GWP_ASAN_PROCESS_SAMPLING=1`) **every Gaten
launch dies with exactly R3's signature**:

| arm | n | result |
|---|---:|---|
| GWP-ASan forced on (`ACOS_ART_GWP_ASAN=on`) | 3 (+1 manual) | **4/4 SIGSEGV** `SEGV_ACCERR`, `android_unsafe_frame_pointer_chase+0x117`, thread 2 `com.gaten` — same registers shape (rbx=128, r12/fault = page boundary, end in 0x7ffc…) |

That is the causal proof the R3 run could not get: turn the sampled allocator on and the launch
dies 100 % in the named frame; nothing else in Gaten changed.

### Fix (committed on this branch)

`crates/acos-art/src/lib.rs::gwp_asan_guest_env` + one call site in
`crates/acos-art/src/launch.rs`: the guest gets `GWP_ASAN_PROCESS_SAMPLING=0` by default, so
GWP-ASan is never initialised for a guest (`MaybeInitGwpAsan` returns false on
`process_sample_rate == 0` before `arc4random` is consulted). `ACOS_ART_GWP_ASAN=on` forces it on,
an explicit host `GWP_ASAN_*` value is forwarded verbatim, and `ACOS_ART_GWP_ASAN=libc` forwards
nothing (libc's own lottery) — the pre-fix baseline, used for the before/after table.

Rationale: the guest must be deterministic. A 1-in-N random runtime mode change that adds a
sampling allocator whose backtrace capture is documented-unsafe on frame-pointer-free code is not
something a verification sandbox may inherit from a field-debugging feature.

### Still to do (checkpoint 1 ends here)

* before/after Gaten launch table at n≥30 each (queued on the ART lock),
* G3c thin-first-frame fix (measured already: 2 of 6 launches bound a **10-node** first frame
  while the same session had 243 nodes 2 s later — GR3 saw 1 node),
* Gaten scenario suite, gates, crate tests, EXPERIMENTS/Leaderboard/Backlog rows.

## Checkpoint 2 (2026-11-13 08:50, ~75 min in) — measured before/after; G3c fixed; suite+gates running

### Before/after, Gaten launches (`target/release/acos launch --engine art --keep --reference`)

One rep = launch → first frame (tree + PNG) → 2 s → tree → destroy. Loop
`.cache/g3cd/loop.py`; every rep through `run-exclusive.sh --task G3CD` + `guarded.sh --mem 8G`.
"thin" = the bound first frame was the 10-node window skeleton (no app text, React root empty);
the same session had 243 nodes two seconds later.

| arm | the only input that differs | n | pass | crash | thin first frames | launch p50 / p90 | host load1 | RAM |
|---|---|---:|---:|---:|---:|---|---|---|
| **before** (`ACOS_ART_GWP_ASAN=libc` — libc's own GWP-ASan lottery; pre-G3c first-frame code) | GWP-ASan: libc decides (1 in 128 processes) | 60 | 59 | **1** (`GUEST_EXITED`, exit 1, before the first frame — evidence lost, see §) | **20 / 59 (34 %)** | 2.336 s / 2.408 s | 1.8–4.7 | 12–15 GiB of 61.9 |
| **forced** (`ACOS_ART_GWP_ASAN=on`) | `GWP_ASAN_PROCESS_SAMPLING=1` | 3 (+1 manual) | 0 | **4/4 `SIGSEGV SEGV_ACCERR`** in `android_unsafe_frame_pointer_chase+0x117`, thread 2 `com.gaten` | — | 1.1–1.4 s (dies during launch) | 2.4–2.5 | — |
| **after** (default: GWP-ASan off + content-aware first frame) | `GWP_ASAN_PROCESS_SAMPLING=0` + G3c wait | 40 | **40** | **0** | **0** | 2.433 s / 2.579 s | 3.3–10.7 | 12–16 GiB |

The forced arm is the causal proof: enabling only libc's sampling allocator kills 4 of 4 launches in
the exact frame R3 recorded; disabling it (the fix) plus waiting for app content gives 40/40 with no
thin first frame. The cost of the G3c wait is the launch p50 2.34 → 2.43 s (+90 ms; a retry is one
extra strict traversal, ~1 s, on the ~1 in 3 launches that used to bind the skeleton).

The before arm's single loss is **not** the GWP-ASan signature: it is
`GUEST_EXITED … exit status: 1` **before** the first live frame, and its run tree was not kept
(there was no crash record to keep), so it stays unclassified — it is not counted as a G3d fix, and
it is the one open risk in the report.

### G3c fix (in code, measured above)

`crates/acos-cli/src/engine/art.rs`: `ArtSession::bind_first_frame` (used by both launch routes —
`SessionBackend::launch` for verify/native and `serve.rs::ensure_launch_tree` for
`acos launch`/`acos_launch`) waits past a **window skeleton** first frame (no text/content-desc/hint
anywhere and ≤ 40 nodes) for up to `ACOS_ART_FIRST_FRAME_MS` (900 ms) / 4 traversals, and:
* a retry that does **not** settle (a screen that keeps drawing, e.g. Gaten's welcome loop, or an
  engine error) rolls the launch frame *and* its settle verdict back to the last settled traversal,
  so the retry can never turn a passing launch into a `TIMEOUT`;
* a frame that never gains content is published with `first_frame.placeholder: true` and
  `frame.meaningful: false` (protocol rule: a placeholder never satisfies a UI assertion) when the
  tree is still the bare skeleton (≤ 16 nodes), and is logged, never silently claimed.
`serve.rs` publishes `result.first_frame` (`traversals`, `waited_ms`, `nodes`, `placeholder`) so the
wait is visible in the reply.

## Gaten scenarios (3 runs each, one scenario per `acos verify`, ART lock held)

| scenario | runs | result | wall p50 (from the run) |
|---|---:|---|---:|
| `scenarios/gaten-home.yaml` | 3 | **3/3 pass** | ~6 s |
| `scenarios/gaten-tabs.yaml` | 3 | **3/3 pass** (GR3 was 5/6 — the lost rep was the G3d SIGSEGV) | ~9 s |
| `scenarios/gaten-settings.yaml` | 3 | **3/3 pass** | ~11 s |
| `scenarios/gaten-search.yaml` | 3 | **3/3 pass** | ~9 s |
| `scenarios/gaten-lesson.yaml` | 3 | **3/3 pass** | ~10 s |
| **total** | 15 | **15/15 pass, 0 crashes, 0 false passes** | |

Negative controls (`scenarios/negative/gaten-{home,tabs,settings,search,lesson}-wrong.yaml`):
**5/5 fail `ASSERTION_FAILED`** — 0 false passes.

## Gates (one `acos verify <scenario> --engine art --json` each, under the ART lock)

| gate | expected | measured |
|---|---|---|
| `v2-art-counter` | pass | **pass** |
| `w22-counter-reset` | pass | **pass** |
| `w54-dialog-pixels` | pass | **pass** |
| `w42-back-frame` | pass | **pass** |
| `w15-dialog-menu` | pass | **pass** |
| `form-validation` | pass | **pass** |
| `suite/T03-render-good` | pass | **pass** |
| `v1-offscreen-pager` | pass | **pass** |
| `suite/T03-render-wrong` | fail `ASSERTION_FAILED` | **fail `ASSERTION_FAILED`** |
| `negative/offscreen-pager-visible` | fail `ELEMENT_OFFSCREEN` | **fail `ELEMENT_OFFSCREEN`** |
| `p11-progress-timeout` | fail `TIMEOUT` | **fail `TIMEOUT`** |

(`.cache/g3cd/suite/gate-*.json`, gitignored. Note: a gate scenario carries its **own** fixture APK;
`--apk` must not override it — the first gate pass was invalidated by exactly that and re-run.)

## What changed

| file | change | why |
|---|---|---|
| `crates/acos-art/src/lib.rs` | `gwp_asan_guest_env(host_env, force)` + unit test | one place that decides GWP-ASan for a guest: default `GWP_ASAN_PROCESS_SAMPLING=0`, `ACOS_ART_GWP_ASAN=on` forces it on, `libc` forwards nothing (pre-fix lottery), an explicit host `GWP_ASAN_*` value is forwarded verbatim |
| `crates/acos-art/src/launch.rs`, `app.rs` | 6-line call site each | apply that policy to the launch/session guest and to `render-app` |
| `crates/acos-cli/src/engine/art.rs` | `ArtSession::bind_first_frame` + `FirstFrameReport` + `first_frame_is_unmounted`/`first_frame_is_bare_skeleton`/`ui_tree_node_count`/`ui_tree_has_app_text` (unit-tested); both launch routes use it | G3c: wait past the window skeleton, roll back an unsettled retry, label a placeholder |
| `crates/acos-cli/src/engine/serve.rs` | publish `result.first_frame` (+ `placeholder`/`meaningful:false` on the frame) | the wait is visible in the reply, never silent |

`launch.rs` changes are deliberately minimal (no reset/relaunch, no idle-timer, no cgroup code):
S3ab/H4/K5/T1 own those files' neighbours.

## Commands

```bash
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export CARGO_TARGET_DIR=$PWD/.cache/g3cd-target
export ACOS_ART_BIN=$CARGO_TARGET_DIR/release/acos-art

# build
tools/guarded.sh --mem 16G --time 900 -- cargo build --release --offline -j 4 -p acos-cli -p acos-art

# before arm (libc's own GWP-ASan lottery), n=60
ACOS_ART_GWP_ASAN=libc tools/art-runtime/run-exclusive.sh --task G3CD -- \
  tools/guarded.sh --mem 8G --time 590 -- python3 .cache/g3cd/loop.py before60 60 --no-shot
# forced arm (GWP-ASan on), n=3
ACOS_ART_GWP_ASAN=on  tools/art-runtime/run-exclusive.sh --task G3CD -- \
  tools/guarded.sh --mem 8G --time 400 -- python3 .cache/g3cd/loop.py force3 3 --no-shot
# after arm (default), n=40
tools/art-runtime/run-exclusive.sh --task G3CD -- \
  tools/guarded.sh --mem 8G --time 590 -- python3 .cache/g3cd/loop.py after40b 40 --no-shot

# Gaten scenarios + negatives, gates
tools/art-runtime/run-exclusive.sh --task G3CD -- tools/guarded.sh --mem 8G --time 590 -- \
  bash -c '.cache/g3cd/run_suite.sh scenarios 3; .cache/g3cd/run_suite.sh negatives'
tools/art-runtime/run-exclusive.sh --task G3CD -- tools/guarded.sh --mem 8G --time 590 -- \
  bash -c '.cache/g3cd/run_suite.sh gates'

# crate tests (per crate, ART lock held)
tools/art-runtime/run-exclusive.sh --task G3CD -- tools/guarded.sh --mem 16G --time 580 -- \
  env ACOS_ART_DATA=$PWD/.cache/g3cd/artdata-tests cargo test --release --offline -j 4 -p acos-art
```

## Open risks

1. **A second, unnamed launch failure exists.** The 60-launch before arm lost one rep to
   `GUEST_EXITED … exit status: 1` *before* the first live frame — not a signal death, no crash
   record was kept (`native_crash` absent, so R3's retention rule correctly dropped the tree), and
   the loop did not snapshot the launcher's run tree at the time. It is a different class from the
   GWP-ASan SIGSEGV and is **not** claimed as fixed here; 0/40 in the after arm, but 40 is not
   enough to call it gone.
2. **The crash is removed, not the unsafe unwind.** A guest that deliberately enables GWP-ASan
   (`GWP_ASAN_*` in the host env, or `ACOS_ART_GWP_ASAN=on`) can still die in
   `android_unsafe_frame_pointer_chase`. bionic's function is unchanged (it is not ours).
3. **"Skeleton" is a measured heuristic.** Gaten's unmounted first frame is 1–10 nodes with no text;
   the wait uses ≤ 40 nodes and no text, the placeholder label ≤ 16 nodes. An app whose *real*
   first screen is text-free and that small pays one extra traversal and is labelled a placeholder
   (fail-closed, never a false pass). `result.first_frame` makes the decision visible.
4. **First-frame wait cost:** launch p50 2.336 → 2.433 s (+97 ms, +4 %) on the ~1 in 3 launches that
   used to bind the skeleton. The wait is capped by `ACOS_ART_FIRST_FRAME_MS` (900 ms) and 4
   traversals.
5. **Host-load interaction (found, not fixed):** the Gaten scenarios' `launch` step needs the guest's
   *strict* settle, and the onboarding screens re-arm a 200 ms auto-advance retry (GR3 §4.1). Under
   heavy load (load1 > ~8 with another agent's N=20/40 loop) the launch traversal can hit an
   animating welcome screen and the step fails `TIMEOUT (queue|newer-store)` — the open G3e owner
   decision, not a G3c regression (verified by A/B: the same scenario passes 3/3 on `main` and 3/3
   here once my settle-verdict regression was fixed — see §"verify launch settle" below).

### verify launch settle (a regression I introduced and removed)

Refactoring `launch()` onto `request_tree_with` made the launch step read the traversal's **settle**
verdict, which the pre-existing code never recorded for a launch (it sent a bare `{"cmd":"tree"}` and
`absorb`ed it). That turned every "welcome screen still animating" launch into
`TIMEOUT (newer-store)` — the whole Gaten suite failed 15/15 and every gate failed. Caught by A/B
against `main`'s binary on the same scenario and data dir (main 3/3 pass, this branch 3/3 fail), fixed
by restoring the pre-launch settle value in `SessionBackend::launch` (`P9` contract unchanged: a
launch is a *fresh* frame, never a settled one), after which mine 3/3 and main 3/3 pass.

## Checkpoint 3 (2026-11-14 07:40) — independent re-verification of `32baa78` (same source, fresh binaries, fresh logs)

Nothing was changed in code. Every run below is on branch head `32baa78`, release binaries rebuilt from
it (`cargo build --release --offline -p acos-cli -p acos-art`, "Finished … in 0.39s" = up to date), each
ART operation under `run-exclusive.sh --task G3CD` + `guarded.sh`, scratch in `.cache/g3cd/`.
The environment blocker recorded at checkpoint 2 is gone: the user systemd bus is reachable again
(`systemctl --user status` → `State: running`, `/run/user/1000/bus` present), so the S5 cgroup tests
were re-run properly (see below).

### Crate tests

| crate | result | log |
|---|---|---|
| `acos-cli` (`-p acos-cli`) | **147 passed, 0 failed**, rc=0 | `.cache/g3cd/test-acos-cli-2.log` |
| `acos-services` (`-p acos-services`) | **302 passed, 0 failed**, rc=0 | `.cache/g3cd/test-acos-services-2.log` |
| `acos-verify` (`-p acos-verify`) | **66 passed, 0 failed**, rc=0 | `.cache/g3cd/test-acos-verify-2.log` |
| `acos-art` (`-p acos-art`, one process) | 127 passed, **1 failed** — `sandbox_s5::allocator_bomb_…` (see below) | `.cache/g3cd/art-tests-065834.log` |
| `acos-art` `sandbox_s5`, **one process per test** | **7/7 pass, rc=0 each** | same log, `S5 rcs: 0 0 0 0 0 0 0` |
| `acos-art` doc-tests | 0 doc-tests, rc=0 | same acquisition |

**The one `acos-art` failure is a test-process/environment collision, not this branch.** In the
single-process run the S5 binary runs its tests in parallel, and `allocator_bomb…` loses the race to
`fork_bomb_is_bounded_by_pids_max`, which deliberately exhausts the pid budget (48 `/bin/sh: fork:
retry: Resource temporarily unavailable` lines in the same window). `RunCgroup::prepare`'s
`systemd-run --user --scope` probe then fails
`CgroupUnavailable("… status Some(1)): Failed to connect to bus")` and the test panics at
`sandbox_s5.rs:170` — the *same* environmental message as checkpoint 2, now shown to be provoked by
the sibling test's fork storm rather than by a dead user bus. With each test in its own process (cold
`OnceLock`, fresh `ACOS_ART_DATA`) all seven pass, including the two cgroup-bounded bombs.

Second, pre-existing test-isolation defect found while doing this (not introduced here, `cgroup.rs` is
byte-identical to `main`): `host_scope_capability()` caches its probe in a per-process `OnceLock`
(H15), so `refuses_to_run_when_no_cgroup_can_be_created` — which fakes a failing `systemd-run` on
`PATH` — only passes when it is the first probe in the process. Serialised after another test it fails
(`RunCgroup { cg: "" }` returned). That is a test-design issue in the repo, not a product defect; it is
untouched here (no assertion was changed, nothing was `#[ignore]`d) and is reported for its owner.

### Gates (one `acos verify` each, `.cache/g3cd/suite3/gate-*.json`)

| gate | expected | measured |
|---|---|---|
| v2-art-counter, w22-counter-reset, w54-dialog-pixels, w42-back-frame, w15-dialog-menu, form-validation, suite/T03-render-good, v1-offscreen-pager | pass | **8/8 pass** |
| suite/T03-render-wrong | `ASSERTION_FAILED` | **`ASSERTION_FAILED`** |
| negative/offscreen-pager-visible | `ELEMENT_OFFSCREEN` | **`ELEMENT_OFFSCREEN`** |
| p11-progress-timeout | `TIMEOUT` | **`TIMEOUT`** |

### Gaten flows (`.cache/g3cd/suite3/`)

`scenarios/gaten-{home,tabs,settings,search,lesson}.yaml` × 3 runs: **15/15 pass**.
`scenarios/negative/gaten-*-wrong.yaml`: **5/5 `ASSERTION_FAILED`** → 0 false passes.

### Launch stability, 30 launches (`.cache/g3cd/loop/stab30/reps.jsonl`)

| n | pass | crash | 1-node/blank first frame | content frame (243 nodes) | labelled placeholder (10–15 nodes) | launch p50 / p90 | load1 |
|---:|---:|---:|---:|---:|---:|---|---|
| 30 | **30** | **0** | **0** | 21 | **9** | 2.309 s / 2.430 s | 1.55–2.0 |

* 0 `SIGSEGV`, 0 `GUEST_EXITED`, no kept crash tree, no `native_crash` record in any rep.
* **Honest qualification of checkpoint 2's "0 thin first frames":** in this arm 9/30 launches bound the
  10–15-node window skeleton, but every one of them was published as a **labelled placeholder**
  (`result.first_frame.placeholder: true`, `frame.meaningful: false`, `traversals` 2–3, `waited_ms`
  ≈1.63 s; raw envelopes `.cache/g3cd/loop/stab30/rep{1,10,14,15,19,20,21,24,27}.json`), and the same
  sessions had 243 nodes when observed 2 s later. So the frame is never *silently* thin and can never
  satisfy an assertion, but the content-aware wait did **not** reach the painted frame in 30 % of this
  arm (checkpoint 2's after-arm: 0/40). The 900 ms/4-traversal budget is the binding constraint: two
  traversals cost ≈1.63 s here, so the retry lands before Gaten's first paint. Latency cost of the
  placeholder path is visible but small (2.39–2.66 s vs 2.29–2.33 s). This does not change the fix or
  the honest direction (fail-closed), it bounds what the G3c heuristic buys.

### Diff since `32baa78`

Documentation/results only (this section + the `EXPERIMENTS.md` re-verification bullet). No source,
test or gate change.
