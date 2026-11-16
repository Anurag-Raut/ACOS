# MERGE3I3 — rel3i3 into rel-030 (REL3H2 + REL3I3, one settle path)

Branch `rel030-3i3` off `rel-030` (`0e3719bf` = REL3H + REL3H2), merge of `rel3i3`
(`b7872474`). Worktree `.t3/worktrees/acos/t3-879b7c7c-6dfb08dd82494c49`.
Every ART run through `tools/art-runtime/run-exclusive.sh` + `tools/guarded.sh`;
private `ACOS_RUNTIME_DIR`/`ACOS_SESSION_SOCKET`; builds in
`$WT/.cache/merge3i3-target` (`CARGO_PROFILE_DEV_DEBUG=0`,
`CARGO_PROFILE_TEST_DEBUG=0`, `CARGO_INCREMENTAL=0`).

## 1. Status: DONE

`git merge rel3i3` produced exactly the two predicted conflicts. Both are
resolved by **keeping both behaviours in one code path**, not by picking a side,
and the precedence between them is stated once and tested.

## 2. The conflicts

### 2.1 `crates/acos-art/src/launch.rs` — the settle wait

| side | what it adds |
|---|---|
| REL3H2 (`rel-030`) | `wait_for_event_settled`/`wait_for_settled_observation` take `unhandled_ack_floor: Option<u64>` + `entry_fnv: Option<u64>`; the no-draw arm builds an `UnhandledSettleGate` and may bind `bound: "unhandled-input"` (`SETTLE_UNHANDLED_INPUT`) |
| REL3I3 (`rel3i3`) | the same functions take `window_removal: Option<(BinderId, u64)>`; `removal_binds` (token-scoped, mark taken at publish) makes a pre-publish draw the post-action state, and the bind is named `bound: "host-window-removal"` |

Resolution: **one signature, one branch chain**.

* both parameter groups are kept (`unhandled_ack_floor`, `entry_fnv`,
  `window_removal`) on both functions, and both are threaded through the
  `wait_for_settled_observation` → `wait_for_event_settled` call;
* the tree request (no input published) passes `None, None, None`;
* the input path passes `unhandled_ack_floor`, `entry_fnv` **and**
  `Some((injected.token, removal_mark))`;
* the no-draw arm reads
  `publish_epoch_ms != 0 && sample.draw_ms < publish_epoch_ms && !removal_binds`.
  The `!removal_binds` guard **is** the precedence (below).

### 2.2 `crates/acos-protocol/src/verdict.rs` — the step's input block

| side | what it adds |
|---|---|
| REL3H2 | `StepResult.warnings: Vec<serde_json::Value>` (the `INPUT_UNHANDLED` records, copied into `provenance.warnings[]`) |
| REL3I3 | `StepResult.input: Option<StepInputResult>` (typed: `handled`, `handled_evidence {source: guest\|host_window_removed\|none, published, acked, unhandled}`, `target`, `expect_change`, `until`, `handled_hint`) |

Resolution: **both fields** — typed `input` **and** `warnings`, in that order, and
`StepResult::new` initialises both (it already did after the auto-merge; only the
field declaration conflicted). Schemas regenerated with
`UPDATE_SCHEMA=1 cargo test -p acos-protocol --test schemas`
(`crates/acos-protocol/schema/verdict.json`: `StepInputResult`, `HandledEvidence`,
`HandledSource` **and** `warnings` present).

## 3. The precedence rule

**A host-recorded removal of the window the input was delivered to outranks the
unhandled shortcut.** A removed dialog *is* handled.

Two places implement it, and they agree:

1. **Settle bind** (`crates/acos-art/src/launch.rs`): when `removal_binds` is
   true the REL3H/REL3H2 arm is skipped (`&& !removal_binds`), so the sample
   falls through to the normal strict rule and binds
   `bound: "host-window-removal"`. It can never bind `bound: "unhandled-input"`
   for that sample: the removal is the step's draw, and the gesture was consumed
   by the window that went away.
   Every REL3H2 guard stays intact for the case that is *not* a removal: the
   bound sample must be strictly newer than the gesture's last ack
   (`UnhandledSettleGate::new(floor, confirm)`) and its `draw_seq` must stay
   newest for one confirmation window (a side effect that draws restarts the
   wait and falls through to the normal rule).
2. **Handled facts** (`crates/acos-protocol/src/ui.rs::handled_source` /
   `handled_answer`, one ladder used by `acos-cli`, `acos-verify` and `acos-mcp`):
   1. a guest `handled: true` ack wins (some window consumed the event);
   2. else the host's removal of the delivered window → `handled: true`,
      `source: host_window_removed` — even when **every** ack of the same gesture
      said `finishInputEvent(handled) = false` (that answer can only describe a
      window that no longer exists, and a disposed `InputEventReceiver` cannot
      ACK at all);
   3. else a complete ack set → `source: guest` (the app's own answer, unchanged
      from REL3E/REL3H2 — this is what still produces `INPUT_UNHANDLED`);
   4. else `source: none`, `handled: null` (unknown, never a guess).

Because `StepInputResult.handled` comes from the same ladder as
`handled_evidence.source`, a step can no longer report `source:
host_window_removed` while `handled: false`. `INPUT_UNHANDLED` is emitted only
when `handled == Some(false)` (executor `input_unhandled_warning`), so a removed
dialog reports no warning and `expect_handled: true` holds; `INPUT_NOT_HANDLED`
likewise cannot fire for it.

Documented for agents in `docs/AGENT_GUIDE.md` (step 5, the `handled` bullets)
and `docs/MCP.md` (`handled_evidence.source`).

## 4. The precedence test (new)

`crates/acos-verify/src/executor.rs::region_tests::merge3i3_a_removed_dialog_outranks_an_unhandled_ack`
— a `back` gesture, 2 published events **both acked `handled: false`**, and
`window_removed: true`: `input.handled == Some(true)`, `source ==
HostWindowRemoved`, the unhandled seqs still named, no `INPUT_UNHANDLED` warning,
`expect_handled: true` holds; and the *same* acks without the removal keep
REL3H2's answer (`source: guest`, warning with `unhandled: [1, 2]`).

Companion at the decision point:
`crates/acos-protocol/src/ui.rs::tests::a_host_window_removal_outranks_an_unhandled_ack`
pins the whole ladder (including "a `handled: true` ack wins", "nothing delivered
⇒ `none`", "a removal with nothing delivered is not evidence").

Fail-before: on `rel3i3` the same facts give `source: guest` / `handled: false`
(its `handled_source` returns `Guest` for `any_unhandled`, and `handled_answer`
keeps `Some(false)`), i.e. the test's first assertion fails and the step would
carry `INPUT_UNHANDLED`; on `rel3h2`/`rel-030` the `window_removed` fact and the
typed `input` block do not exist at all. The `!removal_binds` guard in the settle
arm has no unit seam (it is inside the ART probe loop): it is covered end-to-end
by `back-layers.yaml` step 5 (`bound: "host-window-removal"`,
`source: host_window_removed`, 22/22 pass) and by the REL3H2 scenarios still
binding `unhandled-input` (below).

## 5. Verification on the merged tree

### 5.1 Builds and tests

| command | result | evidence |
|---|---|---|
| `cargo check --offline --workspace --all-targets` | `Finished`, 0 errors (only pre-existing warnings) | `cargo-check-workspace.log` |
| `cargo test -p acos-protocol -p acos-verify -p acos-wm -p acos-binder-peer` | 23 binaries, **382 passed / 0 failed** | `cargo-test-core.log` |
| `cargo test -p acos-art -p acos-services` (lock) | 63 binaries, **581 passed / 0 failed** (`acos-art` lib 162, 1 ignored) | `cargo-test-art-services.log` |
| `cargo test -p acos-cli -p acos-mcp` (lock) | 43 binaries, **451 passed / 0 failed** (`acos-cli` bin 192 + every integration binary; `acos-mcp` lib 93 + guide/docs/e2e/args) | `cargo-test-cli-mcp.log` |
| `UPDATE_SCHEMA=1 cargo test -p acos-protocol --test schemas` | 2 passed, `verdict.json` regenerated | §6 |

### 5.2 ART scenarios (live, release `acos`/`acos-art`, `--engine art`)

| scenario | expected | observed | evidence |
|---|---|---|---|
| `back-layers.yaml` | pass 22/22 | **pass 22/22**; step 5 (dialog `back: {}`) `handled: true`, `source: host_window_removed`, `settle.bound: host-window-removal`, no warning | `art/back-layers/verdict.json`, `art/back-layers.out` |
| `back-layers-ime.yaml` | pass 22/22 | **pass 22/22** | `art/back-layers-ime/verdict.json` |
| `rel3h-unhandled-input.yaml` | pass + `INPUT_UNHANDLED` | **pass**, step 1 `handled: false`/`source: guest`/`bound: unhandled-input`, warning `INPUT_UNHANDLED` in step **and** `provenance.warnings[]` | `art/rel3h-unhandled-input/` |
| `rel3h-unhandled-input-expect-change.yaml` | fail `ASSERTION_FAILED` | **fail** `ASSERTION_FAILED` (`expect_change`) | `art/rel3h-unhandled-input-expect-change/` |
| `rel3h2-expect-handled-false.yaml` | pass, warnings `[]` | **pass**, warnings `[]` (`done` + `handled: true`) | `art/rel3h2-expect-handled-false/` |
| `rel3h2-expect-handled-true.yaml` | fail `INPUT_NOT_HANDLED` | **fail** `INPUT_NOT_HANDLED` | `art/rel3h2-expect-handled-true/` |
| `rel3a-content-settle.yaml` | pass | **pass** (6 steps) | `art/rel3a-content-settle/` |
| `rel3a-content-pixel.yaml` | fail (`content_settle_pixel`) | **fail** `ASSERTION_FAILED` … "settled by `settle: content` … not pixel evidence" (`expectation: non_pass`) | `art/rel3a-content-pixel/` |
| `rel3a-content-sticky.yaml` | timeout (`non_pass`) | **timeout** `TIMEOUT: capture: strict settle timed out` | `art/rel3a-content-sticky/` |
| `form-validation.yaml` | pass 28/28 | **pass 28/28** | `art/form-validation/` |
| `gaten-backstack.yaml` | pass 25/25 | **pass 25/25** | `art/gaten-backstack/` |

Both `rel3i3` behaviours survive the merge unchanged where they do not intersect:
the delivery wake (peer `poll` on the session wake fd) and the IME layer in
`back-layers` are REL3I3 code and pass 22/22; the REL3H2 unhandled-input settle
still binds `unhandled-input` and still warns.

## 6. Test commands actually run

Host env: `ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts`,
`ACOS_VIEW_TEST_DIR=/home/Qui/.cache/m3i3-lv`,
`ACOS_RUNTIME_DIR=/home/Qui/.cache/m3i3-run`,
`ACOS_SESSION_SOCKET=/home/Qui/.cache/m3i3.sock`,
`CARGO_TARGET_DIR=$PWD/.cache/merge3i3-target`,
`CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0`.

```
cargo check --offline --workspace --all-targets                      # Finished
cargo test  --offline -p acos-protocol -p acos-verify -p acos-wm -p acos-binder-peer
tools/art-runtime/run-exclusive.sh --task MERGE3I3 -- tools/guarded.sh --mem 24G --time 600 -- \
  cargo test --offline -p acos-art -p acos-services
tools/art-runtime/run-exclusive.sh --task MERGE3I3 -- tools/guarded.sh --mem 24G --time 600 -- \
  cargo test --offline -p acos-cli -p acos-mcp
UPDATE_SCHEMA=1 cargo test --offline -p acos-protocol --test schemas
tools/art-runtime/run-exclusive.sh --task MERGE3I3 -- tools/guarded.sh --mem 12G --time 900 -- \
  cargo build --offline --release -p acos-cli
tools/art-runtime/run-exclusive.sh --task MERGE3I3 -- tools/guarded.sh --mem 12G --time 900 -- \
  cargo build --offline --release -p acos-art
ACOS_ART_BIN=$PWD/.cache/merge3i3-target/release/acos-art \
  .cache/merge3i3-target/release/acos verify scenarios/<s>.yaml --engine art --artifacts results/handoff/MERGE3I3/art/<s>
```

`ACOS_APP_ONLY="back-layers tap-targets" tools/build-apps.sh` built the two
fixture APKs this worktree lacked (`fixtures/build/` is a hardlinked copy of the
shared prebuilt corpus; `build_apk` unlinks before writing, so the shared copy is
untouched).

### Environment note (not a merge regression)

With a long worktree path, three path-length tests fail unless the test scratch
dirs are short: `acos-cli`'s
`live_registry::scratch_tests::the_test_scratch_root_leaves_room_for_a_unix_socket`
(needs `ACOS_VIEW_TEST_DIR` ≤ 72 bytes **and** short enough for the deepest
socket), `acos-mcp`'s
`private_daemon::tests::private_dir_is_per_process_and_under_the_runtime_dir` and
`default_session_tools_use_the_private_art_daemon` (need a short
`ACOS_RUNTIME_DIR`, and that var must not be inherited when a test sets its own
`XDG_RUNTIME_DIR`). `git diff -r rel-030 HEAD -- crates/acos-cli/src/live_registry.rs
crates/acos-mcp/src/private_daemon.rs` is empty: none of that code is touched by
this merge. All three pass with the short dirs used above; the logs in this
directory are from runs with them.

## 7. What remains

Nothing for this merge. Not run here (out of the requested scope): the
`#[ignore]`d ART test binaries `rel3e_handled`/`rel3s_swipe`/`rel3s_scenario`
(they need `scroll-list.apk`, which this worktree does not build); their
`source: guest` assertions are covered by the live `rel3h*`/`back-layers` runs.
