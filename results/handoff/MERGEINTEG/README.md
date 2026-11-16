# MERGEINTEG — v0.3.0 integration (`integ030` → `rel-030`)

Branch: `rel030-integ`, cut from `rel-030` @ `e4fb2424` ("Merge branch 'perf3'").
`git merge integ030` (`0c219ab7`, cut from the older `rel-030` @ `2022d89c`).
The orchestrator merges `rel030-integ` back into `rel-030` (fast-forward).

Nothing was committed to `rel-030`; nothing was pushed. No benchmark, matrix or
A/B run was made here (the orchestrator owns the release gate).

## Conflicts — one implementation kept per seam

| # | seam | file | rel-030 side | INTEG030 side | kept |
|---|------|------|--------------|---------------|------|
| 1 | compact node flags | `crates/acos-protocol/src/reply.rs` | REL3E: `visible, window, package, focused, password` | FB1/REL3E state flags incl. `enabled, scrollable, checked, selected` | INTEG030's superset list, with the merged comment (copied only when the source node has them; nothing invented). Every rel-030 test on `focused`/`password` still passes, and FB1's `enabled/scrollable/checked/selected` are readable off the compact page |
| 2 | reply cap: string bounding vs pointer fallback | `crates/acos-protocol/src/reply.rs` (`enforce_cap_measured`) | REL3K2: step 3 bounds every long string (head+tail+marker, repeat ⇒ back-reference, halving budget), step 4 replaces the result with a plain path object | INTEG030: one `capped_result_pointer` fallback that also carries `verdict_status`, `failed_step`, `provenance`, `evidence_provenance` | **one pipeline**: ladder bound (2b) → long-string bounding (3, rel-030) → `capped_result_pointer` (4, INTEG030) as the last step. rel-030's "content before structure" rule survives (a launch ladder is bounded, not replaced) and INTEG030's rule survives (when the bulk really cannot be bounded, the pointer still answers did-it-pass / which-step / what-produced-it) |
| 3 | cap helpers | `crates/acos-protocol/src/reply.rs` | `bound_long_strings` / `bound_in_place` / `bound_string_head_tail` / char-boundary helpers | `capped_result_pointer` | **both**, as the two distinct roles they have (string bounding vs the replacement object). No duplicate code path: each is called by exactly one pipeline step |
| 4 | reply-cap tests | `crates/acos-protocol/src/reply.rs` tests | `an_oversized_string_is_bounded_in_place_before_the_result_is_replaced`, `a_blocked_launch_keeps_its_ladder_and_shortens_the_repeated_blocker` | `a_capped_launch_ladder_keeps_every_rung_and_the_blocker`, `a_capped_verify_reply_keeps_the_outcome_failing_step_and_provenance` | **all four** pass against the merged pipeline |
| 5 | closed schemas vs the reply arguments | `crates/acos-mcp/src/descriptors.rs` | REL3K: `with_reply_paging(...with_session_id(...))` for `SESSION_ADDRESSED_PROTOCOL_TOOLS`, `full`/`cursor` only | INTEG030: `protocol_tool_schema` = `session_id` + `PAGED_REPLY_PROTOCOL_TOOLS` + `SETTLE_PROTOCOL_TOOLS` | INTEG030's `protocol_tool_schema` at both call sites (`tool_infos`, `tool_schema`) — it is a strict superset (`acos_verify` paging, `acos_observe` paging + `settle_timeout_ms`, `session_id` on the four session-addressed tools). rel-030's `with_reply_paging` was **deleted** (it had no other caller), so there is one helper and the advertised and enforced schemas still cannot drift |
| 6 | strict-argument reply path | `crates/acos-mcp/src/server.rs` | comment only (`args` shapes the reply on the error path) | comment only (REL3B rejection goes through REL3C's one reply rule) | both intents in one comment; the code is already one path (REL3B's rejection calls REL3C's 5-argument `tool_result(..., &args)`, PERF3's signature) |

No second code path survives any of these: each `<<<<<<<` block resolved to one
call site and one implementation.

### Kept from each side, as required

* INTEG030's SUN_LEN fix and its tests: `crates/acos-art/src/live_view.rs`
  (`scratch_root`), `crates/acos-cli/src/live_registry.rs`, `crates/acos-cli/src/view/mod.rs`,
  `crates/acos-cli/tests/fb1_live_input.rs` — merged in unchanged.
* PERF3's full-tree artifact cache and its tests: `reply::write_full_tree`
  (`TreeFileCache`), `identical_tree_content_reuses_the_named_artifact`,
  `a_different_tree_for_a_used_sequence_gets_its_own_artifact` — merged in
  unchanged.
* INTEG030's `rel3c_capped_verify.rs` e2e test — merged in unchanged.

## §5 decision: a passing run must not show a retried non-pass step

**One step is one entry.** `StepResult.status` is the *final* outcome; the
history of a step that took more than one attempt is a new explicit
`attempts[]` array (`StepAttempt { attempt, status, reason, frame_seq }`), not a
second step entry. `verdict.json`, `steps/NN-<kind>.json` and the inline
`result.verdict` are all built from the same `Verdict`/`StepResult` structure, so
the two views cannot disagree.

* `crates/acos-protocol/src/verdict.rs`: `StepAttempt`, `StepResult.attempts`
  (absent when the step passed on its first attempt), doc comments that become
  the published schema descriptions.
* `crates/acos-verify/src/executor.rs`: `step_attempts()` — the single place
  that turns a multi-attempt step's report into the ledger. Today the only
  multi-attempt step is `scroll_until` (one real gesture + one fresh settled
  traversal per attempt, `attempts` = the attempt that found the selector or the
  total on a miss), so attempts `1..n-1` are `fail` ("selector not visible in
  this attempt's settled traversal"), attempt `n` is the step's own outcome, and
  a miss keeps all of them with "search exhausted" on the last one.
* `docs/AGENT_API.md` §10: documents the rule and the shape with an example.
* `crates/acos-protocol/schema/verdict.json` (+ `artifacts-validation.json`):
  regenerated (`UPDATE_SCHEMA=1`), so the schema doc carries `StepAttempt` and
  `StepResult.attempts`.

Tests (all in `crates/acos-verify/tests/scenarios.rs`, `ScrollBackend`):

| test | proves |
|------|--------|
| `a_retried_step_is_one_entry_with_its_attempts` | find on attempt 2 ⇒ one entry, `status: pass`, `attempts: [fail, pass]`, and **verdict.json's `steps` == the inline `result.verdict`'s `steps`** |
| `a_step_that_never_passes_fails_with_every_attempt` | never found ⇒ `status: fail`, `attempts: [fail, fail, fail]`, last reason "search exhausted", both views equal |
| `a_first_attempt_pass_records_no_attempts_array` | no retry ⇒ no `attempts` key at all (so "a retry happened" is answerable from the reply) |

The capped-reply diagnosis keeps its belt-and-braces gate
(`capped_result_pointer` never names a failing step for a passing verdict); its
comment now points at the new rule instead of the old ambiguity.

## Pre-existing rel-030 reds this merge had to repair (not caused by the merge)

1. **Checked-in JSON Schemas were stale on rel-030.** REL3D
   (`b2935f0d`, merged in `2b883114`) added `ErrorCode::DaemonVersionMismatch`
   without refreshing `crates/acos-protocol/schema/`, so
   `schemas_are_up_to_date` failed on the untouched baseline. Regenerated with
   `UPDATE_SCHEMA=1 cargo test -p acos-protocol`: the only pre-existing delta is
   the `DAEMON_VERSION_MISMATCH` entry (+7 lines in each of
   `envelope.json`, `protocol-error.json`, `verdict.json`,
   `artifacts-validation.json`); the rest is this track's `StepAttempt`.
2. **REL3D's identity handshake vs REL3K/REL3F-era test fakes.** REL3D refuses a
   daemon that cannot name its build, but three test fake daemons predate
   `hello`: `crates/acos-mcp/tests/art_default.rs` (3 tests failed with
   `DAEMON_VERSION_MISMATCH … hello returned no identity`) and
   `crates/acos-cli/tests/rel3f_front_doors.rs` (the fake answered exactly one
   request, so the handshake consumed it and `launch` forwarded `hello`). Both
   fakes now answer `hello` with **the caller's own identity** (they *are* a
   daemon of this build) and keep serving the connection; the front-door fake
   does not capture the handshake as the forwarded call. This changes no
   production code and no assertion; REL3D's own
   `crates/acos-cli/tests/rel3d_daemon_identity.rs` still proves a foreign build
   is refused.

## Gates

| gate | result | evidence |
|------|--------|----------|
| `cargo check --workspace --all-targets --offline` | **PASS**, 0 errors | `cargo check` run in this worktree (`Finished dev profile in 9.48s`) |
| `cargo test --workspace --offline --no-fail-fast` (split by group for the ART lock) | **PASS** except the known fixture failure below | `01-nonart.log` (grep of 241 `test result: ok` lines), `02-mcp.log`, `03-art.log`, `04-cli.log`/`05-cli-after.log`, `06-proto-verify-jni.log` |
| `cargo test -p acos-art` (208 tests) | **208 pass**, 1 ignored | `03-art.log` |
| `cargo test -p acos-mcp` | **PASS**, 1 known fixture failure | `02-mcp.log` |
| `cargo test -p acos-cli` | **PASS**, 1 wall-clock flake (re-ran alone: pass) | `05-cli-after.log`, re-run output above |
| `tools/run-matrix.sh`, benchmarks | **NOT RUN** (orchestrator owns the release gate; not requested here) | — |

Known/environmental failures (neither caused by this branch):

* `acos-jni/tests/flutter_hello.rs::flutter_hello_under_confinement` and
  `acos-mcp/tests/flutter_session.rs::flutter_launch_observe_tap_round_trip` —
  the shared Flutter fixture has no `libapp_probe.so`, so the semantics tree is
  empty (`observe --tree did not return the Increment node`). Same two failures
  INTEG030 reported.
* Wall-clock flakes under load (each passes alone, both already known):
  `acos-cli engine::art::tests::a_zygote_session_signals_and_proves_instead_of_waiting_the_grace`
  (116.2 ms vs the budget) and
  `acos-vm tests/looper_cpu_mt.rs::parked_looper_with_second_thread_burns_no_cpu`
  (55 → 56 CPU ticks). One `acos-cli engine::zygote_host::tests::shutdown_all_leaves_no_keeper_and_no_child_behind`
  run also hit `Text file busy` while another target was being linked; it passes
  alone.
* `cargo test -p acos-mcp` needs the built `acos` CLI in the target dir
  (`cargo build -p acos-cli`); without it 17 MCP protocol tests refuse with
  "the `acos` binary is missing". Not a code failure.

## Disk

Scratch target `.cache/mergeinteg-target` (~1.3 G) deleted at the end;
`.cache/mergeinteg-*` logs/scratch removed. `/` stayed at 77–79 % (206 → 195 G
free) and host RAM under 90 % throughout.
