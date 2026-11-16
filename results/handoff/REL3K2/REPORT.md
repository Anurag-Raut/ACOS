# REL3K2 (v0.3.0) — the last Back items: dialog/IME layering, MCP-on-ART, the red launch test

Branch `rel3k2` off `rel-030` (`8cc9d8c0`), worktree
`.t3/worktrees/acos/t3-4fdffa58-8ad0395b820a231e`. All ART runs through
`tools/art-runtime/run-exclusive.sh` + `tools/guarded.sh`; every build in
`$WT/.cache/rel3k2-target` (release) with `CARGO_PROFILE_DEBUG=0`,
`CARGO_INCREMENTAL=0`.

Commits: `7da45fcb` (reply cap keeps the ladder), `bb2662dc` (back-layers
fixture + scenario + this evidence).

## 1. Status

| item | state |
|---|---|
| 1. Back closes the dialog first, then the IME first | **PARTIAL** — dialog half implemented and green on real ART (`scenarios/back-layers.yaml`, 15 steps, pass). IME half **not implemented**: measured divergence + root cause + options below (§3). |
| 2. One live MCP-on-ART `acos_key {code:"back"}` run | **DONE** — 13 checks pass, transcript committed (§4). |
| 3. The red `native_launch` test | **DONE** — archaeology, product fix, test green, fail-before/pass-after logs (§5). |

## 2. Item 1 — what the dialog Back does now (green)

New fixture `fixtures/apps/back-layers` (registered in `tools/build-apps.sh`,
built with `ACOS_APP_ONLY=back-layers`): one Activity with an `AlertDialog`
button and an `EditText`; tag `AcosBack` logs `dialog_shown`,
`dialog_dismissed`, `input_focus=… ime accepting=… active=…`,
`dispatchKeyBack action=0/1`, `onBackPressed`, `onResume/onPause/onDestroy`.

`scenarios/back-layers.yaml` (real ART, `--engine art`):

1. launch, dialog present (`message` = "Are you sure?", `button1` = OK);
2. **Back** → `assert_not_visible` on the dialog's `message` **and** `button1`,
   `assert_text` on the Activity's `dialog` ("Show dialog") and `status`
   ("Idle"), and an ordered log `dialog_shown → dialog_dismissed`;
3. **Back** → `assert_no_crash`, `assert_not_visible id=dialog`, ordered log
   `dispatchKeyBack action=0 → action=1 → onBackPressed → onPause → onDestroy`.

That is AOSP's order (`Dialog.onBackPressed` → `cancel()` → `dismiss()`; the
Activity below is neither paused nor finished). The ordered log is also the
proof that the *dialog's* Back never reached the Activity: `onPause` can only
appear after the *second* Back's `onBackPressed`.

Evidence: `artifacts/verdict.json` (pass), `artifacts/steps/00..14*.json`,
`artifacts/logs/logcat.txt`, `artifacts/frames/`.

One honest note about the scenario, recorded in its comment: the dialog-dismiss
Back uses `key: {key: back, settle: content}` instead of `back: {}`, because the
strict event settle cannot bind that press (`reason: no-draw-after-action`,
`pixels_changed: true`, `frame_seq: 4`) — see §3b, which is a product bug, not a
scenario setting. The claim asserted there is a tree claim, and the content
rule is the tree rule.

## 3. Item 1 — the IME half: measured, root cause, options

### 3a. What happens today (evidence in `back-layers-probe/`)

With the EditText focused (`input_focus=true ime accepting=true active=true` —
the framework's `InputMethodManager` reports a bound, accepting IME), the next
Back:

* reaches the **Activity's** window (`dispatchKeyBack action=0`, `action=1`),
* runs `onBackPressed`, `onPause`, `onDestroy` — **the task finishes**,
* the step reply is `handled: true`, `settle.bound: "window-gone"`,
  `state_changed: true`, and the tree afterwards is empty.

On a phone the same Back hides the keyboard and the Activity stays resumed.

**Root cause (code, not a guess):** the `InputBindResult` ACOS answers
`IInputMethodManager.startInputOrWindowGainedFocus` with has `channel: None`
(`crates/acos-wm/src/ime.rs`, `InputBindResult::success`; the same in
`crates/acos-services/src/ime.rs`). In `InputMethodManager.dispatchInputEvent`
the IME is only offered a key when `mCurChannel != null`
(`frameworks/base/.../inputmethod/InputMethodManager.java`, `sendInputEventOnMainLooperLocked`);
with no channel it returns `DISPATCH_NOT_HANDLED`, `ViewRootImpl.ImeInputStage`
forwards, and the key goes to the view tree. AOSP's `InputMethodService`
(`onKeyDown`/`onKeyUp` → `requestHideSelf`, reporting the key handled) is
therefore unreachable, and there is no soft-input surface to hide either
(`crates/acos-services/src/ime.rs` documents the null-IME V1 divergence).

### 3b. Second defect the scenario walked into (same subsystem)

The dialog-handling Back cannot bind the strict event settle:
`reason: "no-draw-after-action"`, `pixels_changed: true`, `frame_ms: null`,
`handled: null`, `acked: 0`. Two parts:

* a `finishInputEvent(handled)` is never sent for a key that *destroys* the
  window it was delivered to (the dialog's window), so the reply under-claims
  `handled` (the same under-claim class REL3K listed for the root Back);
* the draw that removes the dialog is not attributed to the press
  (`no-draw-after-action`), so a correct screen change cannot settle strictly.

### 3c. Why this is an orchestrator decision, not something I worked around

An honest "IME hides first" needs an IME end of an `InputChannel` plus a key
handler that hides the soft input and reports the key handled — i.e. ACOS's
in-process IME manager grows the `InputMethodService` key path, and the bind
result starts handing a real channel to the guest. That is a new guest/host
input surface (the plan's §3.2.2 layer stack and the null-IME divergence in
`crates/acos-services/src/ime.rs` both sit above it), and it is exactly the kind
of "moving work host-side/guest-side" change the brief says to stop on.

Options, smallest first:

1. **Channel + IME key handler (recommended, ~1 session of work):** create an
   `InputChannel` pair when the bind succeeds (`InputBindResult::success`),
   keep the server end in the IME manager, read the guest's key parcels from
   it, and on `KEYCODE_BACK` hide the soft input and publish
   `FINISHED(handled=true)` for the sequence. Bonus: the same channel makes
   IME text input (composing text) path-exact later.
2. **Dispatcher-level hide rule (half solution, reject):** let the daemon hide
   the IME when a Back arrives and report handled without involving the guest.
   Cheaper, but the app's `InputMethodManager`/insets state would disagree with
   the framework and it is not AOSP's path.
3. **Declare it:** keep the divergence, assert the *documented* behaviour in a
   `negative/` scenario and say in the guide that Back with the soft input up
   finishes the task on ACOS. Honest, but a v0.3.0 fidelity gap an agent will
   hit.

Fix 3b alongside option 1: finish the input event of a window that is removed
while holding one, and attribute the removal draw to the press.

## 4. Item 2 — live MCP-on-ART Back run (green)

`results/handoff/REL3K2/mcp_art_back.py` (uses `tools/release/mcp_smoke.py`'s
MCP stdio client) against this build's `acos-mcp`, which starts its own private
`acos serve --engine art` daemon. Transcript: `mcp-art-back.jsonl` (every
JSON-RPC frame, one `{"check": …}` line per assertion).

| check | result |
|---|---|
| `initialize`, `acos_key` advertised | ok |
| `acos_launch` (multi-activity) | `ok:true`, `result.engine: "art"`, `stage_reached: "first_frame"` |
| `acos_observe_tree` | `["Message","Send","Multi Activity"]` |
| `acos_tap {id:"send"}` → second Activity | `["Received: ","Finish","Multi Activity"]` |
| **`acos_key {code:"back"}`** | `result.engine: "art"`, **`handled: true`**, `handled_evidence {acked:2,published:2,source:"finishInputEvent"}`, `settle.bound: "strict-event"` (`frame_ms:139`), no task finish |
| `acos_observe_tree` after it | `["Message","Send","Multi Activity"]` — **the first Activity is shown again**, `Received:` gone |
| **second `acos_key {code:"back"}`** | `settle.bound: "window-gone"` — **the task finished** |
| `acos_destroy` | ok |

## 5. Item 3 — the red `native_launch` test (green)

**Which commit changed it:** `41566095` *REL3C: one compact, capped reply rule
for MCP and CLI JSON*. The ladder builder (`ladder_json` in
`crates/acos-cli/src/launch.rs`) and the test are older and unchanged in the
window (`git log -- crates/acos-cli/tests/native_launch.rs`,
`-- crates/acos-cli/src/launch.rs`); `git log -S"pub fn enforce_cap_pretty"`
returns exactly `41566095`, and `crates/acos-protocol/src/reply.rs` does not
exist at `41566095^`. Full transcript: `ARCHAEOLOGY.md`.

**Mechanism:** the reply was **not** a bare top-level error — it was the
envelope with `error.code=UNSUPPORTED_JAVA_API` **and** `result` replaced by
`{reply_capped, cap_bytes, note}` (20 KiB cap). The launch ladder is 72,292
bytes because the Rust VM's blocked native call repeats the same 13,463-byte
blocker text (java-tail + native-tail) on all five unproven rungs; cap step 3
threw the ladder away with it.

**Fix (`7da45fcb`, `crates/acos-protocol/src/reply.rs`):** the cap now bounds
*content* before it drops *structure* — every string over `CAP_STRING_BYTES`
keeps its head and line-aligned tail plus a marker with the byte counts; a
repeat of an already-bounded string becomes a back-reference that keeps its
first line; the per-string budget halves until the reply fits or reaches
`CAP_STRING_FLOOR`. Wholesale replacement remains for genuinely unshrinkable
results (many small strings), and the REL3C test that asserted replacement for
one huge string was rewritten to the new contract (the pointer-for-junk case
now has its own test with many small strings, so nothing was weakened).

**Result** (`launch-ladder-after-fix.summary.txt`): `status unsupported`,
`error.code UNSUPPORTED_JAVA_API`, `reached_stage=bind_application`, 8 rungs,
`application_on_create blocked` with `blocker.code=UNSUPPORTED_JAVA_API` and the
`--native` hint in the message, **8,783 bytes** total.

Tests: `native-launch-fail-before.log` (FAILED, 3 passed 1 failed, before the
fix) vs `native-launch-pass-after.log` (ok, 4 passed); `mcp-protocol-tests.log`
(`-p acos-mcp -p acos-protocol`, all targets green — the cap also runs on the
MCP path after the string bound).

## 6. What remains / smallest next actions

1. §3c option 1 (IME input channel + Back handler) — the only missing piece of
   item 1; then extend `scenarios/back-layers.yaml` with the IME steps
   (`tap input`, `key back` → `assert_text status Idle`, IME hidden) and a third
   Back for the task finish.
2. §3b: finish the input event of a window removed mid-press and attribute the
   removal draw to the press, so `back: {}` can carry the dialog claim with the
   strict rule instead of `settle: content`.
3. `scenarios/back-layers.yaml` prints the pre-existing
   `SCENARIO_ENGINE_MISMATCH` warning (`engine: native-preferred` + `--engine
   art`), like every other scenario in this repo.

## 7. Resources

Peak `guarded.sh` arm: `--mem 14G`; host MemAvailable stayed > 40 GiB
(`free -g` showed 47 GiB available at the busiest point). Disk `/` went 82% →
85% during the session while other agents were building (my own
`$WT/.cache/rel3k2-target` peaked at 1.9 GiB and is deleted at the end; the
committed evidence is < 1 MiB). No `pkill`/`pgrep -f`, no `~/.local`, no other
worktree touched; every process I started was killed by pid.
