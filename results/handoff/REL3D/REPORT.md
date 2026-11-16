# REL3D handoff report

Track: REL3D (ACOS v0.3.0 correctness) — a CLI/MCP call is never answered by an
`acos serve` daemon built from different code. Finding MODA11Y.

Branch `rel3d`, from `rel-030` @ 064f568d.

## Commits

| sha | what |
|-----|------|
| b2935f0d | identity + hello/ping + client refusal + replacement/private-daemon routing + MCP checks + doctor + docs + tests |
| 93e6cd03 | pid of a pre-identity daemon via `ping`; shared daemon on a clean socket (regression found by `serve_daemon.rs`); e2e evidence |

## Root cause and fix (one place for the whole class)

`acos serve` is a shared, long-lived daemon on one socket per user. Nothing in the
wire protocol said which build it was, so any client — the CLI, the MCP server's
private daemon, an installed release after `upgrade` — was answered by whatever
daemon happened to be listening. An older daemon therefore answered with its own
behaviour (`code 5 unsupported` for a service the newer build implements).

* `crates/acos-mcp/src/identity.rs` — `Identity` = crate version + `git_sha` +
  workspace **source digest** (build.rs; identical for every binary of one build)
  + the running binary's `exe_len`/`exe_hash` (hashed once, streamed) + pid,
  engine, `auto_started`. `Identity::mismatch` names the broken rule:
  `protocol` | `version` | `build` | `binary` (same path, other bytes = an
  in-place upgrade under a live daemon).
* daemon: `hello` (new) and `ping` (extended) report the identity, engine and
  live session count; `ServeEngine` owns it; `--auto-started` records that a
  client (not an operator) started it.
* client: `DaemonClient::connect` performs the handshake; `call()` refuses to
  write a request to a mismatched daemon and returns a structured
  `DAEMON_VERSION_MISMATCH` envelope carrying both identities.
* routing (`acos-cli::serve`): shared socket → identity check →
  * idle **auto-started** daemon (or a pre-identity daemon that can prove it is
    an orphan via `ping`'s pid and `ppid == 1`): `SIGTERM`, then a matching
    daemon on the shared socket;
  * daemon with live sessions, operator-started daemon, or an unowned/unknown
    one: **never signalled**; this client starts `$ACOS_RUNTIME_DIR/build-<build_id>/session.sock`
    and emits the structured warning `DAEMON_VERSION_MISMATCH` (stderr JSON:
    `code`, `rule`, `client`, `daemon`, `action`, `socket`);
  * top-level commands (`observe/tap/logs`) never start a daemon: they use an
    already-running build-private one, else this build's in-process session.
* MCP: the private `acos serve --engine art` is identity-checked right after
  spawn (a different `ACOS_BIN`/sibling build is refused, not forwarded to), and
  an external `ACOS_SESSION_DAEMON=1` daemon of another build is not attached.
* `acos doctor`: `session_daemon` check prints the running daemon's identity next
  to the CLI's (ok / warn with both, plus the fix).
* error code `DAEMON_VERSION_MISMATCH` (class `harness`) in
  `acos-protocol::ErrorCode` + docs/ERROR_CODES.md.

## Acceptance criteria

1. **Daemon identity + client comparison before any request** — PASS.
   `crates/acos-mcp/src/identity.rs`, `daemon.rs`; `cargo test -p acos-mcp --lib identity::`
   → 7 passed (`results/handoff/REL3D/tests.log` §1).
2. **Mismatch policy: replace idle auto-started / private daemon when busy, with
   a structured warning** — PASS. `cargo test -p acos-cli --test rel3d_daemon_identity`
   → 2 tests, arms 1–3 (`tests.log` §2): busy foreign daemon left alone + private
   daemon + warning with both build ids; idle auto-started foreign daemon
   replaced; pre-identity daemon (no `hello`) detected, never used, not
   signalled while it has a live owner.
3. **MCP private daemon + external daemon get the same check** — PASS by code
   (`private_daemon.rs::wait_ready`, `session_backend.rs::with_daemon_socket`,
   both emit the structured warning) and no regression in the MCP daemon tests
   (`tests.log` §3: 93 + 8 + 1 + 1 passed).
4. **`acos doctor` shows running daemon identity vs CLI** — PASS.
   `results/handoff/REL3D/e2e_two_builds.log` ("session_daemon: ok | pid … build
   … matches the cli"), and arm 1 of `tests.log` §2 (warn + both identities).
5. **Fail-before / pass-after, incl. two real builds** — PASS.
   `results/handoff/REL3D/e2e_two_builds.sh` + `.log`: old = rel-030 @ 064f568d
   (70438584 B, no identity protocol), new = rel3d; on one socket the old daemon
   answers `hello` with `SESSION_NOT_FOUND` and no identity (the MODA11Y surface,
   and the old client accepts it silently), while the new client prints
   `DAEMON_VERSION_MISMATCH`, replaces the idle orphaned daemon and the daemon
   answering afterwards reports the **new** build id (`…+dirty.ceb7a29f.bf182d16`).
   No `REL3D` assertion was weakened or ignored; the new tests fail on the old
   code by construction (the old binary has no `hello`, no identity, no refusal —
   step 2/3 of the log is that behaviour).
6. **Docs** — PASS: `docs/AGENT_GUIDE.md` (error-code troubleshooting row),
   `docs/MCP.md` ("The daemon must be the same build as the server (REL3D)"),
   `docs/ERROR_CODES.md` row.
7. **No regression in the ART daemon path** — PASS:
   `cargo test -p acos-cli --test serve_daemon` (under the shared ART lock) → 1
   passed (`tests.log` §4). It caught the one regression this work introduced
   (`ensure_daemon` started a private daemon on a clean socket), fixed in 93e6cd03.

## Notes / limits

* The "busy" arm uses a fake daemon that speaks the real wire protocol; the two
  real-build e2e does not hold an ART session in the old daemon (that would need
  a full ART launch from the old tree). Recorded as such in the log.
* `Identity::mismatch` treats a single `unknown` `git_sha`/`exe_hash` as
  non-discriminating (a build without git, an unreadable binary); the source
  digest and the same-path binary hash still decide.
* A daemon started by a client of another build is stopped only when it holds no
  session **and** proves it is auto-started (or is an orphan). This deliberately
  leaves one narrow window: a pre-identity daemon whose spawning client is still
  alive is never replaced — the new client runs its own daemon instead.
