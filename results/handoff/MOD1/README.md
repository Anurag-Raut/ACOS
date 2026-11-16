# MOD1 — the session API (ADR-0029 item 4) and the AOSP re-decisions

Branch `mod1` (created from `mod0` = bb7e920e), 2026-11-16. Implementation only.

## What landed

| Brief item | Where | State |
|---|---|---|
| 1. `SessionModules` (registry + `verdict_modules()`) | `crates/acos-services/src/module.rs`, `core_module.rs` | DONE |
| 2. CLI `--service id[:profile][,config]` (repeatable) + serve/daemon launch; reply `modules[]` | `crates/acos-cli/src/{launch.rs,engine/serve.rs,engine/art.rs}`, `crates/acos-art/src/{main.rs,launch.rs,host_zygote.rs}` | DONE |
| 3. MCP `acos_launch {services}` and `acos_services {op,id,profile,config,session_id}` | `crates/acos-mcp/src/descriptors.rs`, `crates/acos-cli/src/engine/serve.rs` (`acos_services` verb) | DONE |
| 4. Scenario `services:` (schema regenerated) applied at launch in verify; `verdict.json` + provenance + summary line; fidelity notes; `off` module → `unsupported` | `crates/acos-protocol/src/{scenario.rs,verdict.rs}`, `schema/*.json`, `crates/acos-verify/src/{verify.rs,executor.rs}`, `crates/acos-cli/src/verify.rs` | DONE |
| 5. `acos_capabilities {session_id}` shows the attached set | `crates/acos-mcp/src/server.rs` | DONE |
| AOSP cache widening + per-name re-decision | `tools/fetch-aosp-reference.sh`, `docs/AOSP_REFERENCE.md`, `TOMBSTONES`, `AOSP_DECISIONS.md` | DONE (4 names → tombstone, 6 stay `null`, each from file:line) |
| Core budget marked `estimated` | `Budget` JSON in `manifest.to_json`/`ModuleReport::to_json`, limits table | DONE (no measured value is claimed) |

Design note (why the registry lives in the peer thread): the ART launcher builds
one host, attaches `core` + the requested specs + tombstones **before** the guest
boots, and moves the host to the peer thread. A live `acos_services attach`
is therefore a request to that one owner (a small request/reply channel with a
10 s bound), never a second mutation path. `acos-art`'s ready line carries the
attached rows; the engine echoes them in the launch reply, `acos_services` and
the verdict.

## Evidence

### Tests (fail-before / pass-after)

```
# pass-after (results/handoff/MOD1/tests.log)
cargo test -p acos-services --lib        -> 92 passed
cargo test -p acos-services --test module_kit -> 10 passed (results/handoff/MOD1/module_kit.log)
cargo test -p acos-protocol --test services_block -> 3 passed
cargo test -p acos-verify --lib          -> 91 passed
cargo test -p acos-mcp --lib             -> 83 passed
cargo test -p acos-mcp --test guide      -> 13 passed (incl. default-reply 3 KiB budget)
cargo test -p acos-mcp --test docs       -> 1 passed (docs tool table == descriptors)
cargo test -p acos-cli --test service_flag -> 2 passed
```

New tests: `module::tests::a_session_modules_object_attaches_and_reports_the_verdict_rows`,
`module::tests::a_spec_parses_the_object_and_config_spellings`,
`tests/services_block.rs` (3), `executor::service_module_tests` (2),
`guide::the_service_module_tools_advertise_closed_schemas`,
`service_flag.rs` (2).

`results/handoff/MOD1/fail-before.txt`: every API/flag/tool/schema field the tests
exercise is absent on `mod0` (`git grep` empties; `pub service` count 0), so the
tests cannot compile/parse there.

### ART run: counter with `--service feedback.vibrator:stub`

```
acos launch fixtures/build/apps/counter.apk --engine art --service feedback.vibrator:stub --json
  -> results/handoff/MOD1/counter-launch.json   status=pass, reached=first_frame
     result.modules = [core real at-launch, feedback.vibrator stub hot]

acos verify results/handoff/MOD1/counter-services.yaml --engine art --json
  -> results/handoff/MOD1/counter-services-verdict.json   status=pass
     modules = [core real, feedback.vibrator stub]; notes name the stub fidelity
```

### MCP live session (real ART through the private daemon)

`results/handoff/MOD1/live_services.py` → `live-services.json`:
`acos_launch {services:[feedback.vibrator:stub]}` pass with `modules[]`;
`acos_services {op:list}` shows both attached; `detach core` is refused
(`INVALID_ARGUMENT`, `SERVICE_MODULE_REJECTED`, message names the `at-launch`
rule); `detach`/`attach feedback.vibrator` changes the set (the reply carries
the new one); `acos_capabilities {session_id}` returns `session.attached`;
`acos_destroy` is a clean pass.

## Honest limits

* The `core` RAM/startup budget is an **estimate** (16 MiB / 250 ms from
  `results/art-runtime/D-M2-INVENTORY.md`), marked `budget.basis: "estimated"` in
  every manifest/report row and `(estimated)` in the limits table. No memory
  measurement was run (batched by the bench owner).
* `acos_services {op:"status"}` currently answers the same rows as `list` (the
  set plus reasons); there is no extra per-session "allow/refuse" detail yet.
* A step's `requires_services` gate is decided from the scenario's requested
  `services:` (a requested `off` is attached-off). It is exercised by unit tests,
  not by an ART run: no existing scenario asserts on a module-provided service.
* `acos_services` is served by the ART daemon route; with `ACOS_MCP_ENGINE=legacy`
  (the frozen Rust VM) it answers the structured `UNSUPPORTED_BACKEND` (the legacy
  backend has no service table), matching the `--service` refusal on that route.
