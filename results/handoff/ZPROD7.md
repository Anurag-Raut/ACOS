# ZPROD7 handoff — the forked child's log stream: shim-level fix, gate gap still open (CHECKPOINT)

Branch `zygote-prod-1` (first `git merge --no-edit main` = `92e09bf`, both sides kept:
`ACOS_LOGD`-untouched, `acos-art::Error::{Zygote, InvalidGuestUid}` both present,
`ErrorCode::{ZygoteDead, HostMemoryLow}` both present in both `match`es). Design
`docs/decisions/2026-11-14-zygote-prod.md`; `EXPERIMENTS.md` E2.18 (same number). Previous:
`results/handoff/ZPROD6.md`.

**This is a checkpoint, not a pass.** The owner redirected the queue mid-increment; the
orchestrator asked for a clean checkpoint instead of finishing GOAL 2 and the A/B.

## Status

| goal | result |
|---|---|
| GOAL 1 — a forked child's guest log stream goes to *its* session | **PARTIAL.** The defect that the fix addresses is real, is now fixed in the shipped shim, and is covered by a test that fails without it (`crates/acos-art/tests/logd_route.rs`). **`form-validation` with the flag on still fails at step 5 `assert_log` with an empty session log buffer**, so at least one *further* cause exists in the real zygote path that this increment did not isolate. |
| GOAL 2 — `shared-fixed` boot image for `-Xzygote` (F11) | **NOT ATTEMPTED** (orchestrator: skip, owner reprioritised). The ON arm still pins `lz4` for the keeper, unchanged from ZPROD4/5/6. |
| (D) interleaved A/B | **NOT RUN** (it was conditional on GOAL 2 landing). The ZPROD6 numbers stand for the merged tree's OFF/ON arms — they were measured on this branch's ancestry and the only launch-path change here is the logger. |
| (C) ZPROD5/6 live serve proof 13/13 | **NOT RE-RUN** in this increment. |
| No check weakened | **PASS.** No flag default touched, no assertion, settle horizon or gate relaxed, no scenario edited. 0 false passes claimed. |

## 1. What was fixed (and why it is right regardless of the gate)

A zygote child inherits the shim's **connected descriptor**, not the path. The keeper resolves
`/dev/socket/logdw` (its own socket dir) before it forks, so the child's entries were delivered to
the *keeper's* `Logd` sink, which no session reads — while the child's own `/dev/socket` is its
session's bind mount of `<plan>/<tag>/sock`. That is exactly the shape of the ZPROD6 finding
("logs go to the keeper's buffer, the session log buffer is empty").

`crates/acos-art/shim/artshim.c`, `log_socket()`:

* re-resolves the sink path when the **process id changes** (`log_pid`), so every fork child
  connects to the socket its own `/dev/socket` names. One `getpid()` per formatted entry, on
  non-hot paths (dump/crash/CHECK text); hooking `fork()` would be a bigger surface than the one
  syscall it saves;
* a failed connect is **no longer sticky** (`-1` used to be permanent): the next entry retries, so
  an entry written before the session launcher binds its sink (a reset re-binds it) costs one
  entry, not the whole stream;
* the path is `ACOSART_LOGD_SOCK`, default `"/dev/socket/logdw"` — the *build-time* default is
  unchanged and asserted by a test; the `-D` override exists only so a host test can exercise the
  shipped code outside a guest.

## 2. Evidence

### (A) tests — green, and the new suite fails without the fix

* `cargo test -p acos-cli` (`.cache/zprod7/cli-suite.log`): **183 passed, 0 failed**, 5 ignored,
  `--test-threads=1`.
* `cargo test -p acos-art` (`.cache/zprod7/art-suite.log`): **181 passed, 0 failed** (includes the
  order-dependent cgroup test at this acquisition), among them the 3 new ones.
* New: `crates/acos-art/tests/logd_route.rs` drives the **shipped shim source**
  (`acos_art::SHIM_SOURCE`, compiled with the same flags plus the path override) in real processes
  with a real `fork()`, and a per-process working directory standing in for the per-mount
  `/dev/socket` (same path string, different socket inode, no shared state, so two writers are
  genuinely concurrent):
  1. `two_concurrent_forked_children_each_log_only_into_their_own_session` — two sessions, two
     children; the keeper sink holds exactly the two pre-fork lines, session *k* holds exactly its
     own three post-fork lines, no foreign tag appears anywhere, and nothing falls back to stderr.
  2. `an_entry_written_before_the_sink_exists_does_not_silence_the_stream` — a missing sink does
     not stick; the rebound sink receives the next entry.
  3. `the_default_sink_path_is_the_guest_socket` — contract guard on the shipped default.
* **Inverse check (anti-false-pass):** with `log_socket()` temporarily reverted to the ZPROD6 rule
  (no pid check, sticky failure) tests 1 and 2 **fail** (`.cache/zprod7/` run, restored
  immediately); with the fix they pass. So the suite really pins the behaviour.

### (B) the 15-gate matrix, same binary, both arms (`.cache/zprod7/gates/gates.log`)

Binary `acos 12f1fbc0…`, `acos-art 6ab8b97f…`; one lock acquisition for the batch
(`.cache/zprod7/gates.sh` under `run-exclusive.sh --task ZPROD7 -- guarded.sh --mem 10G`).

| gate | OFF | ON |
|---|---|---|
| `v2-art-counter` | pass | **pass** |
| `w22-counter-reset` | pass | **pass** |
| `w54-dialog-pixels` | pass | **`ASSERTION_FAILED`** |
| `w42-back-frame` | pass | **`ASSERTION_FAILED`** |
| `w15-dialog-menu` | pass | pass |
| `form-validation` | pass | **`ASSERTION_FAILED`** (step 5 `assert_log`, `actual: []`) |
| `suite/T03-render-good` | pass | pass |
| `v1-offscreen-pager` | pass | pass |
| `w7-insets-report` | pass | pass |
| `se1-expect-change-tap` | pass | pass |
| `suite/T03-render-wrong` | `ASSERTION_FAILED` | `ASSERTION_FAILED` |
| `w7-insets-carve-negative` | `ASSERTION_FAILED` | `ASSERTION_FAILED` |
| `negative/se1-expect-change-noop` | `ASSERTION_FAILED` | `ASSERTION_FAILED` |
| `negative/offscreen-pager-visible` | `ELEMENT_OFFSCREEN` | `ELEMENT_OFFSCREEN` |
| `p11-progress-timeout` | `TIMEOUT` | `TIMEOUT` |

**OFF arm: 10 pass / 5 fail exactly as specified — the flag-off path is unchanged.** ON arm:
7 pass, and **three** differences from OFF, not one: `form-validation` (the known gap) plus
`w54-dialog-pixels` and `w42-back-frame`. Those two are *new* information (no earlier increment ran
them with the flag on) and are **not** attributed to this increment: the only change here is where
the forked child's log datagrams go, and neither gate's failing step is a log assertion (both fail
on a pixel/tree assertion). They are, however, additional flag-on gaps for the next increment and
are listed under §4.

### (C) what the ON arm did *not* prove

No zygote evidence files survive a `verify` run (`shutdown_all` removes the plan root), so the ON
results above are verdict-level only. A 40 s snapshot loop during one flag-on `form-validation` run
found no plan tree, and the run's artifacts (`out/form-validation/logs/logcat.txt`) contain only
`--------- beginning of main` — consistent with an empty session log buffer.

## 3. Diagnosis of the remaining gap (what is known, what is ruled out)

* **Ruled out:** the shim's path resolution at `-O2`. A throwaway `bwrap` namespace with a real
  `logdw` datagram socket bound at `/dev/socket` received the shim's frame from the *default*
  (`/dev/socket/logdw`) build, so the shipped default path really resolves and delivers.
* **Ruled out:** a stale shim. The installed guest shim in each arm's data root is the freshly
  built one (`build_shim` keys on `SHIM_SOURCE`, mtime matches the run).
* **Ruled out:** the `sun_path`/session-directory class of bug (ZPROD5's): the ON runs launch and
  pass 5 of 6 steps before the log assertion.
* **Not yet isolated:** why the session's `Logd` sink still sees nothing in the live path. The
  remaining candidates, in the order worth testing next:
  1. the forked child's `/dev/socket` bind does **not** actually carry the session launcher's
     `logdw` socket (order: `prepare_session` wipes `<plan>/<tag>/sock` *after* a previous
     launcher's sink was bound, and the launcher's `Logd` binds into the session dir — a race would
     leave the child with a socket path that exists on the host but whose *inode* the child's bind
     covers differently);
  2. the guest's log lines never reach `__android_log_write` in the child because the child's
     `LD_PRELOAD`/shim is not the one that formats them (real liblog in the boot image writing to
     `/dev/socket/logdw` would still land in the session sink, so this needs a live check);
  3. the engine's `logs` command in the zygote path is served by a `Logd` that is *not* the one
     bound at `<plan>/<tag>/sock` (e.g. a second launcher instance).
  The cheapest decisive test is a live one: keep the plan root (add a `ACOS_ART_ZYGOTE_KEEP=1`
  knob), run one flag-on `form-validation`, then compare `<plan>/<tag>/sock/logdw`'s inode with the
  child's `/proc/<child>/mountinfo` entry for `/dev/socket` and read the keeper's log for `[art]`
  fallback lines (the shim prints those when its connect fails).

## 4. Remaining (smallest next actions, in order)

1. **Keep the zygote plan root across a run** (`ACOS_ART_ZYGOTE_KEEP=1` in
   `PersistentZygote::shutdown`), then run one flag-on `form-validation` and answer the three
   candidates in §3 with `/proc/<child>/mountinfo` + `<plan>/<tag>/sock/logdw` + the keeper log.
   That is one run, ~2 minutes, and it either closes GOAL 1 or names the real cause.
2. **`w54-dialog-pixels` and `w42-back-frame` with the flag on** — new flag-on failures (§2B),
   unmeasured before this increment; they are pixel/tree assertions, so they are probably the
   "zygote child draws before its first strict frame" class, not logging.
3. **F11 / GOAL 2 (unchanged):** `-Xzygote` refuses the `shared-fixed` (`/data/img`) oat
   (`Registering a non /system oat file`), so the ON arm runs `lz4`; the launch/reset/teardown
   deltas remain arm-vs-arm, not image-for-image.
4. **The ZPROD5/6 live `serve` proof (13/13) was not re-run** on this tree.
5. **The interleaved A/B was not re-run** (conditional on GOAL 2; ZPROD6's numbers stand).

## 5. Tests (commands)

```bash
WT=/home/Qui/.paseo/worktrees/3oxa6xao/g3cd
# new suite alone
CARGO_TARGET_DIR=$WT/.cache/g3cd-target tools/art-runtime/run-exclusive.sh --task ZPROD7 -- \
  tools/guarded.sh --mem 12G --time 900 -- cargo test --release --offline -p acos-art --test logd_route
# the two crate suites
CARGO_TARGET_DIR=$WT/.cache/g3cd-target tools/art-runtime/run-exclusive.sh --task ZPROD7 -- \
  tools/guarded.sh --mem 12G --time 1500 -- cargo test --release --offline -p acos-cli --no-fail-fast -- --test-threads=1
CARGO_TARGET_DIR=$WT/.cache/g3cd-target tools/art-runtime/run-exclusive.sh --task ZPROD7 -- \
  tools/guarded.sh --mem 12G --time 1500 -- cargo test --release --offline -p acos-art --no-fail-fast -- --test-threads=1
```

## 6. Host

Peak during this increment: MemAvailable never below ~20 GiB (single 10–12 GiB guard at a time),
`df /` 85 % (0.2 % freed by pruning this worktree's stale scratch: `.cache/{cr457,le643,zprod4}`,
`out/`, the zprod6 boot-image cache — 1.2 GiB — before any run). No ART run outside
`run-exclusive.sh`; no process left behind.
