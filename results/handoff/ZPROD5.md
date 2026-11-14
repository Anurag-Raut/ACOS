# ZPROD5 handoff — the serve/daemon `argv 0` stall: root cause, fix, live proof, A/B

Branch `zygote-prod-1`. Design `docs/decisions/2026-11-14-zygote-prod.md`;
`EXPERIMENTS.md` E2.18 (same number, per the increment's convention). Previous:
`results/handoff/ZPROD4.md`.

## Status

| acceptance | result |
|---|---|
| 1. reproduce one ON group, name the `argv 0` stall, fix it, add a test | **PASS.** Root cause: `AF_UNIX` `sun_path` overflow in the **session directory** (the sockets live inside it), plus a contract wait that only watched the keeper. Fixed (3 commits); 4 new tests (8/8 in `zygote_host::tests`). |
| 2. live `acos serve` proof (launch → tap → count 1 → reset → count 0 → destroy; no keeper after exit) | **PASS** (`.cache/zprod5/live/proof.json`) |
| 2b. live dead-keeper proof (`.cache/zprod4/live_dead.sh` equivalent, on the serve path) | **PASS** in the same run: `ZYGOTE_DEAD` + `session:"lost"`, then a new keeper with a new epoch |
| 3. latency A/B, same binaries, interleaved, OFF = the shipped default | see the table below (each arm's load per rep recorded) |
| 4. `cargo test -p acos-cli` / `-p acos-art` post-merge | **PASS**: 30 test binaries, 331 tests, 0 failures (`.cache/zprod5/suites2.log`); the cgroup-conditional test alone with `--exact` green (`.cache/zprod5/cgroup-test.log`) |
| 5. E2.18 + this file | **PASS** |

## 1. The `argv 0` stall: root cause (measured, not hypothesised)

ZPROD4 recorded that under `acos-bench`'s warm `acos serve` cohort every reference
launch died `ZYGOTE_DEAD: \`argv 0\` was not acked within 180002 ms` and that the keeper
itself booted fine. Reproduced with `--keep-scratch`-style evidence:

```bash
# one acos_launch against a warm serve daemon with ACOS_ART_ZYGOTE=on,
# ACOS_ART_BIN=<wrapper that logs argv> (evidence: .cache/zprod5/repro/run1.log)
```

The session launcher (not the keeper) died in about 0.2 s:

```
=== start … pid=<launcher> argv=launch --apk …/counter.apk --until activity-resumed --out …/sessions/s_zprod5_ref/art-frames --animations on --insets report
--- <launcher>.err
acos-art: guards rss_max=2048MiB …
acos-art: launch: static overlays: 1 applied …
acos-art: io: path must be shorter than SUN_LEN
```

`<plan>/0/` never received `argv`, `binds` or `spawned` (only `probe.fifo`, `run/`,
`sock/` from the host's `prepare_session`).

**Why:** a zygote session's directory carries its **sockets**. The forked child
bind-mounts what the host publishes in `binds`
(`<plan>/<tag>/sock` → `/dev/socket`, `<plan>/<tag>/sock/peer.sock` →
`/data/acos-launch.sock`), and the launcher inherits the Z4 rule "the session
directory owns the sockets" (`ShortSocketDir::at(&session/sock)`). An `AF_UNIX`
`sun_path` is **108 bytes including its NUL**, and the host's session directory
was `<artifacts>/zygote/zygote/<key>/plan/<tag>`:

* `acos verify` — artifacts root `out/<scenario>` → `out/v2-art-counter/zygote/…` fits
  (≈ 80 bytes). This is why the flag-on gates were green from ZPROD3b on.
* `acos serve` from `acos-bench` — artifacts root is the *caller's* scratch
  (`…/r1-a/latency/sessions`) → `<session>/sock/peer.sock` ≈ **128 bytes** → `bind()`
  fails with `io: path must be shorter than SUN_LEN`, before the launcher can
  publish `argv`.

**Why it cost 180 s and produced no diagnosis:** `ZygoteHost::wait_contract_and_spawn`
polled `<plan>/<tag>/argv` and watched only the **keeper's** liveness; a launcher
corpse was indistinguishable from a slow launch, and the error carried the
*keeper's* log (which was healthy). The launcher's own stderr — the one line that
named the cause — was drained into a tail that was only created *after* the wait.

## 2. The fix (3 commits on `zygote-prod-1`)

1. **`ZygoteHost::for_artifacts`** chooses the plan root. `<artifacts>/zygote` is
   kept when a worst-case session's socket still fits `sun_path` (the `verify`
   path is unchanged byte for byte — its evidence keeps the historical layout);
   otherwise the plan tree moves to a short runtime root this host owns:
   `ACOS_ART_ZYGOTE_ROOT` → `/run/user/<uid>/z<pid>` → `/dev/shm/z<pid>`. The plan
   directory is host-side only (it is bound into the zygote as `/sessions`), so
   its host location is free — the same argument W43 already made for the exec
   path's sockets. `shutdown_all` removes an owned root; `open_short_root` sweeps
   the `z<pid>` roots of daemons that are gone (a SIGTERM/SIGKILLed `acos serve`
   installs no handler, so its `Drop` never runs).
2. **Fail closed instead of a corpse timeout:** `ZygoteHost::prepare` checks the
   **exact** session path (`<session>/sock/peer.sock`, with the tag this session
   will really use) *before* a keeper boots and refuses with the lengths and the
   knob to fix; and `zygote_prod::wait_contract_with` / `ZygoteHost::
   wait_contract_and_spawn(peer_gone)` take a liveness probe for the **launcher**.
   `engine/art.rs` drains the launcher's stderr before the contract wait and
   passes a probe that reports the launcher's exit status **and its stderr tail**.
   A launcher that dies before `argv` is now a `ZYGOTE_DEAD` in milliseconds that
   names the cause, instead of a 180 s "keeper not responding".
3. The redundant duplicated `zygote` path level is gone
   (`<root>/<key>/plan/<tag>`, the host root already ends with `zygote`), which
   returns 8 bytes of `sun_path` headroom to every session.

**Tests (mock keeper, no ART, 0.2 s), 8/8 green:**

```bash
CARGO_TARGET_DIR=$WT/.cache/g3cd-target tools/guarded.sh --mem 12G --time 900 -- \
  cargo test --release --offline -p acos-cli --bin acos zygote_host::tests
```

* `a_launcher_that_dies_before_argv_fails_fast_with_its_stderr` — a launcher that
  exits 3 without a contract yields `ZYGOTE_DEAD` in < 10 s naming
  `exit status: 3` and its own stderr (`path must be shorter than SUN_LEN`); the
  keeper is untouched.
* `a_root_too_deep_for_sun_path_fails_closed_before_a_keeper_boots` — the deep root
  is refused with `too long for an AF_UNIX socket`, the failing `peer.sock` path,
  the lengths and `ACOS_ART_ZYGOTE_ROOT`; `keeper_count() == 0` and no plan tree.
* `a_deep_artifacts_root_is_displaced_to_a_short_root_and_a_short_one_is_kept` — the
  bench-shaped root is displaced (session socket fits, not under the artifacts
  root), the verify-shaped root is kept at `<artifacts>/zygote`, an explicit
  `ACOS_ART_ZYGOTE_ROOT` always wins, and an owned root is removed on shutdown.
* plus the 5 existing `zygote_host` tests (flag off, reply fields, dead keeper,
  dead-keeper restart/new epoch, `shutdown_all`), unchanged and green.

**Live confirmation of the fix** (`.cache/zprod5/repro/run2.log`, the same
invocation that used to stall): `plan/0/argv` at **+4.21 s**, `binds` and
`spawned` immediately after, `launch wall_s=4.4 ok=True`, `child_pid=8`,
`isolation=fresh-zygote-child`. The 4.2 s is the *cold* `shared-fixed` boot-image
build in that fresh scratch (`boot_image.cache=built prep_ms=3895`), i.e. the
first-launch cost the OFF arm pays in its own reference launch too — not the
stall.

## 3. Live `acos serve` proof (`.cache/zprod5/live_serve.py`, `proof.json`)

One warm daemon, `ACOS_ART_ZYGOTE=on`, the real keeper and real ART:

```
1-launch-ok                  PASS isolation=fresh-zygote-child child_pid=8 epoch=20612077173
2-tap-then-tree=1            PASS tap_ok=True count='1'
3-reset-count=0              PASS count='0' verify_count='0' state_reset=True data_cleared=True
3-reset-new-child-same-epoch PASS child 8 -> 35 (previous=8) epoch …7173 -> …7173
4-destroy                    PASS teardown_ms=307 session_leftovers=[] survivors=[]
5-reset-zygote-dead          PASS code=ZYGOTE_DEAD session=lost session_epoch=20612077173
                                  keeper_epoch=None repointed=False
6-new-keeper-new-epoch       PASS epoch 20612077173 -> 20619119756, child 62 -> 8
6-new-session-live-child     PASS isolation=fresh-zygote-child count='0'
7-no-keeper-after-exit       PASS none
```

The dead-keeper half is the `.cache/zprod4/live_dead.sh` scenario driven through
the **serve** path (SIGKILL the keeper between two requests): the current request
is `ZYGOTE_DEAD` + `details.zygote.session == "lost"` + `repointed=false`, the next
request boots a **new keeper** with a **new `zygote_boot_epoch`** and a fresh
fork, and the old epoch is never re-pointed.

## 4. Latency A/B (measured, 2 interleaved reps per arm)

`ACOS_BENCH_TASK=ZPROD5 tools/bench/acos-bench.sh --reps 1 --groups
"a:latency:8G:300" --latency-reps 12 --no-build --target-dir $WT/.cache/g3cd-target`
per arm, interleaved (off, on, off, on, off, on), same binary
(`sha256 ac8b06bd…`, `.cache/zprod5/ab/ab.log`).

| metric (p50, 2 reps pooled) | OFF — shipped default | ON — zygote | delta |
|---|---:|---:|---:|
| M4 launch strict | 306.5 ms | **259.1 ms** | −15.5 % |
| M4 launch fast | 166.1 ms | **123.1 ms** | −25.9 % |
| M6 reset strict | **333.8 ms** | 574.5 ms | +72.1 % |
| M6 reset fast | **193.1 ms** | 431.7 ms | +123.5 % |
| M8 tap counter strict / fast | 950.1 / 45.0 ms | 947.2 / 45.7 ms | parity |
| M8 tap form strict / fast | 948.3 / 45.6 ms | 942.0 / 47.5 ms | parity |
| M5 screenshot | 24.1 ms | 24.9 ms | parity |
| teardown | **12.5 ms** | 307.0 ms | +2356 % |

Load1 before→after per acquisition: OFF 0.93→1.70, 1.10→0.83; ON 1.70→1.10, 0.83→1.26;
MemAvailable 32.3–32.6 GiB; no noisy phase; every arm `ok=True`, `survivors=[]`.
Zygote evidence (required for an ON arm) is preserved:
`.cache/zprod5/ab/zygote-evidence/<key>/plan/ready` (`epoch=21672502580 boot_ms=13`,
`epoch=21596387361`), 30 session dirs with `sock/{logdw,peer.sock}`, `argv`, `binds`,
`spawned`; raw `keeper.log` copies in `.cache/zprod5/ab/sshots/`.

Honest caveats: (a) an earlier interleaved pair gave the same shape (OFF M4 311.5/169.1 vs ON
259.8/124.4) but its ON arm hit **one** 60 s `NO_LIVE_FRAME` on the discarded `m4-warm`
launch — the first launch after the keeper boots — and its raw JSON was overwritten by a
naming bug in my chain script, so it is not pooled; (b) the two arms do **not** run the same
boot image (OFF `shared-fixed`, ON `lz4` — F11), which is the honest limit of the delta;
(c) M6 and teardown regressions are not investigated further here. OFF is the **shipped default** (exec path,
`shared-fixed` image) — deliberately not an lz4-pinned exec arm (orchestrator
decision, ZPROD5): the zygote has to beat what users get. The keeper keeps its
`ACOS_ART_BOOT_IMAGE=lz4` pin (F11: `-Xzygote` refuses a `/data/img` oat), so the
zygote currently cannot use the shared-fixed image; a follow-up may lift F11.

## 5. Suites (acceptance 4)

```bash
# one lock acquisition, both crates, the cgroup-conditional test skipped
CARGO_TARGET_DIR=$WT/.cache/g3cd-target tools/art-runtime/run-exclusive.sh --task ZPROD5 -- \
  tools/guarded.sh --mem 12G --time 1600 -- cargo test --release --offline \
  -p acos-cli -p acos-art --no-fail-fast -- --test-threads=1 \
  --skip refuses_to_run_when_no_cgroup_can_be_created
# 30 test binaries, 331 passed, 0 failed, CARGO_RC=0            .cache/zprod5/suites2.log
# then, alone, --exact
cargo test --release --offline -p acos-art --test sandbox_s5 -- \
  --exact refuses_to_run_when_no_cgroup_can_be_created
# 1 passed; 0 failed                                            .cache/zprod5/cgroup-test.log
```

One earlier run of the same suite (`suites.log`, without `--no-fail-fast`) failed
`acos-art/tests/launch_hang.rs::guest_exits_before_milestone_returns_exit_code_and_cleans_up`
with `launch took 11.113508533s to report a guest that had already ended (bound: 5 s)`
— a **host-load timing bound**, not a semantic failure (the host was at load1 ≈ 6 with
other tracks' ART runs; the test does not touch the zygote seams). The re-run above
passed it in an 11-test binary finishing in 8.98 s. Reported as flaky-under-load,
never as a pass on the first attempt.

## 6. Remaining

1. **F11: let the zygote use `shared-fixed`.** The keeper pins `lz4` because ART's
   `only_use_system_oat_files_` rejects a `/data/img` oat under `-Xzygote`. Until
   that is lifted the ON arm's child runs the in-memory `lz4` image while the OFF
   arm runs the shared file image — the honest reading of any delta.
   (Cosmetic follow-up: the *session launcher* still resolves `shared-fixed` for
   its own unused sandbox, so a cold cache pays a `prep_ms≈3.9 s` build on the
   first ON launch. Harmless, but the launcher could be told the keeper's mode.)
2. **Memory rows for the zygote** must use an on-disk `ACOS_ART_ZYGOTE_ROOT`: a
   relocated plan tree sits on tmpfs (`/run/user`), and a session's run root
   (dalvik-cache, app-data, tmp) is tmpfs-backed there. Latency is unaffected.
3. F3 per-child cgroup memory (unchanged from ZPROD4).
