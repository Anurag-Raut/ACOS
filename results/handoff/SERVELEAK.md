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
