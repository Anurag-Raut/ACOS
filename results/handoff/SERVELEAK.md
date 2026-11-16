# SERVELEAK — a destroy must always leave no guest; a daemon must not outlive its purpose

Branch `serve-leak-reap` (from `main` 18a5329). Worktree `.paseo/worktrees/3oxa6xao/k5`.
Evidence: `.cache/serveleak/evidence/` (live repros, gate verdicts), original failure
`/home/Qui/Projects/acos/.cache/wt-abmain3/results/bench/run-20261114T132334Z` (read-only).

## 1. Root cause

Two independent faults, one shared shape: **the session's teardown proof did not cover the
guest**.

### 1.1 The guest is in its own process group, so the proof was blind to it

`crates/acos-art/src/launcher.rs` spawns the real guest with `cmd.process_group(0)`, so the
tree is two process groups:

```
acos serve (daemon, own session)
└─ acos-art launch --until activity-resumed --out <artifacts>/art-frames   pgid A  ← what acos-cli owned
   └─ bwrap --unshare-pid …                                                pgid B  ← the guest
      └─ dalvikvm64 …                                                      pgid B
```

`ArtSession::process_group` is A (`engine/art.rs`, `command.process_group(0)` in the CLI's
`ArtSession::start`). `shutdown` wrote `close`, waited the bounded grace, then proved
`kill(-A, 0)` / `processes_in_group(A)`. That proof says nothing about **B**. Consequences:

* the clean path could report `verified: true, survivors: []` over a *live* guest — a false
  clean, and
* the only check that did see B was the wider `processes_mentioning(<artifacts>)` scan, and
  `acos_destroy` used it as a **report**, never as an action: `Ok(report)` with
  `leftovers` non-empty produced `PROCESS_TREE_DIRTY` as `INTERNAL` and killed nothing.

### 1.2 The bench's daemon had no shutdown path at all

`acos serve` installed no signal handler. `d2_warm.Serve.close()` does
`proc.terminate()` (SIGTERM), so the daemon died **before** `engine.shutdown()` and every
live session's guest was left to `PR_SET_PDEATHSIG` / `bwrap --die-with-parent` alone. Those
are asynchronous backstops, not a teardown: in the failing run the `acos-art` launcher and
both `bwrap` processes were still alive after the bench exited, which is exactly what the
run's own `survivors` list records:

```
acos serve --socket …/r1-a/latency/serve.sock --engine art --idle-timeout 900 --max-sessions 8
acos-art launch --apk fixtures/build/apps/form-validation.apk --until activity-resumed --out …
bwrap --unshare-pid …   (x2)
```

Read together with that run's `failures.rows` (`step: "destroy"`, `code: "INTERNAL"`,
`message: ""`, `wall_ms: 0.0`, empty `teardown`), the destroy was an error envelope with no
result — i.e. the `PROCESS_TREE_DIRTY` branch. `wall_ms 19.46` in `teardowns[]` rules out the
2 s survivor grace, so the failing condition was the leftover check: the destroy saw a
process naming the session's artifacts directory, reported `INTERNAL`, and killed nothing.
The m8-form step is also the only one that runs 42 `acos_reset`s, i.e. 42 guest
relaunches under one session directory, which is where a launcher/bwrap from the previous
incarnation can still be winding down at the moment of the destroy.

### 1.3 Why one guest survived while others did not

`kill(-A)` cannot reach B, and B's death depended on the *launcher* dying and the kernel
delivering `PR_SET_PDEATHSIG` to `bwrap`. When the launcher had already been reaped (or was
killed while the guest was mid-teardown / in uninterruptible I/O), nothing in ACOS ever
signalled B, so it stayed until it was killed by hand.

## 2. Fix (what changed)

1. **`acos-art` reports the guest's process group.** The interactive ready line now carries
   `guest_process_group` (the guest's pid/pgid). `crates/acos-art/src/launch.rs`.
2. **A session owns both groups.** `ArtSession` stores `guest_group`; `own_groups()` returns
   the launcher group and the guest group, guarded (never `<= 1`, never this process's own
   group). `sigkill_group`/`kill_group`/`group_is_empty`/`teardown_report` all operate on the
   union, so the proof is about the *guest*, not the wrapper. `crates/acos-cli/src/engine/art.rs`.
3. **Cleanup happens before the report, always.** `shutdown` SIGKILLs the tree, re-signals
   during the bounded poll (a process can fork between the scan and the signal) and only
   reports survivors it could not kill. `acos_destroy`'s leftover check is now
   `reap_leftovers()`: it kills what it finds and reports what is still alive afterwards, so
   "named a leftover" can no longer be an alternative to killing it. The error path
   (`Err` from teardown) also carries `lifecycle`/`session_leftovers` now, but the cleanup has
   already run.
4. **The daemon reaps on SIGTERM/SIGINT/SIGHUP.** `acos serve` records the signal
   (async-signal-safe flag, no `SA_RESTART` so `poll` returns `EINTR`) and exits through
   `engine.shutdown()`, so a terminated daemon destroys its guests and proves them gone before
   the socket disappears.
5. **`acos serve --owner-pid PID`.** The daemon exits (destroying sessions) when the process
   that spawned it for its own use is gone, even after `kill -9`. The bench passes its own pid;
   the shared, auto-started daemon (`acos session …`) passes none, because another client may
   attach to a kept session.
6. **MCP stdin EOF destroys the sessions it launched.** `Backend::shutdown()` (default no-op)
   is called by `Server::serve` after EOF; `SessionBackend` tracks the ids *it* launched and
   destroys exactly those — through `acos_destroy` when it forwards to a daemon, through the
   in-process engine otherwise. Another client's sessions are never touched.
7. **Bench teardown.** `acos-bench.sh` traps EXIT/INT/TERM and reaps only daemons whose command
   line names *this run's* scratch root; `d2_warm.Serve.close()` waits for the daemon and
   group-kills one that had to be SIGKILLed. (A hard `kill -9` of the harness is covered by 5.)

Deliberately **not** changed: `PR_SET_PDEATHSIG` and `bwrap --die-with-parent` stay as
backstops, and the kept-session semantics stay (a client disconnecting does not destroy its
sessions). The chosen mechanism is the smallest one that makes the teardown proof true —
kill the group the guest actually leads — instead of adding another asynchronous watchdog.

## 3. Evidence

### 3.1 Tests (new)

* `crates/acos-cli/src/engine/art.rs`:
  * `destroy_kills_the_guest_group_the_launcher_reports` — a fake `acos-art` whose guest
    runs in its own group (`setsid`), reports it, and **exits on `close` leaving the guest
    running**. Asserts the teardown is clean, `lifecycle`, and the guest pid is gone. This is
    the bench's shape (before the fix: `INTERNAL` + live guest, or a false clean).
  * `destroy_kills_the_whole_tree_when_close_is_ignored` — the graceful path fails (the
    launcher ignores `close` past the grace), both groups die, `lifecycle: "killed"`, no
    survivor.
* `crates/acos-mcp/tests/serveleak_shutdown.rs`:
  `stdin_eof_destroys_the_sessions_the_server_launched` — a real `Server` over pipes against a
  fake daemon socket: `acos_launch` → stdin closed → the daemon receives `acos_destroy` for
  that session id (and only that one).

### 3.2 Suite

`cargo test --release --offline -p acos-cli -p acos-mcp` — green (acos-cli 93 bins tests +
all integration binaries, acos-mcp all suites, incl. the new one), under
`run-exclusive.sh --task SERVELEAK` + `guarded.sh --mem 10G --time 900`.

### 3.3 Live repro A — the owning client is SIGKILLed (B1)

Client spawns `acos serve --owner-pid <self>`, launches `counter.apk` over the socket
(`launch_ok: true`), then the watcher SIGKILLs it.

```
before: daemon=2132689 session procs=[2132689 acos, 2132695 acos-art, 2133212 bwrap, 2133217 bwrap]
client SIGKILLed at t=0
gone_after_s = 0.12   left = {}   daemon_alive = false
```

daemon log: `acos serve: owner pid is gone; destroying sessions and exiting` /
`acos serve: closed ART session s_repro`.
Bound for this design: **≤ ~1 s** after the owner dies (the accept loop's 1 s held-session
tick), measured 0.12 s. Evidence: `evidence/live-repro-b1-verdict.json`, `…-serve.log`.

### 3.4 Live repro B — MCP stdio session (B2)

`acos-mcp` with `ACOS_SESSION_DAEMON=1` against a daemon with `--idle-timeout 2`:
initialize → `acos_launch counter.apk` (forwarded to the daemon → real ART) → close stdin.

```
guest before close: [daemon, acos-art, bwrap, bwrap]
mcp_exit_s = 0.031            mcp log: "acos-mcp: destroyed session s_mcp on exit"
all_gone_after_s = 2.029      left = []   daemon_alive = false
```

Guest gone at MCP exit (0.03 s); the shared daemon then exits by its idle timeout (2 s) —
MCP does not own it. Evidence: `evidence/live-repro-b2-mcp-report.json`, `…-mcp.log`,
`…-serve.log`.

### 3.5 Gates (`acos verify --engine art`, real ART/API 34)

| scenario | verdict | engine | steps |
|---|---|---|---|
| `scenarios/v2-art-counter.yaml` | pass | art | 11 |
| `scenarios/w22-counter-reset.yaml` | pass | art | 9 |
| `scenarios/form-validation.yaml` | pass | art | 28 |

No process of those runs survived (`pgrep` filtered to this worktree's paths: none). Verdicts:
`evidence/gate-*.json`.

### 3.6 Reproduce

```sh
# tests (acos-cli + acos-mcp, incl. the new ones)
tools/art-runtime/run-exclusive.sh --task SERVELEAK -- tools/guarded.sh --mem 10G --time 900 -- \
  env CARGO_TARGET_DIR=$PWD/.cache/k5-target cargo test --release --offline -p acos-cli -p acos-mcp

# gates
for s in v2-art-counter w22-counter-reset form-validation; do
  tools/art-runtime/run-exclusive.sh --task SERVELEAK -- tools/guarded.sh --mem 10G --time 900 -- \
    .cache/k5-target/release/acos verify --scenario scenarios/$s.yaml --engine art --json
done

# live repros (scripts kept out of git: worktree .cache/serveleak/)
#   repro_b.py + watch_b.py   -> SIGKILL the owning client, measure guest+daemon death
#   repro_mcp.py              -> MCP stdio session, close stdin, measure leftovers
```

Two environment notes for this worktree (not code): the suite's e2e test looks for
`<worktree>/target/release/acos`, so `.cache/k5-target/release/acos` was symlinked there; and
`fixtures/build` was a symlink to the main checkout's build output, which made `acos_inspect`
fail with `UNSAFE_PATH` (the path canonicalised outside the allowed root) — it was replaced
with a real copy. Both are gitignored/untracked and unchanged by this branch.

## 4. Remaining / not done

* The `acos session …` auto-started daemon is intentionally not owner-bound (a kept session
  must outlive the launching CLI). A crash of a `acos session` client therefore leaves its
  session until `--idle-timeout` — but the timeout only fires with **no** sessions, so a
  session whose client vanished is still held. A per-session TTL (`--session-ttl`, destroy a
  session untouched for N seconds) is the obvious next step; it changes agent-visible
  behaviour, so it was not added here.
* `acos-art`'s own suite (`cargo test -p acos-art`) was not run in this pass; the change is
  one added ready-line field plus a parameter. The three gates above exercise it end to end
  on real ART.
* The bench itself was not run (not needed): the driver teardown was changed in
  `d2_warm.Serve.close()` + the new `acos-bench.sh` trap, and the daemon-side behaviour is
  covered by 3.3/3.4.

---

# SERVELEAK2 — the kill paths may only kill what this session provably started

Follows SERVELEAK. A safety review (fresh Opus, read-only) said **do not merge**:
SERVELEAK made destroy/shutdown *kill* leftovers, but the kill paths could kill
processes the session never started. ACOS runs many agents' guests on one host,
all binding the same shared sysroot/boot/font paths, so "the same paths on the
command line" is not ownership. This section maps every finding to its fix, its
test and its live evidence. Branch `serve-leak-reap`, merged with `main cb5bd04`.

## F3 first (it decides what the shipped preview needs) — does the collision
## already exist on `main` cb5bd04 / the installed 0.2.0 build?

**The collision is a real hazard in the source; the destructive half exists only
on this branch.** Facts, checked on `main` cb5bd04 and on the installed build:

* `acos-mcp` uses the daemon backend **only** when `ACOS_SESSION_DAEMON` is truthy
  (`daemon::mcp_daemon_requested`). It is unset in this environment and in the
  owner's agent configs, so the default MCP server holds sessions **in-process**,
  where the id space is per process: two servers' `s_mcp` cannot collide.
* `main` has **no** destroy-on-exit: `Server::serve` on stdin EOF only calls
  `shutdown_and_join()` (transport); there is no `Backend::shutdown` call. So even
  in daemon mode with a shared id, another server's EOF destroys nothing.
* The installed 0.2.0 `acos-mcp` (`~/.local/share/acos/0.2.0/bin/acos-mcp`,
  installed 2026-11-14 from `.cache/h4-target`) contains no
  `acos-mcp: destroyed session` string: it predates SERVELEAK's destroy-on-exit
  entirely. The shipped preview therefore cannot kill another agent's guest.
* What *is* already true on `main`: with `ACOS_SESSION_DAEMON=1` and one shared
  daemon, two agents using the default id address the same held session — the
  second launch replaces the first's guest (a correctness hazard: A then talks to
  B's guest), and the daemon falls back to the constant `s_mcp` for a missing
  `base.session_id` (`engine/serve.rs::session_id`). No cross-agent kill.

So the F3 fix is **separable** (its own commit, `serve: a default session id
belongs to one server, not to every agent`) and is the piece to cherry-pick if a
release ever carries SERVELEAK's destroy-on-exit. It is additive on the wire: an
older daemon that does not report tokens keeps SERVELEAK's id-only behaviour.

## Fixes

### F1 (HIGH) — a command-line substring is not ownership
`processes_mentioning` matched `acos-art` + *any* command line containing the
artifacts path, and it was a **kill** path (`reap_session_leftovers` →
`sigkill_group` → `shutdown`/`reap_leftovers`). Agent A `acos verify counter.yaml`
and agent B the same in another worktree both name `out/counter`; inside one
daemon `s1` matched `…/art-sessions/s10`. Now:

* **Ownership is proved three ways and nothing else** (`ArtSession::owned_pids`):
  membership of a process group the session recorded; the `/proc` ppid chain from
  the launcher pid this session spawned; and a random per-session token exported
  as `ACOS_ART_SESSION_TOKEN` into the launcher's environment, which `bwrap`
  inherits (the guest runs under `bwrap --clearenv`, so the token reaches bwrap
  and the guest is covered by bwrap's group/descendant proofs).
* **Only provable pids are signalled** (`provable_pids`, i.e. descendants + token).
  A recorded group's members that cannot be attributed are still *reported* as
  survivors (never a false clean) but are never killed on a guess
  (`serve: report a group member the session cannot attribute, never hide it`).
* `kill(-group)` is issued only for `owned_live_groups` — groups that currently
  hold at least one provable pid (F4 below).
* The command-line match survives **report-only** (`foreign_artifacts_mentions`,
  written to the dm6 trace): it canonicalises the artifacts directory and
  requires a whole argv element to equal it or start with `<dir>/`, so
  `out/counter` cannot match `…/out/counter2`.
* Per-daemon session directories: `acos serve` already separates daemons by
  `ACOS_RUNTIME_DIR`/`XDG_RUNTIME_DIR`, but two daemons under the same runtime
  dir would still share `<runtime>/art-sessions`. The teardown no longer depends
  on directory identity at all (ownership does), so a shared `art-sessions` root
  is no longer a kill hazard; the F3 token/per-launch id work additionally makes
  an MCP-side id unique per server instance.

### F2 (MEDIUM) — the bench trap reaped a *shared* scratch root
`acos-bench.sh`'s trap fired on normal EXIT and matched
`pgrep -f "acos serve .*$SCRATCH_ROOT/r[0-9]"`; `SCRATCH_ROOT` defaults to the
shared `.cache/h6` and both runs create `r1-<group>`, so the first run to finish
SIGKILLed the other run's daemons. Now:

* the bench exports a per-run `ACOS_BENCH_RUN_ID` and `ACOS_BENCH_PIDFILE`;
  `d2_warm.Serve` records `pid starttime` for every daemon it spawns;
* `tools/bench/reap-daemons.sh` reaps **only those pids**, re-verifying each one
  immediately before signalling (exists, run id in `/proc/<pid>/environ`, still
  `acos serve`, identical `/proc/<pid>/stat` start time — a recycled pid fails);
* the leak report (`d2_warm.survivors`) filters by run id, so a concurrent run's
  processes are not reported as this run's survivors.

### F3 (MEDIUM) — a default session id belonged to every agent
See above. `default_session_id()` is `mcp-<pid>-<random>` (per server instance),
injected into the forwarded args (`with_default_session`) because the daemon
otherwise defaults a missing id to `s_mcp`; every `acos_launch` returns a
`session_token` that `session_list` reports, and `SessionBackend::shutdown`
destroys only a session whose current token is still the one it launched.

### F4 (LOW) — a recycled group id must never be signalled
* `shutdown` clears `process_group`/`guest_group`/`launcher_pid`/token/pidfd once
  the teardown **proves** the tree empty (`proven_empty`); a later `Drop` or
  repeat `shutdown` therefore issues no `kill(-group)` at all.
* The launcher is held by a **pidfd** (`pidfd_open`) and SIGKILLed through it;
  the process-group signal is only sent while the leader is still unreaped (its
  pid, hence its group id, cannot be recycled), and group signals are derived
  from provable members (`owned_live_groups`), never from a stale record.
* `d2_warm.Serve.close()` no longer `killpg`s after `wait()`: the group is
  signalled *before* the leader is reaped.
* `kill_group_signals()` (a process-wide counter) is the test seam.

## Evidence

### Tests (new, all green)

| finding | test |
|---|---|
| F1 | `engine::art::tests::destroy_kills_only_its_own_guest_when_artifact_paths_share_a_prefix` (`out/counter` vs `<dir>/x/out/counter`) |
| F1 | `engine::art::tests::destroy_kills_only_its_own_guest_when_session_dirs_are_prefixes` (`s1` vs `s10`) |
| F2 | `tools/bench/test-reap-scope.sh` — two fake runs, one `SCRATCH_ROOT`: only the caller's daemon is reaped; a recycled-pid record and a wrong-run-id daemon are refused (4/4 PASS) |
| F3 | `crates/acos-mcp/tests/serveleak2_ownership.rs::a_default_id_does_not_let_one_server_destroy_another_servers_session` — two `Server`s against one fake daemon, A's EOF leaves B's session alive, B's EOF destroys it |
| F4 | `engine::art::tests::a_proven_empty_session_issues_no_further_group_signal` |

Plus SERVELEAK's own regressions
(`destroy_kills_the_guest_group_the_launcher_reports`,
`destroy_kills_the_whole_tree_when_close_is_ignored`,
`serveleak_shutdown::stdin_eof_destroys_the_sessions_the_server_launched`).

`cargo test --release --offline -p acos-cli -p acos-mcp` under
`run-exclusive.sh --task SERVELEAK2` + `guarded.sh --mem 10G --time 900`:
**28 result groups, 0 failed** (`.cache/serveleak2/suite2.log`).

### Live repros (real ART/API 34, worktree `.cache/serveleak2/`)

* **B1** owner `kill -9`: daemon + launcher + both bwrap gone in **0.337 s**,
  `left = {}`, `daemon_alive = false` (`evidence/live-repro-b1.json`).
* **B2** MCP stdin EOF: guest gone at MCP exit **0.063 s** (log
  `acos-mcp: destroyed session … on exit`), shared daemon exits by its 2 s idle
  timeout, `left = []` (`evidence/live-repro-b2.json`).
* **B3 (new)** two concurrent verifies with the *same relative* artifacts path
  (`out/counter`) from two different directories, one nested inside the other:
  A = `w22-counter-reset` (9 steps, finished 2.47 s), B = `form-validation`
  (28 steps, 8.54 s). Both **pass**, `overlap: true`, B was **live when A's
  teardown ran** (`alive_when_a_exited: true`, 1 live guest), no leftovers
  (`evidence/live-repro-b3.json`). This is the run that, before the fix, killed
  B's guest.

### Gates (`acos verify --engine art`, real ART)

| scenario | verdict | engine | steps |
|---|---|---|---|
| `v2-art-counter` | pass | art | 11 |
| `w22-counter-reset` | pass | art | 9 |
| `form-validation` | pass | art | 28 |

## Decisions / notes

* A recorded-group member that no proof attributes is **reported, not killed**
  (a false `PROCESS_TREE_DIRTY` is the honest outcome; a guess-kill is not).
* The token is best-effort by design: `bwrap --clearenv` strips it from the
  guest's own environment, so the guest rests on the recorded guest group and on
  bwrap's token/descendant chain. All three proofs overlap deliberately.
* `processes_mentioning` is now trace-only; nothing in a teardown decision reads
  a command line.
* Not done: the per-session TTL noted in SERVELEAK §4 is still absent; the
  `pdf`/framebuffer leaks are untouched; the bench was not run end to end (F2 is
  covered by the dry test — wrapping `acos-bench.sh` in `run-exclusive.sh` is
  forbidden by the operating directives).

## Reproduce

```sh
# tests (both crates, incl. the F1/F3/F4 tests)
tools/art-runtime/run-exclusive.sh --task SERVELEAK2 -- tools/guarded.sh --mem 10G --time 900 -- \
  env CARGO_TARGET_DIR=$PWD/.cache/k5-target cargo test --release --offline -p acos-cli -p acos-mcp

# F2 dry test (no ART)
sh tools/bench/test-reap-scope.sh

# gates
for s in v2-art-counter w22-counter-reset form-validation; do
  tools/art-runtime/run-exclusive.sh --task SERVELEAK2 -- tools/guarded.sh --mem 10G --time 900 -- \
    .cache/k5-target/release/acos verify --scenario scenarios/$s.yaml --engine art --json
done

# B3 (two lock slots, one per concurrent run)
WT=$PWD ACOS_BIN=$PWD/.cache/k5-target/release/acos \
  ACOS_WRAP="/home/Qui/Projects/acos/tools/art-runtime/run-exclusive.sh --task SERVELEAK2 -- \
  /home/Qui/Projects/acos/tools/guarded.sh --mem 10G --time 900 --" \
  python3 .cache/serveleak2/b3.py .cache/serveleak2/b3
```

Environment note (not code): `acos-art` must be built in the same target dir as
`acos` (`cargo build --release -p acos-cli -p acos-art`), otherwise a stale
launcher builds the guest from an old source list and every run fails with
`javac failed … SoftwareKeyStore`.
