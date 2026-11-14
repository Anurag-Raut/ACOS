# REL1b — host memory admission + the agent guide in MCP

Branch `rel1b-host-lock-mcp` (from `main` = 389429a). Worktree
`/home/Qui/.paseo/worktrees/3oxa6xao/gr4`.

**Scope change during the run (owner/orchestrator decision).** The originally
planned host-wide slot lock was dropped: ACOS must not take `art.lock` files,
must not cap concurrent sessions, and must not queue. `run-exclusive.sh` and
`art-locks/` stay internal dev tooling, never user-facing. What shipped instead
is the memory admission check below, at the same choke point. No lock code was
written, so nothing is left behind behind a flag.

## A. Host memory admission (`HOST_MEMORY_LOW`)

`crates/acos-cli/src/engine/host_mem.rs`, called from
`ArtSession::start_with_options` — the single place every ART guest starts:
`acos launch`, `acos verify`, `acos serve` and MCP sessions through a daemon,
and the `reset` relaunch.

Rule: `MemAvailable - session budget < 10% of MemTotal` → refuse, fast.

* `MemAvailable`/`MemTotal` from `/proc/meminfo`.
* Session budget = `ACOS_ART_MEM_MAX` (the same value `acos-art` applies to the
  run's cgroup), default 2 GiB.
* Failure is a structured `BackendError::Protocol` with
  `ErrorCode::HostMemoryLow` → status `blocked`, `retryable: true`, and
  `error.details = {mem_available_mib, mem_total_mib, requested_mib,
  min_free_mib, min_free_percent, guard, budget_env}`.
* No waiting, no queue, no slot files, no session-count limit. Nothing is
  created (no guest, no cgroup, no artifacts) on a refusal.
* `ACOS_MEMORY_GUARD=off|0|false|disabled|no` disables only this check. The
  per-session cgroup ceiling is untouched.
* An unreadable/unparsable `/proc/meminfo` logs to stderr and allows the launch
  (the launcher fails later with its own host error; a fabricated refusal would
  be worse than an honest one). Documented in the module.

Unit tests (fabricated meminfo, no host coupling): roomy host admitted, 10% floor
is of total, low memory refused with the exact details, boundary is inclusive,
env off spellings, budget parsing, unparsable meminfo, and an env-injected
end-to-end `check_host_with`.

## B. The MCP server teaches agents how to use ACOS

1. `docs/AGENT_GUIDE.md` (new, ~330 lines) is the single source: loop, every CLI
   command with a runnable example (verified against `acos --help`), scenario
   YAML authoring (step keys, selectors, settle strict vs `settle: fast`,
   `expect_change`, assertion kinds, the two coordinate spaces), error codes →
   action, and limits. It is embedded with
   `include_str!("../../../docs/AGENT_GUIDE.md")`
   (`acos_mcp::help::AGENT_GUIDE`), so the installed binary carries it.
2. `acos_help` returns the guide plus live capabilities/unsupported sets; an
   optional `{"topic": "workflow"|"cli"|"scenarios"|"errors"|"limits"}` returns
   one `<!-- topic:X -->` section.
3. MCP `resources`: `acos://guide`, `acos://guide/{workflow,cli,scenarios,
   errors,limits}`, `acos://errors`; `resources/list`, `resources/templates/list`,
   `resources/read` (unknown URI → `-32602`). `initialize` now advertises
   `capabilities.resources` and `capabilities.prompts`.
4. MCP `prompts`: `verify_android_flow {apk, goal}` → a runnable instruction
   (write the scenario, `acos_verify`, branch on `error.code`, report only what
   the evidence proves); missing args → `-32602`.
5. `tools/release/register-mcp.sh`: registers with Claude Code
   (`claude mcp add --scope user acos -e ACOS_BIN=… -e ACOS_ALLOWED_ROOTS=…
   -- <acos-mcp>`) and prints the `mcpServers` JSON for other clients.
   `--dry-run` prints the command and registers nothing (nothing was registered
   in this track). Defaults `ACOS_ALLOWED_ROOTS=$HOME` so an agent in another
   repo (e.g. `~/Projects/gaten`) can pass its APK/scenario paths; documented in
   `docs/MCP.md` §1.
6. The guide, the resources and the prompt never mention `run-exclusive.sh`,
   `art.lock`, slots, `ACOS_ART_LOCK_SLOT` or any session queue — a test
   (`the_guide_covers_the_contract`) fails if such a word appears.

## Evidence

| check | result |
|---|---|
| `cargo test --release --offline -p acos-mcp` | green (41 unit + all integration incl. new `tests/guide.rs`, 6 tests) |
| `cargo test --release --offline -p acos-cli` (via run-exclusive) | green, 98 unit + every integration suite |
| `cargo test --release --offline -p acos-protocol` | green, schemas regenerated (`HOST_MEMORY_LOW`) |
| live MCP round trip on `counter.apk` | `initialize → tools/list → acos_help (17476 chars) → acos_launch (pass, first_frame) → acos_observe_tree ("0", "Increment") → acos_tap Increment (state_changed=true) → acos_observe_tree ("1") → acos_destroy (pass)` — `results/release/mcp-roundtrip.jsonl` |
| three concurrent MCP-driven ART sessions | 3 independent `acos-art launch` processes, 12 guest processes (3 dalvikvm64 + 6 bwrap), all three trees live at once, no lock and no cap — same transcript, `"event":"art_guests"` |
| `HOST_MEMORY_LOW` refusal | daemon with `ACOS_ART_MEM_MAX=55G`: `status=blocked code=HOST_MEMORY_LOW details={mem_available_mib:29432, mem_total_mib:61908, requested_mib:56320, min_free_mib:6190}`, no guest started — same transcript |
| gate `v2-art-counter --engine art` | pass |
| gate `w22-counter-reset --engine art` | pass |
| gate `form-validation --engine art` | pass |
| gate `gaten-home --engine art` | pass |
| `register-mcp.sh --dry-run` | prints `claude mcp add --scope user acos --env ACOS_BIN=… --env ACOS_ALLOWED_ROOTS=/home/Qui --env ACOS_ARTIFACTS_DIR=… -- …/acos-mcp` + JSON — `results/release/register-mcp-dryrun.txt` |

Note: the four gates prove there is no interaction with the dev lock: the
`acos` process inside `run-exclusive.sh` sees `ACOS_ART_LOCK_SLOT` in its
environment and simply does not care (no lock code exists), and the memory
admission check passed on every run (MemAvailable ≈ 29 GiB, floor 6.2 GiB).

## Decisions

* Module lives in `acos-cli` rather than `acos-art`: `acos-cli` does not depend on
  `acos-art`, and the choke point is `ArtSession::start_with_options` in the CLI
  engine. `acos-art` is unchanged (no cgroup split needed).
* `BackendError::Protocol` (not `Failed`) so the measured numbers reach
  `error.details` unchanged in the CLI JSON envelope, the MCP `notes` and the
  verdict, instead of being flattened into a detail string.
* Guide topics are delimited by explicit `<!-- topic:NAME -->` markers so the
  same file is the documentation and the four MCP/RPC access paths, with no
  second copy to drift (a test enforces the embedded copy == the file).
* No session-count limit and no queue anywhere; the only host protection for
  outside agents is the admission check plus each session's cgroup ceiling.
* Nothing was pushed, merged or rebased. The worktree's `fixtures/build` was
  turned into a hardlinked real directory (it was a symlink to the main
  checkout, which made the MCP path allow-list correctly reject fixture paths —
  `UNSAFE_PATH` — in the e2e test).
