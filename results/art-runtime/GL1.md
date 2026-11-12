# GL1 — kept-session lifetime and bounded session destruction (real ART)

Worktree `/home/Qui/.paseo/worktrees/3oxa6xao/gl1`, branch `gaten-gl1-lifetime`, off `main` `bf70bb7`.
Owner task: G1's §3 rank-3 blocker (`acos launch --keep` + `acos session …` from another process) plus the
2026-11-12 addendum (explicit destruction must be bounded: lifecycle ≠ destruction).

Files touched (all mine, plus `engine/serve.rs` for the destroy reply):

| file | change |
|---|---|
| `crates/acos-cli/src/engine/art.rs` | guest spawn moved onto one process-lifetime spawner thread; bounded teardown (300 ms lifecycle grace); `TeardownPath`; `last_teardown()`; `session_leftovers()`; unit regression test |
| `crates/acos-cli/src/engine/serve.rs` | `acos_destroy` reply reports `lifecycle`, `teardown_ms`, `session_leftovers` and fails on a leftover |
| `crates/acos-cli/tests/gl1_kept_session.rs` | **new**: 4 ignored real-ART tests (kept session, daemon SIGKILL, bounded destroy, forced-kill path) |
| `fixtures/apps/thread-heavy/**`, `tools/build-apps.sh` | **new** fixture: 12 non-daemon threads that never exit (Gaten stand-in) |

---

## 1. Root cause (kept session died with its connection)

`PR_SET_PDEATHSIG` is **per parent thread**, not per parent process. `engine/art.rs` armed
`prctl(PR_SET_PDEATHSIG, SIGKILL)` in the `acos-art` child's `pre_exec` (to guarantee no orphan when the
daemon is SIGKILLed), and `serve.rs` created the session on the **per-connection** thread. So the parent
thread of the guest was that connection thread: when `acos launch --keep` disconnected, the kernel
delivered SIGKILL to `acos-art` — 2 ms after the launch reply, exactly as G1 measured.

Proof it is this and not something else — the same binary, one line changed
(`spawner::spawn(command)` → `command.spawn()`), same fixture, same commands:

```
# before (build from bf70bb7 + the direct spawn):
== client A: launch --keep (separate process, then disconnects)   rc=0, ok=true
== client B: session status                                       rc=1
   GUEST_EXITED: the ART guest for session `s_cli` is not alive (exited: signal 9)
== client B: tree                                                 rc=1  (GUEST_EXITED … signal 9)
# after (this branch):
== client B: session status   ok pass engine art launched True stage first_frame
== client B: tree            ok pass, count = "0"
== client B: tap --id inc    ok
== client B: tree            count = "1"
== client B: screenshot      9 545 882-byte PNG
== client B: destroy         ok, no leftover acos-art
```

`signal 9` is the PDEATHSIG, and it is the *only* SIGKILL in that path.

### Fix (smallest correct one)

One spawner thread per process, created on first use from a `OnceLock` (`mod spawner` in `art.rs`), never
exits; `ArtSession::start` hands it the fully built `Command` (process group + `pre_exec` unchanged) and
receives the `Child`. The parent thread is now the spawner, which lives as long as the process, so:

* kept sessions survive client disconnects (the connection thread is no longer the parent);
* the orphan guarantee is unchanged: when the *process* dies, every thread including the spawner dies,
  and the kernel still delivers the death signal;
* nothing else changes — same process group, same pipes, same `close`/`teardown` reaping, same sandbox.

`kill -9` on the daemon with a live session: **guests gone 1 ms** after the signal (measured by the
integration test and the manual run, both scanning `/proc` for live `acos-art` processes).

---

## 2. Lifecycle vs. explicit destruction (the addendum)

Android separates two things and ACOS now names both:

| phase | Android | ACOS |
|---|---|---|
| activity lifecycle | `onPause`/`onStop`/`onDestroy` delivered, then the process is killed | the guest's own `close` handler runs the lifecycle; the engine waits for the guest to end **within 300 ms** |
| explicit destruction (`acos session destroy`/`stop`, `reset`, daemon shutdown) | AMS `Process.killProcess` — it never *joins* the app's threads | the engine kills the session's process group after the grace; the whole tree dies (guest included, via the launcher's own PDEATHSIG and the launcher's `cgroup.kill`) |
| app that never ends (RN/Hermes, 12 non-daemon threads) | killed after the lifecycle, no wait | killed at the 300 ms bound, `lifecycle: "killed"` in the reply |

Why 300 ms: measured, the guest's own `close` (lifecycle + process exit + evidence write) costs
**p50 18 ms, p95 20 ms** end to end for `counter` *and* for the thread-heavy fixture, so 300 ms is ~15×
the observed cost and keeps `acos_destroy` well under the 1 s bound; anything slower is an app that is
not going to finish on its own, and waiting longer cannot help (it is a `Thread.join`/`CountDownLatch`
find, not a slow lifecycle). Configurable: `ACOS_ART_TEARDOWN_GRACE_MS` (0 = kill immediately, used by
the forced-kill test). Before this change the same wait was a flat **10 s** deadline followed by the
same kill, i.e. a hung app paid 10 s of dead time for nothing.

`acos_destroy` now answers with the path and the proof:

```json
"teardown": {
  "process_group": 2388051,
  "survivors": [],
  "verified": true,
  "lifecycle": "lifecycle",      // or "killed"
  "teardown_ms": 12,
  "session_leftovers": []        // /proc scan for live acos-art naming this session
}
```

A non-empty `session_leftovers` (or a non-empty `survivors`) is a failure (`PROCESS_TREE_DIRTY`), never a
quiet success. Resources checked on the destroy path: the spawned process group (`kill(-pgid,0)` +
`/proc` scan), the session's own artifacts path (launcher command lines), the guest's scope (the
launcher's `RunCgroup::kill` = `cgroup.kill`, else SIGKILL per pid), stdin/stdout/stderr pipes (closed
with the process), the daemon's session map + route table (id no longer listed), and the C1 buffers /
binder peers, which live in the guest process and die with it.

**What remains after a destroy, measured:** no live pid, no zombie in the session group, no
`cgroup.procs` entry, no sockets (the daemon's per-connection sockets close with each client; the guest
never had one), and one 16 KB directory `$ACOS_RUNTIME_DIR/art-sessions/<sid>/art-frames/` holding exactly
two evidence files (`live-session.json`, `session-timing.json`). The frame mirrors, C1 buffers and binder
peers are gone with the guest. That evidence directory is deliberately **kept** — it is the post-mortem
trail the failure classifier and `session-exit.json` reading depend on (W55) — and it is not a leaked
resource (no fd, no mapping, no process). Removing it on destroy would trade a diagnosable session for
16 KB; say so if the opposite is wanted.

### Measurements (`tools/art-runtime/run-exclusive.sh --task GL1 -- tools/guarded.sh --mem 8G`)

| fixture | launches | `session destroy` wall (client, incl. socket) | engine `teardown_ms` | leftover `acos-art` at reply / +1 s | session listed |
|---|---|---|---|---|---|
| `counter` (framework, quiet) | 3 (manual) + 1 (test) | 18 / 17 / 19 ms; 16 ms | 11–12 ms | 0 / 0 | no |
| `thread-heavy` (12 non-daemon threads that never exit) | 3 (manual) + 1 (test) | 20 / 20 / 20 ms; 17 ms | 12 ms | 0 / 0 | no |
| `thread-heavy`, `ACOS_ART_TEARDOWN_GRACE_MS=0` (forced kill path) | 2 (manual) + 1 (test) | 12 ms | 6 ms, `lifecycle: "killed"` | 0 / 0 | no |

These numbers are *after* the fix. The teardown cost is the guest's own lifecycle + exit; the 10 s →
300 ms deadline change only binds the pathological case (Gaten-class apps), which is where GR1 saw the
120 s wait.

---

## 3. Tests

`crates/acos-cli/tests/gl1_kept_session.rs` (all four ignored by default: they boot a real guest):

```bash
tools/art-runtime/run-exclusive.sh --task GL1 -- tools/guarded.sh --mem 8G --time 400 -- \
  cargo test --release -p acos-cli --offline --test gl1_kept_session -- --ignored --test-threads=1 --nocapture
```

| test | asserts |
|---|---|
| `a_kept_session_survives_the_launching_client` | `launch --keep` in **process A**, then status/tree/tap/tree/screenshot from **process B**: `engine=art`, `launched=true`, count `0` → tap → `1`, PNG > 1 kB; then `destroy` → no live guest, and it is not listed |
| `a_sigkilled_daemon_takes_its_guests_with_it` | `kill -9` the daemon with a live session → guest processes gone **within 2 s** (observed: 1 ms), no running pid left |
| `explicit_destruction_is_bounded_and_leaves_nothing_behind` | `counter` **and** `thread-heavy`: destroy < 1 s, `survivors == []`, `session_leftovers == []`, `verified == true`, path named, session list count 0 |
| `a_guest_that_never_ends_is_killed_after_the_bounded_grace` | grace 0 on a never-ending app: `lifecycle == "killed"`, still < 1 s, no leftover |

Unit regression (no guest, runs in the normal suite):

```bash
cargo test --release -p acos-cli --offline --bin acos a_child_spawned
# engine::art::tests::a_child_spawned_through_the_spawner_survives_the_asking_thread ... ok
```

It also asserts the negative control (a child spawned *on* the asking thread is killed when that thread
exits), so it cannot silently stop testing the kernel behaviour it documents.

---

## 4. Validation log (actual output)

### 4.1 `gl1_kept_session` — 4/4 pass (real ART)

```
tools/art-runtime/run-exclusive.sh --task GL1 -- tools/guarded.sh --mem 8G --time 900 \
  -- /home/Qui/Projects/acos/.cache/gl1/validate.sh        # tests + gates in one lock hold

===== tests (gl1_kept_session)
running 4 tests
test a_guest_that_never_ends_is_killed_after_the_bounded_grace ...
  thread-heavy with a 0 ms grace: destroy 12 ms (reported teardown_ms 6, path "killed")  ok
test a_kept_session_survives_the_launching_client ... ok
test a_sigkilled_daemon_takes_its_guests_with_it ...
  live guests before SIGKILL: [2449090];  guests gone 0 ms after the daemon SIGKILL  ok
test explicit_destruction_is_bounded_and_leaves_nothing_behind ...
  counter.apk: destroy 16 ms (reported teardown_ms 11, path "lifecycle")
  thread-heavy.apk: destroy 17 ms (reported teardown_ms 12, path "lifecycle")  ok
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.63s
tests_rc=0
```

### 4.2 Owner gates — `acos verify --engine art`, release binary

| scenario | rc | verdict | steps pass/fail |
|---|---|---|---|
| `v2-art-counter` | 0 | pass | 11/0 |
| `w22-counter-reset` | 0 | pass | 9/0 |
| `w54-dialog-pixels` | 0 | pass | 14/0 |
| `w42-back-frame` | 0 | pass | 17/0 |
| `w15-dialog-menu` | 0 | pass | 3/0 |
| `suite/T03-render-good` | 0 | pass | 5/0 |
| `suite/T03-render-wrong` | 1 | fail | 2/1 (`ASSERTION_FAILED`, the declared expectation) |

**Note on `T03-render-wrong` rc.** The scenario declares `expectation: fail` and the run produced
exactly that (`assert_region` → `ASSERTION_FAILED`, 568 029 differing bytes against the shifted
baseline; `launch` and `wait_for` passed). The raw CLI still exits **1**, because `acos verify`
exits on the verdict (`crates/acos-cli/src/verify.rs:549-562`) and `apply_expectation` only *downgrades* a
false pass (`crates/acos-verify/src/executor.rs:585-646`) — untouched by this change, no GL1 code is on
that path. The expectation-aware rc 0 is the matrix runner's job (`tools/run-matrix.sh:206-216` maps
`expectation: fail` → `expected: fail`, variant `wrong`), so "declared failure met" is satisfied there,
not by the raw CLI. Not changed here: that would alter verification semantics, which the task forbids.

### 4.3 Rust tests — `cargo test --release -p acos-cli -p acos-art -p acos-services --offline`

```
65 test binaries: `test result: ok` in every one, 0 failed, 0 filtered failures.
   (includes the new `engine::art::tests::a_child_spawned_through_the_spawner_survives_the_asking_thread`
    and the new `tests/gl1_kept_session.rs` binary, whose 4 real-ART tests are `ignored` by default)
```
The known worktree-only failure `w50_serve_art::an_unknown_session_is_not_routed_to_the_frozen_rust_vm`
(108-byte socket path) did **not** appear in this run.

### 4.4 Manual before/after (the root-cause proof)

`/home/Qui/Projects/acos/.cache/gl1/repro.sh` (after) and `repro-prefix.sh` (same tree, one line
reverted to `command.spawn()`), both under `run-exclusive --task GL1 -- guarded.sh --mem 8G`:

```
# after:  launch --keep rc=0 → status pass engine art launched True stage first_frame → tree count="1"
#         → 9 545 882-byte PNG → destroy ok → 0 leftover acos-art
# before: launch --keep rc=0 → status rc=1 GUEST_EXITED (exited: signal 9) → tree rc=1 GUEST_EXITED
```

---

## 5. Remaining risks / open items

1. **The 120 s hang GR1 saw did not reproduce here.** `counter` and the new thread-heavy fixture both
   destroy in ~20 ms on the old 10 s deadline, i.e. they never reach the deadline. So the bound is now
   300 ms, but if Gaten's hang is *inside* the guest (uninterruptible I/O, a stuck binder peer) neither
   the old 10 s nor the new 300 ms bound changes the outcome: the group kill already happens. The
   remaining unknown is what GR1's session was doing; the new `lifecycle` / `teardown_ms` /
   `session_leftovers` fields make that diagnosable from the reply alone.
2. **The activity lifecycle is delivered by the guest's own `close` path**, which currently kills the
   guest right after the close ack (`crates/acos-art/src/launch.rs`). Whether `onDestroy` is actually
   delivered to the Activity before that kill is guest-side code (`crates/acos-art/guest`, not mine in
   this task). Explicit destruction is therefore honest as *destroyed*, but the `lifecycle` label means
   "the launcher ended by itself within the grace", not "onDestroy returned". Making the latter true is a
   guest-side change; flagged, not silently claimed.
3. **`session_leftovers` is a command-line scan.** It proves no launcher from this session survives; it
   cannot name a guest `dalvikvm` process (its argv does not carry the session path). The guest is
   covered transitively (launcher death → PDEATHSIG, proven at 1 ms in the SIGKILL test) and by the
   launcher's own `cgroup.kill` on the close path.
4. **Spawner thread is per process.** A caller that relies on `PDEATHSIG` firing when *its own* thread
   exits now gets "fires when the process exits" instead. That is the intended contract; no code relied
   on the thread-level behaviour.
5. Gaten itself was **not** run here: its launch is GR1's path (the RN driver is GR1's file set) and on
   `main` Gaten's release APK still dies at mount (`UNSUPPORTED_RN_DRIVER`, G1 §2.3), so a Gaten "close"
   cannot be reached from this branch without GR1's RN work. The addendum's stand-in
   (`fixtures/apps/thread-heavy`, 12 non-daemon threads that never exit) is what was measured; when GR1's
   launch lands, `acos session destroy --json` already reports `lifecycle`/`teardown_ms`/
   `session_leftovers` for it, which is exactly the evidence GR1's 120 s observation needs.
