# REL3B review fixes — the "fails before" half

Every test below was run in a scratch worktree created with `git worktree add`
under this worktree's `.cache/`, then the worktrees and their target directory
were removed. No `git stash` was used.

## 1. The original REL3B tests, at the pre-REL3B baseline (`09bcdfea`)

Scratch worktree `.cache/wt-fails-before-base` at commit `09bcdfea`, with the
REL3B test files copied in from `9521f6ed`:

| copied file | run | result |
|---|---|---|
| `crates/acos-mcp/tests/args.rs` | `cargo test -p acos-mcp --test args --no-run` | **does not compile**: `error[E0425]: cannot find value DEFAULT_VERIFY_ENGINE_NAME in module acos_protocol::commands` |
| `crates/acos-protocol/tests/scenarios.rs` | `cargo test -p acos-protocol --test scenarios` | **does not compile**: `error[E0599]: no variant or associated item named Art/RustVm found for enum acos_protocol::Engine` (the one-word engine vocabulary does not exist yet) |
| `crates/acos-verify/tests/scenarios.rs`, `crates/acos-cli/tests/verify.rs`, `crates/acos-cli/tests/negative_verify.rs` | same `cargo test --no-run` invocation | same class: the provenance module (`acos_protocol::provenance`), `RunOptions.engine`, the closed-schema `args` module and `DEFAULT_VERIFY_ENGINE_NAME` are all absent at the baseline |

The point of the exercise is the direction of the delta: at `09bcdfea` the tests
that REL3B added cannot even build, so they are measuring new behaviour, not
restating old behaviour.

## 2. The review's three schema findings, at REL3B HEAD (`9521f6ed`)

Scratch worktree `.cache/wt-fails-before-rel3b` at `9521f6ed` (production code
present, fixes absent), with this branch's
`crates/acos-mcp/tests/documentation_args.rs` copied in:

```
$ cargo test -p acos-mcp --test documentation_args
test the_scanner_reads_both_json_and_flow_style_arguments ... ok
test the_engine_argument_documents_the_reference_default ... FAILED
test the_three_arguments_this_test_was_written_for_are_in_their_schemas ... FAILED
test every_documented_argument_is_accepted_by_its_tool_schema ... FAILED
test result: FAILED. 1 passed; 3 failed
```

The scanner named 30 documented-and-honoured arguments that the published
schema rejected — the review's (a), (b) and (c) plus the same class elsewhere:

```
crates/acos-mcp/tests/flutter_session.rs:66:  `acos_launch` apk, flutter, timeout_ms, base
    -> unknown argument for `acos_launch`: `flutter`                       (finding 1b)
crates/acos-mcp/tests/flutter_session.rs:96:  `acos_observe` png, base
    -> unknown argument for `acos_observe`: `png`                           (finding 1c)
crates/acos-mcp/tests/flutter_session.rs:178: `acos_observe` png, base
    -> unknown argument for `acos_observe`: `png`
docs/AGENT_API.md:561 / :646: `acos_observe` png
    -> unknown argument for `acos_observe`: `png`
docs/MCP.md:470 / :471: `acos_observe_tree` session_id, screenshot
    -> unknown argument for `acos_observe_tree`: `session_id`              (finding 1a)
tools/release/mcp_smoke.py:432/1083/1192: `acos_destroy` session_id
    -> unknown argument for `acos_destroy`: `session_id`
tools/release/mcp_smoke.py:1058: `acos_reset` session_id, scope
    -> unknown argument for `acos_reset`: `session_id`
tools/release/mcp_smoke.py:441/1093: `acos_status` session_id
tools/release/mcp_smoke.py:689/764/998: `acos_tap` session_id, ...
tools/release/mcp_smoke.py:826: `acos_text` session_id, text
tools/release/mcp_smoke.py:700/755/806/869/878/959/1031/1072: `acos_observe_tree` session_id
```

(ALL session tools rejected the top-level `session_id` that `docs/MCP.md` and
`mcp_smoke.py` send; `acos_observe` rejected `png`/`screenshot`; `acos_launch`
rejected `flutter`.)

Same worktree, the engine-vocabulary/schema description test:

```
the_engine_argument_documents_the_reference_default ... FAILED

thread panicked at crates/acos-mcp/tests/documentation_args.rs:
the description must state that `reference` selects `rust-vm` when `engine` is
omitted: Execution engine: `art` (default, real ART) or `rust-vm` (frozen).
```

## 3. Finding 2, behaviourally, at REL3B HEAD

Extra test dropped into the scratch worktree (never committed) that uses only
REL3B APIs:

```
$ cargo test -p acos-protocol --test fails_before_engine_comment
test a_declared_engine_that_differs_from_the_run_is_reported ... FAILED
    rel3b has no SCENARIO_ENGINE_MISMATCH warning
test a_legacy_engine_with_a_trailing_comment_still_warns ... FAILED
    no engine warning for `engine: native-only # pinned`: []
```

i.e. (i) the trailing-comment spelling produced **no** rename warning because
`legacy_engine_warning` compared a hand-trimmed text line, and (ii) nothing in
the tree ever reported a scenario/engine disagreement.

## 4. Findings 3–6: before-state, by construction

* **3 (APK digest cost).** REL3B's `provenance::block` called `sha256_file` on
  every reply, so every launch and every verify paid the full hash. Measured on
  the 209,737,744-byte Gaten APK
  (`.cache/gaten/gaten-dev-9feffd2-x86_64-release-unsigned.apk`):
  `sha256sum` = **0.162 s** for the 302 MB build, **≈90 ms** for the 209 MB one
  (the review's number), and 3.16 s in the debug test binary — per launch, with
  no reuse. There was no cache of any kind at rel3b (grep: no `Mutex`/`HashMap`
  in `provenance.rs`).
* **4 (input changed during the run).** `verdict.provenance` was built *after*
  the steps from `provenance::block(&options.apk, Some(&options.scenario_path),
  ...)`, i.e. the digest described whatever was on disk when the run ended.
  Nothing compared size/mtime, and `INPUT_CHANGED_DURING_RUN` did not exist.
* **5 (relative `app:`).** `verify_with_clock` resolved `app:` with
  `resolve_relative(&scenario_path.display().to_string(), ...)` where
  `scenario_path` was already canonicalized, so a symlinked scenario resolved its
  relative `app:` from the link target's directory.
  A standalone copy of
  `a_relative_app_resolves_against_the_scenario_path_as_written` run in the
  `9521f6ed` worktree fails there, exactly as the regression predicts:

  ```
  test a_relative_app_resolves_against_the_scenario_path_as_written ... FAILED
  a relative app next to the named scenario must resolve:
    Some(ProtocolError { code: ApkIo, class: Harness,
      message: "APK not found: app.apk", ... })
  ```

  (the canonicalized scenario directory has no `app.apk`, so resolution fell
  back to the process cwd).
* **6 (engine vocabulary text / `explicit`).** `commands.rs`'s `VerifyEngine`
  doc still called the scenario `engine:` field "a legacy native/reference
  preference (`native-preferred` | `native-only` | `reference-only`)" and
  claimed ART verification does not consume it; `engine.json`'s `explicit` was
  `args.engine != "art" || mode == preview`, so `--reference` (which implies
  `rust-vm`) was reported as an explicit choice.
