# REL3E — input is reported as handled, not just as sent

Branch `rel3e` (base `main` 7507f9d1). Root cause E of the plan
(`docs/PLAN_2026-11-16.md` §1) and feedback entry 9/10 of
`~/.claude/acos-feedback/ACOS_AGENT_FEEDBACK.md` (Gaten FAB run, 2026-11-15).

## What was wrong

`acos_tap {"text": "Floating button"}` on the settings row whose title is not the
pressable (the switch is) was accepted and changed nothing, and the reply said
only `state_changed: false`. Two different situations produced that identical
reply:

* nothing tappable at that point / no handler consumed the press, and
* the app consumed the press and nothing changed.

The framework's own answer was already on the wire and thrown away: the guest's
`InputEventReceiver.finishInputEvent(seq, handled)` is encoded in the
`FINISHED` message right after each `InputMessage`, decoded by
`acos-services::input_channel` and surfaced as `InputAck.handled` — but no reply
carried it, the frozen VM's native ack hook literally did `let _ = handled;`,
and no reply said what the tap point hit.

## The fix (one source)

1. **`handled` is the framework bit.** AOSP `ViewRootImpl` finishes every event
   with `FLAG_FINISHED_HANDLED` (`base/core/java/android/view/ViewRootImpl.java`
   `finishInputEvent`, line 9836) and drops with `false` (`finish(q, false)`).
   It is now carried:
   * ART: `InputAck.handled` → `InputDelivery.handled` (folded over the gesture:
     any unhandled ack → `false`; every published event acked handled → `true`;
     a short ack list → `null` = unknown, never "not handled").
   * frozen Rust VM: `AckRecord.handled` + `InputReport.handled` (the ack hook
     used to discard it), so `acos_tap/key/swipe` on that route report it too.
2. **`target` names what was there.** `acos_tap` replies and scenario `tap`
   step JSONs carry `target = {selector?, node, point, hit}`; `hit` is the
   tree-geometry hit-test at the injected point (`node`, `clickable`,
   `consumer` = nearest clickable ancestor-or-self, `suggest` = nearest
   clickable **sibling**, which wins over a nearer clickable node in another
   subtree). No extra traversal: the tree the selector resolved against is
   reused.
3. **`handled_hint` explains the combination** in one line and only from that
   evidence: "nothing tappable there … tap the nearest clickable
   android.widget.Switch … instead", "the point is on a clickable control but
   the window did not consume it (disabled/intercepted)", "consumed but no
   change (async handler or unchanged state)", "unknown (no ack)".
4. **`expect_change` is explicit.** A `tap` step's `input` block says
   `expect_change: "auto"` (its own `state_change` assertion always runs), or
   `true` when the scenario asked. `input.handled = false` passes with an
   `input_delivery` assertion, never a claimed change.
5. **A failed settle keeps the evidence.** A tap nothing consumed produces no
   new frame, so its strict settle times out (settle semantics are REL3A's);
   the step now still carries `input.handled: false`, the target/hit-test and
   the hint instead of an empty block.
6. **Docs.** `docs/AGENT_GUIDE.md` workflow topic (the `handled` contract, the
   four cases, the `target` block and the `expect_change: auto` rule) and
   `docs/MCP.md` §1d (same contract for API readers).

## Acceptance evidence

| Item | Evidence |
|---|---|
| reply shape unit tests | `cargo test -p acos-cli --bin acos -- rel3e` → 4 passed (`rel3e_tap_target_names_the_hit_and_the_clickable_sibling`, `rel3e_handled_true_with_or_without_a_change`, `rel3e_unknown_handling_is_reported_as_unknown`, `rel3e_reply_carries_handled_and_evidence`) |
| hit-test unit test | `cargo test -p acos-protocol --lib tap_hit` → `ui::tests::tap_hit_finds_the_deepest_node_and_prefers_a_clickable_sibling` |
| step JSON unit test | `cargo test -p acos-verify --lib rel3e` → `rel3e_tap_step_reports_handled_target_and_expect_change_auto` |
| crates green | `cargo test -p acos-protocol -p acos-verify -p acos-services -p acos-runtime -p acos-mcp` → 103 `test result: ok`, 0 failed |
| guide test | `ACOS_BIN=…/release/acos cargo test -p acos-mcp --test guide` → 11 passed |
| real ART, MCP replies | `results/handoff/REL3E/art-test.log` — `tap_on_a_label_is_reported_not_handled_and_names_the_switch ... ok` (label: `handled:false`, `suggest` = the `toggle` switch, hint names it; button: `handled:true`, `state_changed:true`, counter = 1; `acos_swipe` stays a structured `UNSUPPORTED_BACKEND`) |
| real ART, verify step JSON | `results/handoff/REL3E/verify/` — verdict `pass` (4 steps), `steps/01-tap.json` `input.handled=false`, `expect_change:"auto"`, `handled_evidence.unhandled:[3,4]`, `target.hit.node` the `TextView`, `target.hit.consumer=null`, `target.hit.suggest` the `Switch`, `handled_hint` names the switch; `steps/02-tap.json` `input.handled=true`, `input.handled_evidence.unhandled:[]`, `state_change` assertion passes |
| fixture | `fixtures/apps/tap-targets/` (`ACOS_APP_ONLY=tap-targets tools/build-apps.sh`); added to `tools/build-apps.sh` |

Commands used (shared lock + guard, per COMMON_RULES):

```
tools/art-runtime/run-exclusive.sh --task REL3E -- tools/guarded.sh --mem 10G --time 600 -- \
  env CARGO_TARGET_DIR=$PWD/.cache/rel3e-target \
  cargo test -p acos-cli --release --offline --test rel3e_handled -- --ignored --test-threads=1

tools/art-runtime/run-exclusive.sh --task REL3E -- tools/guarded.sh --mem 10G --time 600 -- \
  env -u ACOS_HOME ACOS_ART_BIN=$PWD/.cache/rel3e-target/release/acos-art \
  .cache/rel3e-target/release/acos verify results/handoff/REL3E/rel3e-handled.yaml \
  --artifacts results/handoff/REL3E/verify --engine art
```

## Known gaps / for the orchestrator

1. **ART swipe is still `UNSUPPORTED_BACKEND`.** REL3E's `handled` needs a real
   delivery; ART's input channel has no swipe publisher, so there is no bit to
   report and the verb stays a structured refusal (never a fake send). The
   gestures that *are* delivered on ART (tap/key/text) and the frozen VM's
   `swipe` both report `handled`. Adding swipe to ART is its own feature
   (MotionEvent interpolation + settle) and touches REL3A's settle track.
2. **A no-op tap is a `TIMEOUT` in verify, not a pass.** The strict settle has
   no draw to bind (`no-draw-after-action`); that is REL3A's rule and unchanged.
   The step now explains it (`input.handled: false` + the switch hint), and the
   REL3E scenario uses `settle: content` for that step to keep the run green.
   REL3A may want "unhandled input ⇒ settled" as a rule.
3. `crates/acos-cli/src/view/mod.rs` (2 tests) and `tests/fb1_live_input.rs`
   (1 test) fail in this worktree on sockets (`path must be shorter than
   SUN_LEN` / "serve socket never appeared"). Both are the deep-worktree path
   length, on files this branch does not touch; the same tests pass in a short
   checkout. Not caused by REL3E.

Peak RAM observed: ~8.1 GiB during the release build (guarded 12G); ART run
~700 MiB. Disk: `/` at 79%, this worktree's `.cache/rel3e-target` ≈ 12 GiB
(delete when the branch is merged).
