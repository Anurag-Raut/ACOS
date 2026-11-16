# INTEG030 — merge-breakage catch on rel-030

Branch: `integ030` (cut from `rel-030` @ `2022d89c`, "Merge branch 'rel3p2'").
Nothing was committed to `rel-030`; the orchestrator merges `integ030`.

## What the merge broke (and what each fix keeps)

| # | seam | file | sides | resolution |
|---|------|------|-------|------------|
| 1 | `tool_result` arity | `crates/acos-mcp/src/server.rs` | REL3B added the strict-argument early return at 4 args; REL3C added the caller's `args` (fifth parameter) so every reply is shaped/capped once | keep both: the rejection goes through the one reply rule. `cargo check` failed with `E0061` before, 0 errors after |
| 2 | closed schemas vs the new reply arguments | `crates/acos-mcp/src/descriptors.rs` | REL3B (unknown argument ⇒ `INVALID_ARGUMENT`) vs REL3C (`full`/`cursor` paging) + P12b `settle_timeout_ms` (the daemon sets a per-request settle for *every* session verb) | publish what the routes honour: `PAGED_REPLY_PROTOCOL_TOOLS` (acos_observe, acos_verify), `SETTLE_PROTOCOL_TOOLS` (acos_observe), `settle_timeout_property` for acos_key/acos_text. The check itself stays strict. Fixed: REL3C's own `compact_reply` test (`acos_verify {cursor}`), `serve_daemon` (`session observe`), `session key/text` (settle) |
| 3 | compact screen vs live input | `crates/acos-protocol/src/reply.rs` | REL3C's compact node carried id/resource_id/class/text/content_desc/bounds/clickable/window/package; FB1/REL3E read `focused`/`password` off the same nodes | copy the node's state flags through when the source has them (`focused`, `password`, `enabled`, `scrollable`, `checked`, `selected`); nothing invented |
| 4 | whole-reply cap vs the launch ladder | `crates/acos-protocol/src/reply.rs` | REL3C's cap replaced an oversized `result` wholesale (`native_launch` expected the stage ladder) | the cap now drops only repetition: ladder keeps every rung + `status` + `blocker.code`, a blocker message > 1 KiB keeps its first line (`message_bytes`, `message_truncated`), rung `details` go (the envelope's `error.details` carries the same map); `events` is truncated like `logs`. Real reply: 88 269 → 18 758 bytes, all 8 rungs intact |
| 5 | cap keeps paths but not the answer (REL3C integration check) | `crates/acos-protocol/src/reply.rs` | REL030_INTEGRATION.md | the capped object now also carries `verdict_status`, `failed_step {index, kind, message}`, `provenance` (apk/scenario digests) and `evidence_provenance`; only the bulk is dropped. A passing run never reports a failed step (the step list can hold a retried non-pass attempt) |
| 6 | SUN_LEN (class, not one instance) | `crates/acos-cli/src/{live_registry,view/mod}.rs`, `crates/acos-cli/tests/fb1_live_input.rs`, `crates/acos-art/src/live_view.rs` | test scratch roots derived from the checkout path; `sun_path` is 108 bytes with the NUL and an agent worktree path is ~70 | one short-root rule per crate (`test_scratch_root`, existing `common::ShortSocketDir` for the daemon socket, `scratch_root` in live_view) with a budget assertion so it cannot regress silently |

## Test tally (offline, `CARGO_TARGET_DIR=$PWD/.cache/integ030-target`)

| log | scope | passed | failed | ignored |
|-----|-------|--------|--------|---------|
| `02-test-groupA.log` | 20 non-ART crates (before the fixes) | 984 | 1 | 11 |
| `05-test-vm-services.log` | acos-vm, acos-services | 556 | 0 | 0 |
| `08-test-art.log` | acos-art (before the SUN_LEN fix) | 199 | 9 | 1 |
| `09-test-art-after.log` | acos-art (after) | **208** | **0** | 1 |
| `13-final-cli-mcp-proto.log` | acos-cli + acos-mcp + acos-protocol | 545 | 2 | 8 |
| `12-rel3c-e2e.log` | `rel3c_capped_verify` (live ART, `--ignored`) | **2** | **0** | 0 |

Remaining failures, both not merge breakage:

* `acos-mcp/tests/flutter_session.rs::flutter_launch_observe_tap_round_trip` and
  `acos-jni/tests/flutter_hello.rs::flutter_hello_under_confinement` — the shared
  fixture APK is the plain `libapp.so` build (`lib/x86_64/libapp.so`, no
  `libapp_probe.so`), so the instrumented snapshot never stages
  (`stage-app source=fixture`) and the Flutter semantics tree is empty
  (`"tree": []`, `tree_node_count: 0`). Reproduced on pristine `rel-030` with
  `crates/acos-protocol/src/reply.rs` checked out (`git stash` run in
  `12-…`/`09-…` sequence), and `git diff main -- crates/acos-jni` is empty.
  Environment/fixture-only.
* `acos-cli` `engine::art::tests::a_zygote_session_signals_and_proves_instead_of_waiting_the_grace`
  — a wall-clock assertion (`105.65 ms` vs the budget) that passes in
  `04`, `06` and `07` and failed only in the final run at loadavg 9.8. Load
  flake, not a code path I touched.

## Gates

| gate | result | evidence |
|------|--------|----------|
| `cargo check --workspace --all-targets --offline` | **PASS** (0 errors; 7 pre-existing warnings) | `01-cargo-check.log` (E0061), `01b-cargo-check.log` (clean) |
| `cargo test --workspace --offline` (by crate group, see tally) | **PASS** except the 2 environment failures above | logs 02/05/09/13 |
| `tools/run-matrix.sh` (false_pass must be 0) | **NOT RUN — host gate refused** | `11-matrix-counter.log`: `run-matrix: host gate REFUSED (loadavg1 10.96, MemAvailable 45.69 GiB) / loadavg1 10.96 > 4.00`; retried once at `11.03` (`11-matrix-gaten.log`), same refusal. Never bypassed. |
| Gaten positive (`scenarios/gaten-home.yaml`, release, `--engine art --required-fidelity art-reference`) | **PASS** | `gaten/gaten-home.json`: `status pass`, `ok true`, `verdict_status pass`, `reply_capped true`, no `failed_step` |
| Gaten negative (`scenarios/negative/gaten-home-wrong.yaml`) | **FAIL as intended** | `gaten/gaten-home-wrong.json`: `status fail`, `error.code ASSERTION_FAILED`, `failed_step {"index":7,"kind":"assert_text","message":"expected text containing \"Weekly streak\", got text \"Daily streak\""}` |
| remaining Gaten positives/negatives (10 + 10) | **NOT RUN** | time/host load; the two runs above are the same code path (`run-exclusive` + `guarded`, no timing claims) |

## Observation for the orchestrator (not fixed, not mine to decide)

The inline `result.verdict.steps` of a **passing** Gaten run contains a non-pass
attempt for a step the engine later retried to `pass` (gaten-home: index 10
`tap`, `strict settle timed out (newer-store)`), while the same run's
`verdict.json` on disk has every step `pass`. The capped-reply diagnosis now
gates on the verdict status so it cannot turn that into a false
"failed step", but the two views of one run disagree. Worth a separate look:
either the step list should record attempts explicitly or the retry should
replace the entry.

## Disk

`/` reported 86% `Use` throughout (764–766 G used of 945 G, 132 G available —
the percentage includes ext4's reserved blocks; exact used was 80.8%). Peak
footprint of this track: `.cache/integ030-target` (~9 G, deleted at the end)
plus ~40 MB of evidence. No other agent's data touched.
