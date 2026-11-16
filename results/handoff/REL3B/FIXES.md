# REL3B review fixes (REL3B2)

Branch: `rel3b-review-fixes`, based on `rel3b` HEAD `9521f6ed`
(`rel3b` itself was checked out in another t3 worktree, so this work forks it;
fast-forward `rel3b` to this tip to land it).

Review verdict: MERGE-AFTER-FIXES. Each finding below has its fix and the test
that fails without it (`results/handoff/REL3B/FAILS_BEFORE.md` is the recorded
"before" half).

## 1. HIGH — closed MCP schemas rejected documented, honoured arguments

Decision taken as instructed: **every argument a backend honours is in the
schema with a description**; arguments a backend does not honour are not
advertised.

* `session_id` at the top level (session tools and the session-addressed
  protocol tools `acos_observe`/`acos_reset`/`acos_destroy`/`acos_run`) —
  `crates/acos-mcp/src/descriptors.rs` adds it to the schema the tool list
  publishes **and** to the schema `args::check` enforces, from one helper
  (`session_id_property` + `SESSION_ADDRESSED_PROTOCOL_TOOLS`), so the two cannot
  drift. `acos_doctor` deliberately does not publish it (host preflight, no
  session).
* `acos_launch {flutter|rn}` — both in the launch schema, described as
  "declare the app engine, force the native route; detection is otherwise
  structural and the reply's `engine` says what ran".
* `acos_observe {png|screenshot}` — `ObserveRequest` gains `png: bool` and
  `screenshot: Option<Value>` (`true` | path), with the `["boolean","string"]`
  JSON type and descriptions; `schema/observe-request.json` +
  `schema/commands.json` regenerated. The arguments are now honoured on every
  route that can produce pixels:
  * ART daemon (`acos-cli/src/engine/serve.rs`): `png: true` means
    `screenshot: true` (write under the session's artifacts, return the path)
    unless a destination is named — it used to be read by nobody on that route.
  * Flutter route: unchanged (`png: true` returns the PNG inline,
    `screenshot: <path>` writes it).
  * in-process reference session (`acos-mcp/src/session_backend.rs`): a
    destination-less pixel request is a structured `UNSUPPORTED_BACKEND`
    (that session keeps no artifacts dir) instead of a reply that silently has
    no PNG.
* **The alarm test:** `crates/acos-mcp/tests/documentation_args.rs` harvests
  every tool call written in `docs/*.md` (except `PLAN_*.md`, a roadmap of not
  yet existing API), `tools/**/*mcp*.py` and `crates/acos-mcp/tests/*.rs`,
  extracts each call's top-level argument names (JSON and YAML-flow spellings,
  quoted/unquoted names, `json!` wrappers), and runs them through
  `acos_mcp::args::check` against the published schema. At REL3B HEAD it reported
  **30** documented-but-rejected arguments (three findings + the same class);
  now it passes with ~400 documented calls scanned. `tests/args.rs` (the test
  that sends unknown arguments on purpose) and the scanner file itself are
  excluded with a comment.

## 2. MEDIUM — scenario `engine:` vs the engine that ran

* `crates/acos-protocol/src/scenario.rs`: `declared_engine(yaml)` reads the
  top-level engine spelling **through the YAML parser**
  (`serde_yaml_ng` into `{engine: Option<String>}`), not by trimming text, so
  `engine: native-only # pinned`, `engine: 'reference-only'` and a nested
  `observe: {engine: ...}` all read as serde reads them. `legacy_engine_warning`
  now uses it (the trailing comment case produced no warning before), and a new
  `engine_mismatch_warning(declared, ran)` builds the message.
* `crates/acos-verify/src/verify.rs`: when the document declares an engine and
  the request runs another, `provenance.warnings[]` gets
  `SCENARIO_ENGINE_MISMATCH` with `scenario_engine` + `engine`, and the shared
  `provenance::summary_line` appends `warnings: N [SCENARIO_ENGINE_MISMATCH]`.
  The engine that runs is still the request's (`--engine` / MCP `engine`);
  nothing switches engines.
* Tests: `acos-protocol/tests/scenarios.rs`
  (`the_declared_engine_is_the_parsed_yaml_value_not_a_trimmed_line`,
  `a_legacy_engine_with_a_trailing_comment_still_warns`,
  `an_engine_mismatch_names_both_engines`) and `acos-verify/tests/scenarios.rs`
  (`a_scenario_engine_that_differs_from_the_run_is_a_structured_warning`,
  `an_omitted_engine_is_not_a_mismatch`).

## 3. MEDIUM — APK SHA-256 per launch

* `acos_protocol::provenance::InputFile::capture` caches digests in a
  process-wide `Mutex<HashMap<(canonical path, size, mtime_ns, inode), sha256>>`
  (bounded at 64 entries, cleared rather than grown). Every front door
  (`verify`, CLI `launch`, `acos serve` launch, MCP launch/observe) goes through
  it, so a warm launch in a long-lived MCP server or daemon pays no hash, and a
  rebuilt APK at the same path is re-hashed because the key is the file
  identity.
* Measured (test `a_warm_launch_of_the_gaten_apk_pays_no_digest`, fixture-gated on
  `.cache/gaten`): Gaten `gaten-dev-9feffd2-x86_64-release-unsigned.apk`,
  209,737,744 bytes — **cold 3.16 s (debug test binary), warm 22 µs**; release
  `sha256sum` of the same bytes is ≈90 ms (0.162 s for the 302 MB sibling, the
  review's number), which is what used to be paid per launch.
* **No disk cache.** Decision by measurement: the recurring cost is in
  long-lived processes, which the process-wide cache removes entirely; a one-shot
  CLI invocation hashes once (~90 ms against a multi-second launch) and a disk
  cache would add a stale-digest failure mode for that fraction. Recorded here so
  the choice is auditable.

## 4. LOW — inputs changed during the run

* `acos-verify`'s executor captures `Inputs::capture(apk, scenario)` **before**
  the first step (the digests in the verdict are the bytes the run read), and
  after the run compares size/mtime/inode. A change appends
  `INPUT_CHANGED_DURING_RUN` (with `input`, `path`, `sha256_12`, `change`) to
  `provenance.warnings[]`; the status stays exactly as measured and the shared
  summary line names the warning. `verdict.reproducer`'s scenario digest comes
  from the same pre-run capture.
* Tests: protocol
  (`an_input_rewritten_during_the_run_is_reported_not_silent`,
  `an_input_identity_carries_the_size_and_mtime_it_was_hashed_at`) and
  acos-verify `an_input_rewritten_during_the_run_is_a_warning_in_the_verdict`
  (a backend decorator rewrites the APK during `manifest()`).

## 5. LOW — relative `app:` resolution

* `verify_with_clock` resolves `app:` against the scenario path **as written**
  (`resolve_relative(&request.scenario, ...)`, the pre-REL3B behaviour) and only
  the provenance/verdict identity uses `provenance::absolute` (canonical).
* Test: `a_relative_app_resolves_against_the_scenario_path_as_written` (a
  symlinked scenario whose `app.apk` sits next to the link; fails at REL3B HEAD
  with `APK_IO "APK not found: app.apk"`).

## 6. LOW — stale engine text / `explicit`

* `crates/acos-protocol/src/commands.rs`: `VerifyEngine`'s doc no longer calls the
  scenario `engine:` a legacy native/reference preference; it states the one
  vocabulary, that the scenario field records rather than selects, and that a
  disagreement is `SCENARIO_ENGINE_MISMATCH`. `VerifyRequest::engine` documents
  both defaults: `art`, and `rust-vm` when `reference` is set without `engine`
  (an explicit `engine: art` + `reference` stays `INVALID_ARGUMENT`).
  `schema/verify-request.json` and `schema/commands.json` regenerated.
* `acos-cli/src/verify.rs`: `--engine` is `Option<String>`; `engine_name()`
  (explicit value, else the `--reference` implication, else the constant) and
  `engine_explicit()` replace the name comparison. `engine.json`'s
  `explicit` is now `args.engine_explicit() || mode == Preview`, so
  `--reference` without `--engine` reports `explicit: false`; `--help` keeps
  `[default: art]` (from the doc text, so the existing help test still pins the
  constant).
* Tests: `crates/acos-cli/tests/verify.rs`
  `reference_without_engine_is_not_an_explicit_engine_choice` (implied →
  `explicit:false`, explicit → `true`), and
  `acos-mcp/tests/documentation_args.rs`
  `the_engine_argument_documents_the_reference_default`.

## Acceptance evidence

| command | result |
|---|---|
| `cargo test --offline -p acos-protocol -p acos-verify -p acos-mcp` (via `run-exclusive.sh` + `guarded.sh --mem 12G`) | **379 passed, 0 failed** across 28 suites (`.cache/rel3b2-tests.log`) |
| `cargo test --offline -p acos-cli --test verify --test negative_verify` (same guard) | **10 passed, 0 failed** (`.cache/rel3b2-cli-tests.log`) |
| `cargo test --offline -p acos-cli --bin acos serve::tests` | **27 passed, 0 failed**, incl. the new `png_true_writes_the_frame_and_a_named_destination_wins` |
| `schemas_are_up_to_date` | pass (schemas regenerated: `commands.json`, `observe-request.json`, `verify-request.json`) |

Real-ART smoke on this branch (debug `acos` + debug `acos-art` from the same
source, `results/handoff/REL3B/provenance-smoke.yaml`, `--engine art`, through
`run-exclusive.sh` + `guarded.sh`): **pass**, 3/3 steps, `engine art`,
`fidelity art-reference`, `warnings: []`, and the same input identities REL3B's
own handoff recorded — scenario `sha256_12 4ac610c4ce06`, APK
`sha256_12 8170835e70f2` (12,756 bytes, now also carried as
`provenance.apk.size_bytes`). The scenario declares `engine: art` and the run
used `art`, so no `SCENARIO_ENGINE_MISMATCH`; the executor also exercised the
new pre-run input capture and the non-canonical `app:` resolution on a real
ART run.

`tests/flutter_session.rs` stays fixture-gated and skipped here (no Flutter
fixture in this worktree); the argument-gate half of that path is covered by
`documentation_args.rs`, which is where the regression lived.
