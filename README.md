# ACOS — agent-native Android execution OS

ACOS runs Android apps natively in Rust: a DEX interpreter and managed heap
(`acos-vm`), a real-core-library / framework path, an in-process service layer,
APK/DEX tooling and an MCP/JSON surface for AI agents. See `ARCHITECTURE.md`,
`SPEC.md`, and `docs/V1.md`.

## Status

**v0.2.0.** An agent can launch a real Android APK on real ART (API 34,
x86_64), read its live `View` tree, drive it, and prove a flow with an
evidence-backed verdict — through the MCP tools (`acos_launch` ->
`acos_observe_tree` -> `acos_tap`/`acos_text` -> `acos_verify`) or the CLI with
`--engine art`.

* `STATUS.md` — the milestone table, measured metrics and the current risk list.
* `docs/AGENT_GUIDE.md` — the single source of truth for agents (also served as
  `acos_help`, its topics, and the MCP resources); `docs/MCP.md` — the MCP
  contract.
* `TASKS.md` — the critical path, workstreams and standing gates.

## Building and running

```bash
cargo build --release -p acos-cli
./target/release/acos run-dex fixtures/build/dexprogs/01_hello/classes.dex --main Main
```

Fixtures are built with `tools/build-fixtures.sh` (or `tools/build-apps.sh`);
the differential corpus runs with `tools/diff-dexprogs.sh --corelib real`.

## Host safety (ADR-0018) — mandatory

Host RAM must never exceed 90% (internal target 85%; the host has 62 GiB and no
swap). **Every** command that can execute or build guest code runs through
`tools/guarded.sh`, which applies a hard memory ceiling and wall-clock timeout,
kills the process group on exit/timeout/error, and cleans up scratch:

```bash
tools/guarded.sh --mem 4G --time 900 -- cargo test -p acos-vm
tools/guarded.sh --mem 4G --time 900 -- cargo clippy --workspace -- -D warnings
tools/guarded.sh --mem 2G --time 60  -- ./target/release/acos run-dex x.dex --main Main
```

`tools/build-*.sh`, `tools/diff-dexprogs.sh` and `tools/bench-vm.sh` invoke it
themselves. During a parallel wave run `tools/ramguard.sh &`; it kills any
`target/*/acos*` process once `MemAvailable < 9 GiB` (~85% used) or a single
process exceeds 8 GiB RSS. The VM additionally enforces `ACOS_HOST_RSS_MAX`
(default 2 GiB, `HOST_MEMORY_LIMIT`) and `ACOS_WALL_MAX_SECS` (default 600,
`TIMEOUT`) independent of the guest heap. Details: `tools/README.md`,
`docs/AGENT_API.md` §16, `DECISIONS.md` ADR-0018.

## Documentation map

| Doc | Contents |
|---|---|
| `ARCHITECTURE.md` | system design and crate map |
| `SPEC.md` | product spec and owner directives |
| `DECISIONS.md` | ADRs |
| `docs/AGENT_API.md` | JSON/MCP contract and host ceilings |
| `docs/FINDINGS.md` | append-only dated findings |
| `TASKS.md` | critical path, workstreams and standing gates |
| `tools/README.md` | fixture tooling and the host-safety rule |
