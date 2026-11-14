# LHFLAKE — launch_hang's 5 s bound was timing cold-cache work

Branch `test-launch-hang-cold` (from `main` cb5bd04). Test-only change: no product
code, no behaviour change.

## The problem

`crates/acos-art/tests/launch_hang.rs::guest_exits_before_milestone_returns_exit_code_and_cleans_up`
asserts that a launch which finds its guest already dead reports that within 5 s.
Measured on `main` 2026-11-14: the **first** run after a build took 4.3–11 s (fails),
every later run 0.25–0.29 s. Nothing about the launcher differed between those runs;
only the caches were cold.

## Cause (proved from the product's own stderr, no product change)

`acos-art` already announces every cache decision it makes, so the split is visible
without instrumentation. Manual repro of the same fixture (`--data <seeded copy>`,
`--apk counter.apk`, `--until application-created`, `CC` wrapper that deletes the
Landlock plan), one cold run and one warm run of the *same* command line:

| t (s), cold | t (s), warm | line |
|---|---|---|
| — | 0.003 | `build-cache HIT 73825cfe` (launch-guest `classes.dex`, javac+d8) |
| 1.584 | 0.004 | `build-cache HIT 6b1a5f69` (`libacosart.so`) |
| **1.584** | 0.125 | `build-cache STORE 73825cfe` — the launch guest was **rebuilt**: ~1.58 s of javac+d8 |
| 1.601→1.669 | 0.048→0.125 | `libartshim.so` clang `MISS … built in 84 ms` |
| 1.672 | 0.128 | static overlay idmap tree `HIT` |
| **1.673 → 5.457** | **0.130** | `dex2oat64 (35 boot jars) … rc=Some(0) in 3784 ms` |
| 5.538 | 0.130 | `boot_image: … cache=built … prep_ms=3784` (warm: `cache=hit`, no `prep_ms`) |
| 5.604 | 0.198 | guest seccomp guard clang `MISS … built in 65 ms` |
| 5.634 | 0.227 | `launch wall=24ms … application_created=false` + `seccomp: ERR landlock=plan-missing` |
| **5.635 total** | **0.228 total** | process wall (rc 1 both times) |

The extra seconds are, in order: the shared **boot image** built by `dex2oat`
(3.78 s, `prep_ms=3784`, cached under `$ACOS_BOOT_IMAGE_CACHE`/`~/.cache/acos/art/boot-images`
and keyed by an input digest, so a fresh checkout or a new sysroot copy is a cold
build), the **launch-guest javac+d8** build (~1.6 s, `build-cache STORE`), and two
clang builds (~0.2 s). The guest itself is dead at 5.63 s in the cold run and at
0.23 s in the warm run; the launcher's own view of the window is `wall=24ms` /
`wall=22ms` — i.e. the product already returned promptly. The 5 s bound was
measuring the build cache, not the S6 property.

## What the bound measures now

* `warm_fixture()` runs the **same fixture** once, in a **throwaway seeded `/data`
  copy of its own**, under `WARM_DEADLINE` (180 s) and with **no timing
  assertion** — only "the launcher returned at all". Cold-cache cost (javac/d8,
  dex2oat, clang, page cache for the 860 MB sysroot) is absorbed there.
* The measured run is unchanged and timed from process spawn to exit, so the
  5 s `PROMPT_RETURN_BOUND` now covers "guest is already dead → launcher
  returned", which is exactly the S6 property.
* The warm-up **must not** reuse the timed run's `/data`: the guest seccomp
  guard's build-cache key covers the `CC` path (`file_id(cc)`) and the Landlock
  plan (`landlock=…`), so warming in the same directory made the timed run hit a
  guard `CC` had never compiled — the fixture silently disarmed itself and the
  run `ok`ed with rc 0. That is why the warm-up gets its own directory and its
  own `CC` wrapper; the timed run's guard is still a genuine `CC` build and still
  exits 126 with `landlock=plan-missing`.
* Nothing was weakened: the hard per-run deadline is unchanged, the warm-up still
  fails on a launcher that never returns, and a new unit test
  `promptness_bound_still_bites` calls the pure `prompt_bound_violation()` helper
  and asserts that 4.29 s passes, 5 s + 1 ms fails, and the 11 s cold-run figure
  fails — so if the cold cost ever leaks back into the measured window (or the
  bound is turned into a no-op) the suite goes red again. `cold_cache_evidence()`
  quotes the `build-cache`/`prep_ms` lines in a bound failure so the next reader
  can tell cold prep from a slow launcher immediately.

Also warmed: `ordinary_launch_to_application_created_stays_fast` (its `wall <= 2000 ms`
figure comes from the product's own timer, which already excludes prep, but its
30 s deadline would otherwise absorb a cold `dex2oat`).

## Other tests with wall-clock bounds

`crates/acos-art/tests/` was audited for the same pattern:

* `launch_hang.rs` was the only file with a *promptness* bound over the whole
  process (5 s). Fixed as above.
* `cleanup.rs`, `memory_caps.rs`, `r3_crash_evidence.rs`, `sandbox_s5.rs`,
  `sandbox_s6.rs`, `w31_tree_evidence.rs`, `w37_build_cache.rs`,
  `w39_stage_cache.rs`, `w43_session_exit.rs` use wall clock only as generous
  hang deadlines (10–180 s) or as poll budgets, and assert on RSS/outcomes
  rather than on return latency. No cold-start pattern, no change needed.

## Evidence

### Cold-forced, five runs in a row (first one included)

Caches forced cold **without touching any shared cache**: the run gets private
`ACOS_ART_BUILD_CACHE` and `ACOS_BOOT_IMAGE_CACHE` directories under
`.cache/lh/cold/` (emptied before iteration 1), so the build cache, the boot
image and the page cache are all cold exactly as on a fresh checkout.

| iteration | fresh caches | warm-up (untimed) | measured return | bound | result |
|---|---|---|---|---|---|
| 1 | **yes (cold)** | 6.7748 s | **200.8 ms** | 5 s | pass |
| 2 | no | 220.9 ms | 180.6 ms | 5 s | pass |
| 3 | no | 180.7 ms | 200.8 ms | 5 s | pass |
| 4 | no | 200.8 ms | 180.7 ms | 5 s | pass |
| 5 | no | 200.8 ms | 200.9 ms | 5 s | pass |

The cold iteration's 6.77 s warm-up is the same 4.3–11 s that used to land inside
the timed window and fail; the timed window is now 200 ms. Raw logs:
`.cache/lh/cold-run{1..5}.log`, driver `.cache/lh/acceptance.sh`.

Before/after on the same fixture and host, from the manual repro above:

| | before this change | after this change |
|---|---|---|
| first run after a build (cold caches) | 5.635 s (and 4.3–11 s on `main`) — **fail** | pass: cold cost 6.77 s is absorbed by the untimed warm-up, timed window 200.8 ms |
| later runs (warm) | 0.228 s — pass | pass: 180–201 ms |
| real S6 hang | fail (outer deadline) | fail (outer deadline + `prompt_bound_violation` unit check) |

### Full suite

Command (through the host ART lock and the guard, as required):

```
tools/art-runtime/run-exclusive.sh --task LHFLAKE -- \
  tools/guarded.sh --mem 10G --time 900 -- \
  cargo test --release --offline -p acos-art --no-fail-fast -- \
  --test-threads=1 --skip refuses_to_run_when_no_cgroup_can_be_created
```

Result: **green**, `suite_rc=0`, 2026-11-14T15:17:20Z (20 s end to end, warm shared
caches). Per target: lib 114, cleanup 8, **launch_hang 5** (all four ART cases +
`promptness_bound_still_bites`), memory_caps 6, priority_adapter 1,
r3_crash_evidence 2, sandbox_s5 6 (1 filtered by the `--skip`), sandbox_s6 5,
w31_tree_evidence 3, w37_build_cache 5, w39_stage_cache 5, w43_session_exit 4,
doc-tests 0 — 0 failed, 0 ignored. Raw log `.cache/lh/suite.log`.

The branch was then re-checked after the last (comment-only) edit: full rebuild
green and `--test launch_hang` green again, `.cache/lh/final-verify.out`.

## Decisions

1. **Fix the test, not the product.** The product never hung: `wall=24ms` for a
   dead guest on a cold run, and the guest's outcome (`GUEST_EXITED`/126) was
   always correct. The failure was a measurement artefact.
2. **Warm-up rather than a bigger bound.** Raising the bound to 15 s would have
   stopped the false failure but also stopped detecting a real S6 regression in
   the 5–15 s range, which is the class of bug the file exists for.
3. **Warm-up in its own `/data` copy.** Reusing the timed run's directory
   silently disarmed the `CC` fixture (seccomp key covers the `CC` path and the
   Landlock plan). Separate directory keeps the timed run a real `CC` build; the
   shared javac/d8, clang and boot-image caches are still warmed.
4. **Keep a pure bound helper + unit test** (`prompt_bound_violation`,
   `promptness_bound_still_bites`) so the sanity check on the bound costs no wall
   clock and cannot rot into "the bound always passes".
5. **Diagnostics over silence.** `LHFLAKE warm-up …` / `LHFLAKE measured return: …`
   on stderr (visible under `--nocapture`) and `cold_cache_evidence()` in the
   failure message make a future cold-vs-slow regression self-explaining.

## Peak resources

* Peak RAM: guarded `--mem 10G` for the test runs, `--mem 12G` for the build;
  observed host MemAvailable stayed ≥ 34 GiB, `dex2oat` peak inside the launch is
  ~2 MiB in its own scope (`cgroup: … peak=2MiB`).
* Disk: `/` at 84 % throughout (start 82 %, stop line 85 %); the test's own
  scratch is `.cache/lh/` plus the two 295 MB `seed_data` copies per test, all
  removed on the green path. Build target re-used `.cache/gr4-target` (4.8 GB,
  pre-existing).
