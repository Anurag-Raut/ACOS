# SERVELEAK3 — merging SERVELEAK into REL3a: one signal handler, one default id, one teardown

Track SERVELEAK3. Branch `serve-leak-3` (from `main` 74e4ee9), worktree
`.cache/wt-sl3`, target `.cache/sl3-target`. Merge of `serve-leak-reap`
(a036ec6, base 18a5329) into `serve-leak-3`; semantic resolution only — no
test, gate or assertion was weakened (the one test edit is noted in §3.5).

Evidence: `.cache/sl3-logs/` (suite logs, smoke transcripts, repro scripts and
verdicts), `.cache/sl3/gates-*.json`, `.cache/sl3/b1|b2|b3/`.

## 1. The conflict surface

Git reported exactly two content conflicts; the rest of the merge was textual
and semantically clean (checked file by file, §2.5). The real problem was the
*duplicate mechanism* the textual merge produced everywhere else:

| # | What collided | Where |
|---|---|---|
| 1 | two signal handlers in `acos serve` | `crates/acos-cli/src/serve.rs` (conflict) |
| 2 | two `default_session_id()` | `crates/acos-mcp/src/session_backend.rs` (conflict) |
| 3 | two forwarding paths for session tools | `session_backend.rs` (`forward_session_tool` from REL3a + SERVELEAK's inline daemon branch in `session_call`, conflict) |
| 4 | two teardowns on stdin EOF | `SessionBackend::shutdown` (REL3a, inherent) vs `Backend::shutdown` (SERVELEAK, trait impl) — the inherent method *shadowed* the trait one |
| 5 | owned-session tracking covered only the external daemon | SERVELEAK's bookkeeping lived in the inline branch that REL3a's private daemon bypassed |
| 6 | a list-and-destroy-everything teardown | REL3a's `PrivateDaemon::shutdown` → `destroy_all` (lists **every** session the daemon knows) contradicted SERVELEAK2/F1 ("never kill what you cannot prove you own") |
| 7 | a reaped pid could be signalled again | `PrivateDaemon::shutdown` was not idempotent (SERVELEAK2/F4) |
| 8 | the private daemon had only `PDEATHSIG` as an owner link | SERVELEAK added `--owner-pid` exactly for a self-spawned daemon; REL3a's spawn did not use it |

## 2. How each was resolved

### 2.1 Signals (1) — SERVELEAK's, with REL3a's iteration-top check kept

`install_shutdown_signals()` (SIGTERM, SIGINT, **SIGHUP**, one `SHUTDOWN`
flag, no `SA_RESTART` so `poll` returns `EINTR`) is the only handler.
REL3a's `install_termination_handler()` / `on_terminate` / `TERMINATING` are
deleted. REL3a's `TERMINATING` semantics (exit at the top of the accept loop
without waiting for the poll timeout) are kept as `if shutdown_requested() {
break }` — the same predicate the mid-loop check uses, so there is one flag
and one mechanism. `TERMINATING` was not referenced anywhere else (checked).

Both parts of the contract still hold: a terminated daemon destroys its guests
through `engine.shutdown()` (SERVELEAK), and the MCP server's SIGTERM to its
private daemon reaches that path (REL3a).

### 2.2 Default session id (2) — one, per-instance random

`default_session_id()` is `mcp-<pid>-<16 hex>` (SERVELEAK2/F3's version).
REL3a's `mcp-<pid>` definition is deleted. It is filled **once**, in
`forward_session_tool` (via `session_base`), before anything is forwarded, so
both daemons see the same private id. `with_default_session()` (SERVELEAK's
second copy of that fill) is deleted; `session_base` already does it and also
maps a top-level `session_id` into `base.session_id` (REL3a's compatibility
shim).

### 2.3 One forwarding path (3, 5) — owned tracking on both daemons

`SessionBackend::session_call` no longer has a daemon branch of its own. It
calls `forward_session_tool`, which normalizes the args once and delegates to
the new `forward_to_daemon` (external daemon if configured, else the private
ART daemon, spawning/restarting it exactly as REL3a did). `forward_session_tool`
records a successful `acos_launch` in `owned` **with the launch token** for
whichever daemon served it. The duplicate `remember_owned` in
`impl Backend::session_call` is removed (it recorded an un-normalized id and
depended on entry point).

`destroy_owned_sessions` now applies the token check to every daemon
(`if !current.is_empty()` instead of `if self.daemon.is_some() && ...`): a
private daemon reports `session_token`s too, so the same proof covers it.

### 2.4 One teardown (4, 6, 7, 8)

* `SessionBackend::shutdown()` is the single teardown: `destroy_owned_sessions()`
  (through the daemon, token-checked) **then** take the `PrivateDaemon` out of
  its slot and `shutdown()` it (SIGTERM → the daemon's own handler destroys any
  remaining guest → reap → remove socket dir).
* `impl Backend::shutdown` delegates to it, so `Server::serve`'s stdin-EOF call
  (SERVELEAK) and `main`'s `art_daemon_handle().shutdown()` (REL3a) run the same
  code, in the right order. Order matters: once the daemon is SIGTERMed it can
  no longer answer `acos_destroy`.
* `PrivateDaemon::shutdown` no longer calls `destroy_all` (the function is
  deleted). It signals before reaping, and it sets `stopped` before signalling
  so a second `shutdown`/`Drop` never signals a reaped pid (F4).
* The private daemon is spawned with `--owner-pid <server pid>` in addition to
  `PR_SET_PDEATHSIG`: the kernel link and SERVELEAK's 1-second owner check are
  two independent backstops, and the second one also covers a server killed
  while its spawn thread is gone.

### 2.5 Files the merge touched that needed reading, not editing

* `crates/acos-cli/src/engine/serve.rs`: SERVELEAK's `launch_token` /
  `session_token` in `session_list` / `reap_leftovers()` and REL3a's
  `session_id()` default (now `default_session_id()`) are different functions —
  no interaction. `DEFAULT_SESSION` is gone from the tree.
* `crates/acos-cli/src/engine/art.rs`, `crates/acos-art/src/launch.rs`: single
  side only (SERVELEAK).
* `crates/acos-mcp/src/server.rs`: SERVELEAK's `self.backend.shutdown()` after
  stdin EOF, unchanged.
* `crates/acos-mcp/tests/art_default.rs` — see 2.6.

### 2.6 The one test edit (recorded honestly)

REL3a asserted `id == format!("mcp-{}", pid)` exactly. With the merged
per-instance default that can never hold. The assertion now requires the
`mcp-<this server's pid>-` prefix **and** a non-empty hex suffix, and the
cross-server `assert_ne!` is kept — i.e. the test still proves "the default id
names this server instance, and two instances never share one", which is
strictly stronger than the old exact match. Nothing else in either branch's
tests changed.

## 3. Acceptance

Every ART run and every `acos-cli`/`acos-mcp` test went through
`/home/Qui/Projects/acos/tools/art-runtime/run-exclusive.sh --task SERVELEAK3 --
tools/guarded.sh --mem … --time … --`; builds through `tools/guarded.sh --mem
14G --vmem 40000000 --time 1500 --` with `CARGO_TARGET_DIR=.cache/sl3-target`.

### 3.1 Tests — **PASS**

`cargo test --release --offline -p acos-cli -p acos-mcp`, split per target to
stay inside the lock's 10-minute hold, 0 failed:

| log | contents |
|---|---|
| `.cache/sl3-logs/cli-units.log` | `acos` bin unittests: **118 passed** (incl. SERVELEAK's `engine::art::tests::*`) |
| `.cache/sl3-logs/cli-int-a.log` | cli 6, run_dex 2, sandbox_s8 7, serve_daemon 1, w44_trust 4, w46_error_taxonomy 7, w51_untrusted_policy 5, w55_dead_guest 3 |
| `.cache/sl3-logs/cli-int-b.log` | gl1_kept_session 0/4 ignored (pre-existing), native_launch 4, negative_verify 1, verify 2, verify_engine_art 4, w50_serve_art 3/1 ignored (pre-existing) |
| `.cache/sl3-logs/cli-session.log` | session **11 passed** |
| `.cache/sl3-logs/mcp.log` | acos-mcp: unit 53, art_default 4, bounded 3, docs 1, e2e 1, flutter 1, guide 7, native_session 1, protocol 20, rn_session 1, **serveleak2_ownership 1**, **serveleak_shutdown 1**, session 3 |

The three tests the packet calls out (`art_default`, `serveleak_shutdown`,
`serveleak2_ownership`) are green in the merged tree.

Note: `acos-cli` has no library target, so `--lib --bins` is a cargo error
(exit 101, `no library targets found`); `--bins` is the correct invocation.

### 3.2 Installed-prefix smoke (REL3a contract, defaults) — **PASS**

```
tools/release/install.sh --from-target WT/.cache/sl3-target \
  --prefix WT/.cache/sl3/prefix          # installed ACOS 0.2.0, 1.5G, 3135 files
```

Run from `WT/.cache/sl3/outside` (empty), `env -i` with only `HOME, USER, LANG,
PATH=<prefix>/bin:…, XDG_RUNTIME_DIR=<WT>/.cache/sl3/rt,
DBUS_SESSION_BUS_ADDRESS, ACOS_ALLOWED_ROOTS=/home/Qui,
ACOS_ARTIFACTS_DIR=<WT>/.cache/sl3/artifacts`. **No `ACOS_SESSION_DAEMON`, no
`ACOS_BIN`, no `engine` argument** — the server spawns its own
`acos serve --engine art --owner-pid <pid>`.

| run | transcript | result |
|---|---|---|
| `--mode counter` | `.cache/sl3-logs/smoke-counter.jsonl` | 15/15 checks, `{"smoke": "ok"}`; launch `pass`/`art-reference`/`engine art`, tree `0`+`Increment` → tap → `1` → reset → `0`, destroy, `status` `SESSION_NOT_FOUND` |
| `--mode gaten` | `.cache/sl3-logs/smoke-gaten.jsonl` | 15/15 checks, `{"smoke": "ok"}`; real RN tree, tab/CTA tap from the tree's bounds, tree changed, destroy + status gone |

Environment notes (not code): `env -i` must carry `DBUS_SESSION_BUS_ADDRESS`
and a runtime dir in which `systemd-run --user` can reach the user bus, or the
ART launcher reports `MEMORY_CGROUP_UNAVAILABLE` and the guest exits 2 (that is
what the first two attempts hit, `.cache/sl3-logs/smoke-counter.jsonl` history).
`PATH` must resolve `acos` **inside the prefix**: with the owner's
`~/.local/bin` first, `BackendConfig::discover()` picks the older installed
`acos`, which predates `--owner-pid`, and the private daemon exits immediately
(pre-existing discover-order hazard on `main`, not introduced here).

### 3.3 Gates, `acos verify --engine art` (worktree build) — **PASS**

`.cache/sl3/gates-{scenario}.json`:

| scenario | result |
|---|---|
| `v2-art-counter` | pass, art-reference, engine art, **11/11 steps** |
| `w22-counter-reset` | pass, art-reference, engine art, **9/9 steps** |
| `form-validation` | pass, art-reference, engine art, **28/28 steps** |

### 3.4 Live repros

**B1 — owner `kill -9` → guest + daemon gone: PASS.**
`.cache/sl3-logs/repro_b1.py`, output in the run log; daemon log
`.cache/sl3/b1/serve.log`.

```
launch_ok true   session mcp-3345650-76f0e8b5476dec67
client SIGKILLed; gone_after_s=0.192   left={}   daemon_alive=false
serve.log: "acos serve: owner pid is gone; destroying sessions and exiting"
           "acos serve: closed ART session mcp-3345650-76f0e8b5476dec67"
```

Bound for this design: ≤ ~1 s (the accept loop's 1 s owner tick); measured
0.19 s.

**B2 — MCP stdin EOF → its own session gone: PASS.**
`.cache/sl3-logs/repro_b2.json` (`frames.jsonl`, `mcp.log` beside it).

```
guest before close: [acos-art launch --apk …/counter.apk …]   (1 live guest)
mcp_exit_s=0.063   log: "acos-mcp: destroyed session mcp-3363467-90609d5bf9db4517 on exit"
all_gone_after_s=0.067  left={}  private daemon (3363476) gone
```

**B3 — two concurrent verifies, same relative artifacts path, one nested: PASS.**
`.cache/sl3-logs/repro_b3.py`, `.cache/sl3/b3/{a,b}.log`.

The shape is SERVELEAK2/F1's collision: A's artifacts dir is a *prefix* of
B's, and both are passed as the same relative path `out/counter`.

```
A = .cache/sl3/b3/a      w22-counter-reset  --artifacts out/counter  -> pass, 9/9,  art, 2.48 s
B = .cache/sl3/b3/a/b    form-validation    --artifacts out/counter  -> pass, 28/28, art, 8.58 s
B started first; while B's guest was live (pids 3399441 acos-art, 3399450 bwrap)
A ran and finished:     alive_when_a_exited=true   overlap=true
leftovers=[]
```

So A's teardown ran while B's guest was alive and B still passed — the run that,
before SERVELEAK2/F1, killed B's guest. `verdict: pass`.

Repro-script notes (not product code): the launcher is given the artifacts path
*relative to its cwd*, so guest identity has to be matched on `/proc/<pid>/cwd`
under the run root, not on the absolute artifacts path in the command line (the
first two attempts missed it, which is why they report
`b_guest_seen_while_both_ran=false`). And because each run acquires its own
`run-exclusive` slot, a long queue can serialize the two runs; the script starts
B first and waits for its guest before starting A, so the window is exercised
regardless of queue order.

## 4. Commits

* `7b23c66` — the merge (both parents), with the resolutions above.

## 5. Remaining

* Nothing outstanding for this track's acceptance.
* Not fixed, unchanged, worth a look: `PrivateDaemon`'s artifacts land in
  `<XDG_RUNTIME_DIR>/acos/art-sessions/<session>` and are never pruned (only the
  socket dir is removed), and `BackendConfig::discover()` can pick up any
  `acos` earlier on `PATH` than the installed prefix — the merged MCP then
  talks to a daemon that may not support `--owner-pid` (the private daemon exits
  immediately and every session tool answers `INTERNAL`). The suffix-tolerant
  fix would be to retry the spawn without `--owner-pid` if the daemon exits with
  a usage error; it was not needed for a same-version install, so it is left as
  the next smallest action.

## Review (Opus, 2026-11-15) and fix-up

Verdict: MERGE AFTER FIXES (finding 1). Fixed here:
1. `PrivateDaemon::shutdown` signalled `child.id()` with raw `libc::kill` even when `wait_ready` had
   already reaped the child (daemon died before its socket was ready) — a recycled pid could be hit.
   Now SIGTERM is sent only when `try_wait()` is `Ok(None)` (child provably unreaped).
Deferred (low, no foreign kill): (2) teardown can respawn a dead private daemon (add a no-restart
flag for teardown calls); (3) the destroy-by-id fallback when `session_list` fails skips the token
check (on the external daemon: skip + log a leak instead); (4) 5 s SIGKILL deadline can cut a slow
multi-guest engine shutdown; (5) no signal handler in `acos-mcp` → sessions on an EXTERNAL daemon
leak until its idle timeout on SIGTERM; (6) `/tmp/acos` fallback base dir (no XDG_RUNTIME_DIR /
TMPDIR) is shared between users — use a per-uid base or verify owner/mode. Queued for FB5.
