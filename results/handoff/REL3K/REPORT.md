# REL3K (ACOS v0.3.0) — Back navigation on real ART, merged onto rel-030

Branch `rel3k`, worktree `.cache/wt` of the orchestrator (`t3-713e1712`).
Base `art-back-1` (`15c6a111`), merged with `rel-030` (`2022d89c`).

## 1. What shipped

### 1.1 ROT8b's WIP edit, finished (commit `361470bb`)

`AtmsState::task_finished_proof` is now **positive** instead of negative:

```rust
!a.activity.is_empty() && (a.finished || a.lifecycle == ActivityLifecycle::Destroyed)
```

The old predicate (`!a.lifecycle.is_alive()`) had two defects in opposite
directions:

* **false pass:** a record that never left `Uncreated` is "not alive", so an
  unstarted record made the proof true (`an_unstarted_record_fails_closed`);
* **false stall:** a record the client had already `finish()`ed stayed "alive"
  until the host processed its separate `activityDestroyed` callback, so a
  guest `window-gone` sample could arrive first and the settle waited to the
  deadline (`a_finished_record_ends_before_its_destroy_callback_arrives`).

`tests/rot8b_task_finished.rs` was repaired to drive real lifecycle transitions
(`LaunchActivity` → `Start` → `Resume`, and adopt the guest token the way the
framework does), so the tests exercise what they name. 7 tests:

| test | old predicate | new predicate |
|---|---|---|
| `live_record_is_not_finished` | pass | pass |
| `a_paused_or_stopped_record_is_still_live` | pass | pass |
| `pending_start_blocks_…_destroyed` | pass | pass |
| `a_finished_record_ends_before_its_destroy_callback_arrives` | **FAIL** | pass |
| `all_records_destroyed_and_nothing_pending_is_finished` | pass | pass |
| `an_unstarted_record_fails_closed` | **FAIL** | pass |
| `an_unnamed_record_fails_closed` | pass | pass |

Fail-before log: `results/handoff/REL3K/rot8b-fail-before.log`
(`FAILED. 5 passed; 2 failed` with the old predicate restored; the file also
contains the pass run in the same target).

### 1.2 Merge of `rel-030` (commit `332d2c5a`)

Textual merge was conflict-free. Integration fixes that the merge exposed
(each one a real defect on `rel-030`, none weakening a test):

1. **`acos-mcp` did not compile on `rel-030`**: `tool_result` grew a 5th
   (`args`) parameter and one early-return call site (the invalid-argument
   reply) still passed 4. Pass `&args` so the error reply is shaped with the
   caller's `full`/`cursor` like every other reply.
2. **Compact tree dropped input state** (`acos-protocol/src/reply.rs`): the
   agent-facing compact page kept text/bounds/clickable but not `focused` or
   `password`, so `fb1_live_input`'s typing contract (and an agent typing into
   the node it just found) had no focus/password signal. Both booleans are now
   part of the compact node.
3. **Reply paging args were rejected by the server's own schemas**
   (`acos-mcp/src/descriptors.rs`): `acos_verify` (and `acos_observe`) replies
   are paged with `cursor`/`full` (REL3C), and the CLI's own `session observe`
   sends both; the closed schemas omitted them, so paging a verify reply was
   impossible. One `with_reply_paging` helper feeds both `tool_infos` and
   `tool_schema`, so advertised and enforced schemas cannot drift.
4. **`acos session observe` sent `settle_timeout_ms` to `acos_observe`**
   (`acos-cli/src/session.rs`), a tool with no settle rule and a closed schema.
   The deadline is now sent only to `acos_observe_tree`.
5. **`fb1_live_input`'s scratch dir name overflowed `sun_path`** from a deep
   worktree root (`serve.sock` > 108 bytes). The scratch dir now uses a short
   stable digest of the tag.

Design choices made at the merge (consistent with rel-030's intent, listed for
review): (2) `focused`/`password` are input state an agent acts on, not
decoration, and rel-030's own `fb1` test already asserted them; (3) paging is a
reply property of every tree-carrying tool, so the fix belongs in the MCP
schema layer, not in `VerifyRequest` (the CLI `verify` has no `--cursor`).

### 1.3 MCP `acos_key {"code":"back"}` = scenario `back` (commit `HEAD`)

Both routes converge on the ART daemon's `key` command:

* MCP: `acos_key {"code":"back"}` → `action_key_code` (`session_backend.rs`,
  `"back" => Some(4)`) → the private `acos serve --engine art` daemon;
* CLI/scenario: `InputAction::Back` → key code 4 (`engine/mod.rs:1134`), also
  the daemon's `key` command; the daemon maps `"back" => Some(4)`
  (`session.rs:529`) and injects it through the real `InputChannel`.

New test `crates/acos-mcp/tests/art_default.rs::acos_key_back_goes_to_the_art_daemon_and_reports_handled`
pins the MCP half: the ART daemon receives `code: "back"`, and the REL3E
`handled` bit and the `window-gone`/`task_finished` settle in its reply survive
the MCP boundary verbatim.

## 2. Back cases on real ART (all `--engine art`)

Artifacts under `results/handoff/REL3K/artifacts/<name>/` (verdict + steps + frames).
Logs: `art-batch1.log`, `art-batch2.log`.

| case | scenario | result | proof |
|---|---|---|---|
| Back pops B→A in AOSP order (Pause B → Start/Resume A → Stop B → Destroy B) | `multi-activity` (back #1) | **pass** (22 steps) | step 11: `handled:true`, `handled_evidence {acked:2,published:2,source:finishInputEvent}`, `settle.bound:"strict-event"`, `pixels_changed:true`, new frame 9; `assert_log` ordered Pause/Start/Resume/Stop/Destroy; `assert_text` A's `Hello` |
| Back on the root Activity finishes the task, honestly | `multi-activity` (back #2), `hello-views`, `counter` | **pass** | step 19/23: `settle.bound:"window-gone"`, `task_finished:true`, `window_gone:true`, `pixels_changed:false`, no PNG published; `assert_not_visible` passes; `assert_no_crash` passes |
| Back handled by the app stays on screen | structural: `handled` is the guest window's own bit, reported per step (REL3E) | **pass (evidence)** | B→A step has `handled:true` with 2/2 acks; root-back has `handled:true` when acked (multi-activity) and `handled:null` + honest `handled_hint` when the destroyed window never acked (hello-views) |
| Back closes… (dialog/IME first) | AOSP path is the same `onBackPressed`; the app/framework decides | **not separately scenario-tested** | see §4 |
| finish()+startActivity(Home) must never bind an empty "finished" screen | `finish-start` (positive), `negative/finish-start-wrong-home` | **pass / fail-as-designed** | positive binds Home's first draw (`assert_text Home`, `assert_not_visible go`); negative fails `ASSERTION_FAILED … got text "Home"` |
| No stale frame after Back | `w42-back-frame` (gate) | **pass** (17 steps) | region hashes equal Main's post-Back pixels (`assert_region` x2) |
| A finishing Activity is never asked to save state | `negative/multi-back-save-state` | **fail-as-designed** | `ASSERTION_FAILED: log assertion did not hold` |
| Wrong expectation after Back | `negative/multi-wrong-after-back` | **fail-as-designed** | `ASSERTION_FAILED … got text "Hello"` |
| Text is gone after the task finished | `negative/hello-views-back-text-gone` | **fail-as-designed** | `ELEMENT_NOT_FOUND: selector id=="result" matched no node` (never a stale pass) |
| Finish-button path | `multi-activity-finish` | **pass** (19 steps) | |
| Rotation + Back | `v1-recreate` | **pass** (18 steps) | |

Note: every scenario run prints `SCENARIO_ENGINE_MISMATCH` because the files say
`engine: native-preferred` and the run forces `--engine art`; pre-existing.

## 3. Crate tests

`cargo test --release --offline --no-fail-fast -p acos-services -p acos-art
-p acos-cli -p acos-mcp -p acos-verify -p acos-protocol` (under
`run-exclusive.sh`+`guarded.sh`, `ACOS_VIEW_TEST_DIR=/dev/shm/rel3k-view`):
**all targets green except two**, both investigated and not caused here:

* `acos-art --test cleanup` → `leak_scan_is_scoped_to_this_tests_own_processes`
  failed in the full parallel run, **passes 8/8 in isolation** (1.09 s); the
  same target's `log_spam_…` load flake is already recorded in EXPERIMENTS E9.4
  (LHFLAKE).
* `acos-cli --test native_launch` →
  `native_launch_without_native_reports_unsupported_with_hint` expects a
  `application_on_create` rung with `blocker.code=UNSUPPORTED_JAVA_API`; the
  frozen Rust VM now returns a top-level `UNSUPPORTED_JAVA_API` and an empty
  ladder. `git diff rel-030..HEAD -- crates/acos-cli/src/launch.rs
  crates/acos-runtime crates/acos-vm crates/acos-cli/tests/native_launch.rs` is
  **empty**: identical sources on `rel-030`, pre-existing.

The pre-fix full-suite log is committed as
`rel-030-merge-crate-tests-before-fixes.log` (6 red targets); the fixes log is
`crate-fixes.log`.

## 4. What is not done / smallest next action

1. **Dialog/IME-first Back has no scenario.** The ATMS path is the same
   `IActivityClientController.onBackPressed` the framework uses; on API 34
   `ViewRootImpl$NativePreImeInputStage` claims Back ahead of the IME, which is
   the AOSP behaviour, but the *app-visible* case (open menu/IME, Back closes it
   and stays on screen) should be a scenario before v0.3.0 ships it as a claim.
   Smallest action: add `back` steps to `w15-dialog-menu` (or a new
   `dialog-back.yaml`) and assert the dialog is gone and the activity is not.
2. **The MCP-on-real-ART `acos_key {"code":"back"}` run** is pinned at the
   daemon boundary (new `art_default` test) but not executed against a live ART
   guest; the ART behaviour itself is proven through the identical scenario
   path. Smallest action: one live `acos-mcp` session on `multi-activity`, call
   `acos_key {"code":"back"}`, assert `handled:true` and `settle.bound`.
3. `native_launch_without_native_…` (pre-existing, frozen Rust VM) — not Back;
   needs its own row (either restore the blocked ladder for a `bindApplication`
   failure or move the test to the current error contract).
4. `hello-views`'s root-Back step reports `state_changed:false` while claiming
   `window-gone` (the pre-action probe sample was empty, so no change is
   claimed). It under-claims, never over-claims; `pixels_changed:false` +
   `task_finished:true` carry the evidence. Worth tightening later by passing
   the last bound tree as the input-step baseline.

## 5. For the orchestrator

* Review the four rel-030 integration fixes in §1.2 — they change MCP schemas
  and the compact-tree contract, chosen to keep rel-030's stated intent (agent
  reply paging, typing needs focus/password). All are covered by existing tests
  that were red on `rel-030`.
* The merge is a normal merge commit (`332d2c5a`); nothing was rebased.

## 6. Resources

Peak RSS observed while testing: `guarded.sh` peaks under 10 GiB per run (host
MemAvailable stayed > 45 GiB). Disk `/` went 81% → 82%; the target dir
`.cache/rel3k-target` (~4 GiB) and artifacts are removed/kept as noted at the
end of the run.
