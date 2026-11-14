# FB5 — session hygiene, status list, absolute-path rule, per-rung ms, SERVELEAK3 follow-ups

Track FB5 (owner priority: agent feedback). Branch `fb5-session-hygiene` in
`.cache/wt-sl3`, cut from `serve-leak-3` (d624bc8, SERVELEAK3, **not modified**)
and merged with `52beccf` (int-021, the v0.2.1 candidate).

Evidence: `.cache/fb5/logs/` (live MCP transcript, both smoke runs),
`.cache/fb5/gates-*.json`, `.cache/fb5/gate-art/<scenario>/`.

## 1. The merge

`git merge --no-edit 52beccf` reported **one** conflicted file,
`crates/acos-cli/src/engine/art.rs` (two hunks). Both were kept, semantically:

| hunk | serve-leak-3 side | int-021 side | resolution |
|---|---|---|---|
| 1 | SERVELEAK2/F4 `leader_pidfd = open_pidfd(pid)` | ZPROD5 stderr drain + ZPROD3 zygote contract wait (and its error-path `kill(-pid, SIGKILL)`) | **both**: pidfd first, then the stderr/zygote block. The zygote error-path group kill now calls `note_group_signal()` first, so F4's "every group signal is counted" invariant still holds |
| 2 | `SESSION_TOKEN_ENV` / `GROUP_SIGNALS` / `note_group_signal` / `kill_group_signals` + the `new_session_token` doc | `teardown_grace_ms_for(zygote)` and the `processes_mentioning` doc line | **both**: `teardown_grace_ms_for` inserted after HEAD's counters, the duplicate `processes_mentioning` doc line dropped (the function already has its own doc), `new_session_token`'s doc restored |

The single-mechanism invariants from SERVELEAK3 are intact: one signal handler
(`install_shutdown_signals`), one `default_session_id()`, one
`forward_to_daemon` (now `forward_to_daemon_opts` with the FB5 restart policy),
owned+token tracking on both daemons, one `SessionBackend::shutdown`.

`fixtures/build` in the worktree is a real hardlinked copy (`cp -al`): per-file
link counts > 1, no symlink.

## 2. What was implemented

### (1) Idle-TTL auto-destroy of this server's sessions

* `DEFAULT_IDLE_TTL_S = 600`; `ACOS_SESSION_IDLE_TTL_S` (seconds, `0` disables)
  overrides it; the previously **inert** documented `ACOS_MCP_SESSION_TTL_MS`
  still works as a millisecond fallback; `acos_launch {idle_ttl_s: N}` overrides
  per session.
* `SessionBackend` now keeps `OwnedSession { token, label, launched_at,
  last_used, idle_ttl_s }`; every MCP call on an owned session renews
  `last_used`; a successful `acos_destroy` drops the record.
* `start_idle_reaper()` (called from `main` on the `Arc<SessionBackend>`) runs a
  250 ms sweep thread holding only a `Weak`, so it stops with the backend or at
  `shutdown`. `sweep_idle()` is public for deterministic tests.
* Destroy goes through the daemon with the SERVELEAK2/F3 token check and
  `Restart::Never`; success logs exactly one line
  (`acos-mcp: session <id> (<label>) was idle for N s (idle TTL N s); destroyed it`).
* A reclaimed id becomes a tombstone (`expired`, bounded to 32). Any later call
  on it (except `acos_launch`, which clears it) returns `SESSION_NOT_FOUND`
  with `details.reason = "idle_ttl_expired"`, `details.idle_ttl_s`, the label,
  and a message/hint naming the TTL — the closest existing code, per the packet.
* `launch` and `status` replies carry `label` and `idle_ttl_s`.

### (2) `acos_status` lists this server's sessions

`acos_status` is no longer a bare forward: it first answers from this server's
`owned` map (id, `label`, `package`, `activity`, `age_s`, `idle_s`,
`idle_ttl_s`, `guest_pid`, `guest_rss_kb`, `live`, `stage_reached`) and attaches
`sessions[]` to every status reply, including the non-owned path. A successful
`acos_destroy` removes the session, so `status` after `destroy` is an honest
`SESSION_NOT_FOUND` (never a null-field pass). `label` is accepted on
`acos_launch` (descriptor + docs).

Guest facts come from the daemon's `session_list`, which now reports `activity`
and `guest_pid` (the guest's own process group, `ArtSession::guest_pid()`);
`guest_rss_kb` is read from `/proc/<pid>/status`.

### (3) Relative paths in MCP tool arguments

`safety::relative_path_arg` refuses a relative `apk`, `scenario` or `artifacts`
with `INVALID_ARGUMENT`, `details = {field, reason:"relative_path", argument,
resolved_path, server_cwd}` and a hint that the server's cwd is not the agent's.
Enforced in `SessionBackend::session_call`, `SessionBackend::call` and
`CliBackend::call`, so session tools, forwarded tools and the CLI-run tools all
refuse it. `UNSAFE_PATH` (absolute escape) is unchanged.

### (4) Per-rung `ms`

* ART session ladder (`launch_reply` in `serve.rs`): every rung carries `ms`
  (the measured spawn→ready time, i.e. when that rung's evidence was observed;
  rungs proven by the same observation share it), plus `boot_ms` and
  `ms_basis: "observed_at_ready"`.
* In-process/native ladders (`launch.rs` and `session_backend.rs`
  `ladder_json`): `ms` on every rung (launch wall time for `ok`, `null`
  otherwise), `total_ms`, `ms_basis: "observed_at_end"`.
  `crates/acos-cli/tests/session.rs` compares cold/warm ladders without `ms`
  (timing must differ) but now also asserts `ms` is present on every rung.

### (5) SERVELEAK3 review follow-ups

| finding | fix |
|---|---|
| 2 — teardown could respawn a dead private daemon | `Restart::{OnFailure,Never}`; teardown and idle expiry use `Never`, which neither restarts nor spawns a stopped daemon (and never falls through to the in-process backend, which would create a session to "destroy") |
| 3 — destroy-by-id fallback when `session_list` fails | on the **external** daemon a failed/absent listing now logs `cannot prove ownership … leaving them (leak)` and destroys nothing; the private daemon keeps the id fallback (its ids can only be ours). `None` from the list is never read as "no sessions" |
| 5 — no signal handler in `acos-mcp` | new `signals::install_teardown`: SIGTERM/SIGINT blocked in `main` before threads, a `sigwait` thread runs the same `Backend::shutdown` as stdin EOF, then exits. The private daemon's `pre_exec` unblocks both signals for the child (a blocked mask survives exec and would silence the daemon's own handler) |
| 6 — shared `/tmp/acos` fallback | `daemon::fallback_runtime_dir()` = `/tmp/acos-<uid>`; `ensure_private_dir` creates/verifies 0700 and **refuses** a foreign owner or a group/world mode; an explicit `ACOS_RUNTIME_DIR`/`XDG_RUNTIME_DIR` is repaired to 0700 if ours (`ensure_runtime_base`). `acos serve` uses the strict check only for the default runtime dir |
| 4 — 5 s SIGKILL deadline can cut a slow multi-guest shutdown | **not changed** (out of scope, low; recorded below) |

Tests added: 8 unit tests in `session_backend` (TTL parse/override, expired
envelope, no-restart teardown, sweep keeps an unproven live guest, guest RSS,
ladder `ms`, relative vs absolute path), 2 in `daemon` (per-uid fallback,
0700/refuse-mode), 1 in `signals`, plus
`crates/acos-mcp/tests/fb5_hygiene.rs` (real daemon wire protocol: TTL destroy +
named expiry + status list; external `session_list` failure → no destroy). The
existing fake daemons in `serveleak_shutdown.rs` now answer `session_list` with
a real session table (the token-checked teardown the product runs).

## 3. Acceptance

All ART runs went through
`tools/art-runtime/run-exclusive.sh --task FB5 -- tools/guarded.sh --mem 10G
--time … --`; builds through `tools/guarded.sh --mem 14G --vmem 40000000` with
`CARGO_TARGET_DIR=.cache/fb5-target`. Installed prefix:
`tools/release/install.sh --from-target .cache/fb5-target --prefix
.cache/fb5/prefix`; the MCP client ran from the empty `.cache/fb5/work` and
`.cache/fb5/outside`, with the ambient `XDG_RUNTIME_DIR` /
`DBUS_SESSION_BUS_ADDRESS` passed through (no override, no `ACOS_BIN`, no
`ACOS_SESSION_DAEMON`).

| criterion | result | evidence |
|---|---|---|
| unit tests for each item | **PASS** | `acos-mcp --lib`: 71 passed (`session_backend::tests::idle_*`, `an_expired_session_names_the_ttl_it_missed`, `teardown_never_respawns_a_dead_private_daemon`, `idle_expiry_tries_to_destroy_and_never_marks_a_live_guest_expired`, `guest_rss_reads_the_guest_not_the_tree`, `ladder_reports_ms_on_every_rung`, `relative_path_args_are_refused_with_the_resolved_path`, `absolute_paths_are_left_alone`, `daemon::tests::*`, `signals::tests::*`); `fb5_hygiene`: 2 passed |
| launch with `label` + `idle_ttl_s: 20`, status shows it with `idle_s` | **PASS** | `.cache/fb5/logs/live.log`: `label=counter-fb5`, `idle_ttl_s=20`, `idle_s`, `guest_rss_kb=2352`, package/activity present |
| after 25 s idle it is destroyed (guest gone) | **PASS** | live.log `guest_pid=3759863` then `/proc/<pid>` absent after 25 s, no further call |
| next `acos_tap` returns the structured expired error naming the TTL | **PASS** | live.log: `SESSION_NOT_FOUND`, `details.reason=idle_ttl_expired`, `idle_ttl_s=20`, message `… destroyed after 20 s of idleness (idle TTL 20 s)` |
| relative `apk` → `INVALID_ARGUMENT` with the absolute path | **PASS** | live.log: `resolved_path=/…/.cache/fb5/work/fixtures/build/apps/counter.apk`, `server_cwd=…/work`, hint present |
| SIGTERM to `acos-mcp` → its owned session + private daemon gone, no foreign `acos serve` touched | **PASS** | live.log: guest `3752729`/daemon `3752716` gone 0.06 s after SIGTERM, `before=[] after=[]` foreign list unchanged |
| SERVELEAK B1/B2 repros still pass | **PASS** | live.log B2 (stdin EOF: guest + private daemon gone) and B1 (SIGKILL of the owner: guest + private daemon gone within the owner-check bound) |
| `mcp_smoke.py --mode counter` / `--mode gaten` | **PASS** | `.cache/fb5/logs/smoke-counter.jsonl`, `smoke-gaten.jsonl`: `{"smoke": "ok"}` both |
| gates v2-art-counter / w22-counter-reset / form-validation | **PASS** | `.cache/fb5/gates-*.json`: `pass`, `art-reference`, 11/9/28 steps all `pass` |
| `cargo test --release --offline -p acos-cli -p acos-mcp -p acos-protocol` | **PASS** | acos-protocol 53+3+7+8+4+2; acos-mcp 71 lib + all integration targets (incl. `fb5_hygiene`, `serveleak_shutdown`, `serveleak2_ownership`, `art_default` 4, `protocol` 20); acos-cli bins 139 + every integration target (`session` 11, `serve_daemon`, `verify_engine_art`, `w44/w46/w50/w51/w55`, `native_launch`, `negative_verify`, `verify`, `run_dex`, `sandbox_s8`); `gl1_kept_session` 2 passed / 4 pre-existing ignored |
| `results/handoff/FB5.md` | this file |
| scratch prefix deleted | see §6 |

One flake, not caused by this track: `acos-mcp --lib process::tests::output_limit_is_enforced`
(`/bin/echo` vs a 1-byte cap) fails occasionally when the whole lib suite runs in
parallel and passes every time alone and on a rerun. Pre-existing test race; not
weakened and not touched.

## 4. Decisions for the orchestrator

1. AR **error code**: the packet allowed "the closest existing code", so an
   expired session is `SESSION_NOT_FOUND` + `details.reason=idle_ttl_expired`
   + the TTL in message/details/hint. A dedicated `SESSION_EXPIRED` would need a
   protocol enum + schema change (`UPDATE_SCHEMA=1`); not done.
2. **Where the TTL lives**: in the MCP server (it owns `owned` + labels), not in
   `acos serve`. The daemon keeps serving an idle session until the MCP reaps it,
   so an external CLI daemon is unaffected; only sessions `acos-mcp` launched are
   reapable by it — exactly "sessions this server owns".
3. **Relative paths** are now refused for every MCP client, including one whose
   cwd happens to match. Existing test `protocol::path_escape_attempts_are_rejected`
   was updated honestly: absolute escapes still `UNSAFE_PATH`, relative ones
   (with or without `..`) are `INVALID_ARGUMENT` with the resolved absolute path —
   still an error, still cannot reach `/etc`.
4. **`acos_status` after `destroy`/guest death** is `SESSION_NOT_FOUND`
   (`reason=no_live_guest`), not a null-field pass. This changed the smoke's
   `status_after_destroy` check from "pass" to the contract it always wanted.
5. **Signal handling** uses a `sigwait` thread, not an async handler; the
   private daemon unblocks SIGTERM/SIGINT in `pre_exec`. Teardown is therefore
   the same code as stdin EOF, not a second path.

## 5. Remaining

* SERVELEAK3 review finding 4 (the 5 s SIGKILL deadline after SIGTERM can cut a
  slow multi-guest engine shutdown) is still open. Smallest next action: raise
  the deadline to 10 s, or retry the kill after `try_wait` stays `Ok(None)`,
  and cover it with a fake-daemon test.
* The in-process/native ladder's `ms` is an observation time (`observed_at_end`),
  not a per-stage measurement; a real gradient needs per-stage timestamps in the
  frozen VM path (not worth it for a frozen engine).
* `acos_status`/`acos_launch` `guest_rss_kb` is the guest process's own `VmRSS`,
  not the whole tree's (documented in the descriptor/docs); a tree sum would
  want the daemon to walk the guest's process group.

## 6. Resources

* Peak host RAM during the track: ~31 GiB used of 60 (≈52 %), never near the
  90 % ceiling; `free -g` checked before every big step.
* Disk: `/` started at 84 % (751 G used) and peaked at 86 %; it is 83 %
  (160 G free) at handoff. The scratch prefix (1.7 G) and the release target
  directory (5.7 G) are deleted at the end of the track.

## Review (Opus, 2026-11-15) — verdict MERGE AFTER FIXES (findings pasted from BRIEFS.md §FB5b)

- **H1 (must)**: idle expiry bypasses the external-daemon ownership rule. `sweep_idle` (session_backend.rs ~1991-2023) maps a failed `session_list` to `current=None` and destroys by id with no token check (the SERVELEAK2/F3 id-guess). `daemon_sessions` sends `session_list` through `forward_session_tool_opts`, which fills the DEFAULT session id, so once the default session expired its tombstone (~432-436) makes every later `session_list` "fail" -> instance A's sweep can destroy instance B's live relaunch of a shared explicit id; every exit teardown on the external daemon then leaks. Fix: `session_list` goes straight to `forward_to_daemon_opts` (no tombstone, no touch); in `sweep_idle` on the EXTERNAL daemon `current=None` -> destroy nothing (log). Test: default session expires, then a second sweep with another owned session -> list works, nothing foreign destroyed.
- **M1 (must)**: in-flight/just-finished calls do not protect from expiry (~1975-2023, ~457: touch only after the call; sweep never re-reads `last_used`). Touch / mark in-flight BEFORE forwarding, re-check `last_used` under the owned lock right before destroying; a call arriving mid-destroy gets the `idle_ttl_expired` reason, not a bare SESSION_NOT_FOUND.
- **M2 (must)**: empty-token sessions fall back to id-only ownership (~1997-1998). On an EXTERNAL daemon never idle-expire a tokenless session (log instead); the installed v0.2.1 serve may not return tokens.
- **M3 (should)**: per-uid fallback dir (daemon.rs `ensure_private_dir` / `ensure_runtime_base`) follows symlinks -> `symlink_metadata`, refuse symlinks, never `set_permissions` through a link.
- **L1 (should)**: `owned_status` (~2074-2120) joins on id only -> filter by token like the sweep; never report another client's guest/RSS; read `guest_pid` only while the session is provably ours.
- **L2**: `session_list` must not touch the default session (fixed by H1).
- **L3**: SIGTERM + stdin EOF together -> main must wait for the signal thread's teardown to finish (join); a second SIGTERM/SIGINT during teardown is handled (exit promptly after a bounded wait).
