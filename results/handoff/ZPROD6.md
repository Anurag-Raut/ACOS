# ZPROD6 handoff — reset and teardown on the zygote path: phases, root cause, fix

Branch `zygote-prod-1` (first `git merge --no-edit main` = `cb5bd04`, both sides of the
`acos-art::Error` enum kept: `Zygote` **and** `InvalidGuestUid`). Design
`docs/decisions/2026-11-14-zygote-prod.md`; `EXPERIMENTS.md` E2.18 (same number). Previous:
`results/handoff/ZPROD5.md`.

## Status

| goal | result |
|---|---|
| 1. profile reset/teardown with phase timestamps + a phase table | **PASS.** The two tables below are the engine's own `stop`/`reset` traces (`DM6_RESET_TRACE`) on the live `serve` path. |
| 2. teardown: signal + prove, ≤ 30 ms p50, no survivors | **PASS.** 307 ms → **7 ms** p50 (`teardown_ms`), `survivors=[]`, `session_leftovers=[]`, `verified=true`. |
| 3. reset ≤ OFF + 5 % | **PASS**, and better: **276.7 ms vs OFF 341.7 ms (−19 %)**. |
| 4. zygote stays opt-in; no check weakened | **PASS.** No flag default touched; `survivors` / `data_cleared` / `state_reset` / refork unchanged. |
| D-gate note | `form-validation` fails with the flag **on** at step 5 `assert_log` (empty guest log buffer) — pre-existing, unrelated to this change; see §5. |

One change fixed both regressions; the options the task listed (USAP pre-fork, launcher reuse,
overlapped kill) were **not** needed, and the measurement says why (see §3).

## 1. Phase table — BEFORE (the regression)

`DM6_RESET_TRACE` was already in the engine (inert unless the var names a file). Harness
`.cache/zprod6/profile.py`: one `acos serve` per arm, same verb sequence (`launch strict` →
`tap` → `reset strict` → `destroy`), 2 reps per arm, arms interleaved `off on off on`, one binary
(`sha256 daf1693e…`). Raw: `.cache/zprod6/profile/profile.json`.

| phase (engine trace event) | OFF — exec path | ON — zygote | delta |
|---|---:|---:|---:|
| `reset.stop_ms` — old guest down | **13.5** | **307** | **+293.5** |
| `reset.relaunch_ms` — launcher + fork + ready | 183 | **113** | −70 |
| `reset.bind_ms` — first strict tree after it | 142 | 142 | 0 |
| `reset.engine_total_ms` | 342 | 566 | +224 (+72 %) |
| `stop` `path` / `exit_ms` / `reaped_before_deadline` | `lifecycle` / 13.5 / `true` | **`killed` / 300 / `false`** | — |
| `acos_destroy` → `teardown_ms` | 13 | 307 | +294 |

This is the whole answer to goal 1: **one phase**. `relaunch` is already *70 ms faster* on the
zygote arm (it forks an already-booted runtime instead of booting a launcher's ART) and `bind` is
byte-identical. Nothing was hiding in the refork, the contract round-trip, the reap or the kill.

## 2. Root cause (proven, not hypothesised)

A zygote session's engine-side `child` is **not the app**. It is the session launcher in
foreign-guest mode (ZPROD3/§4.3): it owns the sockets and the `ServiceHost` and starts no ART at
all — the app child was forked **inside the keeper**. `ArtSession::shutdown` writes
`{"cmd":"close"}` and then waits GL1's 300 ms lifecycle bound for *that* process to exit.

It never exits. Every ON stop in the trace reads `reaped_before_deadline=false`, `exit_ms=300`,
`path=killed` — so the 300 ms grace was pure loss, paid twice per reset (once for the stop, once
for the destroy). The app child's actual death proof is the keeper's ack (`kill <tag>` →
`kill(-child, SIGKILL)` + `wait4` in `ZygoteApp.z4KillOne`, written to `<plan>/<tag>/reaped` as
`status=0x9 pid=<n>`), which `close`/`reset` fetch immediately *after* this stop.

## 3. Fix (one function, both goals)

`teardown_grace_ms_for(zygote)`: a zygote session takes a **zero** lifecycle grace
(`DEFAULT_ZYGOTE_TEARDOWN_GRACE_MS = 0`); the exec path keeps GL1's 300 ms.
`ACOS_ART_TEARDOWN_GRACE_MS` still wins on both arms, so the old bound stays reproducible.

The stop then does exactly what goal 2 asks — signal, then prove:

1. `close` the shim (best effort, as before);
2. SIGKILL its own process group (it is a shim: no app lifecycle to deliver);
3. the **unchanged** proof: `kill(-pgid, 0)` (ESRCH = empty) and a `/proc` scan only when the
   kernel says a member still exists, adaptive 1→10 ms poll, `PROCESS_TREE_DIRTY` if it never
   empties;
4. the app child (and its bwrap) is signalled and proven by the keeper's `reaped` ack — a
   `wait4` return, not a poll.

`survivors`, `session_leftovers`, `data_cleared`, `state_reset`, the tag reuse and the boot-epoch
stability are untouched. The flag stays opt-in (ADR-0026 decision 1).

**Why not the AOSP USAP pool / launcher reuse / overlapped kill** (the decision, with numbers):

* *USAP pre-fork* buys `relaunch_ms` (113 ms) toward 0 but costs a resident idle child per keeper
  (memory, and it must be discarded whenever `argv`/`binds` differ) and does not touch the 293 ms.
* *Reusing the session's launcher* removes the ~113 ms launcher round-trip, but the launcher would
  have to survive a `run`-root wipe and re-publish `argv` — the very contract ZPROD5 had to make
  fail-closed.
* *Overlapping the kill with the fork* is unsound: the run root is wiped after the kill, so the
  fresh child cannot bind it before the old one is gone.
* The fix chosen is the smallest one that hits the target, and it hits it with ~65 ms to spare
  instead of the 17 ms the +5 % budget allowed.

## 4. Phase table — AFTER (same harness, same interleaving, 4 samples/arm)

| phase | OFF — exec path | ON — zygote | delta |
|---|---:|---:|---:|
| `reset.stop_ms` | 14 | **7.5** | −6.5 (the zygote stops *faster*: no bwrap teardown) |
| `reset.relaunch_ms` | 184 | 121 | −63 |
| `reset.bind_ms` | 142 | 142 | 0 |
| `reset.engine_total_ms` | 341.7 | **276.7** | **−19 %** (`reset_ms` verb: 341.7 → 276.7) |
| `stop` `path` / `grace_ms` / `stop_ms` | `lifecycle` / 300 / 13.5 | `killed` / **0** / 7.5 | — |
| `acos_destroy` `teardown_ms` p50 | 13 | **7** (bench: 6.5) | −6 (target ≤ 30 met) |
| `acos_launch` (warm daemon) | 297.0 / 312.8 | **265.8 / 273.8** | −9 % |

Every ON stop now reads `path=killed grace_ms=0 stop_ms=7..11 reaped_before_deadline=false`; every
destroy replies `survivors=[] session_leftovers=[] verified=true`; every reset replied
`count=0` and kept `previous_child_pid` → fresh `child_pid` with the **same** `zygote_boot_epoch`
(`8→35`, `62→89`). No keeper, launcher or bwrap survived any arm (`leftovers: none`).

## 5. Acceptance evidence

* **(A) `cargo test -p acos-cli`** (`.cache/zprod6/cli-suite.log`): 16 binaries, **173 passed,
  0 failed**, `--test-threads=1`, `--no-fail-fast`; the 8 `zygote_host::tests` and both new tests
  green.
* **(B) the ZPROD5 live `serve` proof, re-run on this tree — 13/13 PASS** (`RESULT PASS []`):
  launch `child_pid=8 epoch=44075745284` → tap count **1** → reset count **0**
  (`state_reset`/`data_cleared`) child **8→35** same epoch → destroy
  `lifecycle=killed teardown_ms=**6** survivors=[] session_leftovers=[] verified=true` → SIGKILL
  the keeper → `ZYGOTE_DEAD session="lost" repointed=false` → new keeper
  `epoch 44075745284→44081672917`, child 62→8 → no keeper/launcher/bwrap/daemon after exit, and
  the SIGTERM plan-root residue still swept by the next daemon.
* **(C) interleaved A/B** (§4 above; bench p50 over 12 samples).
* **(D) gates, `--engine art`, animations on, same binary:**
  **flag OFF 3/3 pass** (`v2-art-counter`, `w22-counter-reset`, `form-validation`).
  **flag ON 2/3**: `v2-art-counter` and `w22-counter-reset` pass; **`form-validation` fails at
  step 5 `assert_log` with an empty guest log buffer** (`actual: []`, expected
  `^onCreate$ ^onStart$ ^onResume$`), reproduced twice (`.cache/zprod6/gates/`).
  The zygote really served both runs (`ready epoch=43843439484 boot_ms=22`, keeper
  `ZP1 forked tag=0 child=8 probe_stdin=on` … `ZP1 killed tag=0 status=0x9`) and the three steps
  before it pass on a live tree. **This is a pre-existing flag-on gap, not this increment's
  change and not a relaxed check**: a forked child's log lines go to the **keeper's** stdout
  instead of the session's log buffer, so any `assert_log` scenario cannot pass on the zygote
  path. It fails at step 5 of 28, long before any stop — the only thing ZPROD6 touches. No gate
  is claimed that did not pass.

## 6. Tests

```bash
CARGO_TARGET_DIR=$WT/.cache/g3cd-target tools/art-runtime/run-exclusive.sh --task ZPROD6 -- \
  tools/guarded.sh --mem 10G --time 800 -- cargo test --release --offline -p acos-cli \
  --bin acos -- engine::art::tests::only_the_exec_path_pays_the_lifecycle_grace \
  engine::art::tests::a_zygote_session_signals_and_proves_instead_of_waiting_the_grace
```

* `only_the_exec_path_pays_the_lifecycle_grace` — the arm decides (300 vs 0) and the operator knob
  still wins on both.
* `a_zygote_session_signals_and_proves_instead_of_waiting_the_grace` — a launcher shim that ignores
  `close` and stays alive costs the **exec** arm its full bound (asserted ≥ 250 ms, `path=killed`)
  and the **zygote** arm **< 100 ms** (measured ~7 ms), with `survivors=[]` and `verified=true`.
  Without the zygote binding the same shim would have hidden a 300 ms stall behind GL1 forever.

## 7. Remaining

1. **Route the forked child's guest log buffer to the session** so `assert_log` scenarios can
   pass with the flag on (the D-gate finding above; the same class as ZPROD3b's probe channel).
2. **F11 (unchanged):** the keeper pins `lz4` because `-Xzygote` refuses a `shared-fixed`
   (`/data/img`) oat, so the ON arm's child does not run the shipped default boot image. The
   launch/reset deltas above are therefore *arm-vs-arm*, not image-for-image.
3. The bench's `M6 reset` and `GL1 teardown` rows should be re-baselined with the zygote **on**
   once it is the default (the OFF arm keeps the old numbers, which is correct).
4. F3 per-child cgroup memory (unchanged from ZPROD4).
