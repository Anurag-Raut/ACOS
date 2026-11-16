# REL3D evidence index

Branch `rel3d` (from `rel-030` @ 064f568d).

| # | Claim | Evidence |
|---|-------|----------|
| 1 | identity is version + git sha + workspace source digest + binary hash, and the comparison names the rule | `cargo test -p acos-mcp --lib identity::` → 7 passed (`tests.log` §1) |
| 2 | a daemon of another build is never used: busy fake left alone, build-private daemon serves the client, structured `DAEMON_VERSION_MISMATCH` with both identities | `cargo test -p acos-cli --test rel3d_daemon_identity` → 2 passed, 3 arms (`tests.log` §2) |
| 3 | an idle auto-started daemon is replaced; a legacy (pre-`hello`) daemon is detected via `ping` and only replaced when orphaned; an operator-started one never is | arms 1–3 of (2) + `e2e_two_builds.log` "replaced an idle daemon of another build" (the old daemon is rel-030: pre-identity, orphaned) |
| 4 | two real builds on one socket: the new client never gets the old daemon's answer | `e2e_two_builds.log` (old = rel-030 064f568d, new = rel3d; old daemon answers `hello` with `SESSION_NOT_FOUND`, new client refuses + replaces, the shared socket then reports the new build id, `doctor` ok) |
| 5 | `acos doctor` shows the running daemon identity vs the CLI | `e2e_two_builds.log` last section; also arm 1/2 of (2) |
| 6 | MCP: the private daemon is identity-checked, an external `ACOS_SESSION_DAEMON=1` daemon of another build is not attached | `crates/acos-mcp/src/private_daemon.rs` (`wait_ready`), `session_backend.rs` (`with_daemon_socket`); no regression in the MCP daemon tests (`tests.log` §3) |
| 7 | no regression in the ART daemon path | `cargo test -p acos-cli --test serve_daemon` → 1 passed (`tests.log` §4, run under the shared ART lock) |

Build commands (disk-safe, debug with no debug info):

```
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 \
  CARGO_TARGET_DIR=$WT/.cache/rel3d-target cargo build --offline -p acos-cli --tests
# the old build (rel-030 @ 064f568d) for the e2e:
git worktree add --detach .cache/rel3d-old 064f568d
CARGO_PROFILE_DEV_DEBUG=0 CARGO_TARGET_DIR=$WT/.cache/rel3d-old-target \
  cargo build --offline --manifest-path .cache/rel3d-old/Cargo.toml -p acos-cli
```
