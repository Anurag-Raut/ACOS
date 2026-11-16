# MOD0 — service-module framework (ADR-0029 item 1), evidence

Branch `mod0`, 2026-11-16. Owner decision, implementation only.

## What landed

| ADR-0029 item | Where | State |
|---|---|---|
| 1. module + manifest (provides/requires/profiles/budget/attach/controls/security) | `crates/acos-services/src/module.rs` | DONE |
| 2. `attach`/`detach`/`control`/`report` on the `ServiceHost::register` seam; detach leaves nothing | `module.rs`, `host.rs` (`begin_scope`/`end_scope`/`remove_service`) | DONE |
| 3. tombstones (`UNSUPPORTED_ANDROID_API` naming module + attach call) with a per-name AOSP decision | `module.rs` (`TOMBSTONES`), `error.rs` (`ModuleUnavailable`) | DONE for the names in `.cache/aosp`; the rest keep `null` and are reported `off` (see `AOSP_DECISIONS.md`) |
| 4. session API (`acos_launch {services}`, scenario `services:`, CLI `--service`, `acos_services`, verdict `modules`) | — | **NOT STARTED** (see `REMAINING.md`) |
| 5. conformance kit + demo module | `module.rs::conformance`, `tests/module_kit.rs`, `modules/vibrator.rs` | DONE (core + vibrator; density N=20 and RAM budget measurement NOT run) |
| 6. `acos_capabilities` + `acos_help` limits generated from the registry | `acos-mcp/src/backend.rs`, `acos-mcp/src/help.rs`, `docs/AGENT_GUIDE.md` marker | DONE |

## Commands and results

```
# acos-services, whole crate (gate: unchanged behaviour of today's services)
tools/art-runtime/run-exclusive.sh --task MOD0 -- tools/guarded.sh --mem 12G --time 1100 --
  cargo test -p acos-services --offline
   -> 90 passed (lib) + every integration suite passed (see services-tests.log)

# the kit itself
cargo test -p acos-services --test module_kit --offline
   -> 10 passed  (see module_kit.log)

# the generated docs (capabilities + limits)
cargo test -p acos-mcp --lib --offline
   -> 83 passed (includes capabilities_are_generated_from_the_service_module_registry)
cargo test -p acos-mcp --test guide --offline the_limits
   -> 1 passed (the_limits_section_is_generated_from_the_module_registry)
```

`fixtures/build` must exist in the worktree (`cp -al` from the main checkout) or
`manifest_resolve::tests::real_apks_resolve_like_aapt2_could_not` fails for a
missing fixture — unrelated to this change.

## Fail-before / pass-after

Every test in `tests/module_kit.rs` and the two generated-docs tests fails on
`main` because the API does not exist there (`ModuleRegistry`, `Profile`,
`TOMBSTONES`, `Capabilities::to_json().modules`, `help::MODULES_MARKER`). There
was no registry, no tombstone and no profile, so the tests cannot be expressed.

## Honest limits of this evidence

* The density check (N=20 within budget) and the RAM measurement of the kit's
  budget row were **not** run: they need ART sessions, and the brief batches
  those. The kit measures registration/thread/fd neutrality only.
* `core`'s `budget` (16 MiB / 250 ms) is a placeholder derived from
  `results/art-runtime/D-M2-INVENTORY.md` (whole-service-state estimate), not a
  measurement of the module alone.
* Detaching `core` clears the host's registrations; the process-global
  `provider::set_settings_provider_binder` cache keeps its last value until the
  next attach (documented in `core_module.rs`). No session detaches `core`.
